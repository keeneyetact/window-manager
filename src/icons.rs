//! Window icons, as egui textures. A window's icon comes from the window
//! itself, its window class, or the app's exe, whichever has one first.

use crate::hwnd;
use eframe::egui;
use std::collections::HashMap;
use std::ffi::c_void;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

/// How long to wait for a busy window to hand over its icon.
const ASK_MS: u32 = 50;

/// How often to ask each window which icon it has now. Windows can set or
/// change theirs at any time, and some are hidden rather than closed, so the
/// one first seen isn't trusted for good.
const RECHECK_EVERY: Duration = Duration::from_secs(2);

struct Entry {
    texture: Option<egui::TextureHandle>,
    /// The window's icon handle the texture was made from, 0 if it had none.
    source: isize,
    checked: Instant,
}

/// Icons by window. A window without one shows the app's initial instead.
#[derive(Default)]
pub struct Icons(HashMap<isize, Entry>);

impl Icons {
    pub fn get(&mut self, ctx: &egui::Context, h: isize, pid: u32) -> Option<egui::TextureHandle> {
        let now = Instant::now();
        if let Some(e) = self.0.get(&h) {
            if now - e.checked < RECHECK_EVERY {
                return e.texture.clone();
            }
        }
        let icon = icon_handle(h);
        let source = icon.map_or(0, |i| i.0 as isize);
        if let Some(e) = self.0.get_mut(&h) {
            e.checked = now;
            // The exe's icon never changes, so a window still without its own keeps it
            if source == e.source || source == 0 {
                return e.texture.clone();
            }
        }
        let texture = load(ctx, h, pid, icon);
        self.0.insert(h, Entry { texture: texture.clone(), source, checked: now });
        texture
    }

    /// Forgets windows that have closed.
    pub fn retain(&mut self, alive: impl Fn(isize) -> bool) {
        self.0.retain(|&h, _| alive(h));
    }
}

fn load(ctx: &egui::Context, h: isize, pid: u32, icon: Option<HICON>) -> Option<egui::TextureHandle> {
    let (rgba, w, ht) = icon.and_then(icon_pixels).or_else(|| exe_icon(pid))?;
    let image = egui::ColorImage::from_rgba_unmultiplied([w, ht], &rgba);
    Some(ctx.load_texture(format!("icon-{h}"), image, egui::TextureOptions::LINEAR))
}

/// The icon the window shows in its title bar and the taskbar, or failing
/// that its class's. These belong to the window, so they aren't destroyed here.
fn icon_handle(h: isize) -> Option<HICON> {
    unsafe {
        for kind in [ICON_BIG, ICON_SMALL2, ICON_SMALL] {
            let mut icon = 0usize;
            let _ = SendMessageTimeoutW(
                hwnd(h),
                WM_GETICON,
                WPARAM(kind as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                ASK_MS,
                Some(&mut icon),
            );
            if icon != 0 {
                return Some(HICON(icon as *mut c_void));
            }
        }
        for index in [GCLP_HICON, GCLP_HICONSM] {
            let icon = GetClassLongPtrW(hwnd(h), index);
            if icon != 0 {
                return Some(HICON(icon as *mut c_void));
            }
        }
    }
    None
}

/// The first icon in the app's exe.
fn exe_icon(pid: u32) -> Option<(Vec<u8>, usize, usize)> {
    let path = crate::process_path(pid)?;
    // Hosts store apps' windows, whose own icon it would hide behind a generic one
    if path.to_lowercase().ends_with("\\applicationframehost.exe") {
        return None;
    }
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut icon = HICON::default();
    unsafe {
        if ExtractIconExW(PCWSTR(wide.as_ptr()), 0, Some(&mut icon), None, 1) == 0 || icon.is_invalid() {
            return None;
        }
        let pixels = icon_pixels(icon);
        let _ = DestroyIcon(icon);
        pixels
    }
}

/// Reads the bitmap of a bitmap-backed GDI object into top-down BGRA rows.
unsafe fn bitmap_bgra(bitmap: HBITMAP, w: i32, ht: i32) -> Option<Vec<u8>> {
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative for rows from the top down
            biHeight: -ht,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; (w * ht * 4) as usize];
    unsafe {
        let dc = GetDC(None);
        let rows = GetDIBits(dc, bitmap, 0, ht as u32, Some(buf.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        (rows == ht).then_some(buf)
    }
}

/// An icon's pixels as RGBA. Old icons have no alpha channel and mark their
/// transparent pixels in a separate mask instead.
fn icon_pixels(icon: HICON) -> Option<(Vec<u8>, usize, usize)> {
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info).ok()?;
        let (color, mask) = (info.hbmColor, info.hbmMask);
        let pixels = (|| {
            // Black and white icons have no colour bitmap; leave them to the fallback
            if color.is_invalid() {
                return None;
            }
            let mut bm = BITMAP::default();
            if GetObjectW(color, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void)) == 0 {
                return None;
            }
            let (w, ht) = (bm.bmWidth, bm.bmHeight);
            if w <= 0 || ht <= 0 || w > 256 || ht > 256 {
                return None;
            }
            let mut px = bitmap_bgra(color, w, ht)?;
            let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
            let mask_px = if has_alpha { None } else { bitmap_bgra(mask, w, ht) };
            for (i, p) in px.chunks_exact_mut(4).enumerate() {
                p.swap(0, 2);
                if let Some(m) = &mask_px {
                    // A set mask bit is a transparent pixel
                    p[3] = if m[i * 4] == 0 { 255 } else { 0 };
                }
            }
            Some((px, w as usize, ht as usize))
        })();
        let _ = DeleteObject(color);
        let _ = DeleteObject(mask);
        pixels
    }
}
