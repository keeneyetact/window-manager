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

/// How often to ask again for an icon a window didn't have yet. New windows
/// often set theirs a moment after they appear.
const RETRY_EVERY: Duration = Duration::from_secs(2);
/// How long after first seeing a window to stop asking.
const GIVE_UP_AFTER: Duration = Duration::from_secs(30);

enum Entry {
    Loaded(egui::TextureHandle),
    /// No icon yet: when the window was first seen, and when last asked.
    Missing { since: Instant, tried: Instant },
}

/// Icons by window, loaded once each. A window without one shows the app's
/// initial instead, and is asked again for a while in case it sets one late.
#[derive(Default)]
pub struct Icons(HashMap<isize, Entry>);

impl Icons {
    pub fn get(&mut self, ctx: &egui::Context, h: isize, pid: u32) -> Option<egui::TextureHandle> {
        let now = Instant::now();
        let since = match self.0.get(&h) {
            Some(Entry::Loaded(tex)) => return Some(tex.clone()),
            Some(&Entry::Missing { since, tried }) => {
                if now - tried < RETRY_EVERY || now - since > GIVE_UP_AFTER {
                    return None;
                }
                since
            }
            None => now,
        };
        let icon = load(ctx, h, pid);
        let entry = match &icon {
            Some(tex) => Entry::Loaded(tex.clone()),
            None => Entry::Missing { since, tried: now },
        };
        self.0.insert(h, entry);
        icon
    }

    /// Forgets windows that have closed.
    pub fn retain(&mut self, alive: impl Fn(isize) -> bool) {
        self.0.retain(|&h, _| alive(h));
    }
}

fn load(ctx: &egui::Context, h: isize, pid: u32) -> Option<egui::TextureHandle> {
    let (rgba, w, ht) = window_icon(h).or_else(|| exe_icon(pid))?;
    let image = egui::ColorImage::from_rgba_unmultiplied([w, ht], &rgba);
    Some(ctx.load_texture(format!("icon-{h}"), image, egui::TextureOptions::LINEAR))
}

/// The icon the window shows in its title bar and the taskbar. These belong
/// to the window, so they aren't destroyed here.
fn window_icon(h: isize) -> Option<(Vec<u8>, usize, usize)> {
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
                if let Some(pixels) = icon_pixels(HICON(icon as *mut c_void)) {
                    return Some(pixels);
                }
            }
        }
        for index in [GCLP_HICON, GCLP_HICONSM] {
            let icon = GetClassLongPtrW(hwnd(h), index);
            if icon != 0 {
                if let Some(pixels) = icon_pixels(HICON(icon as *mut c_void)) {
                    return Some(pixels);
                }
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
