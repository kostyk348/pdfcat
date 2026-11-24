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
use std::cell::RefCell;
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

/// Запись xref: смещение в файле (тип 1) или «лежит в object stream» (тип 2).
#[derive(Clone, Copy)]
enum XEntry {
    Offset(usize),
    InStm(u32, u32), // номер ObjStm, индекс внутри
}

pub struct PdfDoc {
    data: Vec<u8>,
    /// objnum → смещение «N G obj».
    offsets: HashMap<u32, usize>,
    /// objnum → запись xref (потоковая xref PDF 1.5+).
    xref: HashMap<u32, XEntry>,
    /// Номер ObjStm → список (objnum, смещение внутри потока).
    objstm: RefCell<HashMap<u32, Vec<(u32, usize)>>>,
    /// Распакованное содержимое ObjStm: номер → (First, байты).
    objstm_data: RefCell<HashMap<u32, (usize, Vec<u8>)>>,
}

impl PdfDoc {
    pub fn load(path: &str) -> Result<Self, ExtractError> {
        let data = std::fs::read(path).map_err(|_| ExtractError::NotFound)?;
        if !data.starts_with(b"%PDF") {
            // Некоторые PDF начинаются с мусора до %PDF — ищем подпись в первых 2 КБ
            let head = &data[..data.len().min(2048)];
            let off = find(head, b"%PDF").ok_or(ExtractError::NotPdf)?;
            return Ok(Self {
                data: data[off..].to_vec(),
                offsets: HashMap::new(),
                xref: HashMap::new(),
                objstm: RefCell::new(HashMap::new()),
                objstm_data: RefCell::new(HashMap::new()),
            });
        }
        Ok(Self {
            data,
            offsets: HashMap::new(),
            xref: HashMap::new(),
            objstm: RefCell::new(HashMap::new()),
            objstm_data: RefCell::new(HashMap::new()),
        })
    }

    /// Разбор словаря, начинающегося в позиции pos (пропуск «N G obj», пробелов).
    fn dict_at(&self, pos: usize) -> Option<HashMap<Vec<u8>, Value>> {
        let span = &self.data[pos.min(self.data.len())..];
        let mut start = 0usize;
        // Для «N 0 obj»Case: ищем «obj» в первых 32 байтах и пропускаем его
        if let Some(o) = find(&span[..span.len().min(64)], b"obj") {
            let after = o + 3;
            let mut j = after;
            while j < span.len() && is_ws(span[j]) {
                j += 1;
            }
            if j < span.len() {
                start = j;
            }
        }
        let mut p = Parser::new(&span[start..]);
        match p.parse() {
            Ok(Value::Dict(d)) => Some(d),
            _ => None,
        }
    }

    /// Число из Value (Num или Ref→obj_dict не нужно здесь).
    fn num_of(v: &Value) -> Option<i64> {
        match v {
            Value::Num(n) => Some(*n as i64),
            _ => None,
        }
    }

    /// Распаковка потока объекта с учётом /Filter (FlateDecode) — сырые байты для xref/objstm.
    fn raw_stream_at(&self, pos: usize) -> Option<(HashMap<Vec<u8>, Value>, Vec<u8>)> {
        let dbg = std::env::var("PDFCAT_DEBUG").as_deref() == Ok("1");
        let span = &self.data[pos.min(self.data.len())..];
        let Some(srel) = find(span, b"stream") else {
            if dbg { eprintln!("[raw_stream] no 'stream' at {pos}"); }
            return None;
        };
        if dbg { eprintln!("[raw_stream] at {pos}: srel={srel} span={}", span.len()); }
        let dict_span = &span[..srel];
        let mut dict = match Parser::new(dict_span).parse() {
            Ok(Value::Dict(d)) => d,
            _ => {
                // Заголовок «N G obj» перед словарем — пропускаем
                let head = &dict_span[..dict_span.len().min(64)];
                let Some(o) = find(head, b"obj") else {
                    if dbg { eprintln!("[raw_stream] dict parse failed, no obj header"); }
                    return None;
                };
                let mut j = o + 3;
                while j < dict_span.len() && is_ws(dict_span[j]) {
                    j += 1;
                }
                match Parser::new(&dict_span[j..]).parse() {
                    Ok(Value::Dict(d)) => d,
                    _ => {
                        if dbg { eprintln!("[raw_stream] dict parse failed after header"); }
                        return None;
                    }
                }
            }
        };
        let mut ds = srel + 6;
        if span.get(ds) == Some(&b'\r') {
            ds += 1;
            if span.get(ds) == Some(&b'\n') {
                ds += 1;
            }
        } else if span.get(ds) == Some(&b'\n') {
            ds += 1;
        }
        let len_opt = match dict.get(&b"Length".to_vec()) {
            Some(Value::Num(n)) if n.is_finite() && *n >= 0.0 => Some(*n as usize),
            Some(Value::Ref(n, _)) => self.obj_dict(*n).and_then(|d| match d.get(&b"Length".to_vec()) {
                Some(Value::Num(v)) if v.is_finite() && *v >= 0.0 => Some(*v as usize),
                _ => None,
            }),
            _ => None,
        };
        let raw: Vec<u8> = if let Some(len) = len_opt {
            let end = (ds + len).min(span.len());
            span[ds..end].to_vec()
        } else {
            // fallback: до endstream
            let e = find(&span[ds..], b"endstream")?;
            span[ds..ds + e].to_vec()
        };
        let out = match dict.remove(&b"Filter".to_vec()) {
            Some(Value::Name(n)) if n == b"FlateDecode" => {
                match crate::inflate::zlib_decompress_tolerant(&raw) {
                    Ok(v) => v,
                    Err(e) => {
                        if dbg {
                            eprintln!("[raw_stream] inflate fail: {e:?}, raw len {}", raw.len());
                            let _ = std::fs::write("target/debug_stream.bin", &raw);
                        }
                        return None;
                    }
                }
            }
            Some(Value::Array(items)) => {
                let mut acc = raw.clone();
                for it in items {
                    if let Value::Name(n) = it {
                        if n == b"FlateDecode" {
                            acc = crate::inflate::zlib_decompress(&acc).ok()?;
                        } else {
                            break;
                        }
                    }
                }
                acc
            }
            _ => raw,
        };
        Some((dict, out))
    }

    /// PNG-предиктор 12 (Up): снять предсказание по строкам.
    fn unpredict(mut data: Vec<u8>, columns: usize) -> Vec<u8> {
        if columns == 0 || data.is_empty() {
            return data;
        }
        let row_len = columns + 1; // 1 байт — тип фильтра
        let rows = data.len() / row_len;
        for r in 1..rows {
            for c in 1..=columns {
                let i = r * row_len + c;
                if i >= data.len() {
                    break;
                }
                let above = data[(r - 1) * row_len + c];
                data[i] = data[i].wrapping_add(above);
            }
        }
        // Убираем байты фильтров: сдвигаем строки влево
        let mut out = Vec::with_capacity(rows * columns);
        for r in 0..rows {
            let start = r * row_len + 1;
            let end = (start + columns).min(data.len());
            if start < data.len() {
                out.extend_from_slice(&data[start..end]);
            }
        }
        out
    }

    /// Пройти цепочку xref: потоковые (PDF 1.5+) и классические секции.
    pub(crate) fn load_xref(&mut self, start: usize) {
        let dbg = std::env::var("PDFCAT_DEBUG").as_deref() == Ok("1");
        let mut cur = start;
        for _ in 0..32 {
            if cur >= self.data.len() {
                return;
            }
            let Some(dict) = self.dict_at(cur) else {
                if dbg { eprintln!("[xref] dict_at({cur}) failed"); }
                return;
            };
            if dbg {
                let keys: Vec<String> = dict.keys().take(8).map(|k| String::from_utf8_lossy(k).into_owned()).collect();
                eprintln!("[xref] dict at {cur}: {keys:?}");
            }
            let is_xref_stream = matches!(dict.get(&b"Type".to_vec()), Some(Value::Name(n)) if n == b"XRef");
            if dbg { eprintln!("[xref] is_xref_stream={is_xref_stream}"); }
            if is_xref_stream {
                let w = dict.get(&b"W".to_vec());
                let widths: Vec<usize> = match w {
                    Some(Value::Array(items)) => items
                        .iter()
                        .filter_map(|v| Some(Self::num_of(v)? as usize))
                        .collect(),
                    _ => return,
                };
                if dbg { eprintln!("[xref] widths={widths:?}"); }
                if widths.len() < 3 {
                    return;
                }
                let row_len: usize = widths.iter().sum();
                if dbg { eprintln!("[xref] row_len={row_len}"); }
                if row_len == 0 {
                    return;
                }
                // Индексы: /Index [s n s n …] или [0 /Size]
                let mut ranges: Vec<(u32, u32)> = Vec::new();
                if let Some(Value::Array(items)) = dict.get(&b"Index".to_vec()) {
                    let flat: Vec<i64> = items.iter().filter_map(Self::num_of).collect();
                    let mut k = 0;
                    while k + 1 < flat.len() {
                        ranges.push((flat[k] as u32, flat[k + 1] as u32));
                        k += 2;
                    }
                } else {
                    let n = dict
                        .get(&b"Size".to_vec())
                        .and_then(Self::num_of)
                        .unwrap_or(0) as u32;
                    ranges.push((0, n));
                }
                let columns_total: u32 = ranges.iter().map(|(_, c)| *c).sum();
                let (ddict, mut bytes) = match self.raw_stream_at(cur) {
                    Some(v) => v,
                    None => return,
                };
                let _ = ddict;
                // Предиктор 12 (PNG Up)
                let predictor = dict
                    .get(&b"DecodeParms".to_vec())
                    .and_then(|dp| match dp {
                        Value::Dict(dp) => dp.get(&b"Predictor".to_vec()),
                        _ => None,
                    })
                    .or_else(|| dict.get(&b"Predictor".to_vec()))
                    .and_then(Self::num_of)
                    .unwrap_or(1);
                if dbg {
                    eprintln!("[xref] W={widths:?} row_len={row_len} ranges={ranges:?} raw={} predictor={predictor}", bytes.len());
                }

                if predictor >= 10 {
                    bytes = Self::unpredict(bytes, (columns_total as usize) * row_len);
                }
                if dbg { eprintln!("[xref] after unpredict: {}", bytes.len()); }
                let mut pos = 0usize;
                for (start_num, count) in ranges {
                    for i in 0..count {
                        if pos + row_len > bytes.len() {
                            return;
                        }
                        let fields: Vec<u32> = widths
                            .iter()
                            .scan(0usize, |acc, w| {
                                let mut v = 0u32;
                                for b in 0..*w {
                                    if pos + *acc + b >= bytes.len() {
                                        break;
                                    }
                                    v = (v << 8) | bytes[pos + *acc + b] as u32;
                                }
                                *acc += *w;
                                Some(v)
                            })
                            .collect();
                        if fields.len() < 3 {
                            return;
                        }
                        let obj = start_num + i;
                        match fields[0] {
                            0 => {}
                            1 => {
                                self.xref
                                    .entry(obj)
                                    .or_insert(XEntry::Offset(fields[1] as usize));
                            }
                            2 => {
                                self.xref
                                    .entry(obj)
                                    .or_insert(XEntry::InStm(fields[1], fields[2]));
                            }
                            _ => {}
                        }
                        pos += row_len;
                    }
                }
                match dict.get(&b"Prev".to_vec()).and_then(Self::num_of) {
                    Some(p) if p > 0 => cur = p as usize,
                    _ => return,
                }
            } else {
                // Классическая таблица: «xref» + секции
                return;
            }
            if dbg { eprintln!("[xref] xref map total now {}", self.xref.len()); }
        }
    }

    /// Object stream: распаковать и получить (First, байты) — вызывается лениво.
    fn objstm_load(&self, num: u32) -> Option<()> {
        let dbg = std::env::var("PDFCAT_DEBUG").as_deref() == Ok("1");
        if self.objstm_data.borrow().contains_key(&num) {
            return Some(());
        }
        let off = match self.xref.get(&num) {
            Some(XEntry::Offset(o)) => *o,
            _ => self.offsets.get(&num).copied()?,
        };
        let (dict, bytes) = match self.raw_stream_at(off) {
            Some(v) => v,
            None => {
                if dbg { eprintln!("[objstm] no stream at {off}"); }
                return None;
            }
        };
        let n = match dict.get(&b"N".to_vec()).and_then(Self::num_of) {
            Some(v) => v as usize,
            None => {
                if dbg { eprintln!("[objstm] no /N"); }
                return None;
            }
        };
        let first = Self::num_of(dict.get(&b"First".to_vec())?)? as usize;
        // Первые First байт: N пар (objnum, смещение)
        let head = &bytes[..first.min(bytes.len())];
        let nums: Vec<i64> = {
            let mut out = Vec::new();
            let mut p = Parser::new(head);
            while let Ok(v) = p.parse() {
                if let Some(n) = Self::num_of(&v) {
                    out.push(n);
                }
                if out.len() >= n * 2 {
                    break;
                }
            }
            out
        };
        let mut list = Vec::with_capacity(n);
        for i in 0..n {
            if i * 2 + 1 >= nums.len() {
                break;
            }
            list.push((nums[i * 2] as u32, nums[i * 2 + 1] as usize));
        }
        self.objstm.borrow_mut().insert(num, list);
        self.objstm_data.borrow_mut().insert(num, (first, bytes));
        Some(())
    }

    /// Объект из object stream (по номеру объекта).
    fn objstm_value(&self, num: u32) -> Option<Value> {
        // Ищем ObjStm: линейный перебор по кешу xref (тип 2: InStm)
        let (stm_num, idx) = match self.xref.get(&num) {
            Some(XEntry::InStm(s, i)) => (*s, *i),
            _ => return None,
        };
        self.objstm_load(stm_num)?;
        let list = self.objstm.borrow();
        let list = list.get(&stm_num)?;
        if (idx as usize) >= list.len() {
            return None;
        }
        let (_, rel) = list[idx as usize];
        let guard = self.objstm_data.borrow();
        let (first, bytes) = guard.get(&stm_num)?;
        let abs = first + rel;
        if abs >= bytes.len() {
            return None;
        }
        match Parser::new(&bytes[abs..]).parse() {
            Ok(v) => Some(v),
            Err(_) => None,
        }
    }

    /// Fallback-скан: все «N G obj». Работает и без xref; O(размер файла).
    /// Смещение сохраняется НА начало словаря (после слова obj).
    pub(crate) fn build_index(&mut self) {
        if !self.offsets.is_empty() {
            return;
        }
        // Потоковая xref (PDF 1.5+): строим её один раз, скан файла не нужен
        if self.xref.is_empty() {
            let tail_start = self.data.len().saturating_sub(4096);
            let tail = &self.data[tail_start..].to_vec();
            if let Some(x) = rfind(tail, b"startxref") {
                let s = std::str::from_utf8(&tail[x + 9..]).unwrap_or("");
                if let Ok(num) = s.split_whitespace().next().unwrap_or("0").parse::<u32>() {
                    if num > 0 && (num as usize) < self.data.len() {
                        self.load_xref(num as usize);
                    }
                }
            }
        }
        if self.xref.is_empty() {
            self.scan_objects();
        }
    }

    /// Fallback-скан: все «N G obj» (нет xref или битый xref). O(размер файла).
    fn scan_objects(&mut self) {
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
        if let Some(o) = self.offsets.get(&num) {
            return Some(*o);
        }
        match self.xref.get(&num) {
            Some(XEntry::Offset(o)) => Some(*o),
            Some(XEntry::InStm(_, _)) => None,
            None => None,
        }
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
                // Цепочка: FlateDecode может идти до терминального (DCTDecode и др.),
                // терминальный фильтр оставляет данные как есть (их декодирует consumer).
                let mut out = raw.to_vec();
                for it in items {
                    match it {
                        Value::Name(n) if n == b"FlateDecode" => {
                            out = zlib_decompress(&out).map_err(|_| ExtractError::Unsupported("flate"))?;
                        }
                        _ => break,
                    }
                }
                out
            }
            None => raw.to_vec(),
            // DCTDecode и другие терминальные — сырые байты (JPEG декодирует render_pdf)
            Some(Value::Name(_)) => raw.to_vec(),
            Some(_) => return Err(ExtractError::Unsupported("filter")),
        };
        let _ = dict;
        Ok(Some(filtered))
    }

    pub(crate) fn obj_dict(&self, num: u32) -> Option<HashMap<Vec<u8>, Value>> {
        let dbg = std::env::var("PDFCAT_DEBUG").as_deref() == Ok("1");
        if dbg {
            let ent = self.xref.get(&num);
            let ent_s = match ent {
                Some(XEntry::Offset(o)) => format!("Offset({o})"),
                Some(XEntry::InStm(a, b)) => format!("InStm({a},{b})"),
                None => "none".to_string(),
            };
            eprintln!("[obj_dict] #{num}: xref={ent_s} offsets={:?}", self.offsets.get(&num));
        }
        // Объект может лежать в object stream (PDF 1.5+)
        if matches!(self.xref.get(&num), Some(XEntry::InStm(_, _))) {
            let v = self.objstm_value(num);
            if dbg {
                let kind = match &v {
                    Some(Value::Dict(_)) => "Dict",
                    Some(Value::Array(_)) => "Array",
                    Some(Value::Num(_)) => "Num",
                    Some(Value::Name(_)) => "Name",
                    Some(Value::Bool(_)) => "Bool",
                    Some(Value::Str(_)) => "Str",
                    Some(Value::Str(_)) => "Str",
                    Some(Value::Ref(..)) => "Ref",
                    Some(Value::Null) => "Null",
                    None => "NONE",
                };
                eprintln!("[objstm] #{num} -> {kind}");
            }
            return match v {
                Some(Value::Dict(d)) => Some(d),
                _ => None,
            };
        }
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
                // /Type /XRef — потоковая xref (PDF 1.5): Root лежит в том же словаре
                if find(span, b"/XRef").is_some() {
                    if let Some(d) = self.dict_at(num as usize) {
                        if let Some(r) = d.get(&b"Root".to_vec()) {
                            if let Some(n) = self.resolve_ref(r) {
                                return Ok(n);
                            }
                        }
                    }
                }
            }
        }
        // Fallback: объект с /Catalog среди всех известных (xref + скан)
        self.build_index();
        let mut cands: Vec<u32> = self.offsets.keys().copied().collect();
        cands.extend(self.xref.keys().copied());
        cands.sort_unstable();
        cands.dedup();
        for num in cands.into_iter().take(5000) {
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
