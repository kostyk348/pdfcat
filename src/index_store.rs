//! Персистентность BM25-индекса: «PDFBM25 1» — термины с постингами и длины документов.
//! Один файл, один проход записи/чтения, ноль зависимостей. Термины сортированы — файл детерминирован.

use crate::bm25::Bm25;
use std::io::{self, Read, Write};

const MAGIC: &[u8; 9] = b"PDFBM25 1";

impl Bm25 {
    pub fn save(&self, path: &str) -> io::Result<()> {
        let mut terms: Vec<&Vec<u8>> = self.terms_sorted();
        terms.sort_unstable();
        terms.dedup();
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&(terms.len() as u32).to_le_bytes());
        b.extend_from_slice(&(self.doc_len.len() as u32).to_le_bytes());
        b.extend_from_slice(&self.avgdl.to_bits().to_le_bytes());
        for &l in &self.doc_len {
            b.extend_from_slice(&l.to_le_bytes());
        }
        for t in terms {
            b.extend_from_slice(&(t.len() as u32).to_le_bytes());
            b.extend_from_slice(t);
            let post = self.postings_of(t);
            b.extend_from_slice(&(post.len() as u32).to_le_bytes());
            for &(doc, tf) in post {
                b.extend_from_slice(&doc.to_le_bytes());
                b.extend_from_slice(&tf.to_le_bytes());
            }
        }
        std::fs::File::create(path)?.write_all(&b)
    }

    pub fn load(path: &str) -> io::Result<Self> {
        let mut buf = Vec::new();
        std::fs::File::open(path)?.read_to_end(&mut buf)?;
        if buf.len() < 21 || &buf[..9] != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "не PDFBM25 1"));
        }
        let n_terms = u32le(&buf, 9) as usize;
        let n_docs = u32le(&buf, 13) as usize;
        let avgdl = f32::from_bits(u32le(&buf, 17));
        let mut o = 21usize;
        if buf.len() < o + n_docs * 4 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "обрезанный файл"));
        }
        let mut idx = Bm25::new();
        idx.restore_doc_len(&buf[o..o + n_docs * 4], avgdl);
        o += n_docs * 4;
        for _ in 0..n_terms {
            if o + 4 > buf.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "термин обрезан"));
            }
            let tlen = u32le(&buf, o) as usize;
            o += 4;
            if o + tlen > buf.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "термин обрезан"));
            }
            let term = buf[o..o + tlen].to_vec();
            o += tlen;
            if o + 4 > buf.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "постинги обрезаны"));
            }
            let n_post = u32le(&buf, o) as usize;
            o += 4;
            if o + n_post * 8 > buf.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "постинги обрезаны"));
            }
            let mut post = Vec::with_capacity(n_post);
            for _ in 0..n_post {
                let doc = u32le(&buf, o);
                let tf = u32le(&buf, o + 4);
                o += 8;
                post.push((doc, tf));
            }
            idx.insert_term(term, post);
        }
        Ok(idx)
    }
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Searcher;

    #[test]
    fn index_roundtrip() {
        let mut idx = Bm25::new();
        idx.index(0, "смета на кабели и материалы");
        idx.index(1, "паспорт котла КЭВ отчёт");
        idx.index(2, "смета материалов оборудования смета");
        let p = std::env::temp_dir().join("pdfcat_idx.bin");
        idx.save(p.to_str().unwrap()).unwrap();
        let idx2 = Bm25::load(p.to_str().unwrap()).unwrap();
        assert_eq!(idx2.len(), 3);
        let a = idx.search("смета", 10);
        let b = idx2.search("смета", 10);
        assert_eq!(a.len(), b.len());
        assert_eq!(a[0].doc_id, b[0].doc_id);
        assert!((a[0].score - b[0].score).abs() < 1e-4);
        let _ = std::fs::remove_file(&p);
    }
}
