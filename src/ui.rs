//! ui.rs — Bento Grids: состояние, сетка, отрисовка, ввод.
//! Сетка 8 px, палитра и типоразмеры плиток — по docs/pdfcat-solution.md §5.

use crate::arena::ROOT_SENTINEL;
use crate::bm25::Bm25;
use crate::domain::Catalog;
use crate::pdf::PdfExtractor;
use crate::ports::{Searcher, TextExtractor};
use crate::scan::FsScanner;
use crate::ports::Source;
use crate::win::*;
use std::path::PathBuf;

pub const HEADER_H: i32 = 56;
pub const STATUS_H: i32 = 28;
pub const LEFT_W: i32 = 472;
pub const BOTTOM_H: i32 = 360;
pub const STATS_W: i32 = 232;

pub const BG: u32 = 0x0F1115;
pub const TILE: u32 = 0x171A21;
pub const HOVER: u32 = 0x1E2530;
pub const BORDER: u32 = 0x2A3140;
pub const TEXT: u32 = 0xE6EAF2;
pub const MUTED: u32 = 0x9AA6B8;
pub const ACCENT: u32 = 0x6EE7F9;
pub const OK: u32 = 0x34D399;
pub const BAD: u32 = 0xFB7185;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Tree,
    Graph,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Focus {
    None,
    Search,
}

/// Имя листа пути («Z:\a\b» → «b»); для корня остаётся полный путь.
fn leaf(path: &str) -> &str {
    match path.rfind('\\') {
        Some(i) if i + 1 < path.len() => &path[i + 1..],
        _ => path,
    }
}

pub enum TreeItem {
    Dir { dir_id: u32, depth: u32, name: String },
    File { doc_id: u32, depth: u32, name: String },
}

pub struct Tile {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub title: &'static str,
}

pub struct UiState {
    pub mode: Mode,
    pub focus: Focus,
    pub query: String,
    pub caret: usize,
    pub results: Vec<(u32, f64)>,
    pub selected: Option<u32>,
    pub preview: Option<(u32, String)>,
    pub catalog: Catalog,
    pub index_loaded: bool,
    pub db: PathBuf,
    pub scroll_tree: i32,
    pub scroll_results: i32,
    pub scroll_preview: i32,
    pub status: String,
    pub stats: Stats,
    pub tree: Vec<TreeItem>,
    pub w: i32,
    pub h: i32,
    // Прямоугольники для hit-тестов (обновляются в draw_frame)
    pub r_search: (i32, i32, i32, i32),
    pub r_tree: (i32, i32, i32, i32),
    pub r_results: (i32, i32, i32, i32),
    pub r_stats: (i32, i32, i32, i32),
    pub r_preview: (i32, i32, i32, i32),
    pub btn_mode_tree: (i32, i32, i32, i32),
    pub btn_mode_graph: (i32, i32, i32, i32),
    pub btn_scan: (i32, i32, i32, i32),
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub dirs: u32,
    pub files: u32,
    pub errors: u32,
}

impl UiState {
    pub fn load_or_empty(db: &PathBuf) -> Self {
        let cat_path = db.join("catalog.pdfcat");
        let idx_path = db.join("index.pdfcat");
        let mut s = Self {
            mode: Mode::Tree,
            focus: Focus::None,
            query: String::new(),
            caret: 0,
            results: Vec::new(),
            selected: None,
            preview: None,
            catalog: Catalog::load(cat_path.to_str().unwrap_or("catalog.pdfcat")).unwrap_or_default(),
            index_loaded: idx_path.exists(),
            db: db.to_path_buf(),
            scroll_tree: 0,
            scroll_results: 0,
            scroll_preview: 0,
            status: String::new(),
            stats: Stats::default(),
            tree: Vec::new(),
            w: 1440,
            h: 900,
            r_search: (0, 0, 0, 0),
            r_tree: (0, 0, 0, 0),
            r_results: (0, 0, 0, 0),
            r_stats: (0, 0, 0, 0),
            r_preview: (0, 0, 0, 0),
            btn_mode_tree: (0, 0, 0, 0),
            btn_mode_graph: (0, 0, 0, 0),
            btn_scan: (0, 0, 0, 0),
        };
        s.rebuild_tree();
        s.set_status(&format!(
            "каталог: {} записей, {} каталогов · Ctrl+F поиск · кнопка «Сканировать…» — начать с диска",
            s.catalog.entries.len(),
            s.catalog.dirs.len()
        ));
        s
    }

    pub fn resize(&mut self, w: i32, h: i32) {
        self.w = w;
        self.h = h;
    }

    pub fn set_status(&mut self, s: &str) {
        self.status = s.to_string();
    }

    pub fn rebuild_tree(&mut self) {
        let mut items = Vec::new();
        let root = self.catalog.root;
        if root != ROOT_SENTINEL {
            let mut stack: Vec<(u32, u32)> = vec![(root, 0)]; // (dir, depth)
            // DFS в порядке имён
            let mut dirs_sorted: Vec<(u32, u32, String)> = Vec::new();
            let mut files_of: std::collections::HashMap<u32, Vec<(u32, String)>> = std::collections::HashMap::new();
            for d in 0..self.catalog.dirs.len() as u32 {
                let (o, l) = self.catalog.dirs.path[d as usize];
                let full = self.catalog.arena.get(o, l).to_string();
                let name = if d == self.catalog.root { full.clone() } else { leaf(&full).to_string() };
                dirs_sorted.push((d, self.catalog.dirs.parent[d as usize], name));
            }
            for e in 0..self.catalog.entries.len() as u32 {
                let (o, l) = self.catalog.entries.name[e as usize];
                let name = self.catalog.arena.get(o, l).to_string();
                files_of.entry(self.catalog.entries.parent[e as usize]).or_default().push((e, name));
            }
            for v in files_of.values_mut() {
                v.sort_by(|a, b| a.1.cmp(&b.1));
            }
            dirs_sorted.sort_by(|a, b| a.2.cmp(&b.2));
            // Обход: flatten в порядке имени, dir → его файлы → его дети
            struct Frame {
                dir: u32,
                depth: u32,
                name: String,
            }
            let mut next_stack: Vec<Frame> = Vec::new();
            if let Some((rid, _, rname)) = dirs_sorted.iter().find(|x| x.0 == root) {
                next_stack.push(Frame { dir: *rid, depth: 0, name: rname.clone() });
            }
            let mut push_order: Vec<Frame> = Vec::new();
            while let Some(f) = next_stack.pop() {
                push_order.push(Frame { dir: f.dir, depth: f.depth, name: f.name.clone() });
                let mut kids: Vec<&(u32, u32, String)> =
                    dirs_sorted.iter().filter(|x| x.1 == f.dir).collect();
                kids.sort_by(|a, b| a.2.cmp(&b.2));
                for k in kids.into_iter().rev() {
                    next_stack.push(Frame { dir: k.0, depth: f.depth + 1, name: k.2.clone() });
                }
            }
            for f in push_order {
                items.push(TreeItem::Dir { dir_id: f.dir, depth: f.depth, name: f.name.clone() });
                if let Some(files) = files_of.get(&f.dir) {
                    for (id, name) in files {
                        items.push(TreeItem::File { doc_id: *id, depth: f.depth + 1, name: name.clone() });
                    }
                }
            }
        }
        self.tree = items;
        self.stats.dirs = self.catalog.dirs.len() as u32;
        self.stats.files = self.catalog.entries.len() as u32;
    }
}

// ---------------------------------------------------------------- ввод

pub fn on_click(c: &mut Ctx, x: i32, y: i32) {
    let ui = &mut c.ui;
    let (bx, by, bw, bh) = ui.btn_mode_tree;
    if x >= bx && x < bx + bw && y >= by && y < by + bh {
        ui.mode = Mode::Tree;
        return;
    }
    let (bx, by, bw, bh) = ui.btn_mode_graph;
    if x >= bx && x < bx + bw && y >= by && y < by + bh {
        ui.mode = Mode::Graph;
        return;
    }
    let (bx, by, bw, bh) = ui.btn_scan;
    if x >= bx && x < bx + bw && y >= by && y < by + bh {
        do_scan(c.hwnd, ui);
        return;
    }
    let (sx, sy, sw, sh) = ui.r_search;
    if x >= sx && x < sx + sw && y >= sy && y < sy + sh {
        ui.focus = Focus::Search;
        ui.caret = ui.query.len();
        return;
    }
    let (tx, ty, tw, th) = ui.r_tree;
    if x >= tx && x < tx + tw && y >= ty && y < ty + th {
        ui.focus = Focus::None;
        let row_h = 22;
        let idx = ((y - ty - 34 + ui.scroll_tree) / row_h) as isize;
        if idx >= 0 {
            match ui.tree.get(idx as usize) {
                Some(TreeItem::File { doc_id, .. }) => {
                    ui.selected = Some(*doc_id);
                    load_preview(ui, *doc_id);
                }
                Some(TreeItem::Dir { dir_id, .. }) => show_dir_files(ui, *dir_id),
                None => {}
            }
        }
        return;
    }
    let (rx, ry, rw, rh) = ui.r_results;
    if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
        ui.focus = Focus::None;
        let row_h = 26;
        let idx = ((y - ry - 34 + ui.scroll_results) / row_h) as isize;
        if idx >= 0 && (idx as usize) < ui.results.len() {
            let doc = ui.results[idx as usize].0;
            ui.selected = Some(doc);
            load_preview(ui, doc);
        }
        return;
    }
    ui.focus = Focus::None;
}

pub fn on_dbl_click(c: &mut Ctx, x: i32, y: i32) {
    // Двойной клик по файлу в дереве или в результатах — открыть PDF (только чтение)
    let ui = &mut c.ui;
    let (tx, ty, tw, th) = ui.r_tree;
    let row_h = 22;
    if x >= tx && x < tx + tw && y >= ty && y < ty + th {
        let idx = ((y - ty - 34 + ui.scroll_tree) / row_h) as isize;
        if let Some(TreeItem::File { doc_id, .. }) = ui.tree.get(idx.max(0) as usize) {
            let path = ui.catalog.entry_path(*doc_id).to_string();
            open_pdf(&path);
            ui.set_status(&format!("открыт: {path}"));
            return;
        }
    }
    let (rx, ry, rw, rh) = ui.r_results;
    if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
        let idx = ((y - ry - 34 + ui.scroll_results) / row_h) as isize;
        if idx >= 0 && (idx as usize) < ui.results.len() {
            let doc = ui.results[idx as usize].0;
            let path = ui.catalog.entry_path(doc).to_string();
            open_pdf(&path);
            ui.set_status(&format!("открыт: {path}"));
        }
    }
}

pub fn on_wheel(c: &mut Ctx, x: i32, y: i32, delta: i32) {
    let ui = &mut c.ui;
    let (sx, sy, sw, sh) = ui.r_search;
    if x >= sx && x < sx + sw && y >= sy && y < sy + sh {
        return;
    }
    let (tx, ty, _tw, _th) = ui.r_tree;
    if x >= tx && y >= ty {
        ui.scroll_tree = (ui.scroll_tree - delta * 44).max(0);
        return;
    }
    let (rx, ry, _rw, _rh) = ui.r_results;
    if x >= rx && y >= ry && y < ui.r_preview.1 {
        ui.scroll_results = (ui.scroll_results - delta * 44).max(0);
    }
}

pub fn on_char(c: &mut Ctx, ch: u32) {
    if c.ui.focus != Focus::Search {
        return;
    }
    let ui = &mut c.ui;
    match ch {
        0x08 => {
            let mut b = ui.caret.saturating_sub(1);
            while b > 0 && !ui.query.is_char_boundary(b) {
                b -= 1;
            }
            ui.query.replace_range(b..ui.caret, "");
            ui.caret = b;
        }
        0x0D => run_search(ui),
        0x1B => {}
        ch if ch >= 0x20 => {
            if let Some(sch) = char::from_u32(ch) {
                let mut buf = [0u8; 4];
                let s = sch.encode_utf8(&mut buf);
                ui.query.insert_str(ui.caret, s);
                ui.caret += s.len();
            }
        }
        _ => {}
    }
}

pub fn on_key(c: &mut Ctx, vk: WPARAM) {
    let ui = &mut c.ui;
    match vk {
        VK_RETURN if ui.focus == Focus::Search => run_search(ui),
        VK_BACK | VK_DELETE if ui.focus != Focus::Search => {}
        VK_UP | VK_DOWN if ui.focus != Focus::Search => {
            let list_len = if ui.mode == Mode::Tree { ui.tree.len() } else { ui.results.len() };
            if list_len == 0 {
                return;
            }
            let cur = ui.selected.map(|s| {
                if ui.mode == Mode::Tree {
                    ui.tree.iter().position(|t| matches!(t, TreeItem::File { doc_id, .. } if *doc_id == s)).unwrap_or(0)
                } else {
                    ui.results.iter().position(|r| r.0 == s).unwrap_or(0)
                }
            }).unwrap_or(0);
            let next = if vk == VK_DOWN { (cur + 1).min(list_len - 1) } else { cur.saturating_sub(1) };
            if ui.mode == Mode::Tree {
                if let TreeItem::File { doc_id, .. } = &ui.tree[next] {
                    ui.selected = Some(*doc_id);
                    load_preview(ui, *doc_id);
                }
            } else {
                ui.selected = Some(ui.results[next].0);
                load_preview(ui, ui.results[next].0);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- действия

fn run_search(ui: &mut UiState) {
    let q = ui.query.clone();
    if q.trim().is_empty() {
        return;
    }
    let idx_path = ui.db.join("index.pdfcat");
    ui.scroll_results = 0;
    if let Ok(idx) = Bm25::load(idx_path.to_str().unwrap_or("index.pdfcat")) {
        ui.results = idx.search(&q, 100).into_iter().map(|h| (h.doc_id, h.score)).collect();
        ui.set_status(&format!("BM25 «{}»: {} результатов", q, ui.results.len()));
        if let Some(first) = ui.results.first() {
            ui.selected = Some(first.0);
            load_preview(ui, first.0);
        }
        return;
    }
    // Fallback: по имени
    let ql = q.to_lowercase();
    ui.results.clear();
    for id in 0..ui.catalog.entries.len() as u32 {
        let name = ui.catalog.entry_name(id).to_lowercase();
        if name.contains(&ql) {
            ui.results.push((id, 1.0));
        }
    }
    ui.set_status(&format!("по имени «{}»: {} результатов (текстовый — после index)", q, ui.results.len()));
}

fn show_dir_files(ui: &mut UiState, dir_id: u32) {
    ui.results.clear();
    for id in 0..ui.catalog.entries.len() as u32 {
        if ui.catalog.entries.parent[id as usize] == dir_id {
            ui.results.push((id, 0.0));
        }
    }
    let (o, l) = ui.catalog.dirs.path[dir_id as usize];
    let name = ui.catalog.arena.get(o, l).to_string();
    ui.set_status(&format!("каталог «{}»: {} PDF", name, ui.results.len()));
}

fn load_preview(ui: &mut UiState, doc_id: u32) {
    if ui.preview.as_ref().map(|(d, _)| *d) == Some(doc_id) {
        return;
    }
    let path = ui.catalog.entry_path(doc_id).to_string();
    let ex = PdfExtractor;
    ui.preview = match ex.extract(&path, 64 * 1024 * 1024) {
        Ok(x) => {
            let mut text = x.pages.join("\n");
            text.truncate(6000);
            Some((doc_id, text))
        }
        Err(_) => Some((doc_id, String::new())),
    };
    ui.scroll_preview = 0;
}

pub fn do_scan(hwnd: HWND, ui: &mut UiState) {
    let Some(root) = pick_folder(hwnd) else { return };
    unsafe {
        let wait = LoadCursorW(0, 32514 as *const u16);
        SetCursor(wait);
    }
    ui.set_status(&format!("сканирую {root} …"));
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 0);
        UpdateWindow(hwnd);
    }

    let mut cat = Catalog::new();
    let mut s = FsScanner;
    let st = s.scan(&root, &mut cat, 16);
    let cat_path = ui.db.join("catalog.pdfcat");
    let mut errors = 0u32;
    if cat.save(cat_path.to_str().unwrap_or("catalog.pdfcat")).is_ok() {
        let mut idx = Bm25::new();
        let ex = PdfExtractor;
        let mut done = 0u32;
        for id in 0..cat.entries.len() as u32 {
            let path = cat.entry_path(id).to_string();
            match ex.extract(&path, 64 * 1024 * 1024) {
                Ok(x) => {
                    let text = x.pages.join("\n");
                    idx.index(id, &text);
                    cat.entries.indexed[id as usize] = true;
                    done += 1;
                }
                Err(_) => errors += 1,
            }
        }
        let idx_path = ui.db.join("index.pdfcat");
        let saved = idx.save(idx_path.to_str().unwrap_or("index.pdfcat")).is_ok();
        ui.catalog = cat;
        ui.rebuild_tree();
        ui.index_loaded = saved;
        ui.results.clear();
        ui.preview = None;
        ui.selected = None;
        ui.stats.errors = errors;
        ui.set_status(&format!(
            "скан: {} каталогов, {} PDF, {} мс · индекс: {} документов, {} ошибок",
            st.dirs, st.pdfs, st.elapsed_ms, done, errors
        ));
    } else {
        ui.set_status("не удалось сохранить каталог рядом с exe");
    }
    unsafe {
        let arrow = LoadCursorW(0, 32512 as *const u16);
        SetCursor(arrow);
    }
}

extern "system" {
    fn SetCursor(cursor: isize) -> isize;
}

// ---------------------------------------------------------------- отрисовка

/// Прямая запись пикселей в DIB (BGRA, top-down). hex = 0xRRGGBB.
fn fill(ctx: &Ctx, x: i32, y: i32, w: i32, h: i32, hex: u32) {
    unsafe {
        let bits = ctx.bits;
        if bits.is_null() || w <= 0 || h <= 0 || x < 0 || y < 0 {
            return;
        }
        let x = x.min(ctx.w - 1);
        let y = y.min(ctx.h - 1);
        let w = w.min(ctx.w - x);
        let h = h.min(ctx.h - y);
        let pix = ((hex & 0xFF) << 16) | (hex & 0xFF00) | ((hex >> 16) & 0xFF);
        for row in 0..h {
            let off = ((y + row) as usize) * (ctx.w as usize) + x as usize;
            let line = std::slice::from_raw_parts_mut(bits.add(off), w as usize);
            line.fill(pix);
        }
    }
}

fn text(ctx: &Ctx, x: i32, y: i32, s: &str, size: u8, weight: i32, hex: u32) -> i32 {
    unsafe {
        let f = match size {
            13 => ctx.fonts.h13,
            15 => ctx.fonts.h15,
            18 => ctx.fonts.h18,
            _ => ctx.fonts.h24,
        };
        SelectObject(ctx.mem_dc, f);
        SetTextColor(ctx.mem_dc, colorref(hex));
        SetBkMode(ctx.mem_dc, TRANSPARENT_BK);
        let w = utf8_to_wide(s);
        TextOutW(ctx.mem_dc, x, y, w.as_ptr(), (w.len() - 1) as i32);
        let mut sz = Point { x: 0, y: 0 };
        GetTextExtentPoint32W(ctx.mem_dc, w.as_ptr(), (w.len() - 1) as i32, &mut sz);
        sz.x
    }
}

fn tile(ctx: &Ctx, t: &Tile) {
    fill(ctx, t.x, t.y, t.w, t.h, TILE);
    fill(ctx, t.x, t.y, t.w, 1, BORDER);
    fill(ctx, t.x, t.y + t.h - 1, t.w, 1, BORDER);
    fill(ctx, t.x, t.y, 1, t.h, BORDER);
    fill(ctx, t.x + t.w - 1, t.y, 1, t.h, BORDER);
    text(ctx, t.x + 14, t.y + 10, t.title, 13, FW_SEMIBOLD, MUTED);
}

fn button(ctx: &Ctx, r: (i32, i32, i32, i32), label: &str, active: bool, text_hex: u32) {
    let (x, y, w, h) = r;
    fill(ctx, x, y, w, h, if active { ACCENT } else { TILE });
    fill(ctx, x, y, 1, h, BORDER);
    text(ctx, x + 10, y + 7, label, 15, FW_SEMIBOLD, text_hex);
}

pub fn draw_frame(ctx: &mut Ctx) {
    let w = ctx.w;
    let h = ctx.h;
    fill(ctx, 0, 0, w, h, BG);
    fill(ctx, 0, 0, w, HEADER_H, BG);
    text(ctx, 16, 14, "pdfcat", 24, FW_BOLD, ACCENT);
    text(ctx, 128, 26, "только чтение диска", 13, FW_NORMAL, MUTED);

    let body_top = HEADER_H;
    let body_h = h - HEADER_H - STATUS_H;
    let right_x = 8 + LEFT_W + 8;
    let right_w = (w - right_x - 8).max(240);
    let results_h = (body_h - BOTTOM_H - 8).max(140);

    // ---- Кнопки заголовка (справа)
    let bh = 32;
    let by = (HEADER_H - bh) / 2;
    let mut bx = w - 8;
    let (sw, _) = measure_btn("Сканировать…", 15);
    bx -= sw + 24;
    ctx.ui.btn_scan = (bx, by, sw + 24, bh);
    fill(ctx, bx, by, sw + 24, bh, TILE);
    fill(ctx, bx, by, 1, bh, BORDER);
    text(ctx, bx + 12, by + 7, "Сканировать…", 15, FW_SEMIBOLD, ACCENT);

    let (tw1, _) = measure_btn("Дерево", 15);
    bx -= tw1 + 20 + 8;
    ctx.ui.btn_mode_graph = (bx, by, tw1 + 20, bh);
    button(ctx, ctx.ui.btn_mode_graph, "Граф", ctx.ui.mode == Mode::Graph, if ctx.ui.mode == Mode::Graph { 0x06202B } else { TEXT });
    let (tw2, _) = measure_btn("Дерево", 15);
    bx -= tw2 + 20 + 8;
    ctx.ui.btn_mode_tree = (bx, by, tw2 + 20, bh);
    button(ctx, ctx.ui.btn_mode_tree, "Дерево", ctx.ui.mode == Mode::Tree, if ctx.ui.mode == Mode::Tree { 0x06202B } else { TEXT });

    let r_tree = Tile { x: 8, y: body_top, w: LEFT_W, h: body_h, title: "Каталог" };
    let r_results = Tile { x: right_x, y: body_top, w: right_w, h: results_h, title: "Результаты поиска" };
    let r_stats = Tile { x: right_x, y: body_top + results_h + 8, w: STATS_W, h: BOTTOM_H, title: "Статистика" };
    let r_preview = Tile {
        x: right_x + STATS_W + 8,
        y: body_top + results_h + 8,
        w: (right_w - STATS_W - 8).max(200),
        h: BOTTOM_H,
        title: "Превью текста",
    };

    // ---- Дерево / Граф
    tile(ctx, &r_tree);
    ctx.ui.r_tree = r_tree.xywh();
    match ctx.ui.mode {
        Mode::Tree => draw_tree(ctx, &r_tree),
        Mode::Graph => draw_graph(ctx, &r_tree),
    }

    // ---- Результаты: поле поиска + список
    tile(ctx, &r_results);
    ctx.ui.r_results = r_results.xywh();
    {
        let (sx, sy, sw, _) = r_results.xywh();
        let sh = 40;
        fill(ctx, sx + 14, sy + 30, sw - 28, sh, HOVER);
        fill(ctx, sx + 14, sy + 30, 1, sh, BORDER);
        fill(ctx, sx + sw - 15, sy + 30, 1, sh, BORDER);
        ctx.ui.r_search = (sx + 14, sy + 30, sw - 28, sh);
        let q = ctx.ui.query.clone();
        let cx = text(ctx, sx + 24, sy + 36, &q, 15, FW_NORMAL, TEXT);
        if q.is_empty() {
            text(ctx, sx + 24, sy + 36, "поиск по имени и тексту — Enter…", 15, FW_NORMAL, 0x5B6878);
        }
        if ctx.ui.focus == Focus::Search && ctx.caret_visible {
            fill(ctx, sx + 24 + cx, sy + 36, 2, 20, ACCENT);
        }
        // список
        let (x, y, wid, hei) = r_results.xywh();
        let row_h = 26;
        let list_top = y + 84;
        let max_rows = ((hei - 94) / row_h + 1) as i32;
        let base = (ctx.ui.scroll_results / row_h) as usize;
        let count = ctx.ui.results.len();
        for (i, &(doc, score)) in ctx.ui.results.iter().enumerate().skip(base).take(max_rows as usize) {
            let cy = list_top + (i as i32) * row_h - ctx.ui.scroll_results;
            if cy < list_top - row_h || cy > y + hei - 8 {
                continue;
            }
            let selected = ctx.ui.selected == Some(doc);
            if selected {
                fill(ctx, x + 8, cy - 2, wid - 16, row_h, HOVER);
            }
            let name = ctx.ui.catalog.entry_name(doc).to_string();
            text(ctx, x + 20, cy, &name, 13, FW_SEMIBOLD, if selected { ACCENT } else { TEXT });
            if score > 0.0 {
                let info = format!("{score:.3}");
                text(ctx, x + 260, cy, &info, 13, FW_NORMAL, MUTED);
            }
            let path = ctx.ui.catalog.entry_path(doc).to_string();
            let cut: String = path.chars().rev().take(64).collect::<Vec<_>>().into_iter().rev().collect();
            text(ctx, x + 340, cy, &("…".to_string() + &cut), 13, FW_NORMAL, MUTED);
        }
        if count == 0 {
            text(ctx, x + 20, list_top + 8, "пусто — поиск по Enter или клик по каталогу в «Каталоге»", 13, FW_NORMAL, MUTED);
        }
    }

    // ---- Статистика
    tile(ctx, &r_stats);
    ctx.ui.r_stats = r_stats.xywh();
    {
        let (x, y, _, _) = r_stats.xywh();
        let lines = [
            format!("каталогов   {}", ctx.ui.stats.dirs),
            format!("записей PDF {}", ctx.ui.stats.files),
            format!("ошибок      {}", ctx.ui.stats.errors),
            format!("индекс      {}", if ctx.ui.index_loaded { "есть" } else { "нет" }),
        ];
        let mut cy = y + 42;
        for l in &lines {
            text(ctx, x + 14, cy, l, 13, FW_NORMAL, MUTED);
            cy += 24;
        }
        let mut cy2 = y + 42 + 24 * 4;
        let db_str: String = ctx.ui.db.display().to_string().chars().take(26).collect();
        text(ctx, x + 14, cy2, &db_str, 13, FW_NORMAL, 0x5B6878);
        let _ = cy2;
    }

    // ---- Превью
    tile(ctx, &r_preview);
    ctx.ui.r_preview = r_preview.xywh();
    {
        let (x, y, wid, hei) = r_preview.xywh();
        let mut cy = y + 34;
        match &ctx.ui.preview {
            Some((doc, txt)) => {
                let name = ctx.ui.catalog.entry_name(*doc).to_string();
                text(ctx, x + 14, cy, &name, 15, FW_SEMIBOLD, ACCENT);
                cy += 28;
                let max_w = wid - 28;
                for line in txt.split('\n') {
                    for chunk in wrap(line, max_w, 13) {
                        if cy > y + hei - 22 {
                            break;
                        }
                        text(ctx, x + 14, cy, &chunk, 13, FW_NORMAL, TEXT);
                        cy += 19;
                    }
                    cy += 4;
                    if cy > y + hei - 22 {
                        break;
                    }
                }
            }
            None => {
                text(ctx, x + 14, cy, "выберите PDF в «Каталоге» или «Результатах»", 13, FW_NORMAL, MUTED);
            }
        }
    }

    // ---- Статусная строка
    fill(ctx, 0, h - STATUS_H, w, STATUS_H, BG);
    fill(ctx, 0, h - STATUS_H, w, 1, BORDER);
    text(ctx, 16, h - STATUS_H + 6, &ctx.ui.status, 13, FW_NORMAL, MUTED);
}

fn measure_btn(s: &str, size: u8) -> (i32, i32) {
    let w = (s.chars().count() as i32) * ((size as i32) * 3 / 5) + 4;
    (w, size as i32)
}

trait Xywh {
    fn xywh(&self) -> (i32, i32, i32, i32);
}

impl Xywh for Tile {
    fn xywh(&self) -> (i32, i32, i32, i32) {
        (self.x, self.y, self.w, self.h)
    }
}

impl Xywh for (i32, i32, i32, i32) {
    fn xywh(&self) -> (i32, i32, i32, i32) {
        *self
    }
}

fn wrap(s: &str, max_w: i32, size: u8) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    let char_w = (size as i32) * 3 / 5;
    let per_line = (max_w / char_w).max(8) as usize;
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split(' ') {
        if cur.chars().count() + word.chars().count() + 1 > per_line {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if word.chars().count() > per_line {
                let mut k = word;
                while k.chars().count() > per_line {
                    let head: String = k.chars().take(per_line).collect();
                    out.push(head.clone());
                    k = &k[head.len()..];
                }
                cur = k.to_string();
            } else {
                cur = word.to_string();
            }
        } else {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
    }
    out.push(cur);
    out
}

fn draw_tree(ctx: &mut Ctx, t: &Tile) {
    let row_h = 22;
    let list_top = t.y + 34;
    let max_rows = ((t.h - 44) / row_h + 1) as i32;
    let base = (ctx.ui.scroll_tree / row_h) as usize;
    for (i, item) in ctx.ui.tree.iter().enumerate().skip(base).take(max_rows as usize) {
        let cy = list_top + (i as i32) * row_h - ctx.ui.scroll_tree;
        if cy < list_top - row_h || cy > t.y + t.h - 6 {
            continue;
        }
        match item {
            TreeItem::Dir { depth, name, .. } => {
                let x = t.x + 14 + (*depth as i32) * 16;
                let cut: String = name.chars().take(56).collect();
                text(ctx, x, cy, &cut, 13, FW_SEMIBOLD, MUTED);
            }
            TreeItem::File { doc_id, depth, name } => {
                let x = t.x + 20 + (*depth as i32) * 16;
                let selected = ctx.ui.selected == Some(*doc_id);
                if selected {
                    fill(ctx, t.x + 8, cy - 2, t.w - 16, row_h, HOVER);
                }
                let cut: String = name.chars().take(52).collect();
                text(ctx, x, cy, &cut, 13, FW_NORMAL, if selected { ACCENT } else { TEXT });
            }
        }
    }
}

fn draw_graph(ctx: &mut Ctx, t: &Tile) {
    // Слоистый граф: уровень = глубина каталога; PDF — точки под узлом каталога.
    let mut levels: std::collections::HashMap<u32, Vec<(u32, String)>> = std::collections::HashMap::new();
    for d in 0..ctx.ui.catalog.dirs.len() as u32 {
        let (o, l) = ctx.ui.catalog.dirs.path[d as usize];
        let name = ctx.ui.catalog.arena.get(o, l).to_string();
        levels.entry(ctx.ui.catalog.dirs.parent[d as usize]).or_default().push((d, name));
    }
    let root = ctx.ui.catalog.root;
    if root == ROOT_SENTINEL {
        text(ctx, t.x + 20, t.y + 60, "каталог пуст — нажмите «Сканировать…»", 13, FW_NORMAL, MUTED);
        return;
    }
    let mut cy = t.y + 50 - ctx.ui.scroll_tree;
    let mut frontier: Vec<(u32, i32)> = vec![(root, t.x + t.w / 2)];
    let mut depth = 0u32;
    while !frontier.is_empty() && cy < t.y + t.h {
        let next: Vec<(u32, i32)> = frontier
            .iter()
            .flat_map(|&(d, px)| {
                let mut kids: Vec<(u32, String)> = levels.get(&d).cloned().unwrap_or_default();
                kids.sort_by(|a, b| a.1.cmp(&b.1));
                let n = kids.len().max(1) as i32;
                let spread = (t.w - 80).min(140 * n);
                let ids: Vec<u32> = kids.into_iter().map(|(kd, _)| kd).collect();
                ids.into_iter().enumerate().map(move |(i, kd)| {
                    (kd, px - spread / 2 + (spread / n) * i as i32 + spread / (2 * n))
                })
            })
            .collect();
        // линии к детям (до узлов, чтобы узлы лежали сверху)
        for &(kd, kx) in &next {
            let parent = ctx.ui.catalog.dirs.parent[kd as usize];
            if let Some(&(_, ppx)) = frontier.iter().find(|&&(pd, _)| pd == parent) {
                let y0 = cy + 26;
                let y1 = cy + 62;
                let mid = y0 + (y1 - y0) / 2;
                let (minx, maxx) = (ppx.min(kx), ppx.max(kx));
                fill(ctx, ppx - 1, y0, 2, mid - y0, BORDER);
                fill(ctx, minx, mid, (maxx - minx).max(2), 2, BORDER);
                fill(ctx, kx - 1, mid, 2, y1 - mid, BORDER);
            }
        }
        for &(d, px) in &frontier {
            // PDF каталога — точки в ряду под узлом (до 8)
            let mut dots = 0;
            for id in 0..ctx.ui.catalog.entries.len() as u32 {
                if dots >= 8 {
                    break;
                }
                if ctx.ui.catalog.entries.parent[id as usize] == d {
                    let selected = ctx.ui.selected == Some(id);
                    let dot_x = px - 8 * 5 + dots * 12;
                    fill(ctx, dot_x, cy + 34, 8, 8, if selected { ACCENT } else { OK });
                    dots += 1;
                }
            }
            let (o, l) = ctx.ui.catalog.dirs.path[d as usize];
            let name = leaf(ctx.ui.catalog.arena.get(o, l));
            let label: String = name.chars().take(24).collect();
            let w_name = text_w(&label, 13) + 16;
            let is_root = d == root;
            fill(ctx, px - w_name / 2, cy, w_name, 26, if is_root { HOVER } else { TILE });
            fill(ctx, px - w_name / 2, cy, w_name, 1, BORDER);
            fill(ctx, px - w_name / 2, cy + 25, w_name, 1, BORDER);
            text(ctx, px - w_name / 2 + 8, cy + 5, &label, 13, FW_SEMIBOLD, if is_root { ACCENT } else { TEXT });
        }
        frontier = next;
        cy += 62;
        depth += 1;
        if depth > 24 {
            break;
        }
    }
}

fn text_w(s: &str, size: u8) -> i32 {
    (s.chars().count() as i32) * ((size as i32) * 3 / 5)
}
