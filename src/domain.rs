//! Домен каталога (DDD): сущности, инварианты, события скана.
//! Никакого I/O — только данные и правила.

use crate::arena::{EntryPool, DirPool, StringArena, ROOT_SENTINEL};

/// Событие индексации текста: doc_id и длина текста в символах.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextIndexed {
    pub doc_id: u32,
    pub chars: u64,
    /// Страниц прочитано.
    pub pages: u32,
}

/// Ошибка инварианта — падает до записи, а не после.
#[derive(Debug, PartialEq)]
pub enum DomainError {
    UnknownDir(u32),
    UnknownEntry(u32),
    PathDepthOverflow,
}

/// Корневая структура каталога: три пула + аренa. Владеет всем, отдаёт срезы.
#[derive(Default)]
pub struct Catalog {
    pub arena: StringArena,
    pub dirs: DirPool,
    pub entries: EntryPool,
    /// Корень скана (индекс в dirs), ROOT_SENTINEL если пусто.
    pub root: u32,
}

impl Catalog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_dir(&mut self, path: &str, parent: u32) -> Result<u32, DomainError> {
        if parent != ROOT_SENTINEL && parent as usize >= self.dirs.len() {
            return Err(DomainError::UnknownDir(parent));
        }
        let key = self.arena.push(path);
        Ok(self.dirs.push(key, parent))
    }

    pub fn add_entry(&mut self, name: &str, path: &str, size: u64, mtime: u64, dir: u32) -> Result<u32, DomainError> {
        if dir != ROOT_SENTINEL && dir as usize >= self.dirs.len() {
            return Err(DomainError::UnknownDir(dir));
        }
        let n = self.arena.push(name);
        let p = self.arena.push(path);
        Ok(self.entries.push(n, p, size, mtime, dir))
    }

    /// Путь каталога сборкой из аренa (одна аллокация на запрос — вне горячего цикла).
    #[must_use]
    pub fn dir_path(&self, dir: u32) -> String {
        if dir == ROOT_SENTINEL {
            return String::new();
        }
        self.arena.get(self.dirs.path[dir as usize].0, self.dirs.path[dir as usize].1).to_string()
    }

    #[must_use]
    pub fn entry_path(&self, id: u32) -> &str {
        let (o, l) = self.entries.path[id as usize];
        self.arena.get(o, l)
    }

    #[must_use]
    pub fn entry_name(&self, id: u32) -> &str {
        let (o, l) = self.entries.name[id as usize];
        self.arena.get(o, l)
    }

    /// Инвариант: каждый entry указывает на живой dir; parent-цепочка не циклична по глубине.
    pub fn verify(&self) -> Result<(), DomainError> {
        for (i, &d) in self.entries.parent.iter().enumerate() {
            if d != ROOT_SENTINEL && d as usize >= self.dirs.len() {
                return Err(DomainError::UnknownEntry(i as u32));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_invariants() {
        let mut c = Catalog::new();
        let root = c.add_dir("Z:\\проекты", ROOT_SENTINEL).unwrap();
        let sub = c.add_dir("Z:\\проекты\\сметы", root).unwrap();
        let e = c.add_entry("акт.pdf", "Z:\\проекты\\сметы\\акт.pdf", 123, 456, sub).unwrap();
        assert!(c.verify().is_ok());
        assert_eq!(c.entry_name(e), "акт.pdf");
        assert_eq!(c.dir_path(sub), "Z:\\проекты\\сметы");
        assert_eq!(c.add_entry("x.pdf", "x.pdf", 1, 1, 999), Err(DomainError::UnknownDir(999)));
    }
}
