//! Live copies of other windows. A copy is a borderless window of ours that
//! the compositor (DWM) paints with the source window's pixels as they change.
//! It is only a picture: clicks never reach the source, so a copy can be
//! moved, resized, cropped and closed without touching the real window.

use crate::{ACCENT, hwnd};
use std::cell::RefCell;
use std::collections::HashMap;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES,
    DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateSolidBrush, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const CLASS: PCWSTR = w!("WindowManagerCopy");
const BAND_CLASS: PCWSTR = w!("WindowManagerCopyBand");
/// How often a copy checks that its source still exists and picks up its size.
const WATCH_MS: u32 = 250;
/// Width of the invisible resize border.
const GRIP: i32 = 8;
const MIN_SIZE: i32 = 40;

const ID_TOP: usize = 1;
const ID_REGION: usize = 2;
const ID_WHOLE: usize = 3;
const ID_ACTUAL: usize = 4;
const ID_FRONT: usize = 5;
const ID_CLOSE: usize = 6;

/// Part of the source's client area, as fractions of its size.
#[derive(Clone, Copy, PartialEq)]
struct Crop {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

const WHOLE: Crop = Crop { left: 0.0, top: 0.0, right: 1.0, bottom: 1.0 };

/// Dragging out a region to crop to.
struct Select {
    start: Option<POINT>,
    /// Translucent rectangle that follows the drag.
    band: isize,
}

struct Mirror {
    source: isize,
    thumb: isize,
    crop: Crop,
    topmost: bool,
    select: Option<Select>,
}

thread_local! {
    // Copies live on the UI thread, whose message loop drives them
    static COPIES: RefCell<HashMap<isize, Mirror>> = RefCell::new(HashMap::new());
}

fn with<R>(h: HWND, f: impl FnOnce(&mut Mirror) -> R) -> Option<R> {
    COPIES.with(|c| c.borrow_mut().get_mut(&(h.0 as isize)).map(f))
}

/// Number of open copies of each source window.
pub fn counts() -> HashMap<isize, usize> {
    COPIES.with(|c| {
        let mut n = HashMap::new();
        for copy in c.borrow().values() {
            *n.entry(copy.source).or_default() += 1;
        }
        n
    })
}

pub fn close_all(source: isize) {
    let copies: Vec<isize> = COPIES.with(|c| {
        c.borrow().iter().filter(|(_, m)| m.source == source).map(|(&h, _)| h).collect()
    });
    for h in copies {
        unsafe {
            let _ = DestroyWindow(hwnd(h));
        }
    }
}

fn register_classes() {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default().into();
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let copy = WNDCLASSW {
            lpfnWndProc: Some(copy_proc),
            hInstance: instance,
            hCursor: arrow,
            // Shows around the picture when the copy's shape differs from it
            hbrBackground: CreateSolidBrush(COLORREF(0x00141210)),
            lpszClassName: CLASS,
            ..Default::default()
        };
        let band = WNDCLASSW {
            lpfnWndProc: Some(band_proc),
            hInstance: instance,
            hbrBackground: CreateSolidBrush(COLORREF(
                ACCENT.r() as u32 | (ACCENT.g() as u32) << 8 | (ACCENT.b() as u32) << 16,
            )),
            lpszClassName: BAND_CLASS,
            ..Default::default()
        };
        // Fails harmlessly once the classes exist
        RegisterClassW(&copy);
        RegisterClassW(&band);
    }
}

unsafe extern "system" fn band_proc(h: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, msg, wparam, lparam) }
}

fn client_size(h: isize) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd(h), &mut r);
    }
    (r.right - r.left, r.bottom - r.top)
}

fn window_rect(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    r
}

/// The cropped part of the source, in its client pixels.
fn source_rect(source: isize, crop: Crop) -> RECT {
    let (w, h) = client_size(source);
    RECT {
        left: (crop.left * w as f32).round() as i32,
        top: (crop.top * h as f32).round() as i32,
        right: (crop.right * w as f32).round() as i32,
        bottom: (crop.bottom * h as f32).round() as i32,
    }
}

/// Where the picture sits in a copy of size `w` x `h`: as large as fits,
/// centred, keeping the source's proportions.
fn picture_rect(src: RECT, w: i32, h: i32) -> RECT {
    let (sw, sh) = ((src.right - src.left).max(1) as f32, (src.bottom - src.top).max(1) as f32);
    let scale = (w as f32 / sw).min(h as f32 / sh);
    let (pw, ph) = ((sw * scale).round() as i32, (sh * scale).round() as i32);
    let (x, y) = ((w - pw) / 2, (h - ph) / 2);
    RECT { left: x, top: y, right: x + pw, bottom: y + ph }
}

fn update(h: HWND) {
    let Some((source, thumb, crop)) = with(h, |m| (m.source, m.thumb, m.crop)) else {
        return;
    };
    let src = source_rect(source, crop);
    let (w, ht) = client_size(h.0 as isize);
    let props = DWM_THUMBNAIL_PROPERTIES {
        dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_RECTSOURCE | DWM_TNP_VISIBLE | DWM_TNP_SOURCECLIENTAREAONLY,
        rcDestination: picture_rect(src, w, ht),
        rcSource: src,
        fVisible: true.into(),
        fSourceClientAreaOnly: true.into(),
        ..Default::default()
    };
    unsafe {
        let _ = DwmUpdateThumbnailProperties(thumb, &props);
    }
}

/// Sizes the copy so the picture fits within `max_w` x `max_h`, never larger
/// than the source itself.
fn fit_size(src: RECT, max_w: i32, max_h: i32) -> (i32, i32) {
    let (w, h) = ((src.right - src.left).max(1), (src.bottom - src.top).max(1));
    let scale = (max_w as f32 / w as f32).min(max_h as f32 / h as f32).min(1.0);
    (((w as f32 * scale) as i32).max(MIN_SIZE), ((h as f32 * scale) as i32).max(MIN_SIZE))
}

/// Opens a new copy of `source` at the source's own size, a step below and
/// right of it for each copy already open, kept within its screen.
pub fn create(source: isize) -> windows::core::Result<()> {
    register_classes();
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let mut origin = POINT::default();
    unsafe {
        let _ = GetMonitorInfoW(MonitorFromWindow(hwnd(source), MONITOR_DEFAULTTONEAREST), &mut info);
        let _ = ClientToScreen(hwnd(source), &mut origin);
    }
    let work = info.rcWork;
    let (w, h) = fit_size(source_rect(source, WHOLE), work.right - work.left, work.bottom - work.top);
    let open = COPIES.with(|c| c.borrow().len()) as i32;
    let step = 32 * (open % 8 + 1);
    let x = (origin.x + step).min(work.right - w).max(work.left);
    let y = (origin.y + step).min(work.bottom - h).max(work.top);
    unsafe {
        let copy = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            CLASS,
            w!("Window copy"),
            WS_POPUP | WS_THICKFRAME,
            x,
            y,
            w,
            h,
            None,
            None,
            GetModuleHandleW(None).unwrap_or_default(),
            None,
        )?;
        let thumb = match DwmRegisterThumbnail(copy, hwnd(source)) {
            Ok(t) => t,
            Err(e) => {
                let _ = DestroyWindow(copy);
                return Err(e);
            }
        };
        COPIES.with(|c| {
            c.borrow_mut().insert(
                copy.0 as isize,
                Mirror { source, thumb, crop: WHOLE, topmost: true, select: None },
            )
        });
        // Matches the rest of the app, which stays out of screen captures
        let _ = SetWindowDisplayAffinity(copy, WDA_EXCLUDEFROMCAPTURE);
        update(copy);
        SetTimer(copy, 1, WATCH_MS, None);
        let _ = ShowWindow(copy, SW_SHOWNOACTIVATE);
    }
    Ok(())
}

fn set_size(h: HWND, x: i32, y: i32, w: i32, ht: i32) {
    unsafe {
        let _ = SetWindowPos(h, None, x, y, w, ht, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

fn set_topmost(h: HWND, on: bool) {
    with(h, |m| m.topmost = on);
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe {
        let _ = SetWindowPos(h, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

fn start_select(h: HWND) {
    with(h, |m| m.select = Some(Select { start: None, band: 0 }));
    unsafe {
        let _ = SetForegroundWindow(h);
        let _ = SetFocus(h);
    }
}

fn end_select(h: HWND) -> Option<Select> {
    let select = with(h, |m| m.select.take()).flatten()?;
    unsafe {
        let _ = ReleaseCapture();
        if select.band != 0 {
            let _ = DestroyWindow(hwnd(select.band));
        }
    }
    Some(select)
}

/// The dragged rectangle from the start point to `p`, in copy coordinates.
fn drag_rect(start: POINT, p: POINT) -> RECT {
    RECT {
        left: start.x.min(p.x),
        top: start.y.min(p.y),
        right: start.x.max(p.x),
        bottom: start.y.max(p.y),
    }
}

fn point(lparam: LPARAM) -> POINT {
    POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 }
}

fn move_band(h: HWND, r: RECT) {
    let Some(band) = with(h, |m| m.select.as_ref().map(|s| s.band)).flatten() else {
        return;
    };
    let origin = window_rect(h);
    let (x, y) = (origin.left + r.left, origin.top + r.top);
    let (w, ht) = (r.right - r.left, r.bottom - r.top);
    unsafe {
        if band == 0 {
            let Ok(b) = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                BAND_CLASS,
                w!(""),
                WS_POPUP,
                x,
                y,
                w,
                ht,
                h,
                None,
                GetModuleHandleW(None).unwrap_or_default(),
                None,
            ) else {
                return;
            };
            let _ = SetLayeredWindowAttributes(b, COLORREF(0), 90, LWA_ALPHA);
            let _ = SetWindowDisplayAffinity(b, WDA_EXCLUDEFROMCAPTURE);
            let _ = ShowWindow(b, SW_SHOWNOACTIVATE);
            with(h, |m| {
                if let Some(s) = &mut m.select {
                    s.band = b.0 as isize;
                }
            });
        } else {
            let _ = SetWindowPos(hwnd(band), HWND_TOPMOST, x, y, w, ht, SWP_NOACTIVATE);
        }
    }
}

/// Crops the copy to the dragged rectangle and shrinks it around that part,
/// so the region stays where and how large it was on screen.
fn finish_select(h: HWND, start: POINT, end: POINT) {
    end_select(h);
    let Some((source, crop)) = with(h, |m| (m.source, m.crop)) else {
        return;
    };
    let (w, ht) = client_size(h.0 as isize);
    let pic = picture_rect(source_rect(source, crop), w, ht);
    let sel = drag_rect(start, end);
    let sel = RECT {
        left: sel.left.max(pic.left),
        top: sel.top.max(pic.top),
        right: sel.right.min(pic.right),
        bottom: sel.bottom.min(pic.bottom),
    };
    if sel.right - sel.left < 8 || sel.bottom - sel.top < 8 {
        return;
    }
    let (pw, ph) = ((pic.right - pic.left) as f32, (pic.bottom - pic.top) as f32);
    let (cw, ch) = (crop.right - crop.left, crop.bottom - crop.top);
    let fx = |x: i32| crop.left + (x - pic.left) as f32 / pw * cw;
    let fy = |y: i32| crop.top + (y - pic.top) as f32 / ph * ch;
    let new = Crop { left: fx(sel.left), top: fy(sel.top), right: fx(sel.right), bottom: fy(sel.bottom) };
    with(h, |m| m.crop = new);
    let origin = window_rect(h);
    set_size(
        h,
        origin.left + sel.left,
        origin.top + sel.top,
        (sel.right - sel.left).max(MIN_SIZE),
        (sel.bottom - sel.top).max(MIN_SIZE),
    );
    update(h);
}

/// Resizes the copy so its picture shows `crop` at the scale it shows now.
fn recrop(h: HWND, crop: Crop) {
    let Some((source, old)) = with(h, |m| (m.source, m.crop)) else {
        return;
    };
    let (w, ht) = client_size(h.0 as isize);
    let old_src = source_rect(source, old);
    let pic = picture_rect(old_src, w, ht);
    let scale = (pic.right - pic.left) as f32 / (old_src.right - old_src.left).max(1) as f32;
    let src = source_rect(source, crop);
    with(h, |m| m.crop = crop);
    let r = window_rect(h);
    let nw = (((src.right - src.left) as f32 * scale) as i32).max(MIN_SIZE);
    let nh = (((src.bottom - src.top) as f32 * scale) as i32).max(MIN_SIZE);
    set_size(h, r.left, r.top, nw, nh);
    update(h);
}

/// Resizes the copy around its centre, keeping the picture's proportions.
fn zoom(h: HWND, factor: f32) {
    let Some((source, crop)) = with(h, |m| (m.source, m.crop)) else {
        return;
    };
    let r = window_rect(h);
    let (w, ht) = (r.right - r.left, r.bottom - r.top);
    let pic = picture_rect(source_rect(source, crop), w, ht);
    let nw = (((pic.right - pic.left) as f32 * factor) as i32).max(MIN_SIZE);
    let nh = (((pic.bottom - pic.top) as f32 * factor) as i32).max(MIN_SIZE);
    set_size(h, r.left + (w - nw) / 2, r.top + (ht - nh) / 2, nw, nh);
}

fn show_menu(h: HWND, x: i32, y: i32) {
    let Some((source, crop, topmost)) = with(h, |m| (m.source, m.crop, m.topmost)) else {
        return;
    };
    let cropped = crop != WHOLE;
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let check = if topmost { MF_CHECKED } else { MF_UNCHECKED };
        let whole = if cropped { MF_ENABLED } else { MF_GRAYED };
        let _ = AppendMenuW(menu, MF_STRING | check, ID_TOP, w!("Always on top"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_REGION, w!("Select region…"));
        let _ = AppendMenuW(menu, MF_STRING | whole, ID_WHOLE, w!("Show whole window"));
        let _ = AppendMenuW(menu, MF_STRING, ID_ACTUAL, w!("Actual size"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_FRONT, w!("Bring original to front"));
        let _ = AppendMenuW(menu, MF_STRING, ID_CLOSE, w!("Close copy"));
        // Lets the menu close when clicking elsewhere
        let _ = SetForegroundWindow(h);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, 0, h, None).0 as usize;
        let _ = DestroyMenu(menu);
        match cmd {
            ID_TOP => set_topmost(h, !topmost),
            ID_REGION => start_select(h),
            ID_WHOLE => recrop(h, WHOLE),
            ID_ACTUAL => {
                let src = source_rect(source, crop);
                let r = window_rect(h);
                set_size(h, r.left, r.top, src.right - src.left, src.bottom - src.top);
            }
            ID_FRONT => crate::bring_to_front(source),
            ID_CLOSE => {
                let _ = DestroyWindow(h);
            }
            _ => {}
        }
    }
}

/// Hit test for a borderless window: the edges resize, the rest drags.
fn hit_test(h: HWND, lparam: LPARAM) -> u32 {
    let p = point(lparam);
    let r = window_rect(h);
    let (l, t, rt, b) = (p.x < r.left + GRIP, p.y < r.top + GRIP, p.x >= r.right - GRIP, p.y >= r.bottom - GRIP);
    match (l, t, rt, b) {
        (true, true, _, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, _, true, true) => HTBOTTOMRIGHT,
        (true, ..) => HTLEFT,
        (_, true, ..) => HTTOP,
        (_, _, true, _) => HTRIGHT,
        (.., true) => HTBOTTOM,
        _ => HTCAPTION,
    }
}

unsafe extern "system" fn copy_proc(h: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let selecting = with(h, |m| m.select.as_ref().map(|s| s.start)).flatten();
        match msg {
            // The whole window is client area; the frame only enables resizing
            WM_NCCALCSIZE if wparam.0 != 0 => return LRESULT(0),
            WM_NCHITTEST => {
                return LRESULT(if selecting.is_some() { HTCLIENT } else { hit_test(h, lparam) } as isize);
            }
            WM_GETMINMAXINFO => {
                let info = &mut *(lparam.0 as *mut MINMAXINFO);
                info.ptMinTrackSize = POINT { x: MIN_SIZE, y: MIN_SIZE };
                return LRESULT(0);
            }
            WM_SIZE => update(h),
            WM_TIMER => {
                let source = with(h, |m| m.source).unwrap_or(0);
                if !IsWindow(hwnd(source)).as_bool() {
                    let _ = DestroyWindow(h);
                } else {
                    // Follows the source as it resizes
                    update(h);
                }
                return LRESULT(0);
            }
            WM_SETCURSOR if selecting.is_some() => {
                SetCursor(LoadCursorW(None, IDC_CROSS).unwrap_or_default());
                return LRESULT(1);
            }
            WM_LBUTTONDOWN if selecting.is_some() => {
                let p = point(lparam);
                with(h, |m| {
                    if let Some(s) = &mut m.select {
                        s.start = Some(p);
                    }
                });
                SetCapture(h);
                return LRESULT(0);
            }
            WM_MOUSEMOVE => {
                if let Some(Some(start)) = selecting {
                    move_band(h, drag_rect(start, point(lparam)));
                }
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                if let Some(Some(start)) = selecting {
                    finish_select(h, start, point(lparam));
                }
                return LRESULT(0);
            }
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
                end_select(h);
                return LRESULT(0);
            }
            WM_RBUTTONUP if selecting.is_some() => {
                end_select(h);
                return LRESULT(0);
            }
            WM_CAPTURECHANGED if selecting.is_some_and(|s| s.is_some()) => {
                end_select(h);
            }
            WM_NCRBUTTONUP | WM_CONTEXTMENU => {
                let p = point(lparam);
                show_menu(h, p.x, p.y);
                return LRESULT(0);
            }
            WM_NCLBUTTONDBLCLK => {
                if let Some(source) = with(h, |m| m.source) {
                    crate::bring_to_front(source);
                }
                return LRESULT(0);
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
                zoom(h, if delta > 0 { 1.1 } else { 1.0 / 1.1 });
                return LRESULT(0);
            }
            WM_DESTROY => {
                end_select(h);
                let _ = KillTimer(h, 1);
                let copy = COPIES.with(|c| c.borrow_mut().remove(&(h.0 as isize)));
                if let Some(copy) = copy {
                    let _ = DwmUnregisterThumbnail(copy.thumb);
                }
            }
            _ => {}
        }
        DefWindowProcW(h, msg, wparam, lparam)
    }
}
