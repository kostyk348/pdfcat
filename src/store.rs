//! Бинарный формат каталога «PDFCAT 1»: заголовок, счётчики, аренa, столбцы.
//! Данные пишутся подряд — чтение одним проходом, без зависимостей.

use crate::arena::{EntryPool, StringArena, DirPool, ROOT_SENTINEL};
use crate::domain::Catalog;
use std::io::{self, Read, Write};

const MAGIC: &[u8; 7] = b"PDFCAT1";

fn u32le(v: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn u64le(v: u64, out: &mut Vec<u8>) {
    out.extend_from_slice(&v.to_le_bytes());
}

impl Catalog {
    pub fn save(&self, path: &str) -> io::Result<()> {
        let mut b = Vec::with_capacity(1024 + self.arena.len());
        b.extend_from_slice(MAGIC);
        u32le(self.dirs.len() as u32, &mut b);
        u32le(self.entries.len() as u32, &mut b);
        u32le(self.arena.len() as u32, &mut b);
        u32le(self.root, &mut b);
        // Арена
        b.extend_from_slice(self.arena.bytes());
        // dirs: path(off,len), parent
        for i in 0..self.dirs.len() {
            u32le(self.dirs.path[i].0, &mut b);
            u32le(self.dirs.path[i].1, &mut b);
            u32le(self.dirs.parent[i], &mut b);
        }
        // entries
        for i in 0..self.entries.len() {
            u32le(self.entries.name[i].0, &mut b);
            u32le(self.entries.name[i].1, &mut b);
            u32le(self.entries.path[i].0, &mut b);
            u32le(self.entries.path[i].1, &mut b);
            u64le(self.entries.size[i], &mut b);
            u64le(self.entries.mtime[i], &mut b);
            u32le(self.entries.parent[i], &mut b);
            b.push(u8::from(self.entries.indexed[i]));
        }
        let mut f = std::fs::File::create(path)?;
        f.write_all(&b)?;
        Ok(())
    }

    pub fn load(path: &str) -> io::Result<Catalog> {
        let mut buf = Vec::new();
        std::fs::File::open(path)?.read_to_end(&mut buf)?;
        if buf.len() < 24 || &buf[..7] != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "не PDFCAT1"));
        }
        let rd_u32 = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let rd_u64 = |b: &[u8], o: usize| {
            u64::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7]])
        };
        let n_dirs = rd_u32(&buf, 7) as usize;
        let n_entries = rd_u32(&buf, 11) as usize;
        let arena_len = rd_u32(&buf, 15) as usize;
        let root = rd_u32(&buf, 19);
        let mut o = 23;
        if buf.len() < o + arena_len {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "обрезанный файл"));
        }
        let mut cat = Catalog {
            arena: StringArena::from_bytes(buf[o..o + arena_len].to_vec()),
            dirs: DirPool::new(),
            entries: EntryPool::new(),
            root,
        };
        o += arena_len;
        cat.dirs.path.reserve(n_dirs);
        for _ in 0..n_dirs {
            let (po, pl) = (rd_u32(&buf, o), rd_u32(&buf, o + 4));
            let parent = rd_u32(&buf, o + 8);
            o += 12;
            // Восстановление связного списка детей (first_child/next_sibling)
            let id = cat.dirs.path.len() as u32;
            cat.dirs.path.push((po, pl));
            cat.dirs.parent.push(parent);
            cat.dirs.first_child.push(ROOT_SENTINEL);
            cat.dirs.next_sibling.push(ROOT_SENTINEL);
            if parent != ROOT_SENTINEL {
                let prev = cat.dirs.first_child[parent as usize];
                cat.dirs.first_child[parent as usize] = id;
                cat.dirs.next_sibling[id as usize] = prev;
            }
        }
        cat.entries.name.reserve(n_entries);
        for _ in 0..n_entries {
            let no = rd_u32(&buf, o);
            let nl = rd_u32(&buf, o + 4);
            let po = rd_u32(&buf, o + 8);
            let pl = rd_u32(&buf, o + 12);
            let size = rd_u64(&buf, o + 16);
            let mtime = rd_u64(&buf, o + 24);
            let parent = rd_u32(&buf, o + 32);
            let indexed = buf[o + 36] != 0;
            o += 37;
            cat.entries.name.push((no, nl));
            cat.entries.path.push((po, pl));
            cat.entries.size.push(size);
            cat.entries.mtime.push(mtime);
            cat.entries.parent.push(parent);
            cat.entries.indexed.push(indexed);
        }
        Ok(cat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Catalog as C;

    #[test]
    fn roundtrip_catalog() {
        let mut c = C::new();
        let root = c.add_dir("Z:\\база", ROOT_SENTINEL).unwrap();
        let sub = c.add_dir("Z:\\база\\2026", root).unwrap();
        c.add_entry("акт.pdf", "Z:\\база\\2026\\акт.pdf", 1234, 777, sub).unwrap();
        let p = std::env::temp_dir().join("pdfcat_rt.bin");
        c.save(p.to_str().unwrap()).unwrap();
        let c2 = C::load(p.to_str().unwrap()).unwrap();
        assert_eq!(c2.dirs.len(), 2);
        assert_eq!(c2.entries.len(), 1);
        assert_eq!(c2.entry_name(0), "акт.pdf");
        assert_eq!(c2.dir_path(sub), "Z:\\база\\2026");
        let kids: Vec<u32> = c2.dirs.children(root).collect();
        assert_eq!(kids, vec![sub]);
        let _ = std::fs::remove_file(&p);
    }
}
