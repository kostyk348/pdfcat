//! Собственный baseline-JPEG декодер (DCTDecode в PDF): SOF0, Huffman, DRI,
//! YCbCr/Gray, сэмплирование 1×1..2×2. Прогрессивный JPEG — честный отказ.

#[derive(Debug, PartialEq)]
pub enum JpegError {
    NotJpeg,
    Unsupported(&'static str),
    Truncated,
    BadData,
}

#[derive(Clone)]
struct Huff {
    // Каноническое дерево: быстрый побитовый декод через counts/points (стиль stb_image)
    counts: [u8; 17],
    symbols: Vec<u8>,
    mincode: [i32; 17],
    maxcode: [i32; 18],
    valptr: [i32; 17],
}

fn build_huff(lengths: &[u8], symbols: &[u8]) -> Result<Huff, JpegError> {
    let mut h = Huff {
        counts: [0; 17],
        symbols: symbols.to_vec(),
        mincode: [0; 17],
        maxcode: [0; 18],
        valptr: [0; 17],
    };
    for &l in lengths {
        h.counts[l as usize] += 1;
    }
    let mut code = 0i32;
    let mut k = 0i32;
    for len in 1..=16usize {
        h.valptr[len] = k;
        h.mincode[len] = code;
        code += i32::from(h.counts[len]);
        k += i32::from(h.counts[len]);
        h.maxcode[len] = code - 1;
        code <<= 1;
        if h.counts[len] > 0 && h.maxcode[len] >= h.mincode[len] {
            // ок
        }
    }
    h.maxcode[17] = 0xFFFFF;
    Ok(h)
}

/// JPEG DHT: counts[len-1] = число кодов длины len; symbols — в порядке кодов.
fn build_huff_from_counts(counts: &[u8; 16], symbols: &[u8]) -> Huff {
    let mut h = Huff {
        counts: [0; 17],
        symbols: symbols.to_vec(),
        mincode: [0; 17],
        maxcode: [0; 18],
        valptr: [0; 17],
    };
    let mut code = 0i32;
    let mut k = 0i32;
    for len in 1..=16usize {
        h.counts[len] = counts[len - 1];
        h.valptr[len] = k;
        h.mincode[len] = code;
        code += i32::from(h.counts[len]);
        k += i32::from(h.counts[len]);
        h.maxcode[len] = code - 1;
        code <<= 1;
    }
    h
}

struct Jbits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    cnt: u32,
}

impl<'a> Jbits<'a> {
    fn bit(&mut self) -> Result<u32, JpegError> {
        if self.cnt == 0 {
            if self.pos >= self.data.len() {
                return Err(JpegError::Truncated);
            }
            let b = self.data[self.pos];
            self.pos += 1;
            if b == 0xFF {
                // byte stuffing: FF00 — данные; другой маркер — оставим (обработка RST отдельно)
                if self.pos < self.data.len() && self.data[self.pos] != 0x00 {
                    // маркер: откат, чтобы внешний цикл увидел
                    self.pos -= 1;
                    return Err(JpegError::Truncated);
                }
                self.pos += 1;
            }
            self.acc = u32::from(b);
            self.cnt = 8;
        }
        self.cnt -= 1;
        Ok((self.acc >> self.cnt) & 1)
    }

    fn bits(&mut self, n: u32) -> Result<u32, JpegError> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }

    fn decode_huff(&mut self, h: &Huff) -> Result<u8, JpegError> {
        let mut code = 0i32;
        for len in 1..=16usize {
            code = (code << 1) | self.bit()? as i32;
            if h.counts[len] > 0 && code <= i32::from(h.maxcode[len]) {
                let idx = h.valptr[len] + code - h.mincode[len];
                return Ok(h.symbols[idx as usize]);
            }
        }
        if std::env::var("JPEG_DEBUG").as_deref() == Ok("1") {
            eprintln!("[huff] FAIL code={code} pos={} cnt={}", self.pos, self.cnt);
        }
        Err(JpegError::BadData)
    }
}

/// Расширение значения из битов (JPEG Huffman: знаковое смещение).
fn extend(v: u32, n: u32) -> i32 {
    if n == 0 {
        return 0;
    }
    if v < (1u32 << (n - 1)) {
        v as i32 - (1i32 << n) + 1
    } else {
        v as i32
    }
}

// Стандартные таблицы (если в файле их нет — нельзя, JPEG их всегда несёт; ок).

#[derive(Clone, Copy)]
struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    dc: usize,
    ac: usize,
    pred: i32,
}

/// Декодировать baseline-JPEG в RGB (3 канала) или Gray (1 канал, продублирован).
/// Возвращает (width, height, rgb_bytes_topdown).
pub fn decode_jpeg(data: &[u8]) -> Result<(usize, usize, Vec<u8>), JpegError> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(JpegError::NotJpeg);
    }
    let mut i = 2usize;
    let mut w = 0usize;
    let mut h = 0usize;
    let mut comps: Vec<Comp> = Vec::new();
    let mut qt: [[u16; 64]; 4] = [[0; 64]; 4];
    let mut dhts: Vec<(u8, Huff)> = Vec::new(); // (class<<4|id, Huff)
    let mut restart_interval = 0usize;
    let mut scan_data_start = 0usize;
    let mut scan_header: Option<(usize, u8, Vec<Comp>, usize)> = None; // (SOS pos, ns, comps_at_scan, ss_se)

    // Проходим маркеры до SOS
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        i += 2;
        match marker {
            0xD8 => {}                                     // SOI (повтор)
            0xD9 => return Err(JpegError::Truncated),      // EOI без SOS
            0x01 => {}                                     // TEM
            0xC0 | 0xC1 => {
                // SOF0/SOF1 baseline
                let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
                let prec = data[i + 2];
                if prec != 8 {
                    return Err(JpegError::Unsupported("12-bit"));
                }
                h = u16::from_be_bytes([data[i + 3], data[i + 4]]) as usize;
                w = u16::from_be_bytes([data[i + 5], data[i + 6]]) as usize;
                let nc = data[i + 7] as usize;
                comps.clear();
                for c in 0..nc {
                    let id = data[i + 8 + c * 3];
                    let hv = data[i + 9 + c * 3];
                    let tq = data[i + 10 + c * 3] as usize;
                    comps.push(Comp {
                        id,
                        h: (hv >> 4) as usize,
                        v: (hv & 0xF) as usize,
                        tq,
                        dc: 0,
                        ac: 0,
                        pred: 0,
                    });
                }
                if comps.is_empty() || comps.len() > 4 {
                    return Err(JpegError::Unsupported("components"));
                }
                let _ = len;
                i += len;
            }
            0xC2 => return Err(JpegError::Unsupported("progressive")),
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err(JpegError::Unsupported("sof"));
            }
            0xC4 => {
                // DHT: может содержать несколько таблиц
                let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
                let mut p = i + 2;
                let end = i + len;
                while p < end {
                    let tc = (data[p] >> 4) & 1;
                    let th = data[p] & 0xF;
                    let counts = &data[p + 1..p + 17];
                    let total: usize = counts.iter().map(|&c| c as usize).sum();
                    let symbols = &data[p + 17..p + 17 + total];
                    dhts.push(((tc << 4) | th, build_huff_from_counts(counts.try_into().expect("counts16"), symbols)));
                    p += 17 + total;
                }
                i += len;
            }
            0xDB => {
                let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
                let mut p = i + 2;
                let end = i + len;
                while p < end {
                    let pq = data[p] >> 4;
                    let tq = (data[p] & 0xF) as usize;
                    p += 1;
                    for k in 0..64 {
                        if pq == 0 {
                            qt[tq][ZIGZAG[k]] = u16::from(data[p]);
                            p += 1;
                        } else {
                            return Err(JpegError::Unsupported("16-bit quant"));
                        }
                    }
                }
                i += len;
            }
            0xDD => {
                restart_interval = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                i += 4;
            }
            0xDA => {
                // SOS: запоминаем позицию заголовка скана и выходим на декодирование
                let ns = data[i + 2] as usize;
                let mut scan_comps = Vec::new();
                for c in 0..ns {
                    let cs = data[i + 3 + c * 2];
                    let tables = data[i + 4 + c * 2];
                    if let Some(comp) = comps.iter_mut().find(|x| x.id == cs) {
                        comp.dc = (tables >> 4) as usize;
                        comp.ac = (tables & 0xF) as usize;
                        scan_comps.push(*comp);
                    }
                }
                scan_header = Some((i, ns as u8, scan_comps, restart_interval));
                scan_data_start = i;
                break;
            }
            _ => {
                // APPn/COM/прочее: пропускаем сегмент
                if i + 2 > data.len() {
                    return Err(JpegError::Truncated);
                }
                let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
                i += len;
            }
        }
    }

    let Some((sos_pos, _ns, scan_comps, ri)) = scan_header else {
        return Err(JpegError::Unsupported("no SOS"));
    };
    let _ = sos_pos;

    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return Err(JpegError::Unsupported("size"));
    }
    // Таблицы для скана
    let mut dc_h: Vec<Option<Huff>> = vec![None; 4];
    let mut ac_h: Vec<Option<Huff>> = vec![None; 4];
    for (key, h) in &dhts {
        let class = (key >> 4) & 1;
        let id = (key & 0xF) as usize;
        if class == 0 && id < 4 {
            dc_h[id] = Some(h.clone());
        } else if class == 1 && id < 4 {
            ac_h[id] = Some(h.clone());
        }
    }
    let mut comps = scan_comps.clone();
    // (предикторы мутируют — работаем со своей копией)
    for c in comps.iter_mut() {
        if c.dc >= 4 || c.ac >= 4 || dc_h[c.dc].is_none() || ac_h[c.ac].is_none() {
            return Err(JpegError::Unsupported("huff tables"));
        }
        c.pred = 0;
    }

    // Данные скана: от конца заголовка SOS до EOI/конца
    let sh_len = u16::from_be_bytes([data[scan_data_start], data[scan_data_start + 1]]) as usize;
    let mut sd = scan_data_start + sh_len;
    let mut scan = &data[sd..];

    let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
    let mcu_w = hmax * 8;
    let mcu_h = vmax * 8;
    let mcus_x = (w + mcu_w - 1) / mcu_w;
    let mcus_y = (h + mcu_h - 1) / mcu_h;

    // Планы компонент (pad по MCU-сетке)
    let mut planes: Vec<Vec<i16>> = Vec::new();
    for c in &comps {
        let pw = mcus_x * c.h * 8;
        let ph = mcus_y * c.v * 8;
        planes.push(vec![0i16; pw * ph]);
    }

    let mut br = Jbits { data: scan, pos: 0, acc: 0, cnt: 0 };
    let mut block: [i32; 64] = [0; 64];
    let mut mcu_count = 0usize;
    let mut rst_expected = 0u8;

    'mcus: for my in 0..mcus_y {
        for mx in 0..mcus_x {
            if ri > 0 && mcu_count > 0 && mcu_count % ri == 0 {
                // RSTn: сброс предикторов, пропустить маркер
                for c in comps.iter_mut() {
                    c.pred = 0;
                }
                // выровнять биты и пропустить FFD0+rst_expected
                br.cnt = 0;
                let _ = rst_expected;
                while br.pos + 1 < scan.len() {
                    if scan[br.pos] == 0xFF && matches!(scan[br.pos + 1], 0xD0..=0xD7) {
                        br.pos += 2;
                        break;
                    }
                    br.pos += 1;
                }
                rst_expected = (rst_expected + 1) % 8;
            }
            for (ci, c) in comps.iter_mut().enumerate() {
                for vv in 0..c.v {
                    for hh in 0..c.h {
                        // Декодировать блок 8×8
                        block = [0; 64];
                        if mcu_count < 2 && std::env::var("JPEG_DEBUG").as_deref() == Ok("1") {
                            eprintln!("[blk] mcu={mcu_count} ci={ci} pos={} bitcnt={}", br.pos, br.cnt);
                        }
                        if std::env::var("JPEG_DEBUG").as_deref() == Ok("1") {
                            let bits_str: String = scan[..br.pos.min(scan.len())]
                                .iter()
                                .map(|&b| format!("{:08b}", b))
                                .collect::<Vec<_>>()
                                .join(" ");
                            eprintln!("[bits] pos={} bytes={}", br.pos, bits_str);
                        }
                        let s = br.decode_huff(dc_h[c.dc].as_ref().unwrap())?;
                        let diff = extend(br.bits(u32::from(s))?, u32::from(s));
                        c.pred += diff;
                        block[0] = c.pred * i32::from(qt[c.tq][0]);
                        let mut k = 1usize;
                        while k < 64 {
                            let rs = br.decode_huff(ac_h[c.ac].as_ref().unwrap())?;
                            let r = (rs >> 4) & 0xF;
                            let ss = rs & 0xF;
                            if ss == 0 {
                                if r != 15 {
                                    break; // EOB
                                }
                                k += 16;
                                continue;
                            }
                            k += r as usize;
                            if k > 63 {
                                if std::env::var("JPEG_DEBUG").as_deref() == Ok("1") {
                                    eprintln!("[jpeg] k>63: k={k} mcu={mcu_count} ci={ci} pos={}", br.pos);
                                }
                                return Err(JpegError::BadData);
                            }
                            block[ZIGZAG[k]] = extend(br.bits(u32::from(ss))?, u32::from(ss)) * i32::from(qt[c.tq][k]);
                            k += 1;
                        }
                        idct8x8(&mut block);
                        // Записать в план компоненты
                        let px = (mx * c.h + hh) * 8;
                        let py = (my * c.v + vv) * 8;
                        let pw = mcus_x * c.h * 8;
                        let plane = &mut planes[ci];
                        for row in 0..8 {
                            for col in 0..8 {
                                let v = (block[row * 8 + col]).clamp(-128, 1023) as i16 + 128;
                                let dst = (py + row) * pw + px + col;
                                if dst < plane.len() {
                                    plane[dst] = v;
                                }
                            }
                        }
                    }
                }
            }
            mcu_count += 1;
            if mcu_count <= 3 && std::env::var("JPEG_DEBUG").as_deref() == Ok("1") {
                eprintln!("[jpeg] mcu {mcu_count} ok");
            }
            if mcu_count > mcus_x * mcus_y + 4 {
                break 'mcus;
            }
        }
    }

    // Цвет: 1 компонента — gray; 3 — YCbCr (простая репликация сэмплирования)
    if std::env::var("JPEG_DEBUG").as_deref() == Ok("1") {
        for (ci, c) in comps.iter().enumerate() {
            eprintln!("[plane] ci={ci} первый={:?}", planes[ci][0]);
        }
    }
    let mut out = vec![0u8; w * h * 3];
    if comps.len() == 1 {
        let pw = mcus_x * comps[0].h * 8;
        for y in 0..h {
            for x in 0..w {
                let v = planes[0][y * pw + x].clamp(0, 255) as u8;
                let o = (y * w + x) * 3;
                out[o] = v;
                out[o + 1] = v;
                out[o + 2] = v;
            }
        }
    } else {
        // Y = comps[0], Cb = 1, Cr = 2 (стандартный JFIF)
        let pw = |c: &Comp| mcus_x * c.h * 8;
        let ypw = pw(&comps[0]);
        let cbpw = pw(&comps[1]);
        let crpw = pw(&comps[2]);
        for y in 0..h {
            let cy = y * comps[0].v / vmax;
            let cb_y = y * comps[1].v / vmax;
            let cr_y = y * comps[2].v / vmax;
            for x in 0..w {
                let cx = x * comps[0].h / hmax;
                let cb_x = x * comps[1].h / hmax;
                let cr_x = x * comps[2].h / hmax;
                let yy = i32::from(planes[0][cy * ypw + cx]);
                let cb = i32::from(planes[1][cb_y * cbpw + cb_x]) - 128;
                let cr = i32::from(planes[2][cr_y * crpw + cr_x]) - 128;
                let r = (yy + 91881 * cr / 65536).clamp(0, 255) as u8;
                let g = (yy - 22554 * cb / 65536 - 46802 * cr / 65536).clamp(0, 255) as u8;
                let bch = (yy + 116130 * cb / 65536).clamp(0, 255) as u8;
                let o = (y * w + x) * 3;
                out[o] = r;
                out[o + 1] = g;
                out[o + 2] = bch;
            }
        }
    }
    Ok((w, h, out))
}

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

/// Разделяемая IDCT 8×8 (float): блок коэффициентов -> блок пространственных значений (уже +128 вне).
fn idct8x8(block: &mut [i32; 64]) {
    const PI: f64 = std::f64::consts::PI;
    // Предвычисленная таблица косинусов: c[u][x]
    let mut tmp = [0f64; 64];
    for (y, row) in tmp.chunks_mut(8).enumerate() {
        // 1D IDCT по строкам
        for x in 0..8 {
            let mut s = 0f64;
            for u in 0..8 {
                let cu = if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                s += cu as f64 * block[y * 8 + u] as f64 * (PI / 8.0 * (x as f64 + 0.5) * u as f64).cos();
            }
            row[x] = s * 0.5;
        }
    }
    // 1D по столбцам
    for x in 0..8 {
        for y in 0..8 {
            let mut s = 0f64;
            for v in 0..8 {
                let cv = if v == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                s += cv * tmp[v * 8 + x] * (PI / 8.0 * (y as f64 + 0.5) * v as f64).cos();
            }
            block[y * 8 + x] = (s * 0.5).round() as i32; // уровень +128 добавляется при записи в план
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn mean_err(px: &[u8], w: usize, h: usize, expect: [u8; 3]) -> f64 {
        // среднее |канал - ожидание|
        let mut sum = 0f64;
        for p in px.chunks(3) {
            for k in 0..3 {
                sum += (i32::from(p[k]) - i32::from(expect[k])).abs() as f64;
            }
        }
        sum / (w * h * 3) as f64
    }

    #[test]
    fn solid_red() {
        let data = std::fs::read("tests/fixtures/red.jpg").unwrap();
        let (w, h, px) = decode_jpeg(&data).unwrap();
        assert_eq!((w, h), (160, 120));
        let err = mean_err(&px, w, h, [255, 0, 0]);
        assert!(err < 12.0, "средняя ошибка красного: {err}");
    }

    #[test]
    fn solid_blue() {
        let data = std::fs::read("tests/fixtures/blue.jpg").unwrap();
        let (w, h, px) = decode_jpeg(&data).unwrap();
        assert_eq!((w, h), (200, 100));
        let err = mean_err(&px, w, h, [0x1E, 0x90, 0xFF]);
        assert!(err < 12.0, "средняя ошибка синего: {err}");
    }

    #[test]
    fn not_jpeg_rejected() {
        assert_eq!(decode_jpeg(b"MZ"), Err(JpegError::NotJpeg));
    }
}
