//! Собственный DEFLATE-декодер (zlib-обёртка): stored / fixed / dynamic Huffman.
//! Стиль puff.c: декодирование по счётчикам длин без битовых таблиц.
//! Потолок выходного буфера — защита от «бомбы».

const MAX_OUT: usize = 512 * 1024 * 1024;

#[derive(Debug, PartialEq)]
pub enum InflateError {
    Truncated,
    BadBlock,
    OutputOverflow,
    BadZlibHeader,
    AdlerMismatch,
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bitbuf: u32,
    bitcnt: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, bitbuf: 0, bitcnt: 0 }
    }

    #[inline]
    fn bits(&mut self, need: u32) -> Result<u32, InflateError> {
        while self.bitcnt < need {
            if self.pos >= self.data.len() {
                return Err(InflateError::Truncated);
            }
            self.bitbuf |= u32::from(self.data[self.pos]) << self.bitcnt;
            self.pos += 1;
            self.bitcnt += 8;
        }
        let val = self.bitbuf & ((1u32 << need) - 1);
        self.bitbuf >>= need;
        self.bitcnt -= need;
        Ok(val)
    }

    fn align(&mut self) {
        self.bitcnt = 0;
        self.bitbuf = 0;
    }

    /// Байт-ориентированное чтение (stored-блок) — после align.
    fn byte(&mut self) -> Result<u8, InflateError> {
        if self.pos >= self.data.len() {
            return Err(InflateError::Truncated);
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }
}

/// Канонический Хаффман: count[len], symbols — по возрастанию кодов.
struct Huffman {
    count: [u16; 16],
    symbols: Vec<u16>,
}

fn build(lengths: &[u8]) -> Result<Huffman, InflateError> {
    let mut count = [0u16; 16];
    for &l in lengths {
        if l > 15 {
            return Err(InflateError::BadBlock);
        }
        count[l as usize] += 1;
    }
    if count[0] as usize == lengths.len() {
        return Err(InflateError::BadBlock); // пустое дерево
    }
    // Проверка переполнения (left = 1; каждый уровень left -= count; left <<= 1)
    let mut left: i32 = 1;
    for len in 1..16 {
        left <<= 1;
        left -= i32::from(count[len]);
        if left < 0 {
            return Err(InflateError::BadBlock);
        }
    }
    let mut offs = [0u16; 16];
    for len in 1..15 {
        offs[len + 1] = offs[len] + count[len];
    }
    let mut symbols = vec![0u16; lengths.len()];
    for (sym, &l) in lengths.iter().enumerate() {
        if l != 0 {
            symbols[offs[l as usize] as usize] = sym as u16;
            offs[l as usize] += 1;
        }
    }
    Ok(Huffman { count, symbols })
}

impl Huffman {
    /// Декодирование одного символа: O(длина кода), без таблиц.
    fn decode(&self, br: &mut BitReader<'_>) -> Result<u16, InflateError> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16usize {
            code |= br.bits(1)? as i32;
            let cnt = i32::from(self.count[len]);
            if code - first < cnt {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += cnt;
            first = (first + cnt) << 1;
            code <<= 1;
        }
        Err(InflateError::BadBlock)
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn fixed_lit_lengths() -> [u8; 288] {
    let mut l = [0u8; 288];
    for (i, item) in l.iter_mut().enumerate() {
        *item = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    l
}

/// Инфляция сырого DEFLATE-потока в свежий Vec.
pub fn inflate(data: &[u8]) -> Result<Vec<u8>, InflateError> {
    let mut out: Vec<u8> = Vec::with_capacity(data.len() * 4);
    let mut br = BitReader::new(data);
    loop {
        let bfinal = br.bits(1)?;
        let btype = br.bits(2)?;
        match btype {
            0 => {
                br.align();
                let len = u16::from(br.byte()?) | (u16::from(br.byte()?) << 8);
                let nlen = u16::from(br.byte()?) | (u16::from(br.byte()?) << 8);
                if len != !nlen {
                    return Err(InflateError::BadBlock);
                }
                let usize_len = usize::from(len);
                if out.len() + usize_len > MAX_OUT {
                    return Err(InflateError::OutputOverflow);
                }
                for _ in 0..usize_len {
                    out.push(br.byte()?);
                }
            }
            1 | 2 => {
                let (lit, dist);
                if btype == 1 {
                    lit = build(&fixed_lit_lengths())?;
                    dist = build(&[5u8; 30])?;
                } else {
                    let hlit = (br.bits(5)? as usize) + 257;
                    let hdist = (br.bits(5)? as usize) + 1;
                    let hclen = (br.bits(4)? as usize) + 4;
                    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                    let mut cl = [0u8; 19];
                    for &o in ORDER.iter().take(hclen) {
                        cl[o] = br.bits(3)? as u8;
                    }
                    let clh = build(&cl)?;
                    let mut lengths = vec![0u8; hlit + hdist];
                    let mut i = 0;
                    while i < lengths.len() {
                        let sym = clh.decode(&mut br)?;
                        match sym {
                            0..=15 => {
                                lengths[i] = sym as u8;
                                i += 1;
                            }
                            16 => {
                                if i == 0 {
                                    return Err(InflateError::BadBlock);
                                }
                                let rep = (br.bits(2)? as usize) + 3;
                                let prev = lengths[i - 1];
                                if i + rep > lengths.len() {
                                    return Err(InflateError::BadBlock);
                                }
                                lengths[i..i + rep].fill(prev);
                                i += rep;
                            }
                            17 => {
                                let rep = (br.bits(3)? as usize) + 3;
                                if i + rep > lengths.len() {
                                    return Err(InflateError::BadBlock);
                                }
                                lengths[i..i + rep].fill(0);
                                i += rep;
                            }
                            18 => {
                                let rep = (br.bits(7)? as usize) + 11;
                                if i + rep > lengths.len() {
                                    return Err(InflateError::BadBlock);
                                }
                                lengths[i..i + rep].fill(0);
                                i += rep;
                            }
                            _ => return Err(InflateError::BadBlock),
                        }
                    }
                    lit = build(&lengths[..hlit])?;
                    dist = build(&lengths[hlit..])?;
                }
                // Блок данных
                loop {
                    let sym = lit.decode(&mut br)?;
                    if sym < 256 {
                        if out.len() >= MAX_OUT {
                            return Err(InflateError::OutputOverflow);
                        }
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let idx = usize::from(sym) - 257;
                        if idx >= 29 {
                            return Err(InflateError::BadBlock);
                        }
                        let len = usize::from(LEN_BASE[idx]) + (br.bits(u32::from(LEN_EXTRA[idx]))? as usize);
                        let dsym = dist.decode(&mut br)? as usize;
                        if dsym >= 30 {
                            return Err(InflateError::BadBlock);
                        }
                        let d = usize::from(DIST_BASE[dsym]) + (br.bits(u32::from(DIST_EXTRA[dsym]))? as usize);
                        if d > out.len() {
                            return Err(InflateError::BadBlock);
                        }
                        if out.len() + len > MAX_OUT {
                            return Err(InflateError::OutputOverflow);
                        }
                        // Копирование с перекрытием: побайтово (окно ≤ 258, это дёшево).
                        let start = out.len() - d;
                        for k in 0..len {
                            let b = out[start + k];
                            out.push(b);
                        }
                    }
                }
            }
            _ => return Err(InflateError::BadBlock),
        }
        if bfinal == 1 {
            break;
        }
    }
    Ok(out)
}

/// Adler-32 (zlib-контрольная сумма).
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// Полный zlib-поток: заголовок + deflate + adler32.
pub fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, InflateError> {
    if data.len() < 6 {
        return Err(InflateError::BadZlibHeader);
    }
    // CMF/FLG: метод 8, FDICT запрещён (словарь не поддерживаем — редкость, честный отказ).
    if data[0] & 0x0F != 8 {
        return Err(InflateError::BadZlibHeader);
    }
    if data[1] & 0x20 != 0 {
        return Err(InflateError::BadZlibHeader);
    }
    let body = &data[2..data.len() - 4];
    let out = inflate(body)?;
    let expect = u32::from_be_bytes([data[data.len() - 4], data[data.len() - 3], data[data.len() - 2], data[data.len() - 1]]);
    if adler32(&out) != expect {
        return Err(InflateError::AdlerMismatch);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adler_known() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"abc"), 0x024d0127);
    }

    #[test]
    fn stored_block() {
        // BFINAL=1, BTYPE=00, LEN=3, NLEN, данные
        let data = [0x01, 0x03, 0x00, 0xFC, 0xFF, b'p', b'd', b'f'];
        let out = inflate(&data).unwrap();
        assert_eq!(out, b"pdf");
    }

    #[test]
    fn truncated_rejected() {
        assert_eq!(inflate(&[0x07]), Err(InflateError::BadBlock)); // BFINAL=1, BTYPE=11 — запрещено
    }
}
