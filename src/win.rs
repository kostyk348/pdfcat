//! win.rs — окно Bento на GDI (Win7-compatible), ноль зависимостей.
//! Паттерны перенесены из dxfsign: FreeConsole, размер по рабочей области,
//! DIB-двойная буферизация, перерисовка по событиям (не по таймеру FPS).

#![allow(non_snake_case, dead_code)]

use crate::ui::{draw_frame, on_char, on_click, on_dbl_click, on_key, on_wheel, UiState, Focus};
use std::ffi::c_void;

pub type HWND = isize;
pub type HDC = isize;
pub type HBITMAP = isize;
pub type HGDIOBJ = isize;
pub type HFONT = isize;
pub type HBRUSH = isize;
pub type HINSTANCE = isize;
pub type WPARAM = usize;
pub type LPARAM = isize;
pub type LRESULT = isize;

pub const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
pub const WS_VISIBLE: u32 = 0x1000_0000;
pub const CS_HREDRAW: u32 = 0x0002;
pub const CS_VREDRAW: u32 = 0x0001;
pub const IDC_ARROW: *const u16 = 32512 as *const u16;
pub const CW_USEDEFAULT: i32 = 0x8000_0000u32 as i32;

pub const WM_DESTROY: u32 = 0x0002;
pub const WM_PAINT: u32 = 0x000F;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_CHAR: u32 = 0x0102;
pub const WM_TIMER: u32 = 0x0113;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_MOUSEWHEEL: u32 = 0x020A;
pub const WM_SIZE: u32 = 0x0005;
pub const WM_SETCURSOR: u32 = 0x0020;

pub const VK_BACK: usize = 0x08;
pub const VK_RETURN: usize = 0x0D;
pub const VK_SHIFT: usize = 0x10;
pub const VK_ESCAPE: usize = 0x1B;
pub const VK_HOME: usize = 0x24;
pub const VK_END: usize = 0x23;
pub const VK_LEFT: usize = 0x25;
pub const VK_UP: usize = 0x26;
pub const VK_RIGHT: usize = 0x27;
pub const VK_DOWN: usize = 0x28;
pub const VK_DELETE: usize = 0x2E;
pub const VK_PRIOR: usize = 0x21;
pub const VK_NEXT: usize = 0x22;

pub const SW_HIDE: i32 = 0;
pub const SW_SHOW: i32 = 5;
pub const SW_SHOWNORMAL: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
pub struct Msg {
    pub hwnd: HWND,
    pub message: u32,
    pub wParam: WPARAM,
    pub lParam: LPARAM,
    pub time: u32,
    pub pt: Point,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
pub struct WndClassW {
    pub style: u32,
    pub lpfn_wnd_proc: Option<extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
    pub cb_cls_extra: i32,
    pub cb_wnd_extra: i32,
    pub h_instance: HINSTANCE,
    pub h_icon: isize,
    pub h_cursor: isize,
    pub h_br_background: HBRUSH,
    pub lpsz_menu_name: *const u16,
    pub lpsz_class_name: *const u16,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct BitmapInfoHeader {
    pub bi_size: u32,
    pub bi_width: i32,
    pub bi_height: i32,
    pub bi_planes: u16,
    pub bi_bit_count: u16,
    pub bi_compression: u32,
    pub bi_size_image: u32,
    pub bi_x_pels: i32,
    pub bi_y_pels: i32,
    pub bi_clr_used: u32,
    pub bi_clr_important: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BrowseInfoW {
    pub hwnd_owner: HWND,
    pub pidl_root: isize,
    pub psz_display_name: *mut u16,
    pub lpsz_title: *const u16,
    pub ul_flags: u32,
    pub lpfn: isize,
    pub lParam: LPARAM,
    pub i_image: i32,
}

#[link(name = "user32")]
extern "system" {
    pub fn RegisterClassW(pc: *const WndClassW) -> u16;
    pub fn CreateWindowExW(
        ex: u32, class: *const u16, title: *const u16, style: u32,
        x: i32, y: i32, w: i32, h: i32, parent: HWND, menu: isize, inst: HINSTANCE, param: *mut c_void,
    ) -> HWND;
    pub fn DefWindowProcW(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT;
    pub fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    pub fn UpdateWindow(hwnd: HWND) -> i32;
    pub fn GetMessageW(m: *mut Msg, hwnd: HWND, min: u32, max: u32) -> i32;
    pub fn TranslateMessage(m: *const Msg) -> i32;
    pub fn DispatchMessageW(m: *const Msg) -> LRESULT;
    pub fn PostQuitMessage(code: i32);
    pub fn DestroyWindow(hwnd: HWND) -> i32;
    pub fn SetTimer(hwnd: HWND, id: usize, ms: u32) -> usize;
    pub fn KillTimer(hwnd: HWND, id: usize) -> i32;
    pub fn InvalidateRect(hwnd: HWND, r: *const Rect, erase: i32) -> i32;
    pub fn GetClientRect(hwnd: HWND, r: *mut Rect) -> i32;
    pub fn LoadCursorW(inst: HINSTANCE, name: *const u16) -> isize;
    pub fn SetFocus(hwnd: HWND) -> HWND;
    pub fn BeginPaint(hwnd: HWND, ps: *mut PaintStruct) -> HDC;
    pub fn EndPaint(hwnd: HWND, ps: *const PaintStruct) -> i32;
    pub fn FillRect(hdc: HDC, r: *const Rect, brush: HBRUSH) -> i32;
    pub fn SetWindowTextW(hwnd: HWND, s: *const u16) -> i32;
    pub fn LoadStringW(inst: HINSTANCE, id: u32, buf: *mut u16, max: i32) -> i32;
}

#[repr(C)]
pub struct PaintStruct {
    pub hdc: HDC,
    pub f_erase: i32,
    pub rc_paint: Rect,
    pub f_restore: i32,
    pub f_inc_update: i32,
    pub reserved: [u8; 32],
}

#[link(name = "gdi32")]
extern "system" {
    pub fn CreateCompatibleDC(hdc: HDC) -> HDC;
    pub fn DeleteDC(hdc: HDC) -> i32;
    pub fn CreateDIBSection(
        hdc: HDC, bmi: *const BitmapInfoHeader, usage: u32,
        bits: *mut *mut c_void, section: isize, offset: u32,
    ) -> HBITMAP;
    pub fn SelectObject(hdc: HDC, obj: HGDIOBJ) -> HGDIOBJ;
    pub fn DeleteObject(obj: HGDIOBJ) -> i32;
    pub fn CreateFontW(
        height: i32, width: i32, esc: i32, orient: i32, weight: i32,
        italic: u32, underline: u32, strike: u32, charset: u32,
        out_prec: u32, clip_prec: u32, quality: u32, pitch: u32, face: *const u16,
    ) -> HFONT;
    pub fn SetTextColor(hdc: HDC, color: u32) -> u32;
    pub fn SetBkColor(hdc: HDC, color: u32) -> i32;
    pub fn SetBkMode(hdc: HDC, mode: i32) -> i32;
    pub fn TextOutW(hdc: HDC, x: i32, y: i32, s: *const u16, c: i32) -> i32;
    pub fn GetTextExtentPoint32W(hdc: HDC, s: *const u16, c: i32, size: *mut Point) -> i32;
    pub fn BitBlt(dst: HDC, dx: i32, dy: i32, w: i32, h: i32, src: HDC, sx: i32, sy: i32, rop: u32) -> i32;
    pub fn CreateSolidBrush(color: u32) -> HBRUSH;
    pub fn CreatePen(style: i32, width: i32, color: u32) -> HGDIOBJ;
    pub fn MoveToEx(hdc: HDC, x: i32, y: i32, old: *mut Point) -> i32;
    pub fn LineTo(hdc: HDC, x: i32, y: i32) -> i32;
    pub fn Polygon(hdc: HDC, pts: *const Point, n: i32) -> i32;
    pub fn Rectangle(hdc: HDC, l: i32, t: i32, r: i32, b: i32) -> i32;
    pub fn SetTextAlign(hdc: HDC, mode: u32) -> u32;
    pub fn GetStockObject(obj: i32) -> HGDIOBJ;
    pub fn CreateRectRgn(l: i32, t: i32, r: i32, b: i32) -> HGDIOBJ;
    pub fn SelectClipRgn(hdc: HDC, rgn: HGDIOBJ) -> i32;
    pub fn SaveDC(hdc: HDC) -> i32;
    pub fn RestoreDC(hdc: HDC, saved: i32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
    pub fn GetModuleFileNameW(inst: HINSTANCE, buf: *mut u16, max: u32) -> u32;
}

#[link(name = "shell32")]
extern "system" {
    pub fn SHBrowseForFolderW(bi: *mut BrowseInfoW) -> isize;
    pub fn SHGetPathFromIDListW(pidl: isize, buf: *mut u16) -> i32;
    pub fn ShellExecuteW(
        hwnd: HWND, op: *const u16, file: *const u16, params: *const u16,
        dir: *const u16, show: i32,
    ) -> isize;
}

pub const SRCCOPY: u32 = 0x00CC_0020;
pub const PS_SOLID: i32 = 0;
pub const PS_NULL: i32 = 5;
pub const NULL_BRUSH: i32 = 5;
pub const NULL_PEN: i32 = 8;
pub const TA_BASELINE: u32 = 24;
pub const TA_LEFT: u32 = 0;
pub const TRANSPARENT_BK: i32 = 1;
pub const DEFAULT_CHARSET: u32 = 1;
pub const CLEARTYPE_QUALITY: u32 = 5;
pub const FW_NORMAL: i32 = 400;
pub const FW_SEMIBOLD: i32 = 600;
pub const FW_BOLD: i32 = 700;
pub const BIF_RETURNONLYFSDIRS: u32 = 0x0001;

pub fn utf8_to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn wide_to_string(w: &[u16]) -> String {
    let len = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..len])
}

/// Пиксель -> COLORREF (0x00BBGGRR) из #RRGGBB-строки.
pub fn colorref(hex: u32) -> u32 {
    // hex = 0xRRGGBB; COLORREF = B<<16 | G<<8 | R
    let r = (hex >> 16) & 0xFF;
    let g = (hex >> 8) & 0xFF;
    let b = hex & 0xFF;
    (b << 16) | (g << 8) | r
}

// ------------------------------------------------------------- Глобальное состояние

pub struct Ctx {
    pub ui: UiState,
    pub hwnd: HWND,
    pub mem_dc: HDC,
    pub bmp: HBITMAP,
    pub old_bmp: HGDIOBJ,
    pub bits: *mut u32,
    pub w: i32,
    pub h: i32,
    pub fonts: Fonts,
    pub caret_visible: bool,
}

pub struct Fonts {
    pub h13: HFONT,
    pub h15: HFONT,
    pub h18: HFONT,
    pub h24: HFONT,
}

fn make_font(size_px: i32, weight: i32) -> HFONT {
    unsafe {
        let face = utf8_to_wide("Segoe UI");
        CreateFontW(-size_px, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, 0, 0, CLEARTYPE_QUALITY, 0, face.as_ptr())
    }
}

impl Fonts {
    pub fn new() -> Self {
        Self {
            h13: make_font(13, FW_NORMAL),
            h15: make_font(15, FW_NORMAL),
            h18: make_font(18, FW_SEMIBOLD),
            h24: make_font(24, FW_BOLD),
        }
    }
}

pub static mut CTX: *mut Ctx = std::ptr::null_mut();

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let ctx = CTX;
        if ctx.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let c = &mut *ctx;
        match msg {
            WM_PAINT => {
                let mut ps: PaintStruct = std::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                draw_frame(c);
                BitBlt(hdc, 0, 0, c.w, c.h, c.mem_dc, 0, 0, SRCCOPY);
                EndPaint(hwnd, &ps);
                0
            }
            WM_SIZE => {
                let w = (lp & 0xFFFF) as i32;
                let h = ((lp >> 16) & 0xFFFF) as i32;
                if w > 0 && h > 0 {
                    resize_dib(c, w, h);
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                0
            }
            WM_LBUTTONDOWN => {
                let x = (lp & 0xFFFF) as i32;
                let y = ((lp >> 16) & 0xFFFF) as i32;
                on_click(c, x, y);
                InvalidateRect(hwnd, std::ptr::null(), 0);
                0
            }
            WM_LBUTTONDBLCLK => {
                let x = (lp & 0xFFFF) as i32;
                let y = ((lp >> 16) & 0xFFFF) as i32;
                on_dbl_click(c, x, y);
                InvalidateRect(hwnd, std::ptr::null(), 0);
                0
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp >> 16) & 0xFFFF) as i16 as i32 / 120;
                let y = ((lp >> 16) & 0xFFFF) as i16 as i32;
                let x = (lp & 0xFFFF) as i16 as i32;
                on_wheel(c, x, y, delta);
                InvalidateRect(hwnd, std::ptr::null(), 0);
                0
            }
            WM_CHAR => {
                on_char(c, wp as u32);
                InvalidateRect(hwnd, std::ptr::null(), 0);
                0
            }
            WM_KEYDOWN => {
                on_key(c, wp);
                InvalidateRect(hwnd, std::ptr::null(), 0);
                if wp == VK_ESCAPE {
                    PostQuitMessage(0);
                }
                0
            }
            WM_TIMER => {
                c.caret_visible = !c.caret_visible;
                if c.ui.focus == Focus::Search {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                0
            }
            WM_SETCURSOR => 1, // стрелка всегда; курсор занятости ставим вручную
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn resize_dib(c: &mut Ctx, w: i32, h: i32) {
    unsafe {
        if c.bmp != 0 {
            SelectObject(c.mem_dc, c.old_bmp);
            DeleteObject(c.bmp);
        }
        let mut bmi = BitmapInfoHeader::default();
        bmi.bi_size = 40;
        bmi.bi_width = w;
        bmi.bi_height = -h; // top-down
        bmi.bi_planes = 1;
        bmi.bi_bit_count = 32;
        bmi.bi_compression = 0; // BI_RGB
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(c.mem_dc, &bmi, 0, &mut bits, 0, 0);
        let old = SelectObject(c.mem_dc, bmp);
        c.bmp = bmp;
        c.old_bmp = old;
        c.bits = bits as *mut u32;
        c.w = w;
        c.h = h;
        c.ui.resize(w, h);
    }
}

/// Папка БД: рядом с exe; если недоступна для записи — %LOCALAPPDATA%\pdfcat (решение владельца).
pub fn db_dir() -> std::path::PathBuf {
    unsafe {
        let mut buf = [0u16; 1024];
        let n = GetModuleFileNameW(0, buf.as_mut_ptr(), 1024) as usize;
        let exe = wide_to_string(&buf[..n]);
        let dir = std::path::Path::new(&exe).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let probe = dir.join(".pdfcat_write_probe");
        let ok = std::fs::write(&probe, b"ok").is_ok();
        let _ = std::fs::remove_file(&probe);
        if ok {
            dir
        } else {
            let la = std::env::var_os("LOCALAPPDATA")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let d = la.join("pdfcat");
            let _ = std::fs::create_dir_all(&d);
            d
        }
    }
}

/// Диалог выбора папки (SHBrowseForFolderW) → Option<String>.
pub fn pick_folder(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut name = [0u16; 2048];
        let title = utf8_to_wide("Выберите сетевой диск или папку:");
        let mut bi: BrowseInfoW = std::mem::zeroed();
        bi.hwnd_owner = hwnd;
        bi.psz_display_name = name.as_mut_ptr();
        bi.lpsz_title = title.as_ptr();
        bi.ul_flags = BIF_RETURNONLYFSDIRS;
        let pidl = SHBrowseForFolderW(&mut bi);
        if pidl == 0 {
            return None;
        }
        let mut path = [0u16; 1024];
        let ok = SHGetPathFromIDListW(pidl, path.as_mut_ptr());
        if ok == 0 {
            return None;
        }
        Some(wide_to_string(&path))
    }
}

pub fn open_pdf(path: &str) {
    unsafe {
        let op = utf8_to_wide("open");
        let f = utf8_to_wide(path);
        ShellExecuteW(0, op.as_ptr(), f.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
    }
}

/// Оффscreen-рендер кадра в BMP (для снимков без показа окна).
pub fn render_to_bmp(w: i32, h: i32, ui: UiState, path: &str) -> bool {
    unsafe {
        let mem = CreateCompatibleDC(0);
        let mut bmi = BitmapInfoHeader::default();
        bmi.bi_size = 40;
        bmi.bi_width = w;
        bmi.bi_height = -h;
        bmi.bi_planes = 1;
        bmi.bi_bit_count = 32;
        bmi.bi_compression = 0;
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(mem, &bmi, 0, &mut bits, 0, 0);
        SelectObject(mem, bmp);
        let fonts = Fonts::new();
        let mut c = Ctx {
            ui,
            hwnd: 0,
            mem_dc: mem,
            bmp,
            old_bmp: 0,
            bits: bits as *mut u32,
            w,
            h,
            fonts,
            caret_visible: true,
        };
        c.ui.resize(w, h);
        draw_frame(&mut c);
        // BMP-файл: header 14 + info 40 + биты (top-down, 32bpp)
        let row = (w * 4) as usize;
        let data_size = row * h as usize;
        let mut f = Vec::with_capacity(54 + data_size);
        f.extend_from_slice(b"BM");
        f.extend_from_slice(&((54 + data_size) as u32).to_le_bytes());
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(&54u32.to_le_bytes());
        f.extend_from_slice(&40u32.to_le_bytes());
        f.extend_from_slice(&w.to_le_bytes());
        f.extend_from_slice(&(-(h)).to_le_bytes()); // top-down: строки уже сверху вниз
        f.extend_from_slice(&1u16.to_le_bytes());
        f.extend_from_slice(&32u16.to_le_bytes());
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(&(data_size as u32).to_le_bytes());
        f.extend_from_slice(&2835i32.to_le_bytes());
        f.extend_from_slice(&2835i32.to_le_bytes());
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(&0u32.to_le_bytes());
        let bits_slice = std::slice::from_raw_parts(bits as *const u8, data_size);
        f.extend_from_slice(bits_slice);
        let ok = std::fs::write(path, &f).is_ok();
        DeleteObject(bmp);
        DeleteDC(mem);
        ok
    }
}

pub fn run_gui() -> i32 {
    unsafe {
        let _ = FreeConsole();
        let (mut win_w, mut win_h) = (1440i32, 900i32);
        {
            #[repr(C)]
            struct RectW {
                l: i32,
                t: i32,
                r: i32,
                b: i32,
            }
            let mut ra = RectW { l: 0, t: 0, r: 0, b: 0 };
            // SPI_GETWORKAREA = 0x0030
            if system_parameters_info_a(0x0030, 0, &mut ra as *mut RectW as *mut c_void, 0) != 0 {
                win_w = win_w.min((ra.r - ra.l) - 40).max(900);
                win_h = win_h.min((ra.b - ra.t) - 60).max(600);
            }
        }
        let hinst = GetModuleHandleW(std::ptr::null());
        let class_name = utf8_to_wide("pdfcat_window");
        let wc = WndClassW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfn_wnd_proc: Some(wndproc),
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: hinst,
            h_icon: 0,
            h_cursor: LoadCursorW(0, IDC_ARROW),
            h_br_background: 0,
            lpsz_menu_name: std::ptr::null(),
            lpsz_class_name: class_name.as_ptr(),
        };
        RegisterClassW(&wc);

        let ui = UiState::load_or_empty(&db_dir());
        let ctx = Box::new(Ctx {
            ui,
            hwnd: 0,
            mem_dc: CreateCompatibleDC(0),
            bmp: 0,
            old_bmp: 0,
            bits: std::ptr::null_mut(),
            w: win_w,
            h: win_h,
            fonts: Fonts::new(),
            caret_visible: true,
        });
        CTX = Box::into_raw(ctx);

        let title = utf8_to_wide("pdfcat — каталог PDF (только чтение диска)");
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            win_w,
            win_h,
            0,
            0,
            hinst,
            std::ptr::null_mut(),
        );
        (*CTX).hwnd = hwnd;
        resize_dib(&mut *CTX, win_w, win_h);
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        SetTimer(hwnd, 1, 500);

        let mut msg: Msg = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // Сохранить состояние не нужно: всё в файлах БД
        let _ = DestroyWindow(hwnd);
        0
    }
}

#[link(name = "user32")]
extern "system" {
    #[link_name = "SystemParametersInfoA"]
    fn system_parameters_info_a(action: u32, param: u32, pv: *mut c_void, init: u32) -> i32;
    fn FreeConsole() -> i32;
}
