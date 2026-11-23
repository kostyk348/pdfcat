//! CLI-скелет: scan · index · search · stats.
//! Hexagonal: сборка адаптеров происходит только здесь.

use crate::bm25::Bm25;
use crate::domain::Catalog;
use crate::pdf::{debug_contents, PdfExtractor};
use crate::ports::{Searcher, TextExtractor};
use crate::scan::FsScanner;
use crate::ports::Source;
use std::time::Instant;

pub fn default_db() -> String {
    crate::win::db_dir().join("catalog.pdfcat").to_string_lossy().into_owned()
}
pub fn default_index() -> String {
    crate::win::db_dir().join("index.pdfcat").to_string_lossy().into_owned()
}
const DEFAULT_DB: &str = "catalog.pdfcat";
const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;

pub fn cmd_scan(root: &str, max_depth: u32) -> i32 {
    let mut cat = Catalog::new();
    let mut s = FsScanner;
    let st = s.scan(root, &mut cat, max_depth);
    if cat.verify().is_err() {
        eprintln!("инвариант каталога нарушен после скана");
        return 2;
    }
    if let Err(e) = cat.save(&default_db()) {
        eprintln!("сохранение: {e}");
        return 1;
    }
    println!(
        "каталогов {} · файлов {} · pdf {} · пропущено по глубине {} · {} мс → {}",
        st.dirs, st.files, st.pdfs, st.skipped_depth, st.elapsed_ms, DEFAULT_DB
    );
    0
}

pub fn cmd_index(limit: Option<u32>) -> i32 {
    let mut cat = match Catalog::load(&default_db()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("нет каталога ({e}); сначала scan");
            return 1;
        }
    };
    let mut idx = Bm25::new();
    let ex = PdfExtractor;
    let t0 = Instant::now();
    let mut done = 0u32;
    let mut failed = 0u32;
    for id in 0..cat.entries.len() as u32 {
        if limit.is_some_and(|l| done >= l) {
            break;
        }
        let path = cat.entry_path(id).to_string();
        match ex.extract(&path, MAX_PDF_BYTES) {
            Ok(extracted) => {
                let text = extracted.full_text();
                idx.index(id, &text);
                cat.entries.indexed[id as usize] = true;
                done += 1;
            }
            Err(e) => {
                failed += 1;
                if failed <= 10 {
                    eprintln!("  пропуск {}: {e:?}", cat.entry_name(id));
                }
            }
        }
    }
    let (terms, tokens) = idx.stats();
    println!(
        "проиндексировано {done} (ошибок {failed}) · терминов {terms} · токенов {tokens} · {} мс",
        t0.elapsed().as_millis()
    );
    if let Err(e) = idx.save(&default_index()) {
        eprintln!("сохранение индекса: {e}");
        return 1;
    }
    println!("индекс → {}", default_index());
    0
}

pub fn cmd_search(query: &str, limit: usize) -> i32 {
    let cat = match Catalog::load(&default_db()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("нет каталога ({e}); сначала scan");
            return 1;
        }
    };
    // Текстовый BM25, если индекс построен
    if let Ok(idx) = Bm25::load(&default_index()) {
        let t0 = std::time::Instant::now();
        let hits = idx.search(query, limit);
        let ms = t0.elapsed().as_micros();
        println!("BM25: {} результатов за {} мкс", hits.len(), ms);
        for h in &hits {
            println!("{:8.3}  {}", h.score, cat.entry_path(h.doc_id));
        }
        return 0;
    }
    // Fallback: поиск по имени (подстрока)
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

pub fn cmd_peek(n: u32) -> i32 {
    let cat = match Catalog::load(&default_db()) {
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
    if let Ok(raws) = debug_contents(&path) {
        for (i, r) in raws.iter().enumerate().take(2) {
            let cut: String = r.chars().take(200).collect();
            println!("RAW стр {} ({} байт): {}", i + 1, r.len(), cut);
        }
    }
    let ex = PdfExtractor;
    match ex.extract(&path, MAX_PDF_BYTES) {
        Ok(x) => {
            println!("путь: {path}");
            println!("страниц: {}", x.pages.len());
            for (i, pg) in x.pages.iter().enumerate().take(3) {
                println!("--- стр {}: {} символов", i + 1, pg.chars().count());
                let cut: String = pg.chars().take(300).collect();
                println!("{}", cut);
            }
            0
        }
        Err(e) => {
            eprintln!("ошибка: {e:?}");
            1
        }
    }
}

pub fn cmd_stats() -> i32 {
    match Catalog::load(&default_db()) {
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

pub fn help() {
    println!("pdfcat 0.1.0 — каталогизатор PDF (Win7, ноль зависимостей)");
    println!("  pdfcat scan  <путь> [глубина]   собрать каталог (по умолчанию глубина 16)");
    println!("  pdfcat index  [лимит]           извлечь текст и построить BM25-индекс");
    println!("  pdfcat search <запрос> [лимит]  поиск по имени (текст — после index)");
    println!("  pdfcat stats                    статистика каталога");
}

