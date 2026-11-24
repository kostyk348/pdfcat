//! CLI: scan (скан+индекс одним вызовом) · search · stats · peek · snapshot.
//! Hexagonal: сборка адаптеров происходит только здесь.

use crate::bm25::Bm25;
use crate::domain::Catalog;
use crate::pdf::{debug_contents, PdfExtractor};
use crate::ports::Searcher;
use crate::ports::TextExtractor;

pub fn default_db() -> String {
    crate::win::db_dir().join("catalog.pdfcat").to_string_lossy().into_owned()
}
pub fn default_index() -> String {
    crate::win::db_dir().join("index.pdfcat").to_string_lossy().into_owned()
}
const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;

pub fn cmd_scan_index(root: &str, max_depth: u32) -> i32 {
    let db = crate::win::db_dir();
    let progress = std::sync::Arc::new(std::sync::Mutex::new(String::from("сканирую…")));
    match crate::pipeline::scan_and_index(root, &db, max_depth, 8, &progress) {
        Ok(st) => {
            println!(
                "каталогов {} · файлов {} · PDF {} · проиндексировано {} (ошибок {}) · скан {} мс · индекс {} мс · токенов {}",
                st.dirs, st.files, st.pdfs, st.indexed, st.errors, st.scan_ms, st.index_ms, st.tokens
            );
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

pub fn cmd_stats() -> i32 {
    let db = crate::win::db_dir();
    match Catalog::load(db.join("catalog.pdfcat").to_str().unwrap_or("catalog.pdfcat")) {
        Ok(c) => {
            println!(
                "каталогов {} · записей {} · корень: {}",
                c.dirs.len(),
                c.entries.len(),
                c.dir_path(c.root)
            );
            let mut total = 0u64;
            for i in 0..c.entries.len() {
                total += c.entries.size[i];
            }
            println!("суммарный объём PDF: {} МБ", total / (1024 * 1024));
            0
        }
        Err(e) => {
            eprintln!("нет каталога ({e})");
            1
        }
    }
}

pub fn cmd_search(query: &str, limit: usize) -> i32 {
    let db = crate::win::db_dir();
    let cat = match Catalog::load(db.join("catalog.pdfcat").to_str().unwrap_or("catalog.pdfcat")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("нет каталога ({e}); сначала scan");
            return 1;
        }
    };
    // Текстовый BM25, если индекс построен
    if let Ok(idx) = Bm25::load(db.join("index.pdfcat").to_str().unwrap_or("index.pdfcat")) {
        let t0 = std::time::Instant::now();
        let hits = idx.search(query, limit);
        let ms = t0.elapsed().as_micros();
        println!("BM25: {} результатов за {} мкс", hits.len(), ms);
        for h in &hits {
            println!("{:8.3}  {}", h.score, cat.entry_path(h.doc_id));
        }
        return 0;
    }
    // Fallback: по имени
    let ql = query.to_lowercase();
    let mut hits: Vec<(u32, f64)> = Vec::new();
    for id in 0..cat.entries.len() as u32 {
        let name = cat.entry_name(id).to_lowercase();
        if name.contains(&ql) {
            hits.push((id, 1.0));
        }
    }
    if hits.is_empty() {
        println!("не найдено (текстовый поиск — после index)");
        return 0;
    }
    for (id, score) in hits.iter().take(limit) {
        println!("{score:.2}  {}", cat.entry_path(*id));
    }
    0
}

/// Диагностика: показать извлечённый текст записи N.
pub fn cmd_peek(n: u32) -> i32 {
    let db = crate::win::db_dir();
    let cat = match Catalog::load(db.join("catalog.pdfcat").to_str().unwrap_or("catalog.pdfcat")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("нет каталога ({e})");
            return 1;
        }
    };
    if n as usize >= cat.entries.len() {
        eprintln!("нет записи {n}");
        return 1;
    }
    let path = cat.entry_path(n).to_string();
    let ex = PdfExtractor;
    match ex.extract(&path, MAX_PDF_BYTES) {
        Ok(x) => {
            println!("путь: {path}");
            println!("страниц: {}", x.pages.len());
            for (i, pg) in x.pages.iter().enumerate().take(3) {
                println!("--- стр {}: {} символов", i + 1, pg.chars().count());
                println!("{}", &pg[..pg.len().min(300)]);
            }
            0
        }
        Err(e) => {
            eprintln!("ошибка: {e:?}");
            1
        }
    }
}

pub fn help() {
    println!("pdfcat 0.3.0 — каталогизатор PDF (Win7, ноль зависимостей)");
    println!("  pdfcat                окно Bento (GUI)");
    println!("  pdfcat scan  <путь> [глубина]   скан + параллельная индексация");
    println!("  pdfcat search <запрос> [лимит]  BM25 по тексту / по имени");
    println!("  pdfcat stats                    статистика каталога");
    println!("  pdfcat peek  <N>                извлечённый текст записи N");
}
