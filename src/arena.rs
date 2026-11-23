//! Арена строк и SoA-хранилище каталога (DOP).
//! Строки — один растущий буфер; везде u32-индексы вместо ссылок.

/// Пул строк: append-only, идентификатор — (смещение, длина).
/// Без инвалидации: строки не редактируются, только добавляются.
pub struct StringArena {
    pub(crate) buf: Vec<u8>,
}

impl Default for StringArena {
    fn default() -> Self {
        Self::new()
    }
}

impl StringArena {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(1 << 16) }
    }

    /// Добавить строку, вернуть (offset, len). Выделяет ровно один раз на строку.
    #[must_use]
    pub fn push(&mut self, s: &str) -> (u32, u32) {
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(s.as_bytes());
        (off, s.len() as u32)
    }

    #[must_use]
    pub fn get(&self, off: u32, len: u32) -> &str {
        let o = off as usize;
        let l = len as usize;
        debug_assert!(o + l <= self.buf.len());
        // Буфер заполнен только валидным UTF-8 целиком, поэтому срез безопасен.
        std::str::from_utf8(&self.buf[o..o + l]).unwrap_or("")
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    #[must_use]
    pub fn from_bytes(b: Vec<u8>) -> Self {
        Self { buf: b }
    }
}

/// SoA-хранилище записей каталога: массивы-столбцы вместо объектов.
/// Горячие операции (скан, поиск имён, фильтры) ходят по столбцам — кеш-линейно.
#[derive(Default)]
pub struct EntryPool {
    /// Имя файла в арене (offset, len).
    pub name: Vec<(u32, u32)>,
    /// Полный путь в арене (offset, len).
    pub path: Vec<(u32, u32)>,
    /// Размер в байтах.
    pub size: Vec<u64>,
    /// mtime как Windows FILETIME (u64, интервалы 100 нс).
    pub mtime: Vec<u64>,
    /// Индекс каталога-родителя в `DirPool`.
    pub parent: Vec<u32>,
    /// Есть ли извлечённый текст (индекс текста — тот же doc_id).
    pub indexed: Vec<bool>,
}

impl EntryPool {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, name: (u32, u32), path: (u32, u32), size: u64, mtime: u64, parent: u32) -> u32 {
        let id = self.name.len() as u32;
        self.name.push(name);
        self.path.push(path);
        self.size.push(size);
        self.mtime.push(mtime);
        self.parent.push(parent);
        self.indexed.push(false);
        id
    }

    pub fn len(&self) -> usize {
        self.name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

/// Каталог-узел графа: пути каталогов — тоже арендные строки.
#[derive(Default)]
pub struct DirPool {
    pub path: Vec<(u32, u32)>,
    pub parent: Vec<u32>, // u32::MAX для корня
    pub first_child: Vec<u32>,
    pub next_sibling: Vec<u32>,
}

pub const ROOT_SENTINEL: u32 = u32::MAX;

impl DirPool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            path: Vec::new(),
            parent: Vec::new(),
            first_child: Vec::new(),
            next_sibling: Vec::new(),
        }
    }

    /// Связный список детей: first_child/next_sibling вместо Vec у каждого узла.
    pub fn push(&mut self, path: (u32, u32), parent: u32) -> u32 {
        let id = self.path.len() as u32;
        self.path.push(path);
        self.parent.push(parent);
        self.first_child.push(ROOT_SENTINEL);
        self.next_sibling.push(ROOT_SENTINEL);
        if parent != ROOT_SENTINEL {
            let prev_first = self.first_child[parent as usize];
            self.first_child[parent as usize] = id;
            self.next_sibling[id as usize] = prev_first;
        }
        id
    }

    pub fn len(&self) -> usize {
        self.path.len()
    }

    pub fn is_empty(&self) -> bool {
        self.path.is_empty()
    }

    /// Обход детей узла — итератор без аллокаций.
    pub fn children(&self, dir: u32) -> ChildIter<'_> {
        ChildIter { pool: self, next: self.first_child[dir as usize] }
    }
}

pub struct ChildIter<'a> {
    pool: &'a DirPool,
    next: u32,
}

impl<'a> Iterator for ChildIter<'a> {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        let cur = self.next;
        if cur == ROOT_SENTINEL {
            return None;
        }
        self.next = self.pool.next_sibling[cur as usize];
        Some(cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_roundtrip() {
        let mut a = StringArena::new();
        let (o1, l1) = a.push("отчёт 2026.pdf");
        let (o2, l2) = a.push("спецификация.pdf");
        assert_eq!(a.get(o1, l1), "отчёт 2026.pdf");
        assert_eq!(a.get(o2, l2), "спецификация.pdf");
        assert_eq!(a.len(), "отчёт 2026.pdf".len() + "спецификация.pdf".len());
    }

    #[test]
    fn dir_children_linked_list() {
        let mut d = DirPool::new();
        let root = d.push((0, 0), ROOT_SENTINEL);
        let a = d.push((4, 1), root);
        let b = d.push((8, 1), root);
        let _c = d.push((12, 1), a); // ребёнок ребёнка не попадает в root
        let kids: Vec<u32> = d.children(root).collect();
        assert_eq!(kids, vec![b, a]); // LIFO вставка
    }

    #[test]
    fn entry_pool_columns() {
        let mut arena = StringArena::new();
        let mut p = EntryPool::new();
        let (o, l) = arena.push("x.pdf");
        let id = p.push((o, l), (o, l), 100, 0, ROOT_SENTINEL);
        assert_eq!(p.len(), 1);
        assert_eq!(p.parent[id as usize], ROOT_SENTINEL);
        assert!(!p.indexed[id as usize]);
    }
}
