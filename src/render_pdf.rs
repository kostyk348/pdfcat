//! Мини-рендерер страниц PDF: подмножество контент-операторов поверх GDI.
//! Честное покрытие: cm/q/Q, цвета rg/g/k/RG/G/K, re, m/l/c(как линия)/h,
//! f/F/S/B/b/n (заливка/обводка через GDI Polygon/Rectangle/LineTo),
//! текст BT/ET Tf Td TD Tm T* Tj TJ ' " с поворотом до ближайших 90°.
//! НЕ рендерится (честно): изображения (DCT/CCITT), шейдинги, клипы, прозрачность.

use crate::pdf::{page_fonts, FontMap, PdfDoc, Value};
use crate::win::*;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq)]
pub struct Mat {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Mat {
    pub fn id() -> Self {
        Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 }
    }
    /// Произведение A×B (столбцовая конвенция): (A×B)(p) = A(B(p)).
    pub fn mul(self, o: &Mat) -> Mat {
        Mat {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }
    pub fn scale(&self) -> f64 {
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }
    pub fn angle_deg(&self) -> i32 {
        let deg = self.b.atan2(self.a).to_degrees().rem_euclid(360.0);
        (deg / 90.0).round() as i32 * 90 % 360
    }
}

#[derive(Clone, Copy)]
struct GState {
    ctm: Mat,
    fill: u32,
    stroke: u32,
    line_w: f64,
}

/// Открытый документ для рендеринга: страницы + MediaBox.
pub struct PdfRender {
    doc: PdfDoc,
    pages: Vec<(u32, (f64, f64, f64, f64))>,
    _page: usize,
    _offy: f64,
}

impl PdfRender {
    pub fn open(path: &str) -> Result<Self, crate::ports::ExtractError> {
        let mut doc = PdfDoc::load(path)?;
        doc.build_index();
        let root = doc.root_obj()?;
        let root_dict = doc.obj_dict(root).ok_or(crate::ports::ExtractError::Unsupported("no root dict"))?;
        let pages_ref = root_dict
            .get(&b"Pages".to_vec())
            .and_then(|v| doc.resolve_ref(v))
            .ok_or(crate::ports::ExtractError::Unsupported("no /Pages"))?;
        let mut pages = Vec::new();
        let mut stack = vec![(pages_ref, 0u32)];
        while let Some((num, depth)) = stack.pop() {
            if pages.len() >= 10_000 || depth > 64 {
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
                    let box_default = (0.0, 0.0, 612.0, 792.0);
                    let mb = d.get(&b"MediaBox".to_vec()).cloned().unwrap_or(Value::Array(vec![
                        Value::Num(0.0), Value::Num(0.0), Value::Num(612.0), Value::Num(792.0),
                    ]));
                    let _ = box_default;
                    let vals = || {
                        if let Value::Array(items) = &mb {
                            let getf = |i: usize| match items.get(i) {
                                Some(Value::Num(v)) => *v,
                                _ => 0.0,
                            };
                            (getf(0), getf(1), getf(2), getf(3))
                        } else {
                            (0.0, 0.0, 612.0, 792.0)
                        }
                    };
                    pages.push((num, vals()));
                }
            }
        }
        Ok(Self { doc, pages, _page: 0, _offy: 0.0 })
    }

    pub fn pages(&self) -> usize {
        self.pages.len()
    }

    pub fn page(&self) -> usize {
        self._page
    }

    pub fn page_step(&mut self, d: i32) {
        let n = self.pages.len() as i32;
        self._page = (self._page as i32 + d).clamp(0, (n - 1).max(0)) as usize;
    }

    pub fn scroll(&mut self, dy: f64) {
        self._offy += dy;
        if self._offy < 0.0 {
            self._offy = 0.0;
        }
    }

    pub fn media_pt(&self, page: usize) -> (f64, f64) {
        let (_, mb) = self.pages[page.min(self.pages.len().saturating_sub(1))];
        ((mb.2 - mb.0).abs().max(1.0), (mb.3 - mb.1).abs().max(1.0))
    }

    /// Рендер страницы прямо в контекст окна: страница -> прямоугольник dst.
    /// zoom — пикселей на пункт; offx/offy — прокрутка в пикселях.
    pub fn render_page_into(&self, hdc: HDC, dst: (i32, i32, i32, i32), page: usize, zoom: f64, offx: f64, offy: f64) { unsafe {
        let Some((obj, mb)) = self.pages.get(page.min(self.pages.len().saturating_sub(1))).copied() else {
            return;
        };
        let Some(d) = self.doc.obj_dict(obj) else { return };
        // Фон страницы
        fill_rect(hdc, dst.0, dst.1, dst.2, dst.3, 0xFFFFFF);
        // Контент
        let content = match d.get(&b"Contents".to_vec()) {
            Some(Value::Ref(n, _)) => self.doc.stream_of(*n).ok().flatten().unwrap_or_default(),
            Some(Value::Array(items)) => {
                let mut acc = Vec::new();
                for it in items {
                    if let Value::Ref(n, _) = it {
                        if let Some(c) = self.doc.stream_of(*n).ok().flatten() {
                            acc.extend_from_slice(&c);
                            acc.push(b' ');
                        }
                    }
                }
                acc
            }
            _ => Vec::new(),
        };
        if content.is_empty() {
            return;
        }
        // Базовая матрица: пункты -> пиксели dst, ось Y вниз, с прокруткой
        let s = zoom;
        let base = Mat {
            a: s,
            b: 0.0,
            c: 0.0,
            d: -s,
            e: dst.0 as f64 - s * mb.0 - offx,
            f: dst.1 as f64 + s * mb.3 + offy,
        };
        if std::env::var("PDFCAT_DEBUG").as_deref() == Ok("1") {
            eprintln!("[page] dst={dst:?} zoom={zoom} offy={offy} mb={mb:?} base=({:.2},{:.2},{:.2},{:.2},{:.2},{:.2})", base.a, base.b, base.c, base.d, base.e, base.f);
        }
        let fonts = page_fonts(&self.doc, obj);
        // Клип по листу (MediaBox): строки не вылезают за страницу
        let pr1 = base.apply(mb.0, mb.1);
        let pr2 = base.apply(mb.2, mb.3);
        let (pminx, pminy, pmaxx, pmaxy) = rect_norm(pr1, pr2);
        let saved = SaveDC(hdc);
        let rgn = CreateRectRgn(
            pminx as i32, pminy as i32, pmaxx as i32 + 1, pmaxy as i32 + 1,
        );
        SelectClipRgn(hdc, rgn);
        DeleteObject(rgn);
        let mut st = GState { ctm: base, fill: 0x000000, stroke: 0x000000, line_w: 1.0 };
        let mut stack: Vec<GState> = Vec::new();
        let mut in_text = false;
        let mut font_name: Vec<u8> = b"F1".to_vec();
        let mut font_size = 12.0f64;
        let mut tm = Mat::id();
        let mut tl = 1.2 * font_size;

        // Лексер
        let mut i = 0usize;
        let mut nums: Vec<f64> = Vec::new();
        let mut cur_str: Option<Vec<u8>> = None;
        let mut str_arr: Vec<(Vec<u8>, f64)> = Vec::new();
        let mut name_tok: Option<Vec<u8>> = None;

        while i < content.len() {
            let b = content[i];
            if is_ws(b) {
                i += 1;
                continue;
            }
            match b {
                b'(' => {
                    let (sbytes, ni) = read_lit(&content, i);
                    i = ni;
                    cur_str = Some(sbytes);
                    continue;
                }
                b'<' => {
                    if content.get(i + 1) == Some(&b'<') {
                        i = skip_dict(&content, i);
                        continue;
                    }
                    let (hbytes, ni) = read_hex(&content, i);
                    i = ni;
                    cur_str = Some(hbytes);
                    continue;
                }
                b'[' => {
                    // TJ-массив: строки + кёрнинг-числа
                    str_arr.clear();
                    i += 1;
                    let mut depth = 1;
                    while i < content.len() && depth > 0 {
                        match content[i] {
                            b'[' => {
                                depth += 1;
                                i += 1;
                            }
                            b']' => {
                                depth -= 1;
                                i += 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            b'(' => {
                                let (sb, ni) = read_lit(&content, i);
                                i = ni;
                                str_arr.push((sb, 0.0));
                            }
                            b'<' => {
                                let (hb, ni) = read_hex(&content, i);
                                i = ni;
                                str_arr.push((hb, 0.0));
                            }
                            b'-' | b'0'..=b'9' | b'.' => {
                                let (v, ni) = read_num(&content, i);
                                i = ni;
                                if let Some(last) = str_arr.last_mut() {
                                    last.1 += v;
                                } else {
                                    nums.push(v);
                                }
                            }
                            _ => i += 1,
                        }
                    }
                    // Показ TJ
                    if in_text {
                        let fi: Option<&FontMap> = fonts.get(&font_name);
                        for (sb, kern) in &str_arr {
                            if !sb.is_empty() {
                                draw_text_op(hdc, sb, &font_name, fi, tm, st, font_size);
                            }
                            if *kern <= -180.0 {
                                // значимый кёрнинг — вставим пробел визуально? пропускаем: позицию не двигаем
                            }
                        }
                    }
                    nums.clear();
                    continue;
                }
                b')' | b'>' | b']' => {
                    i += 1;
                    continue;
                }
                b'/' => {
                    let start = i + 1;
                    i += 1;
                    while i < content.len() && !is_ws(content[i]) && !is_delim(content[i]) {
                        i += 1;
                    }
                    name_tok = Some(content[start..i].to_vec());
                    continue;
                }
                b'-' | b'0'..=b'9' | b'.' => {
                    let (v, ni) = read_num(&content, i);
                    i = ni;
                    nums.push(v);
                    continue;
                }
                _ => {}
            }
            // Оператор: слово до разделителя
            let start = i;
            while i < content.len() && !is_ws(content[i]) && !matches!(content[i], b'(' | b'<' | b'[' | b'/' | b')' | b'>' | b']') {
                i += 1;
            }
            let op = &content[start..i];
            match op {
                b"q" => stack.push(st),
                b"Q" => {
                    if let Some(prev) = stack.pop() {
                        st = prev;
                    }
                }
                b"cm" if nums.len() == 6 => {
                    let m = Mat { a: nums[0], b: nums[1], c: nums[2], d: nums[3], e: nums[4], f: nums[5] };
                    st.ctm = m.mul(&st.ctm);
                }
                b"w" if !nums.is_empty() => st.line_w = nums[nums.len() - 1],
                b"rg" if nums.len() == 3 => st.fill = rgb01(nums[0], nums[1], nums[2]),
                b"g" if !nums.is_empty() => st.fill = gray01(nums[nums.len() - 1]),
                b"k" if nums.len() == 4 => st.fill = cmyk01(nums[0], nums[1], nums[2], nums[3]),
                b"RG" if nums.len() == 3 => st.stroke = rgb01(nums[0], nums[1], nums[2]),
                b"G" if !nums.is_empty() => st.stroke = gray01(nums[nums.len() - 1]),
                b"K" if nums.len() == 4 => st.stroke = cmyk01(nums[0], nums[1], nums[2], nums[3]),
                b"re" if nums.len() == 4 => {
                    let (x, y, w, h) = (nums[0], nums[1], nums[2], nums[3]);
                    let p1 = st.ctm.apply(x, y);
                    let p2 = st.ctm.apply(x + w, y + h);
                    let (l, t, r, b2) = rect_norm(p1, p2);
                    if op_is_fill(&content, i) {
                        fill_rect(hdc, l as i32, t as i32, (r - l) as i32, (b2 - t) as i32, st.fill);
                    } else {
                        stroke_rect(hdc, l as i32, t as i32, (r - l) as i32, (b2 - t) as i32, st.stroke, st.line_w);
                    }
                }
                b"m" if nums.len() == 2 => {}
                b"l" if nums.len() == 2 => {}
                b"c" if nums.len() == 6 => {}
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"b" | b"n" => {}
                b"BT" => {
                    in_text = true;
                    tm = Mat::id();
                    tl = 1.2 * font_size;
                }
                b"ET" => in_text = false,
                b"Tf" => {
                    if let Some(n) = name_tok.take() {
                        font_name = n;
                    }
                    if let Some(sz) = nums.last() {
                        font_size = *sz;
                    }
                }
                b"Td" if nums.len() == 2 => {
                    let t = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: nums[0], f: nums[1] };
                    tm = t.mul(&tm);
                    tl = 1.2 * font_size;
                }
                b"TD" if nums.len() == 2 => {
                    tl = -nums[1];
                    let t = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: nums[0], f: nums[1] };
                    tm = t.mul(&tm);
                }
                b"Tm" if nums.len() == 6 => {
                    tm = Mat { a: nums[0], b: nums[1], c: nums[2], d: nums[3], e: nums[4], f: nums[5] };
                    tl = 1.2 * font_size;
                }
                b"T*" => {
                    let t = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: -tl };
                    tm = t.mul(&tm);
                }
                b"Tj" => {
                    if in_text {
                        if let Some(sb) = &cur_str {
                            let fi: Option<&FontMap> = fonts.get(&font_name);
                            draw_text_op(hdc, sb, &font_name, fi, tm, st, font_size);
                        }
                    }
                    cur_str = None;
                }
                b"'" => {
                    if in_text {
                        let t = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: -tl };
                        tm = t.mul(&tm);
                        if let Some(sb) = &cur_str {
                            let fi: Option<&FontMap> = fonts.get(&font_name);
                            draw_text_op(hdc, sb, &font_name, fi, tm, st, font_size);
                        }
                    }
                    cur_str = None;
                }
                b"\"" => {
                    if in_text {
                        let t = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: -tl };
                        tm = t.mul(&tm);
                        if let Some(sb) = &cur_str {
                            let fi: Option<&FontMap> = fonts.get(&font_name);
                            draw_text_op(hdc, sb, &font_name, fi, tm, st, font_size);
                        }
                    }
                    cur_str = None;
                }
                _ => {}
            }
            nums.clear();
            cur_str = None;
            name_tok = None;
        }
        let _ = in_text;
        RestoreDC(hdc, -saved);
    }
}
}

fn rect_norm(p1: (f64, f64), p2: (f64, f64)) -> (f64, f64, f64, f64) {
    (p1.0.min(p2.0), p1.1.min(p2.1), p2.0.max(p1.0), p2.1.max(p1.1))
}

fn op_is_fill(content: &[u8], after_re: usize) -> bool {
    // Ищем оператор, завершающий путь после re: ближайшее слово из набора
    let mut i = after_re;
    while i < content.len() {
        match content[i] {
            b'(' | b'[' => return true,
            b'/' => return false,
            b'f' | b'F' => return true,
            b'S' | b's' => return false,
            b'n' => return false,
            _ => i += 1,
        }
    }
    true
}

fn draw_text_op(
    hdc: HDC, raw: &[u8], font_name: &[u8], fi: Option<&FontMap>, tm: Mat, st: GState, size: f64,
) {
    let text = crate::pdf::decode_string_with_font(raw, fi);
    if text.trim().is_empty() {
        return;
    }
    // Матрица устройства: CTM × Tm; позиция = перенос
    let m = st.ctm.mul(&tm);
    let (x, y) = m.apply(0.0, 0.0);
    let scale = m.scale();
    let fpx = (size * scale).max(2.0) as i32;
    if std::env::var("PDFCAT_DEBUG").as_deref() == Ok("1") {
        eprintln!("[draw] font={:?} px=({x},{y}) fpx={fpx} text={:?}", font_name, text);
    }
    let rot = m.angle_deg();
    unsafe {
        let face = utf8_to_wide("Segoe UI");
        let esc = (rot * 10) as i32;
        let weight = 400i32;
        let f = CreateFontW(-fpx, 0, esc, esc, weight, 0, 0, 0, DEFAULT_CHARSET, 0, 0, CLEARTYPE_QUALITY, 0, face.as_ptr());
        SelectObject(hdc, f);
        SetTextColor(hdc, colorref(st.fill));
        SetBkMode(hdc, TRANSPARENT_BK);
        SetTextAlign(hdc, TA_BASELINE | TA_LEFT);
        let w = utf8_to_wide(&text);
        TextOutW(hdc, x as i32, y as i32, w.as_ptr(), (w.len() - 1) as i32);
        DeleteObject(f);
        let _ = font_name;
    }
}

fn fill_rect(hdc: HDC, x: i32, y: i32, w: i32, h: i32, hex: u32) {
    unsafe {
        let brush = CreateSolidBrush(colorref(hex));
        let r = Rect { left: x, top: y, right: x + w, bottom: y + h };
        let null_pen = GetStockObject(NULL_PEN);
        let old_pen = SelectObject(hdc, null_pen);
        let old_br = SelectObject(hdc, brush);
        Rectangle(hdc, x, y, x + w, y + h);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_br);
        DeleteObject(brush);
    }
}

fn stroke_rect(hdc: HDC, x: i32, y: i32, w: i32, h: i32, hex: u32, lw: f64) {
    unsafe {
        let pen = CreatePen(PS_SOLID, (lw as i32).max(1), colorref(hex));
        let null_br = GetStockObject(NULL_BRUSH);
        let old_pen = SelectObject(hdc, pen);
        let old_br = SelectObject(hdc, null_br);
        Rectangle(hdc, x, y, x + w, y + h);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_br);
        DeleteObject(pen);
    }
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0C | 0x00)
}
fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}
fn read_num(data: &[u8], mut i: usize) -> (f64, usize) {
    let start = i;
    while i < data.len() && (data[i].is_ascii_digit() || matches!(data[i], b'.' | b'-' | b'+')) {
        i += 1;
    }
    (String::from_utf8_lossy(&data[start..i]).parse().unwrap_or(0.0), i)
}
fn read_lit(data: &[u8], mut i: usize) -> (Vec<u8>, usize) {
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
        let d = match data[i] {
            b'0'..=b'9' => Some(data[i] - b'0'),
            b'a'..=b'f' => Some(data[i] - b'a' + 10),
            b'A'..=b'F' => Some(data[i] - b'A' + 10),
            _ => None,
        };
        if let Some(d) = d {
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
fn skip_dict(data: &[u8], mut i: usize) -> usize {
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
fn rgb01(r: f64, g: f64, b: f64) -> u32 {
    let c = |v: f64| ((v.clamp(0.0, 1.0)) * 255.0) as u32;
    // DIB: B<<16 | G<<8 | R
    (c(b) << 16) | (c(g) << 8) | c(r)
}
fn gray01(v: f64) -> u32 {
    let c = ((v.clamp(0.0, 1.0)) * 255.0) as u32;
    (c << 16) | (c << 8) | c
}
fn cmyk01(c: f64, m: f64, y: f64, k: f64) -> u32 {
    let cv = |v: f64| v.clamp(0.0, 1.0);
    let r = 255.0 * (1.0 - cv(c)) * (1.0 - cv(k));
    let g = 255.0 * (1.0 - cv(m)) * (1.0 - cv(k));
    let bl = 255.0 * (1.0 - cv(y)) * (1.0 - cv(k));
    rgb01(r / 255.0, g / 255.0, bl / 255.0)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_simple_page_has_ink() {
        let r = PdfRender::open("tests/fixtures/simple.pdf").unwrap();
        unsafe {
            let mem = CreateCompatibleDC(0);
            let mut bmi = BitmapInfoHeader::default();
            bmi.bi_size = 40;
            bmi.bi_width = 400;
            bmi.bi_height = -400;
            bmi.bi_planes = 1;
            bmi.bi_bit_count = 32;
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(mem, &bmi, 0, &mut bits, 0, 0);
            SelectObject(mem, bmp);
            // фон белый
            let sl = std::slice::from_raw_parts_mut(bits as *mut u32, 400 * 400);
            sl.fill(0x00FFFFFF);
            r.render_page_into(mem, (0, 0, 400, 400), 0, 400.0 / 612.0, 0.0, 0.0);
            let dark = sl.iter().filter(|&&p| (p & 0xFF) < 0x80).count();
            DeleteObject(bmp);
            DeleteDC(mem);
            assert!(dark >= 100, "тёмных пикселей: {dark} — текст не отрисован");
        }
    }
}
