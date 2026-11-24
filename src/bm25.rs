//! BM25-индекс: инвертированные постинги SoA, полная развёртка запроса,
//! накопление очков — SIMD (AVX2 при наличии, иначе SSE2, иначе скаляр).
//! Горячий путь: суммирование f32-веса по массиву постингов.

use crate::ports::{Hit, Searcher};
use std::collections::HashMap;

const K1: f32 = 1.2;
const B: f32 = 0.75;

pub struct Bm25 {
    /// Термин → постинги (doc_id, tf) — doc_id возрастает (упрощает AND-пересечения позже).
    pub(crate) postings: HashMap<Vec<u8>, Vec<(u32, u32)>>,
    /// Длина документа в токенах.
    pub(crate) doc_len: Vec<u32>,
    pub(crate) avgdl: f32,
    total_terms: u64,
}

impl Bm25 {
    #[must_use]
    pub fn new() -> Self {
        Self { postings: HashMap::new(), doc_len: Vec::new(), avgdl: 0.0, total_terms: 0 }
    }

    fn recompute_avgdl(&mut self) {
        let n = self.doc_len.len();
        if n > 0 {
            // SIMD-сумма длин (агрегат по SoA-столбцу — как в wirecraft).
            self.avgdl = sum_f32_simd(&self.doc_len.iter().map(|l| *l as f32).collect::<Vec<f32>>()) / n as f32;
        }
    }

    fn index_text(&mut self, doc_id: u32, text: &[u8]) {
        let mut len: u32 = 0;
        let mut start: Option<usize> = None;
        for (i, &b) in text.iter().chain(std::iter::once(&b' ')).enumerate() {
            let is_letter = b.is_ascii_alphanumeric() || b >= 0x80;
            if is_letter {
                if start.is_none() {
                    start = Some(i);
                }
            } else if let Some(s) = start.take() {
                let key = lower_key(&text[s..i]);
                len += 1;
                let post = self.postings.entry(key).or_default();
                match post.last_mut() {
                    Some(last_item) if last_item.0 == doc_id => last_item.1 += 1,
                    _ => post.push((doc_id, 1)),
                }
                self.total_terms += 1;
            }
        }
        self.doc_len.push(len);
        self.recompute_avgdl();
    }

    /// Развёртка терминов: постинги обрабатываются батчами по 8 — tf, doc_len (gather)
    /// и арифметика BM25 в AVX2; запись очков — 8 скалярных инкрементов (scatter в AVX2 нет).
    /// O(суммарных постингов запроса) по времени.
    fn score_all(&self, terms: &[Vec<u8>]) -> Vec<f32> {
        let n = self.doc_len.len();
        let mut scores = vec![0.0f32; n];
        let dfn = n as f32;
        for term in terms {
            let Some(post) = self.postings.get(term) else { continue };
            let df = post.len() as f32;
            let idf = (1.0 + (dfn - df + 0.5) / (df + 0.5)).ln();
            let ids: Vec<u32> = post.iter().map(|&(d, _)| d).collect();
            let tfs: Vec<f32> = post.iter().map(|&(_, t)| t as f32).collect();
            #[cfg(target_arch = "x86_64")]
            {
                if is_x86_feature_detected!("avx2") {
                    unsafe { score_batch_avx2(&ids, &tfs, self.doc_len.as_slice(), self.avgdl.max(1.0), idf, &mut scores) };
                    continue;
                }
            }
            score_batch_scalar(&ids, &tfs, self.doc_len.as_slice(), self.avgdl.max(1.0), idf, &mut scores);
        }
        scores
    }

    /// Слияние чанка: doc_id сдвигается на offset (чанки не пересекаются).
    pub fn merge_chunk(&mut self, other: Bm25, offset: u32) {
        for (term, posts) in other.postings {
            self.postings
                .entry(term)
                .or_default()
                .extend(posts.into_iter().map(|(d, tf)| (d + offset, tf)));
        }
        self.doc_len.extend(other.doc_len);
        self.total_terms += other.total_terms;
        self.recompute_avgdl();
    }

    pub fn stats(&self) -> (usize, u64) {
        (self.postings.len(), self.total_terms)
    }

    // --- доступ для персистентности (index_store.rs) ---

    pub(crate) fn terms_sorted(&self) -> Vec<&Vec<u8>> {
        self.postings.keys().collect()
    }

    pub(crate) fn postings_of(&self, term: &[u8]) -> &[(u32, u32)] {
        self.postings.get(term).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub(crate) fn insert_term(&mut self, term: Vec<u8>, post: Vec<(u32, u32)>) {
        self.postings.insert(term, post);
    }

    /// Восстановление длин из файла БЕЗ пересчёта avgdl (он сохранён явно).
    pub(crate) fn restore_doc_len(&mut self, lens_le: &[u8], avgdl: f32) {
        self.doc_len = lens_le
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        self.avgdl = avgdl;
    }
}

fn score_batch_scalar(ids: &[u32], tfs: &[f32], doc_len: &[u32], avgdl: f32, idf: f32, scores: &mut [f32]) {
    for (i, &doc) in ids.iter().enumerate() {
        let tf = tfs[i];
        let dl = doc_len[doc as usize] as f32;
        let norm = K1 * (1.0 - B + B * dl / avgdl);
        scores[doc as usize] += idf * tf * (K1 + 1.0) / (tf + norm);
    }
}

/// SIMD-батч: gather doc_len по индексам документов, арифметика BM25 вектором,
/// инкремент очков — 8 адресов (scatter в AVX2 отсутствует; AVX-512 сознательно не трогаем — Win7/старые CPU).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn score_batch_avx2(ids: &[u32], tfs: &[f32], doc_len: &[u32], avgdl: f32, idf: f32, scores: &mut [f32]) {
    use std::arch::x86_64::*;
    let k1 = _mm256_set1_ps(K1);
    let b = _mm256_set1_ps(B);
    let one = _mm256_set1_ps(1.0);
    let k1p1 = _mm256_set1_ps(K1 + 1.0);
    let avg = _mm256_set1_ps(avgdl);
    let idf_v = _mm256_set1_ps(idf as f32);
    let mut i = 0;
    while i + 8 <= ids.len() {
        let mut idx_i32 = [0i32; 8];
        for k in 0..8 {
            idx_i32[k] = ids[i + k] as i32;
        }
        let idx = _mm256_loadu_si256(idx_i32.as_ptr() as *const __m256i);
        let dl = _mm256_i32gather_ps(doc_len.as_ptr() as *const f32, idx, 4);
        let tf = _mm256_loadu_ps(tfs[i..].as_ptr());
        let norm = _mm256_mul_ps(k1, _mm256_add_ps(one, _mm256_mul_ps(b, _mm256_div_ps(dl, avg))));
        let s = _mm256_mul_ps(idf_v, _mm256_div_ps(_mm256_mul_ps(tf, k1p1), _mm256_add_ps(tf, norm)));
        // Инкремент по 8 адресам (без scatter)
        let mut arr = [0f32; 8];
        _mm256_storeu_ps(arr.as_mut_ptr(), s);
        for k in 0..8 {
            scores[ids[i + k] as usize] += arr[k];
        }
        i += 8;
    }
    if i < ids.len() {
        score_batch_scalar(&ids[i..], &tfs[i..], doc_len, avgdl, idf, scores);
    }
}

/// SIMD-сумма f32 (AVX2 → SSE2 → скаляр).
fn sum_f32_simd(v: &[f32]) -> f32 {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { sum_avx2(v) };
        }
        if is_x86_feature_detected!("sse2") {
            return unsafe { sum_sse2(v) };
        }
    }
    v.iter().sum()
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sum_avx2(v: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let mut acc = _mm256_setzero_ps();
    let mut i = 0;
    while i + 8 <= v.len() {
        acc = _mm256_add_ps(acc, _mm256_loadu_ps(v[i..].as_ptr()));
        i += 8;
    }
    let mut t = [0f32; 8];
    _mm256_storeu_ps(t.as_mut_ptr(), acc);
    let mut s: f32 = t.iter().sum();
    s += v[i..].iter().sum::<f32>();
    s
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn sum_sse2(v: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let mut acc = _mm_setzero_ps();
    let mut i = 0;
    while i + 4 <= v.len() {
        acc = _mm_add_ps(acc, _mm_loadu_ps(v[i..].as_ptr()));
        i += 4;
    }
    let mut t = [0f32; 4];
    _mm_storeu_ps(t.as_mut_ptr(), acc);
    let mut s: f32 = t.iter().sum();
    s += v[i..].iter().sum::<f32>();
    s
}

fn lower_key(bytes: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_lowercase().into_bytes(),
        Err(_) => bytes.to_ascii_lowercase(),
    }
}

impl Searcher for Bm25 {
    fn index(&mut self, doc_id: u32, text: &str) {
        self.index_text(doc_id, text.as_bytes());
    }

    fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let mut terms: Vec<Vec<u8>> = Vec::new();
        let bytes = query.as_bytes();
        let mut start: Option<usize> = None;
        for (i, &b) in bytes.iter().chain(std::iter::once(&b' ')).enumerate() {
            let is_letter = b.is_ascii_alphanumeric() || b >= 0x80;
            if is_letter {
                if start.is_none() {
                    start = Some(i);
                }
            } else if let Some(s) = start.take() {
                terms.push(lower_key(&bytes[s..i]));
            }
        }
        if terms.is_empty() {
            return Vec::new();
        }
        let scores = self.score_all(&terms);
        let mut hits: Vec<Hit> = Vec::with_capacity(limit);
        // Полная развёртка: выбираем top-k простым частичным отбором.
        // k маленькое (UI-страница), O(N) — приемлемо; k-heap — этап позже.
        let mut cand: Vec<(u32, f32)> = Vec::new();
        for (i, &s) in scores.iter().enumerate() {
            if s > 0.0 {
                cand.push((i as u32, s));
            }
        }
        cand.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (doc, s) in cand.into_iter().take(limit) {
            hits.push(Hit { doc_id: doc, score: f64::from(s) });
        }
        hits
    }

    fn len(&self) -> usize {
        self.doc_len.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(text: &str) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let b = text.as_bytes();
        let mut start: Option<usize> = None;
        for (i, &c) in b.iter().chain(std::iter::once(&b' ')).enumerate() {
            let is_letter = c.is_ascii_alphanumeric() || c >= 0x80;
            if is_letter {
                if start.is_none() {
                    start = Some(i);
                }
            } else if let Some(s) = start.take() {
                out.push(lower_key(&b[s..i]));
            }
        }
        out
    }

    #[test]
    fn bm25_ranks_relevant_first() {
        let mut idx = Bm25::new();
        idx.index(0, "отчёт по кабелям и смета на кабели");
        idx.index(1, "паспорт котла КЭВ и технические данные");
        idx.index(2, "смета материалов и оборудования");
        let hits = idx.search("смета", 10);
        assert!(!hits.is_empty());
        // Документы 0 и 2 содержат «смета»; 1 — нет
        assert!(hits.iter().all(|h| h.doc_id != 1));
        // Оценки убывают
        for w in hits.windows(2) {
            assert!(w[0].score >= w[1].score);
        }
    }

    #[test]
    fn cyrillic_tokenization() {
        let t = tok("Смета-2026 и ПДФ");
        assert_eq!(t.len(), 4); // смета · 2026 · и · пдф
        assert_eq!(t[0], "смета".as_bytes()); // юникодный lowercase
        assert_eq!(t[1], b"2026");
    }

    #[test]
    fn empty_query_empty_hits() {
        let idx = Bm25::new();
        assert!(idx.search("", 5).is_empty());
    }
}
