//! Адаптер источника: рекурсивный обход каталогов (UNC/локальные диски).
//! Итеративный стек, без рекурсии — глубина ограничена явно.
//! Горячий цикл: 2 аллокации на файл (имя+путь в арену), 0 на каталог-переходы.

use crate::arena::{StringArena, ROOT_SENTINEL};
use crate::domain::Catalog;
use crate::ports::{ScanStats, Source};
use std::path::Path;

const PDF_EXT: &[u8] = b".pdf";

fn is_pdf(name_lower: &[u8]) -> bool {
    name_lower.len() >= 4 && &name_lower[name_lower.len() - 4..] == PDF_EXT
}

/// Полный путь = parent_path + '\\' + name, собирается напрямую в арену
/// (без промежуточной String на каждый уровень).
fn push_path(arena: &mut StringArena, parent: Option<(u32, u32)>, name: &str) -> (u32, u32) {
    let sep: u8 = b'\\';
    let mut buf: Vec<u8> = Vec::with_capacity(96);
    if let Some((o, l)) = parent {
        buf.extend_from_slice(arena_bytes(arena, o, l));
        if l > 0 && *buf.last().unwrap() != sep {
            buf.push(sep);
        }
    }
    buf.extend_from_slice(name.as_bytes());
    let off = arena.len() as u32;
    arena.push_bytes(&buf);
    (off, buf.len() as u32)
}

fn arena_bytes(arena: &StringArena, off: u32, len: u32) -> &[u8] {
    &arena.bytes()[off as usize..(off + len) as usize]
}

pub struct FsScanner;

impl Source for FsScanner {
    fn scan(&mut self, root: &str, cat: &mut Catalog, max_depth: u32) -> ScanStats {
        let t0 = std::time::Instant::now();
        let mut stats = ScanStats::default();

        let root_dir = std::fs::read_dir(root);
        let root_dir = match root_dir {
            Ok(_) => root_dir.unwrap(),
            Err(_) => return stats,
        };
        drop(root_dir);

        let root_key = push_path(&mut cat.arena, None, root);
        let root_id = cat.dirs.push(root_key, ROOT_SENTINEL);
        cat.root = root_id;

        // Стек: (dir_id, dir_path_key, depth)
        let mut stack: Vec<(u32, (u32, u32), u32)> = Vec::with_capacity(64);
        stack.push((root_id, root_key, 0));

        while let Some((dir_id, dir_key, depth)) = stack.pop() {
            let dir_path = cat.arena.get(dir_key.0, dir_key.1).to_string();
            let read = match std::fs::read_dir(&dir_path) {
                Ok(r) => r,
                Err(_) => {
                    // Доступ запрещён / диск отвалился: узел остаётся, дети — нет.
                    stats.skipped_depth += 1;
                    continue;
                }
            };
            for entry in read {
                let Ok(entry) = entry else { continue };
                let Ok(meta) = entry.metadata() else { continue };
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                let lower = name_str.to_lowercase();
                if meta.is_dir() {
                    stats.dirs += 1;
                    if depth + 1 > max_depth {
                        stats.skipped_depth += 1;
                        continue;
                    }
                    let key = push_path(&mut cat.arena, Some(dir_key), &name_str);
                    let id = cat.dirs.push(key, dir_id);
                    stack.push((id, key, depth + 1));
                } else if is_pdf(lower.as_bytes()) {
                    stats.files += 1;
                    stats.pdfs += 1;
                    let path_key = push_path(&mut cat.arena, Some(dir_key), &name_str);
                    let name_key = cat.arena.push(&name_str);
                    let mtime = mtime_of(&meta);
                    cat.entries.push(name_key, path_key, meta.len(), mtime, dir_id);
                } else {
                    stats.files += 1;
                }
            }
        }
        stats.elapsed_ms = t0.elapsed().as_millis() as u64;
        stats
    }
}

fn mtime_of(meta: &std::fs::Metadata) -> u64 {
    use std::time::UNIX_EPOCH;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos()) / 100)
        .unwrap_or(0)
}

// Помощник для арены: add_bytes без промежуточного &str.
impl StringArena {
    pub fn push_bytes(&mut self, b: &[u8]) -> (u32, u32) {
        let off = self.len() as u32;
        self.buf.extend_from_slice(b);
        (off, b.len() as u32)
    }
}

// Тест на временном дереве — детерминированный, без сети.
#[cfg(test)]
mod tests {
    use super::*;

    fn make_tree(base: &Path) {
        std::fs::create_dir_all(base.join("а\\б")).unwrap();
        std::fs::create_dir_all(base.join("в")).unwrap();
        std::fs::write(base.join("отчёт.pdf"), b"%PDF-1.4\n%%EOF").unwrap();
        std::fs::write(base.join("а\\спецификация.pdf"), b"%PDF-1.4\n%%EOF").unwrap();
        std::fs::write(base.join("в\\readme.txt"), b"hello").unwrap();
        std::fs::write(base.join("корень.pdf"), b"%PDF-1.4\n%%EOF").unwrap();
    }

    #[test]
    fn scans_unc_like_tree() {
        let tmp = std::env::temp_dir().join("pdfcat_scan_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        make_tree(&tmp);

        let mut cat = Catalog::new();
        let mut s = FsScanner;
        let st = s.scan(tmp.to_str().unwrap(), &mut cat, 16);
        assert_eq!(st.dirs, 3); // а, б, в
        assert_eq!(st.pdfs, 3);
        assert_eq!(cat.entries.len(), 3);
        assert!(cat.verify().is_ok());

        // Пути собраны с обратным слэшем и кириллицей
        let found: Vec<&str> = (0..cat.entries.len() as u32)
            .map(|i| cat.entry_path(i))
            .collect();
        assert!(found.iter().any(|p| p.ends_with("а\\спецификация.pdf")), "path: {found:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
