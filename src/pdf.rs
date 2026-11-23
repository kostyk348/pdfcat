//! Адаптер извлечения текста PDF: собственный парсер, ноль зависимостей.
//!
//! Покрытое подмножество (честно, документировано):
//! - классический xref-таблицы и надёжный fallback-скан «N G obj»;
//! - страницы через /Root → /Pages → /Kids;
//! - потоки без фильтра и /FlateDecode (свой inflate);
//! - строки: literal `(...)` и hex `<...>`, все экранирования;
//! - шрифты: простые (WinAnsi/PDFDocEncoding) и Type0/Identity-H через
//!   /ToUnicode (bfchar + bfrange) — двухбайтовые CID → Unicode;
//! - операторы текста: BT/ET Tf Tj TJ ' " Td TD Tm T*.
//!
//! НЕ поддерживается (ошибка Unsupported, документ пропускается):
//! xref-потоки/объектные потоки PDF 1.5+, /Differences-кодировки, RunLength/LZW,
//! шрифты без /ToUnicode с не-WinAnsi кодировкой, фото-OCR (это не векторный текст).

use crate::inflate::zlib_decompress;
use crate::ports::{ExtractError, Extracted, TextExtractor};
use std::collections::HashMap;

const MAX_PAGES: usize = 10_000;
const MAX_STREAM: usize = 256 * 1024 * 1024;
const MAX_CMAP: usize = 16 * 1024 * 1024;

// ---------------------------------------------------------------- Values

#[derive(Debug, Clone)]
pub(crate) enum Value {
    Num(f64),
    Ref(u32, u32),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Array(Vec<Value>),
    Dict(HashMap<Vec<u8>, Value>),
    Bool(bool),
    Null,
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    depth: u32,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0C | 0x00)
}

fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, depth: 0 }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while self.pos < self.data.len() && self.data[self.pos] != b'\n' && self.data[self.pos] != b'\r' {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn parse(&mut self) -> Result<Value, ExtractError> {
        self.depth += 1;
        if self.depth > 64 {
            return Err(ExtractError::Unsupported("nesting depth"));
        }
        self.skip_ws();
        let v = self.parse_inner();
        self.depth -= 1;
        v
    }

    fn parse_inner(&mut self) -> Result<Value, ExtractError> {
        if self.pos >= self.data.len() {
            return Err(ExtractError::Unsupported("eof in object"));
        }
        let b = self.data[self.pos];
        match b {
            b'/' => {
                self.pos += 1;
                let start = self.pos;
                while self.pos < self.data.len() {
                    let c = self.data[self.pos];
                    if is_ws(c) || is_delim(c) {
                        break;
                    }
                    self.pos += 1;
                }
                // Имена с #XX — раскодируем
                Ok(Value::Name(decode_name(&self.data[start..self.pos])))
            }
            b'(' => self.parse_literal(),
            b'<' if self.data.get(self.pos + 1) == Some(&b'<') => self.parse_dict(),
            b'<' => self.parse_hex(),
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.pos >= self.data.len() || self.data[self.pos] == b']' {
                        break;
                    }
                    items.push(self.parse()?);
                }
                self.pos = (self.pos + 1).min(self.data.len());
                Ok(Value::Array(items))
            }
            _ => {
                // Число, ref, true/false/null или ключевое слово (obj/endobj/stream)
                let start = self.pos;
                while self.pos < self.data.len() && !is_ws(self.data[self.pos]) && !is_delim(self.data[self.pos]) {
                    self.pos += 1;
                }
                let tok = &self.data[start..self.pos];
                let s = std::str::from_utf8(tok).unwrap_or("");
                if s == "true" {
                    return Ok(Value::Bool(true));
                }
                if s == "false" || s == "null" {
                    return Ok(Value::Null);
                }
                // Ref: N G R — читаем дальше
                self.skip_ws();
                let start2 = self.pos;
                while self.pos < self.data.len() && !is_ws(self.data[self.pos]) && !is_delim(self.data[self.pos]) {
                    self.pos += 1;
                }
                let gen_s = std::str::from_utf8(&self.data[start2..self.pos]).unwrap_or("");
                self.skip_ws();
                if self.data.get(self.pos) == Some(&b'R') && is_int(s) && is_int(gen_s) {
                    self.pos += 1;
                    return Ok(Value::Ref(parse_u(s), parse_u(gen_s)));
                }
                // Возврат позиции, если это не ref (число или слово)
                self.pos = start2;
                Ok(Value::Num(s.parse::<f64>().unwrap_or(f64::NAN)))
            }
        }
    }

    fn parse_literal(&mut self) -> Result<Value, ExtractError> {
        self.pos += 1; // (
        let mut out = Vec::new();
        let mut depth = 1;
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            self.pos += 1;
            match b {
                b'\\' => {
                    if self.pos >= self.data.len() {
                        break;
                    }
                    let e = self.data[self.pos];
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'(' => out.push(b'('),
                        b')' => out.push(b')'),
                        b'\\' => out.push(b'\\'),
                        b'0'..=b'7' => {
                            let mut v: u32 = u32::from(e - b'0');
                            let mut n = 1;
                            while n < 3 && self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() && self.data[self.pos] < b'8' {
                                v = v * 8 + u32::from(self.data[self.pos] - b'0');
                                self.pos += 1;
                                n += 1;
                            }
                            out.push((v & 0xFF) as u8);
                        }
                        b'\r' => {
                            if self.data.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            } // перенос строки
                        }
                        b'\n' => {}
                        _ => out.push(e),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(b'(');
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(Value::Str(out));
                    }
                    out.push(b')');
                }
                _ => out.push(b),
            }
        }
        Err(ExtractError::Unsupported("unterminated string"))
    }

    fn parse_hex(&mut self) -> Result<Value, ExtractError> {
        self.pos += 1; // <
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while self.pos < self.data.len() && self.data[self.pos] != b'>' {
            let c = self.data[self.pos];
            self.pos += 1;
            let d = match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                b'A'..=b'F' => Some(c - b'A' + 10),
                _ => None, // пробелы внутри hex допустимы
            };
            if let Some(d) = d {
                match hi.take() {
                    None => hi = Some(d),
                    Some(h) => {
                        out.push(h * 16 + d);
                    }
                }
            }
        }
        if let Some(h) = hi {
            out.push(h * 16);
        }
        self.pos = (self.pos + 1).min(self.data.len());
        Ok(Value::Str(out))
    }

    fn parse_dict(&mut self) -> Result<Value, ExtractError> {
        self.pos += 2; // <<
        let mut map = HashMap::new();
        loop {
            self.skip_ws();
            if self.pos >= self.data.len() {
                return Err(ExtractError::Unsupported("unterminated dict"));
            }
            if self.data[self.pos] == b'>' && self.data.get(self.pos + 1) == Some(&b'>') {
                self.pos += 2;
                return Ok(Value::Dict(map));
            }
            if self.data[self.pos] != b'/' {
                return Err(ExtractError::Unsupported("dict key expected"));
            }
            let key = match self.parse()? {
                Value::Name(n) => n,
                _ => return Err(ExtractError::Unsupported("dict key expected")),
            };
            let val = self.parse()?;
            // «stream» после значения — не часть словаря, вернём позицию через наружный вызов
            map.insert(key, val);
        }
    }
}

fn decode_name(raw: &[u8]) -> Vec<u8> {
    if !raw.contains(&b'#') {
        return raw.to_vec();
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#' && i + 2 < raw.len() {
            let h = hex_val(raw[i + 1]);
            let l = hex_val(raw[i + 2]);
            if let (Some(h), Some(l)) = (h, l) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn is_int(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse_u(s: &str) -> u32 {
    s.parse().unwrap_or(0)
}

// ---------------------------------------------------------------- WinAnsi (CP1252) → Unicode

const WINANSI: [u16; 128] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0,
    0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
    0x00A0, 0x00A1, 0x00A2, 0x00A3, 0x00A4, 0x00A5, 0x00A6, 0x00A7, 0x00A8, 0x00A9, 0x00AA, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x00AF,
    0x00B0, 0x00B1, 0x00B2, 0x00B3, 0x00B4, 0x00B5, 0x00B6, 0x00B7, 0x00B8, 0x00B9, 0x00BA, 0x00BB, 0x00BC, 0x00BD, 0x00BE, 0x00BF,
    0x00C0, 0x00C1, 0x00C2, 0x00C3, 0x00C4, 0x00C5, 0x00C6, 0x00C7, 0x00C8, 0x00C9, 0x00CA, 0x00CB, 0x00CC, 0x00CD, 0x00CE, 0x00CF,
    0x00D0, 0x00D1, 0x00D2, 0x00D3, 0x00D4, 0x00D5, 0x00D6, 0x00D7, 0x00D8, 0x00D9, 0x00DA, 0x00DB, 0x00DC, 0x00DD, 0x00DE, 0x00DF,
    0x00E0, 0x00E1, 0x00E2, 0x00E3, 0x00E4, 0x00E5, 0x00E6, 0x00E7, 0x00E8, 0x00E9, 0x00EA, 0x00EB, 0x00EC, 0x00ED, 0x00EE, 0x00EF,
    0x00F0, 0x00F1, 0x00F2, 0x00F3, 0x00F4, 0x00F5, 0x00F6, 0x00F7, 0x00F8, 0x00F9, 0x00FA, 0x00FB, 0x00FC, 0x00FD, 0x00FE, 0x00FF,
];

fn byte_to_unicode(b: u8) -> Option<char> {
    match b {
        0x00..=0x7F => char::from_u32(u32::from(b)),
        0x80..=0xFF => {
            let u = WINANSI[(b - 0x80) as usize];
            if u == 0 {
                None
            } else {
                char::from_u32(u32::from(u))
            }
        }
    }
}

// ---------------------------------------------------------------- Объекты и документ

pub struct PdfDoc {
    data: Vec<u8>,
    /// objnum → смещение «N G obj».
    offsets: HashMap<u32, usize>,
}

impl PdfDoc {
    pub fn load(path: &str) -> Result<Self, ExtractError> {
        let data = std::fs::read(path).map_err(|_| ExtractError::NotFound)?;
        if !data.starts_with(b"%PDF") {
            // Некоторые PDF начинаются с мусора до %PDF — ищем подпись в первых 2 КБ
            let head = &data[..data.len().min(2048)];
            let off = find(head, b"%PDF").ok_or(ExtractError::NotPdf)?;
            return Ok(Self { data: data[off..].to_vec(), offsets: HashMap::new() });
        }
        Ok(Self { data, offsets: HashMap::new() })
    }

    /// Fallback-скан: все «N G obj». Работает и без xref; O(размер файла).
    /// Смещение сохраняется НА начало словаря (после слова obj).
    pub(crate) fn build_index(&mut self) {
        if !self.offsets.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.data);
        let mut i = 0;
        while i + 4 <= data.len() {
            if &data[i..i + 3] == b"obj" {
                // Назад: пробелы, gen (цифры), пробелы, номер (цифры)
                let mut p = i;
                while p > 0 && is_ws(data[p - 1]) {
                    p -= 1;
                }
                let gen_end = p;
                while p > 0 && data[p - 1].is_ascii_digit() {
                    p -= 1;
                }
                let gen_s = &data[p..gen_end];
                if p > 0 && !gen_s.is_empty() {
                    while p > 0 && is_ws(data[p - 1]) {
                        p -= 1;
                    }
                    let num_end = p;
                    while p > 0 && data[p - 1].is_ascii_digit() {
                        p -= 1;
                    }
                    let num_s = &data[p..num_end];
                    if let (Ok(num), Ok(_gen)) = (atoi(num_s), atoi(gen_s)) {
                        // Тело после "obj": пропускаем пробелы до первого слеша/скобки
                        let mut body = i + 3;
                        while body < data.len() && is_ws(data[body]) {
                            body += 1;
                        }
                        self.offsets.insert(num, body);
                        i += 3;
                        continue;
                    }
                }
                i += 3;
            } else {
                i += 1;
            }
        }
        self.data = data;
    }

    fn obj_start(&self, num: u32) -> Option<usize> {
        self.offsets.get(&num).copied()
    }

    pub(crate) fn obj_body(&self, num: u32) -> Option<(usize, usize)> {
        let start = self.obj_start(num)?;
        let end = find(&self.data[start..], b"endobj")
            .map(|e| start + e)
            .unwrap_or(self.data.len());
        Some((start, end))
    }

    /// Распакованный поток объекта (FlateDecode или сырой).
    pub(crate) fn stream_of(&self, num: u32) -> Result<Option<Vec<u8>>, ExtractError> {
        let Some((start, end)) = self.obj_body(num) else {
            return Ok(None);
        };
        let span = &self.data[start..end];
        let Some(srel) = find(span, b"stream") else {
            return Ok(None);
        };
        let dict_span = &span[..srel];
        let mut p = Parser::new(dict_span);
        let mut dict = match p.parse()? {
            Value::Dict(d) => d,
            _ => return Ok(None),
        };
        // начало данных: после слова stream + ровно один EOL (CR / LF / CRLF)
        let mut ds = srel + 6;
        if span.get(srel + 6) == Some(&b'\r') {
            ds += 1;
            if span.get(srel + 7) == Some(&b'\n') {
                ds += 1;
            }
        } else if span.get(srel + 6) == Some(&b'\n') {
            ds += 1;
        }
        // Длина потока: /Length честнее поиска endstream — сжатые данные могут
        // содержать байты 0x0A/0x0D и даже сам маркер «endstream».
        let len_opt = match dict.get(&b"Length".to_vec()) {
            Some(Value::Num(n)) if n.is_finite() && *n >= 0.0 => Some(*n as usize),
            Some(Value::Ref(n, _)) => {
                // Косвенная длина: объект N содержит /Length
                match self.obj_dict(*n) {
                    Some(d) => match d.get(&b"Length".to_vec()) {
                        Some(Value::Num(v)) if v.is_finite() && *v >= 0.0 => Some(*v as usize),
                        _ => None,
                    },
                    None => None,
                }
            }
            _ => None,
        };
        let de = if let Some(len) = len_opt {
            // Длина правдоподобна и указывает на endstream — верим ей
            let cand = ds + len;
            if cand + 9 <= span.len() && &span[cand..cand + 9] == b"endstream" {
                cand
            } else if cand + 11 <= span.len()
                && (&span[cand..cand + 11] == b"\r\nendstream" || &span[cand..cand + 11] == b"\n\nendstream")
            {
                cand
            } else if cand + 10 <= span.len() && &span[cand..cand + 10] == b"\nendstream" {
                cand
            } else {
                // /Length не сходится — fallback на поиск маркера
                let e_rel = find(&span[ds..], b"endstream").ok_or(ExtractError::Unsupported("no endstream"))?;
                let mut de = ds + e_rel;
                if de >= ds + 2 && span[de - 2] == b'\r' && span[de - 1] == b'\n' { de -= 2; }
                else if de > ds && (span[de - 1] == b'\n' || span[de - 1] == b'\r') { de -= 1; }
                de
            }
        } else {
            let e_rel = find(&span[ds..], b"endstream").ok_or(ExtractError::Unsupported("no endstream"))?;
            let mut de = ds + e_rel;
            if de >= ds + 2 && span[de - 2] == b'\r' && span[de - 1] == b'\n' { de -= 2; }
            else if de > ds && (span[de - 1] == b'\n' || span[de - 1] == b'\r') { de -= 1; }
            de
        };
        if de < ds || de - ds > MAX_STREAM {
            return Err(ExtractError::TooLarge((de.saturating_sub(ds)) as u64));
        }
        let raw = &span[ds..de];
        let filtered = match dict.remove(&b"Filter".to_vec()) {
            Some(Value::Name(n)) if n == b"FlateDecode" => zlib_decompress(raw)
                .map_err(|_| ExtractError::Unsupported("flate"))?,
            Some(Value::Array(items)) => {
                let mut out = raw.to_vec();
                for it in items {
                    if let Value::Name(n) = it {
                        if n == b"FlateDecode" {
                            out = zlib_decompress(&out).map_err(|_| ExtractError::Unsupported("flate"))?;
                        } else {
                            return Err(ExtractError::Unsupported("filter"));
                        }
                    }
                }
                out
            }
            None => raw.to_vec(),
            Some(_) => return Err(ExtractError::Unsupported("filter")),
        };
        let _ = dict;
        Ok(Some(filtered))
    }

    pub(crate) fn obj_dict(&self, num: u32) -> Option<HashMap<Vec<u8>, Value>> {
        let (start, end) = self.obj_body(num)?;
        let mut p = Parser::new(&self.data[start..end]);
        match p.parse() {
            Ok(Value::Dict(d)) => Some(d),
            _ => None,
        }
    }

    pub(crate) fn resolve_ref(&self, v: &Value) -> Option<u32> {
        if let Value::Ref(n, _) = v {
            Some(*n)
        } else {
            None
        }
    }

    /// Классический xref / trailer → objnum /Root; fallback: скан b"/Catalog".
    pub(crate) fn root_obj(&mut self) -> Result<u32, ExtractError> {
        let tail_start = self.data.len().saturating_sub(4096);
        let tail = &self.data[tail_start..];
        if let Some(x) = rfind(tail, b"startxref") {
            let s = std::str::from_utf8(&tail[x + 9..]).unwrap_or("");
            let num: u32 = s
                .split_whitespace()
                .next()
                .and_then(|t| t.parse().ok())
                .unwrap_or(0);
            if num > 0 && (num as usize) < self.data.len() {
                let span = &self.data[num as usize..];
                if let Some(trel) = find(span, b"trailer") {
                    // Пропускаем слово trailer
                    let mut after = trel + 7;
                    while after < span.len() && is_ws(span[after]) {
                        after += 1;
                    }
                    let mut p = Parser::new(&span[after..]);
                    if let Ok(Value::Dict(d)) = p.parse() {
                        if let Some(r) = d.get(&b"Root".to_vec()) {
                            if let Some(n) = self.resolve_ref(r) {
                                return Ok(n);
                            }
                        }
                    }
                }
                // /Type /XRef — потоковая xref (PDF 1.5+): честный отказ
                if find(span, b"/XRef").is_some() {
                    return Err(ExtractError::Unsupported("xref streams (PDF 1.5+)"));
                }
            }
        }
        // Fallback: ищем объект с /Catalog
        self.build_index();
        for (&num, _) in &self.offsets {
            if let Some(d) = self.obj_dict(num) {
                if matches!(d.get(&b"Type".to_vec()), Some(Value::Name(n)) if n == b"Catalog") {
                    return Ok(num);
                }
            }
        }
        Err(ExtractError::Unsupported("no /Catalog"))
    }
}

fn atoi(b: &[u8]) -> Result<u32, ()> {
    if b.is_empty() {
        return Err(());
    }
    let s = std::str::from_utf8(b).map_err(|_| ())?;
    s.trim().parse().map_err(|_| ())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let last = hay.len() - needle.len() + 1;
    let mut i = 0;
    while i < last {
        if hay[i] == needle[0] && &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    for i in (0..=hay.len() - needle.len()).rev() {
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
    }
    None
}

// ---------------------------------------------------------------- ToUnicode CMap

#[derive(Default, Clone)]
struct ToUnicode {
    /// CID/код → строка Unicode.
    map: HashMap<u32, String>,
    two_byte: bool,
}

fn parse_cmap(src: &[u8]) -> ToUnicode {
    let mut m = ToUnicode::default();
    // Размерность src: begincodespacerange <hex> <hex> — длина первого hex
    if let Some(cr) = find(src, b"begincodespacerange") {
        let span = &src[cr..];
        if let Some(o1) = find(span, b"<") {
            if let Some(o2) = find(&span[o1..], b">") {
                m.two_byte = o2 > 3; // между < и > больше 2 hex-цифр
            }
        }
    }
    let mut i = 0;
    while i + 11 <= src.len() {
        if &src[i..i + 11] == b"beginbfchar" {
            let end = find(&src[i..], b"endbfchar").map(|e| i + e).unwrap_or(src.len());
            parse_bfchar(&src[i..end], &mut m);
            i = end;
            continue;
        }
        if &src[i..i + 11] == b"beginbfrang" {
            let end = find(&src[i..], b"endbfrange").map(|e| i + e).unwrap_or(src.len());
            parse_bfrange(&src[i..end], &mut m);
            i = end;
            continue;
        }
        i += 1;
    }
    m
}

/// <src> <dstUTF16BE> в bfchar.
fn parse_bfchar(span: &[u8], m: &mut ToUnicode) {
    let mut i = 0;
    while i + 6 <= span.len() {
        if span[i] == b'<' {
            let close = find(&span[i..], b">").map(|e| i + e).unwrap_or(span.len());
            let src_hex = &span[i + 1..close];
            let src_len = if m.two_byte { 4 } else { 2 };
            let code = hex_to_u32(&src_hex[..src_hex.len().min(src_len)]);
            // dst: следующая <...>
            let mut j = close + 1;
            while j < span.len() && span[j] != b'<' {
                j += 1;
            }
            if j < span.len() {
                let c2 = find(&span[j..], b">").map(|e| j + e).unwrap_or(span.len());
                let dst_bytes = hex_decode_ascii(&span[j + 1..c2]);
                if let Some(ustr) = utf16be_to_string(&dst_bytes) {
                    m.map.insert(code, ustr);
                }
                i = c2;
                continue;
            }
        }
        i += 1;
    }
}

fn parse_bfrange(span: &[u8], m: &mut ToUnicode) {
    let mut i = 0;
    while i + 6 <= span.len() {
        if span[i] == b'<' {
            let c1 = find(&span[i..], b">").map(|e| i + e).unwrap_or(span.len());
            let src_len = if m.two_byte { 4 } else { 2 };
            let from = hex_to_u32(&span[i + 1..(i + 1 + src_len).min(c1)]);
            let mut j = c1 + 1;
            while j < span.len() && span[j] != b'<' {
                j += 1;
            }
            if j >= span.len() {
                return;
            }
            let c2 = find(&span[j..], b">").map(|e| j + e).unwrap_or(span.len());
            let to = hex_to_u32(&span[j + 1..(j + 1 + src_len).min(c2)]);
            let mut k = c2 + 1;
            while k < span.len() && span[k] != b'<' {
                k += 1;
            }
            if k >= span.len() {
                return;
            }
            let c3 = find(&span[k..], b">").map(|e| k + e).unwrap_or(span.len());
            let dst_bytes = hex_decode_ascii(&span[k + 1..c3]);
            if let Some(start_ustr) = utf16be_to_string(&dst_bytes) {
                // Массив назначений — <from> [<dst1> <dst2> ...]
                let after = &span[c3 + 1..(c3 + 1).min(span.len())];
                let arr_end = find(after, b"endbfrange").unwrap_or(after.len());
                let mut p = 0;
                while p < arr_end && is_ws(after[p]) {
                    p += 1;
                }
                let is_array = p < arr_end && after[p] == b'[';
                if is_array {
                    let mut d = 0;
                    while p < arr_end {
                        if after[p] == b'<' {
                            let ce = find(&after[p..], b">").map(|e| p + e).unwrap_or(arr_end);
                            let a_bytes = hex_decode_ascii(&after[p + 1..ce]);
                            if let Some(ustr) = utf16be_to_string(&a_bytes) {
                                m.map.insert(from + d, ustr);
                                d += 1;
                            }
                            p = ce;
                        }
                        p += 1;
                    }
                } else {
                    let start_cp = start_ustr.chars().next().map(|c| c as u32).unwrap_or(0);
                    let mut c = from;
                    let mut cp = start_cp;
                    while c <= to && to - from < 0xFFFF {
                        if let Some(ch) = char::from_u32(cp) {
                            m.map.insert(c, ch.to_string());
                        }
                        c += 1;
                        cp += 1;
                    }
                }
            }
            i = c3;
            continue;
        }
        i += 1;
    }
}

fn hex_to_u32(hex: &[u8]) -> u32 {
    let mut v = 0u32;
    for &h in hex {
        if let Some(d) = hex_val(h) {
            v = v * 16 + u32::from(d);
        }
    }
    v
}

/// Hex-текст CMap («0410») → байты [0x04, 0x10] (пробелы игнорируются).
fn hex_decode_ascii(hex: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hex.len() / 2 + 1);
    let mut hi: Option<u8> = None;
    for &c in hex {
        if let Some(d) = hex_val(c) {
            match hi.take() {
                None => hi = Some(d),
                Some(h) => out.push(h * 16 + d),
            }
        }
    }
    if let Some(h) = hi {
        out.push(h * 16);
    }
    out
}

fn utf16be_to_string(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() % 2 != 0 {
        return None;
    }
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        units.push(u16::from_be_bytes([pair[0], pair[1]]));
    }
    Some(String::from_utf16_lossy(&units))
}

// ---------------------------------------------------------------- Извлечение текста

#[derive(Clone)]
pub(crate) struct FontMap {
    pub two_byte: bool,
    pub cmap: ToUnicode,
}


/// Отладка: сырые контент-стримы страниц (для CLI `peek --raw`).
pub fn debug_contents(path: &str) -> Result<Vec<String>, ExtractError> {
    let mut doc = PdfDoc::load(path)?;
    doc.build_index();
    let root = doc.root_obj()?;
    let root_dict = doc.obj_dict(root).ok_or(ExtractError::Unsupported("no root dict"))?;
    let pages_ref = root_dict.get(&b"Pages".to_vec()).and_then(|v| doc.resolve_ref(v))
        .ok_or(ExtractError::Unsupported("no /Pages"))?;
    let mut pages = Vec::new();
    let mut stack = vec![(pages_ref, 0u32)];
    while let Some((num, depth)) = stack.pop() {
        if pages.len() >= MAX_PAGES || depth > 64 { break; }
        let Some(d) = doc.obj_dict(num) else { continue };
        match d.get(&b"Type".to_vec()) {
            Some(Value::Name(n)) if n == b"Pages" => {
                if let Some(Value::Array(kids)) = d.get(&b"Kids".to_vec()) {
                    for k in kids.iter().rev() {
                        if let Some(n) = doc.resolve_ref(k) { stack.push((n, depth + 1)); }
                    }
                }
            }
            _ => pages.push(num),
        }
    }
    let mut out = Vec::new();
    for &p in &pages {
        let Some(d) = doc.obj_dict(p) else { continue };
        let c = match d.get(&b"Contents".to_vec()) {
            Some(Value::Ref(n, _)) => doc.stream_of(*n)?.unwrap_or_default(),
            _ => Vec::new(),
        };
        out.push(String::from_utf8_lossy(&c).into_owned());
    }
    Ok(out)
}

pub struct PdfExtractor;

impl TextExtractor for PdfExtractor {
    fn extract(&self, path: &str, max_bytes: u64) -> Result<Extracted, ExtractError> {
        let meta = std::fs::metadata(path).map_err(|_| ExtractError::NotFound)?;
        if meta.len() > max_bytes {
            return Err(ExtractError::TooLarge(meta.len()));
        }
        let mut doc = PdfDoc::load(path)?;
        doc.build_index();
        let root = doc.root_obj()?;
        let root_dict = doc.obj_dict(root).ok_or(ExtractError::Unsupported("no root dict"))?;
        let pages_ref = root_dict
            .get(&b"Pages".to_vec())
            .and_then(|v| doc.resolve_ref(v))
            .ok_or(ExtractError::Unsupported("no /Pages"))?;

        let mut pages: Vec<u32> = Vec::new();
        let mut stack = vec![(pages_ref, 0u32)];
        while let Some((num, depth)) = stack.pop() {
            if pages.len() >= MAX_PAGES || depth > 64 {
                break;
            }
            let Some(d) = doc.obj_dict(num) else { continue };
            match d.get(&b"Type".to_vec()) {
                Some(Value::Name(n)) if n == b"Pages" => {
                    if let Some(Value::Array(kids)) = d.get(&b"Kids".to_vec()) {
                        for k in kids.iter().rev() {
                            if let Some(n) = doc.resolve_ref(k) {
                                stack.push((n, depth + 1));
                            }
                        }
                    }
                }
                _ => {
                    // Страница
                    pages.push(num);
                }
            }
        }

        let mut out = Extracted::default();
        for &p in &pages {
            let Some(d) = doc.obj_dict(p) else { continue };
            let fonts = page_fonts(&doc, p);
            // Контент: один поток или массив
            let mut content: Vec<u8> = Vec::new();
            match d.get(&b"Contents".to_vec()) {
                Some(Value::Ref(n, _)) => {
                    content = doc.stream_of(*n)?.unwrap_or_default();
                }
                Some(Value::Array(items)) => {
                    for it in items {
                        if let Value::Ref(n, _) = it {
                            if let Some(c) = doc.stream_of(*n)? {
                                content.extend_from_slice(&c);
                                content.push(b' ');
                            }
                        }
                    }
                }
                _ => {}
            }
            out.pages.push(extract_text_page(&content, &fonts));
        }
        Ok(out)
    }
}

pub(crate) fn decode_string_with_font(raw: &[u8], f: Option<&FontMap>) -> String {
    decode_bytes(raw, f)
}

/// Шрифты страницы: имя -> карта кодирования (ToUnicode/WinAnsi).
pub(crate) fn page_fonts(doc: &PdfDoc, page_obj: u32) -> HashMap<Vec<u8>, FontMap> {
    let mut fonts: HashMap<Vec<u8>, FontMap> = HashMap::new();
    let Some(d) = doc.obj_dict(page_obj) else { return fonts };
    let Some(res) = d.get(&b"Resources".to_vec()) else { return fonts };
    let res_dict = match res {
        Value::Dict(_) => Some(res.clone()),
        Value::Ref(n, _) => doc.obj_dict(*n).map(Value::Dict),
        _ => None,
    };
    if let Some(Value::Dict(rd)) = res_dict {
        if let Some(fv) = rd.get(&b"Font".to_vec()) {
            let font_dict = match fv {
                Value::Dict(_) => Some(fv.clone()),
                Value::Ref(n, _) => doc.obj_dict(*n).map(Value::Dict),
                _ => None,
            };
            if let Some(Value::Dict(fd)) = font_dict {
                for (name, fval) in &fd {
                    let Some(fnum) = (match fval {
                        Value::Ref(n, _) => Some(*n),
                        _ => None,
                    }) else { continue };
                    let Some(fdict) = doc.obj_dict(fnum) else { continue };
                    let subtype = match fdict.get(&b"Subtype".to_vec()) {
                        Some(Value::Name(n)) => n.clone(),
                        _ => Vec::new(),
                    };
                    let mut fm = FontMap { two_byte: subtype == b"Type0", cmap: ToUnicode::default() };
                    if let Some(tu) = fdict.get(&b"ToUnicode".to_vec()).and_then(|v| doc.resolve_ref(v)) {
                        if let Ok(Some(cmap_src)) = doc.stream_of(tu) {
                            if cmap_src.len() <= 16 * 1024 * 1024 {
                                fm.cmap = parse_cmap(&cmap_src);
                            }
                        }
                    }
                    fonts.insert(name.clone(), fm);
                }
            }
        }
    }
    fonts
}

/// Токенайзер контент-стрима: собирает Tj/TJ/'/" с декодированием по текущему шрифту.
fn extract_text_page(content: &[u8], fonts: &HashMap<Vec<u8>, FontMap>) -> String {
    let mut out = String::new();
    let mut i = 0;
    let mut in_text = false;
    let mut cur_font: Option<FontMap> = None;

    macro_rules! push_str {
        ($s:expr) => {{
            let s: &str = $s;
            if !s.is_empty() {
                if !out.is_empty() && !out.ends_with([' ', '\n']) {
                    out.push(' ');
                }
                out.push_str(s);
            }
        }};
    }

    while i < content.len() {
        let b = content[i];
        match b {
            b'(' => {
                let (s, ni) = read_literal(content, i);
                i = ni;
                if in_text {
                    let dec = decode_bytes(&s, cur_font.as_ref());
                    push_str!(dec.as_str());
                }
                continue;
            }
            b'<' => {
                if i + 1 < content.len() && content[i + 1] == b'<' {
                    i += skip_dict(content, i);
                    continue;
                }
                let (s, ni) = read_hex(content, i);
                i = ni;
                if in_text {
                    let dec = decode_bytes(&s, cur_font.as_ref());
                    push_str!(dec.as_str());
                }
                continue;
            }
            b'[' => {
                // TJ-массив: читаем до ]
                let (strs, ni) = read_array_strings(content, i);
                i = ni;
                if in_text {
                    for s in &strs {
                        let dec = decode_bytes(s, cur_font.as_ref());
                        push_str!(dec.as_str());
                    }
                }
                continue;
            }
            b']' | b'>' | b')' => {
                i += 1;
                continue;
            }
            _ => {}
        }
        // Слова/операторы
        if is_ws(b) {
            i += 1;
            continue;
        }
        let start = i;
        while i < content.len() && !is_ws(content[i]) && !matches!(content[i], b'(' | b'<' | b'[' | b')' | b'>' | b']') {
            i += 1;
        }
        let tok = &content[start..i];
        match tok {
            b"BT" => {
                in_text = true;
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            b"ET" => {
                in_text = false;
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            b"T*" | b"Td" | b"TD" | b"\"" | b"'" => {
                // Перевод строки в выводе — эвристика для читабельности
                if in_text && !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                if tok == b"\"" || tok == b"'" {
                    // Показ строки уже был прочитан выше как literal — ничего.
                }
            }
            b"Tf" => {
                // Токен ДО Tf — имя шрифта; вытащим из потока назад
                let name = last_name_before(content, start);
                cur_font = name.and_then(|n| fonts.get(&n).cloned());
            }
            _ => {}
        }
    }
    out.trim().to_string()
}

fn last_name_before(data: &[u8], pos: usize) -> Option<Vec<u8>> {
    // pos — после оператора (например Tf). Операнды стоят ПЕРЕД оператором:
    // «/F2 12 Tf» → идём назад через операнды, пока не встретим имя «/Имя».
    let mut p = pos;
    for _attempt in 0..4 {
        while p > 0 && is_ws(data[p - 1]) {
            p -= 1;
        }
        if p == 0 {
            return None;
        }
        let end = p;
        let mut s = end;
        while s > 0 && !is_ws(data[s - 1]) {
            s -= 1;
        }
        let tok = &data[s..end];
        if !tok.is_empty() && tok[0] == b'/' {
            return Some(decode_name(&tok[1..]));
        }
        if s > 0 && data[s - 1] == b'/' {
            return Some(decode_name(tok));
        }
        p = s; // это операнд-число: ищем дальше назад
    }
    None
}

fn skip_dict(data: &[u8], mut i: usize) -> usize {
    // << ... >> — просто перескок по счётчику вложенности
    let mut depth = 0;
    while i < data.len() {
        if data[i] == b'<' && data.get(i + 1) == Some(&b'<') {
            depth += 1;
            i += 2;
        } else if data[i] == b'>' && data.get(i + 1) == Some(&b'>') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    i
}

fn read_literal(data: &[u8], mut i: usize) -> (Vec<u8>, usize) {
    i += 1;
    let mut out = Vec::new();
    let mut depth = 1;
    while i < data.len() {
        let b = data[i];
        i += 1;
        match b {
            b'\\' => {
                if i >= data.len() {
                    break;
                }
                let e = data[i];
                i += 1;
                match e {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'(' => out.push(b'('),
                    b')' => out.push(b')'),
                    b'\\' => out.push(b'\\'),
                    b'0'..=b'7' => {
                        let mut v: u32 = u32::from(e - b'0');
                        let mut n = 1;
                        while n < 3 && i < data.len() && data[i].is_ascii_digit() && data[i] < b'8' {
                            v = v * 8 + u32::from(data[i] - b'0');
                            i += 1;
                            n += 1;
                        }
                        out.push((v & 0xFF) as u8);
                    }
                    b'\r' => {
                        if data.get(i) == Some(&b'\n') {
                            i += 1;
                        }
                    }
                    b'\n' => {}
                    _ => out.push(e),
                }
            }
            b'(' => {
                depth += 1;
                out.push(b'(');
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return (out, i);
                }
                out.push(b')');
            }
            _ => out.push(b),
        }
    }
    (out, i)
}

fn read_hex(data: &[u8], mut i: usize) -> (Vec<u8>, usize) {
    i += 1;
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    while i < data.len() && data[i] != b'>' {
        if let Some(d) = hex_val(data[i]) {
            match hi.take() {
                None => hi = Some(d),
                Some(h) => out.push(h * 16 + d),
            }
        }
        i += 1;
    }
    if let Some(h) = hi {
        out.push(h * 16);
    }
    (out, i + 1)
}

/// TJ-массив: достаём строки, игнорируя числа (кёрнинг).
fn read_array_strings(data: &[u8], mut i: usize) -> (Vec<Vec<u8>>, usize) {
    i += 1;
    let mut out = Vec::new();
    let mut depth = 1;
    while i < data.len() {
        match data[i] {
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return (out, i);
                }
            }
            b'(' => {
                let (s, ni) = read_literal(data, i);
                out.push(s);
                i = ni;
            }
            b'<' => {
                let (s, ni) = read_hex(data, i);
                out.push(s);
                i = ni;
            }
            _ => i += 1,
        }
    }
    (out, i)
}

fn decode_bytes(raw: &[u8], font: Option<&FontMap>) -> String {
    if let Some(f) = font {
        if f.two_byte {
            let mut s = String::new();
            for pair in raw.chunks(2) {
                let cid = if pair.len() == 2 {
                    u32::from(pair[0]) * 256 + u32::from(pair[1])
                } else {
                    u32::from(pair[0])
                };
                match f.cmap.map.get(&cid) {
                    Some(t) => s.push_str(t),
                    None => s.push('\u{FFFD}'),
                }
            }
            return s;
        }
        if !f.cmap.map.is_empty() {
            let mut s = String::new();
            for &b in raw {
                match f.cmap.map.get(&u32::from(b)) {
                    Some(t) => s.push_str(t),
                    None => {
                        if let Some(c) = byte_to_unicode(b) {
                            s.push(c);
                        }
                    }
                }
            }
            return s;
        }
    }
    let mut s = String::new();
    for &b in raw {
        if let Some(c) = byte_to_unicode(b) {
            s.push(c);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const FX: &str = "tests/fixtures/";

    #[test]
    fn simple_winansi() {
        let ex = PdfExtractor;
        let out = ex.extract(&format!("{FX}simple.pdf"), 1 << 20).unwrap();
        assert_eq!(out.pages.len(), 1);
        let t = &out.pages[0];
        assert!(t.contains("Boiler KEV spec and cables"), "got: {t}");
        assert!(t.contains("Akt 45"), "got: {t}");
    }

    #[test]
    fn type0_cyrillic_via_tounicode() {
        let ex = PdfExtractor;
        let out = ex.extract(&format!("{FX}cyrillic-type0.pdf"), 1 << 20).unwrap();
        let t = &out.pages[0];
        assert!(t.contains('А') && t.contains('Б') && t.contains('С'), "got: {t}");
    }

    #[test]
    fn fifty_pages() {
        let ex = PdfExtractor;
        let out = ex.extract(&format!("{FX}big50.pdf"), 4 << 20).unwrap();
        assert_eq!(out.pages.len(), 50);
        assert!(out.pages[6].contains("sheet 7"), "got: {}", out.pages[6]);
        assert!(out.pages[49].contains("of 50"));
    }


    #[test]
    fn not_a_pdf_rejected() {
        let ex = PdfExtractor;
        let bad = std::env::temp_dir().join("pdfcat_not_pdf.pdf");
        std::fs::write(&bad, b"MZ garbage").unwrap();
        assert!(matches!(ex.extract(bad.to_str().unwrap(), 1 << 20), Err(ExtractError::NotPdf)));
        let _ = std::fs::remove_file(&bad);
    }
}
