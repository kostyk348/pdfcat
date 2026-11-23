//! Порты (Hexagonal): домен ничего не знает о файловой системе, PDF-формате и рантайме поиска.

use crate::domain::Catalog;

/// Источник файлов: отдаёт записи каталога. Адаптер — файловая система/UNC.
pub trait Source {
    /// Рекурсивный скан корня. На каждый файл — вызов замыкания (без промежуточных Vec).
    fn scan(&mut self, root: &str, cat: &mut Catalog, max_depth: u32) -> ScanStats;
}

/// Извлекатель текста PDF. Адаптер — собственный парсер (0 зависимостей).
pub trait TextExtractor {
    fn extract(&self, path: &str, max_bytes: u64) -> Result<Extracted, ExtractError>;
}

#[derive(Default)]
pub struct Extracted {
    /// Страницы текста (по одной строке-блоку на страницу).
    pub pages: Vec<String>,
}

impl Extracted {
    pub fn full_text(&self) -> String {
        self.pages.join("\n")
    }
}

#[derive(Debug)]
pub enum ExtractError {
    NotFound,
    NotPdf,
    /// Повреждение в пределах подмножества парсера — документ пропускается.
    Unsupported(&'static str),
    TooLarge(u64),
}

/// Индекс полнотекстового поиска. Адаптер — BM25 поверх постингов.
pub trait Searcher {
    /// doc_id = индекс entry в каталоге.
    fn index(&mut self, doc_id: u32, text: &str);
    fn search(&self, query: &str, limit: usize) -> Vec<Hit>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub doc_id: u32,
    pub score: f64,
}

/// Сохранение/загрузка каталога и индекса (адаптер — собственный бинарный формат).
pub trait CatalogStore {
    fn save(&mut self, path: &str) -> std::io::Result<()>;
}

#[derive(Default, Debug, Clone, Copy)]
pub struct ScanStats {
    pub dirs: u32,
    pub files: u32,
    pub pdfs: u32,
    pub skipped_depth: u32,
    pub elapsed_ms: u64,
}
