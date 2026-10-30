#![windows_subsystem = "windows"]

mod icons;
mod mirror;
mod update;

use eframe::egui::{self, Color32, RichText, Rounding, Sense, Stroke, Vec2};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{BOOL, CloseHandle, COLORREF, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_CLOAK, DWMWA_CLOAKED, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PWSTR, w};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const ACCENT: Color32 = Color32::from_rgb(99, 140, 255);
const BG: Color32 = Color32::from_rgb(22, 24, 30);
const PANEL: Color32 = Color32::from_rgb(30, 33, 41);
const CARD: Color32 = Color32::from_rgb(38, 42, 52);
const HOVER: Color32 = Color32::from_rgb(46, 51, 63);
/// Buttons and slider rails, which sit on cards.
const CONTROL: Color32 = Color32::from_rgb(52, 58, 72);
const CONTROL_HOVER: Color32 = Color32::from_rgb(64, 71, 88);
const MUTED: Color32 = Color32::from_rgb(140, 147, 163);
const DANGER: Color32 = Color32::from_rgb(255, 110, 110);
const REFRESH_EVERY: Duration = Duration::from_secs(2);
const ORDER_EVERY: Duration = Duration::from_millis(150);
/// Passed by the Windows startup entry: start in the tray without a window.
const BACKGROUND_ARG: &str = "--background";

thread_local! {
    // Kept on the UI thread so Quit can remove the icon before exiting
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct WinInfo {
    hwnd: isize,
    pid: u32,
    title: String,
    process: String,
}

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

unsafe extern "system" fn enum_proc(h: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let list = &mut *(lparam.0 as *mut Vec<WinInfo>);
        if !IsWindowVisible(h).as_bool() {
            return TRUE;
        }
        // Skip hidden UWP "ghost" windows
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return TRUE;
        }
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return TRUE;
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(h, &mut buf);
        if len > 0 {
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            list.push(WinInfo {
                hwnd: h.0 as isize,
                pid,
                title: String::from_utf16_lossy(&buf[..len as usize]),
                process: String::new(),
            });
        }
        TRUE
    }
}

/// Full path of the process's exe, unless it can't be read (such as an app
/// running as administrator).
pub(crate) fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        ok.then(|| String::from_utf16_lossy(&buf[..size as usize]))
    }
}

fn process_name(pid: u32) -> String {
    match process_path(pid) {
        Some(path) => path.rsplit('\\').next().unwrap_or(&path).to_string(),
        None => "unknown".into(),
    }
}

fn list_windows(names: &mut HashMap<u32, String>) -> Vec<WinInfo> {
    let mut list = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut list as *mut _ as isize));
    }
    let own = std::process::id();
    list.retain(|w: &WinInfo| w.pid != own);
    for w in &mut list {
        w.process = names
            .entry(w.pid)
            .or_insert_with(|| process_name(w.pid))
            .clone();
    }
    list
}

unsafe extern "system" fn own_enum_proc(h: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let list = &mut *(lparam.0 as *mut Vec<isize>);
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == std::process::id() {
            list.push(h.0 as isize);
        }
        TRUE
    }
}

/// Leaves `h` out of screenshots, recordings and screen sharing. Windows
/// before 10 version 2004 can't exclude a window, so there it shows as black.
pub(crate) fn hide_from_capture(h: HWND) {
    unsafe {
        if SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE).is_err() {
            let _ = SetWindowDisplayAffinity(h, WDA_MONITOR);
        }
    }
}

/// Like Signal's "Screen security": hide this app's own windows from
/// screenshots, recordings and screen sharing.
fn set_screen_security() {
    let mut own = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(own_enum_proc), LPARAM(&mut own as *mut _ as isize));
    }
    for h in own {
        hide_from_capture(hwnd(h));
    }
}

fn is_alive(h: isize) -> bool {
    unsafe { IsWindow(hwnd(h)).as_bool() }
}

fn get_opacity(h: isize) -> u8 {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd(h), GWL_EXSTYLE) as u32;
        if ex & WS_EX_LAYERED.0 == 0 {
            return 255;
        }
        let mut alpha = 255u8;
        let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
        if GetLayeredWindowAttributes(hwnd(h), None, Some(&mut alpha), Some(&mut flags)).is_ok()
            && flags.0 & LWA_ALPHA.0 != 0
        {
            alpha
        } else {
            255
        }
    }
}

fn set_opacity(h: isize, alpha: u8) -> windows::core::Result<()> {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd(h), GWL_EXSTYLE);
        if ex as u32 & WS_EX_LAYERED.0 == 0 {
            SetWindowLongPtrW(hwnd(h), GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
        }
        SetLayeredWindowAttributes(hwnd(h), COLORREF(0), alpha, LWA_ALPHA)
    }
}

const MIN_WIDTH: i32 = 100;

fn window_size(h: isize) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd(h), &mut r);
    }
    (r.right - r.left, r.bottom - r.top)
}

/// Resizes the window, keeping its left edge and height, and returns the width
/// it took. Apps can refuse to go below their own minimum (Chrome browser
/// windows stop near 500 px; `chrome --app=<url>` windows go much narrower).
fn set_width(h: isize, width: i32) -> windows::core::Result<i32> {
    unsafe {
        if IsZoomed(hwnd(h)).as_bool() {
            let _ = ShowWindow(hwnd(h), SW_RESTORE);
        }
        let (_, height) = window_size(h);
        SetWindowPos(
            hwnd(h),
            None,
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )?;
    }
    Ok(window_size(h).0)
}

fn is_topmost(h: isize) -> bool {
    unsafe { GetWindowLongPtrW(hwnd(h), GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0 }
}

fn set_topmost(h: isize, on: bool) -> windows::core::Result<()> {
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe {
        SetWindowPos(
            hwnd(h),
            after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    }
}

/// Topmost windows, from the top of the z-order down.
fn topmost_band() -> Vec<isize> {
    let mut band = Vec::new();
    unsafe {
        let mut next = GetTopWindow(None);
        while let Ok(h) = next {
            if h.is_invalid() || !is_topmost(h.0 as isize) {
                break;
            }
            band.push(h.0 as isize);
            next = GetWindow(h, GW_HWNDNEXT);
        }
    }
    band
}

/// Index of the stack level holding `h`.
fn level_of(order: &[Vec<isize>], h: isize) -> Option<usize> {
    order.iter().position(|l| l.contains(&h))
}

/// Windows ranks windows that share always-on-top by whichever was activated
/// last. Pinned windows are kept in levels (`order`, top first); windows on
/// the same level keep that behaviour among themselves. When a level falls
/// out of rank, restack every pinned window directly below the one above it,
/// keeping the current stacking within each level.
fn enforce_order(order: &[Vec<isize>]) {
    let band = topmost_band();
    let pos = |h: isize| band.iter().position(|&b| b == h).unwrap_or(usize::MAX);
    let levels: Vec<Vec<isize>> = order
        .iter()
        .map(|l| {
            let mut l: Vec<isize> = l.iter().copied().filter(|&h| is_alive(h) && is_topmost(h)).collect();
            l.sort_by_key(|&h| pos(h));
            l
        })
        .filter(|l| !l.is_empty())
        .collect();
    if levels.windows(2).all(|p| pos(*p[0].last().unwrap()) < pos(p[1][0])) {
        return;
    }
    let live = levels.concat();
    for p in live.windows(2) {
        unsafe {
            let _ = SetWindowPos(
                hwnd(p[1]),
                hwnd(p[0]),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

fn spawn_order_keeper(order: Arc<Mutex<Vec<Vec<isize>>>>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(ORDER_EVERY);
            enforce_order(&order.lock().unwrap());
        }
    });
}

fn bring_to_front(h: isize) {
    unsafe {
        if IsIconic(hwnd(h)).as_bool() {
            let _ = ShowWindow(hwnd(h), SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd(h));
    }
}

fn show_main(h: isize) {
    set_cloak(h, false);
    set_dark_title_bar(h);
    // Hiding the window can drop its capture exclusion
    set_screen_security();
    unsafe {
        // winit marks every window as an app window, which forces a taskbar
        // button even on owned windows; the shell checks this when showing
        let ex = GetWindowLongPtrW(hwnd(h), GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd(h), GWL_EXSTYLE, ex & !(WS_EX_APPWINDOW.0 as isize));
        let _ = ShowWindow(hwnd(h), SW_SHOW);
    }
    bring_to_front(h);
}

fn hide_main(h: isize) {
    unsafe {
        let _ = ShowWindow(hwnd(h), SW_HIDE);
    }
}

/// Minimizing hides the window like closing does. An owned window with no
/// taskbar button would otherwise linger as a title-bar stub on the desktop.
unsafe extern "system" fn minimize_to_tray(
    h: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_SYSCOMMAND && (wparam.0 & 0xFFF0) as u32 == SC_MINIMIZE {
            let _ = ShowWindow(h, SW_HIDE);
            return LRESULT(0);
        }
        let result = DefSubclassProc(h, msg, wparam, lparam);
        // Minimized some other way, such as by the shell
        if msg == WM_SIZE && wparam.0 as u32 == SIZE_MINIMIZED {
            let _ = ShowWindow(h, SW_HIDE);
        }
        result
    }
}

fn hide_on_minimize(h: isize) {
    unsafe {
        let _ = SetWindowSubclass(hwnd(h), Some(minimize_to_tray), 1, 0);
    }
}

/// An owned window gets no taskbar button, so the app lives only in the
/// tray. The owner is a hidden window that is never shown.
fn set_hidden_owner(h: isize) {
    unsafe {
        if let Ok(owner) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Window Manager owner"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        ) {
            SetWindowLongPtrW(hwnd(h), GWLP_HWNDPARENT, owner.0 as isize);
        }
    }
}

/// Matches the title bar to the app's dark look in Windows' light mode too.
fn set_dark_title_bar(h: isize) {
    let on = TRUE;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd(h),
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &on as *const _ as *const c_void,
            std::mem::size_of::<BOOL>() as u32,
        );
        // The frame keeps its old colour until it is redrawn
        let _ = SetWindowPos(
            hwnd(h),
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// A cloaked window is drawn nowhere even while Windows considers it shown.
fn set_cloak(h: isize, on: bool) {
    let on = BOOL::from(on);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd(h),
            DWMWA_CLOAK,
            &on as *const _ as *const c_void,
            std::mem::size_of::<BOOL>() as u32,
        );
    }
}

const ICON_SIZE: i32 = 32;

/// A rounded accent tile with two overlapping white windows, as RGBA pixels.
fn icon_rgba() -> Vec<u8> {
    const N: i32 = ICON_SIZE;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    let inside = |x: i32, y: i32, x0: i32, y0: i32, x1: i32, y1: i32| x >= x0 && x < x1 && y >= y0 && y < y1;
    for y in 0..N {
        for x in 0..N {
            // Corner radius of 6px
            let (cx, cy) = (x.clamp(6, N - 7), y.clamp(6, N - 7));
            let tile = (x - cx).pow(2) + (y - cy).pow(2) <= 36;
            let front = inside(x, y, 12, 13, 26, 25);
            let back = inside(x, y, 6, 7, 20, 19) && !inside(x, y, 11, 12, 27, 26);
            let c = if !tile {
                [0, 0, 0, 0]
            } else if front || back {
                [255, 255, 255, 255]
            } else {
                [ACCENT.r(), ACCENT.g(), ACCENT.b(), 255]
            };
            rgba.extend_from_slice(&c);
        }
    }
    rgba
}

fn tray_image() -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(icon_rgba(), ICON_SIZE as u32, ICON_SIZE as u32).expect("valid icon")
}

/// Adds the notification-area icon: left click opens the window, the right
/// click menu opens it or quits the app.
fn create_tray(ctx: egui::Context, main: isize) -> Result<(), String> {
    let show = MenuItem::new("Open Window Manager", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[&show, &PredefinedMenuItem::separator(), &quit])
        .map_err(|e| e.to_string())?;
    let tray = TrayIconBuilder::new()
        .with_icon(tray_image())
        .with_tooltip("Window Manager")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| e.to_string())?;
    TRAY.with(|t| *t.borrow_mut() = Some(tray));

    let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
    let menu_ctx = ctx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == show_id {
            show_main(main);
            menu_ctx.request_repaint();
        } else if e.id == quit_id {
            TRAY.with(|t| t.borrow_mut().take());
            std::process::exit(0);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = e
        {
            show_main(main);
            ctx.request_repaint();
        }
    }));
    Ok(())
}

fn opacity_pct(alpha: u8) -> u32 {
    (alpha as f32 / 255.0 * 100.0).round() as u32
}

fn pct_to_alpha(pct: f32) -> u8 {
    (pct / 100.0 * 255.0).round() as u8
}

/// iOS-style toggle switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = ui.spacing().interact_size.y * egui::vec2(2.0, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool(response.id, *on);
        let radius = 0.5 * rect.height();
        let track = Color32::from_rgb(70, 76, 90).lerp_to_gamma(ACCENT, t);
        ui.painter().rect_filled(rect, radius, track);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        ui.painter()
            .circle_filled(egui::pos2(x, rect.center().y), radius * 0.78, Color32::WHITE);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Single-line text truncated with an ellipsis to fit `max_width`.
fn truncated(ui: &egui::Ui, text: &str, size: f32, color: Color32, max_width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id: egui::FontId::proportional(size),
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts(|f| f.layout_job(job))
}

fn badge(painter: &egui::Painter, ui: &egui::Ui, right: f32, y: f32, text: &str, color: Color32) -> f32 {
    let galley = truncated(ui, text, 11.0, color, f32::INFINITY);
    let pad = Vec2::new(7.0, 3.0);
    let size = galley.size() + pad * 2.0;
    let rect = egui::Rect::from_min_size(egui::pos2(right - size.x, y - size.y / 2.0), size);
    painter.rect_filled(rect, 8.0, color.gamma_multiply(0.18));
    painter.galley(rect.min + pad, galley, color);
    rect.left() - 6.0
}

/// The app logo: an accent tile with two overlapping windows, as in the tray.
fn paint_logo(painter: &egui::Painter, rect: egui::Rect) {
    let s = rect.width() / 32.0;
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| {
        egui::Rect::from_min_max(rect.min + Vec2::new(x0, y0) * s, rect.min + Vec2::new(x1, y1) * s)
    };
    painter.rect_filled(rect, 7.0 * s, ACCENT);
    painter.rect_filled(r(6.0, 7.0, 20.0, 19.0), 1.5 * s, Color32::WHITE);
    painter.rect_filled(r(10.5, 11.5, 27.5, 26.5), 2.0 * s, ACCENT);
    painter.rect_filled(r(12.0, 13.0, 26.0, 25.0), 1.5 * s, Color32::WHITE);
}

/// The window's own icon, or failing that a round tile with the app's
/// initial, tinted per app so rows are easy to tell apart at a glance.
fn paint_avatar(
    painter: &egui::Painter,
    ui: &egui::Ui,
    center: egui::Pos2,
    process: &str,
    icon: Option<&egui::TextureHandle>,
) {
    if let Some(icon) = icon {
        let rect = egui::Rect::from_center_size(center, Vec2::splat(26.0));
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(icon.id(), rect, uv, Color32::WHITE);
        return;
    }
    const TINTS: [Color32; 6] = [
        Color32::from_rgb(99, 140, 255),
        Color32::from_rgb(120, 210, 170),
        Color32::from_rgb(255, 196, 90),
        Color32::from_rgb(190, 160, 255),
        Color32::from_rgb(255, 140, 160),
        Color32::from_rgb(110, 200, 230),
    ];
    let name = process.strip_suffix(".exe").unwrap_or(process);
    let hash = name.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b.to_ascii_lowercase() as usize));
    let tint = TINTS[hash % TINTS.len()];
    let initial: String = name.chars().find(|c| c.is_alphanumeric()).unwrap_or('?').to_uppercase().collect();
    painter.circle_filled(center, 15.0, tint.gamma_multiply(0.2));
    let galley = truncated(ui, &initial, 14.0, tint, f32::INFINITY);
    painter.galley(center - galley.size() / 2.0, galley, tint);
}

/// A card's title row, with an optional value shown large on the right.
fn card_title(ui: &mut egui::Ui, title: &str, value: Option<String>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong().color(Color32::WHITE));
        if let Some(value) = value {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(value).size(17.0).color(ACCENT).strong());
            });
        }
    });
}

fn hint(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).color(MUTED).size(12.0));
}

fn setup_style(ctx: &egui::Context) {
    // egui follows the Windows light/dark setting by default, which would
    // swap in stock light widgets on top of this app's dark panels
    ctx.set_theme(egui::Theme::Dark);
    // Keeps winit from switching the title bar back when the setting changes
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(Color32::from_rgb(226, 230, 238));
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = CARD;
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.hyperlink_color = ACCENT;
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.noninteractive,
    ] {
        w.rounding = Rounding::same(8.0);
    }
    v.widgets.inactive.weak_bg_fill = CONTROL;
    v.widgets.inactive.bg_fill = CONTROL;
    v.widgets.hovered.weak_bg_fill = CONTROL_HOVER;
    v.widgets.hovered.bg_fill = CONTROL_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.6));
    v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.7);
    v.widgets.active.bg_fill = ACCENT.gamma_multiply(0.7);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, HOVER);
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    // Slider rail and handle
    v.widgets.inactive.fg_stroke.color = Color32::from_rgb(200, 205, 215);
    v.slider_trailing_fill = true;
    v.window_rounding = Rounding::same(10.0);
    // Both themes get the same look, so a theme change can't undo it
    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.slider_rail_height = 6.0;
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(11.5));
    });
}

struct App {
    /// This app's own top-level window.
    main: isize,
    names: HashMap<u32, String>,
    windows: Vec<WinInfo>,
    last_refresh: Instant,
    filter: String,
    selected: Option<WinInfo>,
    opacity: u8,
    topmost: bool,
    /// Width the selected window stopped at when asked to go narrower.
    width_floor: Option<i32>,
    /// Levels of pinned windows, top first; a level can hold several windows
    /// that share its rank. Shared with the order keeper thread, which holds
    /// the lock while restacking.
    order: Arc<Mutex<Vec<Vec<isize>>>>,
    status: String,
    /// eframe shows the window itself after the first frame, taskbar button
    /// and all, so it stays cloaked until it can be hidden on the next one
    /// and then reshown by `show_main` unless starting in the background.
    starting: bool,
    background: bool,
    update: Arc<Mutex<update::State>>,
    icons: icons::Icons,
}

impl App {
    fn new(cc: &eframe::CreationContext, background: bool) -> Self {
        setup_style(&cc.egui_ctx);
        let main = match cc.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        };
        set_hidden_owner(main);
        hide_on_minimize(main);
        set_cloak(main, true);
        // Before the tray adds its own hidden window
        set_screen_security();
        let tray = create_tray(cc.egui_ctx.clone(), main);
        let mut names = HashMap::new();
        let windows = list_windows(&mut names);
        let order = Arc::new(Mutex::new(Vec::new()));
        spawn_order_keeper(order.clone());
        let update = Arc::new(Mutex::new(update::State::Idle));
        update::spawn_checker(update.clone(), cc.egui_ctx.clone());
        let mut app = Self {
            main,
            names,
            windows,
            last_refresh: Instant::now(),
            filter: String::new(),
            selected: None,
            opacity: 255,
            topmost: false,
            width_floor: None,
            order,
            status: String::new(),
            starting: true,
            background,
            update,
            icons: icons::Icons::default(),
        };
        app.sync_order();
        if let Err(e) = tray {
            app.status = format!("Tray icon unavailable: {e}");
        }
        app
    }

    fn refresh(&mut self) {
        self.windows = list_windows(&mut self.names);
        self.last_refresh = Instant::now();
        self.icons.retain(is_alive);
        self.sync_order();
    }

    /// Drops windows that closed or were unpinned elsewhere, and ranks windows
    /// pinned elsewhere on top, keeping their current stacking among themselves.
    fn sync_order(&mut self) {
        let mut order = self.order.lock().unwrap();
        for l in order.iter_mut() {
            l.retain(|&h| is_alive(h) && is_topmost(h));
        }
        order.retain(|l| !l.is_empty());
        let new: Vec<Vec<isize>> = self
            .windows
            .iter()
            .map(|w| w.hwnd)
            .filter(|&h| is_topmost(h) && level_of(&order, h).is_none())
            .map(|h| vec![h])
            .collect();
        order.splice(0..0, new);
    }

    fn pin(&mut self, h: isize, on: bool) {
        let r = {
            let mut order = self.order.lock().unwrap();
            for l in order.iter_mut() {
                l.retain(|&x| x != h);
            }
            order.retain(|l| !l.is_empty());
            if on {
                order.insert(0, vec![h]);
            }
            set_topmost(h, on)
        };
        self.topmost = on;
        self.report(r);
    }

    /// Moves a pinned window half a step: a window sharing its level gets a
    /// level of its own just above or below, and a window alone on its level
    /// joins the next level up or down.
    fn shift(&mut self, h: isize, up: bool) {
        let mut order = self.order.lock().unwrap();
        let Some(i) = level_of(&order, h) else {
            return;
        };
        if order[i].len() > 1 {
            order[i].retain(|&x| x != h);
            order.insert(if up { i } else { i + 1 }, vec![h]);
        } else if up && i > 0 {
            order.remove(i);
            order[i - 1].push(h);
        } else if !up && i + 1 < order.len() {
            // The level below moves up into slot `i`
            order.remove(i);
            order[i].push(h);
        } else {
            return;
        }
        enforce_order(&order);
    }

    fn select(&mut self, w: WinInfo) {
        self.opacity = get_opacity(w.hwnd);
        self.topmost = is_topmost(w.hwnd);
        self.width_floor = None;
        self.status.clear();
        self.selected = Some(w);
    }

    fn report(&mut self, r: windows::core::Result<()>) {
        self.status = match r {
            Ok(()) => String::new(),
            // Windows of apps running as administrator can't be changed from here
            Err(e) if e.code() == windows::Win32::Foundation::E_ACCESSDENIED => format!(
                "{} This window belongs to an app running as administrator. Run Window Manager as administrator to change it.",
                e.message()
            ),
            Err(e) => e.message().to_string(),
        };
    }

    fn apply_opacity(&mut self, h: isize, alpha: u8) {
        self.opacity = alpha;
        let r = set_opacity(h, alpha);
        self.report(r);
    }

    fn window_list(&mut self, ui: &mut egui::Ui) {
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .id(egui::Id::new("search"))
                .hint_text("🔍  Search windows or apps   (Ctrl+F)")
                .desired_width(f32::INFINITY)
                .margin(Vec2::new(10.0, 8.0)),
        );
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            search.request_focus();
        }
        if search.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.filter.clear();
        }

        let filter = self.filter.to_lowercase();
        let selected = self.selected.as_ref().map(|s| s.hwnd);
        let order = self.order.lock().unwrap().clone();
        let pinned: usize = order.iter().map(Vec::len).sum();
        let copies = mirror::counts();
        let mut clicked = None;
        let matches: Vec<&WinInfo> = self
            .windows
            .iter()
            .filter(|w| {
                filter.is_empty()
                    || w.title.to_lowercase().contains(&filter)
                    || w.process.to_lowercase().contains(&filter)
            })
            .collect();

        ui.add_space(2.0);
        let count = if filter.is_empty() {
            format!("{} open windows", self.windows.len())
        } else {
            format!("{} of {} windows", matches.len(), self.windows.len())
        };
        ui.label(RichText::new(count).color(MUTED).size(12.0));
        let icons: Vec<_> = matches.iter().map(|w| self.icons.get(ui.ctx(), w.hwnd, w.pid)).collect();

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            // Room for the scroll bar, so it never covers the badges
            ui.set_max_width(ui.available_width() - 8.0);
            for (w, icon) in matches.iter().zip(&icons) {
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 50.0), Sense::click());
                let is_sel = selected == Some(w.hwnd);
                let painter = ui.painter_at(rect);
                let fill = if is_sel {
                    ACCENT.gamma_multiply(0.22)
                } else if resp.hovered() {
                    HOVER
                } else {
                    PANEL
                };
                painter.rect_filled(rect, 10.0, fill);
                if is_sel {
                    painter.rect_stroke(rect.shrink(0.5), 10.0, Stroke::new(1.0_f32, ACCENT));
                }

                let mut right = rect.right() - 12.0;
                let alpha = get_opacity(w.hwnd);
                if alpha < 255 {
                    right = badge(&painter, ui, right, rect.center().y, &format!("{}%", opacity_pct(alpha)), Color32::from_rgb(120, 210, 170));
                }
                if is_topmost(w.hwnd) {
                    let label = match level_of(&order, w.hwnd) {
                        Some(i) if pinned > 1 => format!("📌 #{}", i + 1),
                        _ => "📌 TOP".into(),
                    };
                    right = badge(&painter, ui, right, rect.center().y, &label, Color32::from_rgb(255, 196, 90));
                }
                if let Some(n) = copies.get(&w.hwnd) {
                    right = badge(&painter, ui, right, rect.center().y, &format!("🗐 {n}"), Color32::from_rgb(190, 160, 255));
                }

                paint_avatar(&painter, ui, egui::pos2(rect.left() + 27.0, rect.center().y), &w.process, icon.as_ref());
                let text_left = rect.left() + 52.0;
                let text_w = (right - text_left - 6.0).max(20.0);
                let title = truncated(ui, &w.title, 14.0, Color32::WHITE, text_w);
                let proc_ = truncated(ui, &w.process, 11.5, MUTED, text_w);
                painter.galley(egui::pos2(text_left, rect.top() + 8.0), title, Color32::WHITE);
                painter.galley(egui::pos2(text_left, rect.top() + 28.0), proc_, MUTED);

                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    clicked = Some((*w).clone());
                }
            }
            if matches.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("No matching windows").strong());
                    hint(ui, "Try a different search, or clear it with Esc.");
                });
            }
        });

        if let Some(w) = clicked {
            self.select(w);
        }
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let Some(w) = self.selected.clone() else {
            ui.add_space((ui.available_height() * 0.3).max(20.0));
            ui.vertical_centered(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
                paint_logo(ui.painter(), rect);
                ui.add_space(6.0);
                ui.label(RichText::new("No window selected").size(16.0).strong());
                hint(ui, "Pick a window from the list to change its\nopacity, width, stacking or make live copies.");
            });
            return;
        };

        // Errors go first so they can't end up scrolled out of view
        if !self.status.is_empty() {
            let mut dismiss = false;
            egui::Frame::none()
                .fill(DANGER.gamma_multiply(0.15))
                .rounding(10.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(format!("⚠  {}", self.status)).color(DANGER).size(12.5))
                                .wrap(),
                        );
                    });
                    dismiss = ui.small_button("Dismiss").clicked();
                });
            if dismiss {
                self.status.clear();
            }
            ui.add_space(8.0);
        }

        let card = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
            egui::Frame::none()
                .fill(CARD)
                .rounding(12.0)
                .inner_margin(14.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 6.0;
                    add(ui)
                });
            ui.add_space(8.0);
        };

        let icon = self.icons.get(ui.ctx(), w.hwnd, w.pid);
        let mut reset = false;
        card(ui, &mut |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                paint_avatar(ui.painter(), ui, rect.center(), &w.process, icon.as_ref());
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let title = truncated(ui, &w.title, 15.0, Color32::WHITE, ui.available_width());
                    ui.label(title).on_hover_text(&w.title);
                    hint(ui, &w.process);
                });
            });
            ui.horizontal(|ui| {
                let bw = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                if ui
                    .add_sized([bw, 30.0], egui::Button::new("↗  Bring to front"))
                    .clicked()
                {
                    bring_to_front(w.hwnd);
                }
                reset = ui
                    .add_sized([bw, 30.0], egui::Button::new("↺  Reset"))
                    .on_hover_text("Full opacity and not pinned on top")
                    .clicked();
            });
        });
        if reset {
            self.apply_opacity(w.hwnd, 255);
            if self.topmost {
                self.pin(w.hwnd, false);
            }
        }

        let mut new_alpha = None;
        let alpha = self.opacity;
        card(ui, &mut |ui| {
            card_title(ui, "Opacity", Some(format!("{}%", opacity_pct(alpha))));
            let mut pct = alpha as f32 / 255.0 * 100.0;
            ui.spacing_mut().slider_width = ui.available_width();
            if ui
                .add(egui::Slider::new(&mut pct, 10.0..=100.0).show_value(false))
                .changed()
            {
                new_alpha = Some(pct_to_alpha(pct));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let bw = (ui.available_width() - 18.0) / 4.0;
                for p in [25u32, 50, 75, 100] {
                    let active = opacity_pct(alpha) == p;
                    let btn = egui::Button::new(RichText::new(format!("{p}%")).size(13.0))
                        .min_size(Vec2::new(bw, 28.0))
                        .fill(if active { ACCENT.gamma_multiply(0.5) } else { CONTROL });
                    if ui.add(btn).clicked() {
                        new_alpha = Some(pct_to_alpha(p as f32));
                    }
                }
            });
        });
        if let Some(a) = new_alpha {
            self.apply_opacity(w.hwnd, a);
        }

        let width = window_size(w.hwnd).0;
        let screen = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) }.max(width);
        let floor = self.width_floor;
        let chrome = w.process.to_lowercase().contains("chrome");
        let mut new_width = None;
        card(ui, &mut |ui| {
            card_title(ui, "Width", Some(format!("{width} px")));
            hint(ui, "Resizes the window, keeping its left edge in place.");
            let mut px = width;
            ui.spacing_mut().slider_width = ui.available_width();
            if ui
                .add(egui::Slider::new(&mut px, MIN_WIDTH..=screen).show_value(false))
                .changed()
            {
                new_width = Some(px);
            }
            if let Some(f) = floor {
                let mut note = format!("This window won't go narrower than {f} px.");
                if chrome {
                    note += " Chrome app windows opened with chrome --app=<url> go down to about 180 px.";
                }
                hint(ui, note);
            }
        });
        if let Some(px) = new_width {
            match set_width(w.hwnd, px) {
                Ok(got) => {
                    self.status.clear();
                    if got > px {
                        self.width_floor = Some(got);
                    } else if floor.is_some_and(|f| px < f) {
                        // The app's minimum changed, e.g. after a reload
                        self.width_floor = None;
                    }
                }
                Err(e) => self.report(Err(e)),
            }
        }

        let mut top = self.topmost;
        let mut top_changed = false;
        let mut shift = None;
        let (rank, levels, pinned) = {
            let order = self.order.lock().unwrap();
            let rank = level_of(&order, w.hwnd).map(|i| (i, order[i].len() - 1));
            (rank, order.len(), order.iter().map(Vec::len).sum::<usize>())
        };
        card(ui, &mut |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Always on top").strong().color(Color32::WHITE));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    top_changed = toggle(ui, &mut top).changed();
                });
            });
            hint(ui, "Keeps the window above other windows.");
            if let (true, Some((i, others))) = (self.topmost && pinned > 1, rank) {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(format!("Stack order #{} of {levels}", i + 1)).size(13.0));
                        match others {
                            0 => {}
                            1 => hint(ui, "Shared with 1 window"),
                            n => hint(ui, format!("Shared with {n} windows")),
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (up_tip, down_tip) = if others > 0 {
                            (format!("Move above the others at #{}", i + 1), format!("Move below the others at #{}", i + 1))
                        } else {
                            (format!("Share #{}", i), format!("Share #{}", i + 2))
                        };
                        let arrow = |s: &str| egui::Button::new(s).min_size(Vec2::new(32.0, 28.0));
                        if ui.add_enabled(others > 0 || i + 1 < levels, arrow("⏷")).on_hover_text(down_tip).clicked() {
                            shift = Some(false);
                        }
                        if ui.add_enabled(others > 0 || i > 0, arrow("⏶")).on_hover_text(up_tip).clicked() {
                            shift = Some(true);
                        }
                    });
                });
            }
        });
        if top_changed {
            self.pin(w.hwnd, top);
        }
        if let Some(up) = shift {
            self.shift(w.hwnd, up);
        }

        let open = mirror::counts().get(&w.hwnd).copied().unwrap_or(0);
        let (mut new_copy, mut close_copies) = (false, false);
        card(ui, &mut |ui| {
            card_title(ui, "Live copies", (open > 0).then(|| format!("{open} open")));
            hint(ui, "Live pictures of the window that you can place anywhere.");
            ui.horizontal(|ui| {
                let bw = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                new_copy = ui.add_sized([bw, 30.0], egui::Button::new("🗐  New copy")).clicked();
                close_copies = ui
                    .add_enabled_ui(open > 0, |ui| ui.add_sized([bw, 30.0], egui::Button::new("Close all")))
                    .inner
                    .clicked();
            });
            hint(ui, "Drag a copy to move it. Drag its edges or scroll to resize. Right-click it to crop a region.");
        });
        if new_copy {
            let r = mirror::create(w.hwnd);
            self.report(r);
        }
        if close_copies {
            mirror::close_all(w.hwnd);
        }
    }
}

impl App {
    fn update_banner(&mut self, ctx: &egui::Context) {
        let state = self.update.lock().unwrap().clone();
        if state == update::State::Restart {
            TRAY.with(|t| t.borrow_mut().take());
            std::process::exit(0);
        }
        if state == update::State::Idle {
            return;
        }
        egui::TopBottomPanel::top("update")
            .frame(egui::Frame::none().fill(ACCENT.gamma_multiply(0.25)).inner_margin(egui::Margin::symmetric(16.0, 8.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| match &state {
                    update::State::Available { version, page, .. } => {
                        ui.label(RichText::new(format!("Version {version} is available")).strong());
                        ui.hyperlink_to("What's new", page);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Later").clicked() {
                                *self.update.lock().unwrap() = update::State::Idle;
                            }
                            if ui.button("⬇  Update now").clicked() {
                                update::install(self.update.clone(), ctx.clone());
                            }
                        });
                    }
                    update::State::Downloading { version } => {
                        ui.spinner();
                        ui.label(format!("Downloading version {version}…"));
                    }
                    update::State::Failed(e) => {
                        ui.label(RichText::new(e).color(DANGER));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Dismiss").clicked() {
                                *self.update.lock().unwrap() = update::State::Idle;
                            }
                        });
                    }
                    _ => {}
                });
            });
        if matches!(state, update::State::Downloading { .. }) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_refresh.elapsed() >= REFRESH_EVERY {
            self.refresh();
        }
        ctx.request_repaint_after(REFRESH_EVERY);

        if self.starting {
            if ctx.cumulative_pass_nr() == 0 {
                ctx.request_repaint();
            } else {
                self.starting = false;
                hide_main(self.main);
                if !self.background {
                    show_main(self.main);
                }
            }
        }

        // Closing only hides the window; Quit from the tray menu exits
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            hide_main(self.main);
        }

        if let Some(w) = &self.selected {
            if !is_alive(w.hwnd) {
                self.selected = None;
                self.status.clear();
                self.refresh();
            } else {
                // Pick up changes made outside this tool
                self.topmost = is_topmost(w.hwnd);
            }
        }

        egui::TopBottomPanel::top("header")
            .frame(egui::Frame::none().fill(PANEL).inner_margin(egui::Margin::symmetric(16.0, 10.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                    paint_logo(ui.painter(), rect);
                    ui.label(RichText::new("Window Manager").size(17.0).strong().color(Color32::WHITE));
                    ui.label(RichText::new(format!("v{}", update::VERSION)).color(MUTED).size(12.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("⟳  Refresh").on_hover_text("The list also refreshes by itself").clicked() {
                            self.refresh();
                        }
                    });
                });
            });

        self.update_banner(ctx);

        // The details take a bit over a third of the window, within limits
        let details_w = (ctx.screen_rect().width() * 0.42).clamp(290.0, 360.0);
        egui::SidePanel::right("details")
            .exact_width(details_w)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 6.0, right: 8.0, top: 14.0, bottom: 0.0 }))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.set_max_width(ui.available_width() - 6.0);
                    self.details(ui);
                    ui.add_space(6.0);
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 14.0, right: 6.0, top: 14.0, bottom: 0.0 }))
            .show(ctx, |ui| self.window_list(ui));
    }
}

/// The starting size: roomy, but never larger than the screen it opens on.
fn initial_size() -> [f32; 2] {
    let mut work = RECT::default();
    let scale = unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut _ as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        // Matches the work area: 96 until eframe makes the app DPI aware
        windows::Win32::UI::HiDpi::GetDpiForSystem().max(96) as f32 / 96.0
    };
    let (w, h) = ((work.right - work.left) as f32 / scale, (work.bottom - work.top) as f32 / scale);
    if w <= 0.0 || h <= 0.0 {
        return [860.0, 600.0];
    }
    [860f32.min(w * 0.9).max(MIN_SIZE[0]), 600f32.min(h * 0.9).max(MIN_SIZE[1])]
}

const MIN_SIZE: [f32; 2] = [600.0, 400.0];

fn main() {
    let background = std::env::args().any(|a| a == BACKGROUND_ARG);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Window Manager")
            .with_icon(egui::IconData { rgba: icon_rgba(), width: ICON_SIZE as u32, height: ICON_SIZE as u32 })
            .with_taskbar(false)
            .with_inner_size(initial_size())
            .with_min_inner_size(MIN_SIZE),
        centered: true,
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            // A tray app has no need to wake a laptop's discrete GPU
            power_preference: eframe::wgpu::PowerPreference::LowPower,
            ..Default::default()
        },
        ..Default::default()
    };
    let result =
        eframe::run_native("Window Manager", options, Box::new(move |cc| Ok(Box::new(App::new(cc, background)))));
    // There is no console, so say why instead of vanishing
    if let Err(e) = result {
        let text: Vec<u16> = format!("Window Manager couldn't start.\n\n{e}\0").encode_utf16().collect();
        unsafe {
            MessageBoxW(None, windows::core::PCWSTR(text.as_ptr()), w!("Window Manager"), MB_OK | MB_ICONERROR);
        }
    }
}
