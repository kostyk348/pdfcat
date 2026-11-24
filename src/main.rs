//! pdfcat — организатор каталогов PDF на сетевом диске.
//! Скелет (MVP): scan/index/search/stats. UI Bento — этап E5 (см. pdfcat-solution.md).

mod arena;
mod bm25;
mod cli;
mod domain;
mod index_store;
mod inflate;
mod jpeg;
mod pdf;
mod pipeline;
mod ports;
mod render_pdf;
mod scan;
mod store;
mod ui;
mod win;

fn main() {
    std::panic::set_hook(Box::new(|info| {
        win::startup_log(&format!("PANIC: {info}"));
    }));
    win::startup_log("pdfcat запущен");
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(|s| s.as_str()) {
        None => win::run_gui(),
        Some("snapshot") => {
            let out = args.get(2).cloned().unwrap_or_else(|| "snapshot.bmp".into());
            let w = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1440);
            let h = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(900);
            let db = win::db_dir();
            let mut ui = ui::UiState::load_or_empty(&db);
            if std::env::var("PDFCAT_MODE").as_deref() == Ok("graph") {
                ui.mode = ui::Mode::Graph;
            }
            if let Ok(n) = std::env::var("PDFCAT_SELECT") {
                if let Ok(id) = n.parse::<u32>() {
                    ui::load_preview(&mut ui, id);
                }
            }
            let ok = win::render_to_bmp(w, h, ui, &out);
            println!("{}", if ok { format!("снимок → {out}") } else { "ошибка записи bmp".into() });
            if ok { 0 } else { 1 }
        }
        Some("scan") if args.len() >= 3 => {
            let depth = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(16);
            cli::cmd_scan_index(&args[2], depth)
        }
        Some("search") if args.len() >= 3 => {
            let limit = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(20);
            cli::cmd_search(&args[2], limit)
        }
        Some("stats") => cli::cmd_stats(),
        Some("peek") if args.len() >= 3 => cli::cmd_peek(args[2].parse().unwrap_or(0)),
        _ => {
            cli::help();
            0
        }
    };
    std::process::exit(code);
}
