//! Конвейер скан+индекс (общий для CLI и GUI). Параллельная индексация по чанкам.

use crate::bm25::Bm25;
use crate::domain::Catalog;
use crate::pdf::PdfExtractor;
use crate::ports::{Searcher, TextExtractor};
use crate::scan::FsScanner;
use crate::ports::Source;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Default, Clone, Copy)]
pub struct PipelineStats {
    pub dirs: u32,
    pub files: u32,
    pub pdfs: u32,
    pub indexed: u32,
    pub errors: u32,
    pub scan_ms: u64,
    pub index_ms: u64,
    pub tokens: u64,
    pub terms: usize,
}

/// Скан + сохранение каталога + параллельная индексация + сохранение индекса.
/// `progress` — колбэк статуса (для GUI-статусной строки / CLI-вывода).
pub fn scan_and_index(
    root: &str,
    db: &Path,
    max_depth: u32,
    threads: usize,
    progress: &std::sync::Mutex<String>,
) -> Result<PipelineStats, String> {
    let set_progress = |msg: &str| {
        if let Ok(mut g) = progress.lock() {
            *g = msg.to_string();
        }
    };
    let t0 = std::time::Instant::now();
    let mut cat = Catalog::new();
    let mut s = FsScanner;
    let st = s.scan(root, &mut cat, max_depth);
    let scan_ms = t0.elapsed().as_millis() as u64;
    if cat.verify().is_err() {
        return Err("инвариант каталога нарушен".into());
    }
    let cat_path = db.join("catalog.pdfcat");
    cat.save(cat_path.to_str().unwrap_or("catalog.pdfcat"))
        .map_err(|e| format!("сохранение каталога: {e}"))?;
    set_progress(&format!("скан: {} PDF, {} мс — индексирую…", st.pdfs, scan_ms));

    // Параллельная индексация: чанки записей по потокам, слияние с offset
    let t1 = std::time::Instant::now();
    let n = cat.entries.len();
    let nthreads = threads.clamp(1, 16);
    let chunk = (n + nthreads - 1) / nthreads.max(1);
    let mut partials: Vec<Bm25> = Vec::new();
    let mut err_counts: Vec<u32> = Vec::new();
    let done_counter = std::sync::Arc::new(AtomicUsize::new(0));
    let cat_ref = &cat;
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..nthreads)
            .map(|t| {
                let done = std::sync::Arc::clone(&done_counter);
                scope.spawn(move || {
                    let mut idx = Bm25::new();
                    let mut errs = 0u32;
                    let ex = PdfExtractor;
                    let start = (t * chunk) as usize;
                    let end = ((t + 1) * chunk).min(n);
                    let mut first_dbg = true;
                    for id in start..end {
                        let path = cat_ref.entry_path(id as u32).to_string();
                        match ex.extract(&path, MAX_PDF_BYTES) {
                            Ok(x) => {
                                let text = x.pages.join("\n");
                                if first_dbg {
                                    if std::env::var("PDFCAT_DEBUG").as_deref() == Ok("1") {
                                        eprintln!("[pipeline] t={t} id={id} chars={}", text.chars().count());
                                    }
                                    first_dbg = false;
                                }
                                idx.index((id - start) as u32, &text);
                            }
                            Err(e) => {
                                errs += 1;
                                if std::env::var("PDFCAT_DEBUG").as_deref() == Ok("1") {
                                    eprintln!("[pipeline] t={t} id={id} err={e:?}");
                                }
                            }
                        }
                        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                        if d % 25 == 0 {
                            if let Ok(mut g) = progress.lock() {
                                *g = format!("индексирую {d}/{n}…");
                            }
                        }
                    }
                    (idx, errs)
                })
            })
            .collect();
        for h in handles {
            match h.join() {
                Ok(pair) => {
                    err_counts.push(pair.1);
                    partials.push(pair.0);
                }
                Err(_) => {}
            }
        }
    });
    // Слить чанки с offset: postings doc_id + offset
    let n_partials = partials.len();
    let mut full = Bm25::new();
    for (t, part) in partials.into_iter().enumerate() {
        let offset = (t * chunk) as u32;
        full.merge_chunk(part, offset);
        let _ = t;
    }
    let err_total: u32 = err_counts.iter().sum();
    let (terms, tokens) = full.stats();
    if std::env::var("PDFCAT_DEBUG").as_deref() == Ok("1") {
        eprintln!("[pipeline] partials={n_partials} terms={terms} tokens={tokens}");
    }
    let index_ms = t1.elapsed().as_millis() as u64;
    let idx_path = db.join("index.pdfcat");
    full.save(idx_path.to_str().unwrap_or("index.pdfcat"))
        .map_err(|e| format!("сохранение индекса: {e}"))?;

    Ok(PipelineStats {
        dirs: st.dirs,
        files: st.files,
        pdfs: st.pdfs,
        indexed: n as u32,
        errors: err_total,
        scan_ms,
        index_ms,
        tokens,
        terms,
    })
}
