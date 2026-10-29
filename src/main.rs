#![windows_subsystem = "windows"]

use eframe::egui::{self, Color32, RichText, Rounding, Sense, Stroke, Vec2};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{BOOL, CloseHandle, COLORREF, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, DeleteObject, GetWindowRgnBox, SetWindowRgn, HRGN, NULLREGION, RGN_ERROR,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_CLOAK, DWMWA_CLOAKED};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PWSTR, w};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const ACCENT: Color32 = Color32::from_rgb(99, 140, 255);
const BG: Color32 = Color32::from_rgb(22, 24, 30);
const PANEL: Color32 = Color32::from_rgb(30, 33, 41);
const CARD: Color32 = Color32::from_rgb(38, 42, 52);
const HOVER: Color32 = Color32::from_rgb(46, 51, 63);
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

fn process_name(pid: u32) -> String {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return "unknown".into();
        };
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
        if !ok {
            return "unknown".into();
        }
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        path.rsplit('\\').next().unwrap_or(&path).to_string()
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

/// Like Signal's "Screen security": hide this app's own windows from
/// screenshots, recordings and screen sharing.
fn set_screen_security(on: bool) -> windows::core::Result<()> {
    let mut own = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(own_enum_proc), LPARAM(&mut own as *mut _ as isize));
    }
    let affinity = if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
    for h in own {
        unsafe { SetWindowDisplayAffinity(hwnd(h), affinity)? };
    }
    Ok(())
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

const MIN_CROP: i32 = 80;

fn window_width(h: isize) -> i32 {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd(h), &mut r);
    }
    r.right - r.left
}

/// Width of the strip left visible by `set_crop`, if the window is cropped.
fn get_crop(h: isize) -> Option<i32> {
    let mut r = RECT::default();
    let kind = unsafe { GetWindowRgnBox(hwnd(h), &mut r) };
    (kind != RGN_ERROR && kind != NULLREGION).then_some(r.right - r.left)
}

/// Apps such as Chrome refuse to shrink below their own minimum width, so a
/// window region hides everything right of `width` instead. `None` removes it.
fn set_crop(h: isize, width: Option<i32>) -> windows::core::Result<()> {
    unsafe {
        let rgn = match width {
            // Tall enough to survive the window growing taller
            Some(w) => CreateRectRgn(0, 0, w, 1 << 15),
            None => HRGN::default(),
        };
        // The system owns the region once it is set
        if SetWindowRgn(hwnd(h), rgn, TRUE) == 0 {
            let _ = DeleteObject(rgn);
            return Err(windows::core::Error::from_win32());
        }
    }
    Ok(())
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
    // Hiding the window can drop its capture exclusion
    let _ = set_screen_security(true);
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

/// A rounded accent tile with two overlapping white windows.
fn tray_image() -> tray_icon::Icon {
    const N: i32 = 32;
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
    tray_icon::Icon::from_rgba(rgba, N as u32, N as u32).expect("valid icon")
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

fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
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
    v.widgets.inactive.weak_bg_fill = CARD;
    v.widgets.inactive.bg_fill = CARD;
    v.widgets.hovered.weak_bg_fill = HOVER;
    v.widgets.hovered.bg_fill = HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.6));
    v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.7);
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 24.0;
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
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
}

impl App {
    fn new(cc: &eframe::CreationContext, background: bool) -> Self {
        setup_style(&cc.egui_ctx);
        let main = match cc.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        };
        set_hidden_owner(main);
        set_cloak(main, true);
        // Before the tray adds its own hidden window
        let security = set_screen_security(true);
        let tray = create_tray(cc.egui_ctx.clone(), main);
        let mut names = HashMap::new();
        let windows = list_windows(&mut names);
        let order = Arc::new(Mutex::new(Vec::new()));
        spawn_order_keeper(order.clone());
        let mut app = Self {
            main,
            names,
            windows,
            last_refresh: Instant::now(),
            filter: String::new(),
            selected: None,
            opacity: 255,
            topmost: false,
            order,
            status: String::new(),
            starting: true,
            background,
        };
        app.sync_order();
        app.report(security);
        if let Err(e) = tray {
            app.status = format!("Tray icon unavailable: {e}");
        }
        app
    }

    fn refresh(&mut self) {
        self.windows = list_windows(&mut self.names);
        self.last_refresh = Instant::now();
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
        self.status.clear();
        self.selected = Some(w);
    }

    fn report(&mut self, r: windows::core::Result<()>) {
        self.status = match r {
            Ok(()) => String::new(),
            Err(e) => format!("{} — try running as administrator.", e.message()),
        };
    }

    fn apply_opacity(&mut self, h: isize, alpha: u8) {
        self.opacity = alpha;
        let r = set_opacity(h, alpha);
        self.report(r);
    }

    fn window_list(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("🔍  Search windows or apps…")
                .desired_width(f32::INFINITY)
                .margin(Vec2::new(10.0, 8.0)),
        );
        ui.add_space(4.0);

        let filter = self.filter.to_lowercase();
        let selected = self.selected.as_ref().map(|s| s.hwnd);
        let order = self.order.lock().unwrap().clone();
        let pinned: usize = order.iter().map(Vec::len).sum();
        let mut clicked = None;
        let mut shown = 0;

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for w in &self.windows {
                if !filter.is_empty()
                    && !w.title.to_lowercase().contains(&filter)
                    && !w.process.to_lowercase().contains(&filter)
                {
                    continue;
                }
                shown += 1;
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::click());
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
                if let Some(px) = get_crop(w.hwnd) {
                    right = badge(&painter, ui, right, rect.center().y, &format!("✂ {px}px"), Color32::from_rgb(200, 150, 255));
                }
                if is_topmost(w.hwnd) {
                    let label = match level_of(&order, w.hwnd) {
                        Some(i) if pinned > 1 => format!("📌 #{}", i + 1),
                        _ => "📌 TOP".into(),
                    };
                    right = badge(&painter, ui, right, rect.center().y, &label, Color32::from_rgb(255, 196, 90));
                }

                let text_w = (right - rect.left() - 18.0).max(20.0);
                let title = truncated(ui, &w.title, 14.0, Color32::WHITE, text_w);
                let proc_ = truncated(ui, &w.process, 11.5, MUTED, text_w);
                painter.galley(rect.left_top() + Vec2::new(14.0, 8.0), title, Color32::WHITE);
                painter.galley(rect.left_top() + Vec2::new(14.0, 28.0), proc_, MUTED);

                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    clicked = Some(w.clone());
                }
            }
            if shown == 0 {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("No matching windows").color(MUTED)));
            }
        });

        if let Some(w) = clicked {
            self.select(w);
        }
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let Some(w) = self.selected.clone() else {
            ui.add_space(ui.available_height() * 0.35);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("🪟").size(40.0));
                ui.label(RichText::new("No window selected").size(16.0).strong());
                ui.label(RichText::new("Pick a window from the list\nto adjust it.").color(MUTED));
            });
            return;
        };

        let card = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
            egui::Frame::none()
                .fill(CARD)
                .rounding(12.0)
                .inner_margin(14.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    add(ui)
                });
        };

        card(ui, &mut |ui| {
            let width = ui.available_width();
            let title = truncated(ui, &w.title, 16.0, Color32::WHITE, width);
            ui.label(title);
            ui.label(RichText::new(&w.process).color(MUTED).size(12.0));
            ui.add_space(2.0);
            if ui.button("↗  Bring to front").clicked() {
                bring_to_front(w.hwnd);
            }
        });
        ui.add_space(6.0);

        let mut new_alpha = None;
        let alpha = self.opacity;
        card(ui, &mut |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Opacity").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("{}%", opacity_pct(alpha))).size(18.0).color(ACCENT).strong());
                });
            });
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
                        .fill(if active { ACCENT.gamma_multiply(0.5) } else { PANEL });
                    if ui.add(btn).clicked() {
                        new_alpha = Some(pct_to_alpha(p as f32));
                    }
                }
            });
        });
        if let Some(a) = new_alpha {
            self.apply_opacity(w.hwnd, a);
        }
        ui.add_space(6.0);

        let full = window_width(w.hwnd);
        let crop = get_crop(w.hwnd);
        let mut new_crop = None;
        card(ui, &mut |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new("Visible width").strong());
                    ui.label(RichText::new("Hide the right side, past the app's minimum").color(MUTED).size(12.0));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let shown = crop.unwrap_or(full).min(full);
                    ui.label(RichText::new(format!("{shown} px")).size(18.0).color(ACCENT).strong());
                });
            });
            let mut px = crop.unwrap_or(full).min(full);
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = ui.available_width() - 90.0;
                if ui
                    .add(egui::Slider::new(&mut px, MIN_CROP.min(full)..=full).show_value(false))
                    .changed()
                {
                    new_crop = Some((px < full).then_some(px));
                }
                if ui.add_enabled(crop.is_some(), egui::Button::new("Full width")).clicked() {
                    new_crop = Some(None);
                }
            });
        });
        if let Some(c) = new_crop {
            let r = set_crop(w.hwnd, c);
            self.report(r);
        }
        ui.add_space(6.0);

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
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new("Always on top").strong());
                    ui.label(RichText::new("Keep above other windows").color(MUTED).size(12.0));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    top_changed = toggle(ui, &mut top).changed();
                });
            });
            if let (true, Some((i, others))) = (self.topmost && pinned > 1, rank) {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let mut text = format!("Stack order #{} of {levels}", i + 1);
                    match others {
                        0 => {}
                        1 => text += ", shared with 1 window",
                        n => text += &format!(", shared with {n} windows"),
                    }
                    ui.label(RichText::new(text).color(MUTED).size(12.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (up_tip, down_tip) = if others > 0 {
                            (format!("Move above the others at #{}", i + 1), format!("Move below the others at #{}", i + 1))
                        } else {
                            (format!("Share #{}", i), format!("Share #{}", i + 2))
                        };
                        if ui.add_enabled(others > 0 || i + 1 < levels, egui::Button::new("⏷")).on_hover_text(down_tip).clicked() {
                            shift = Some(false);
                        }
                        if ui.add_enabled(others > 0 || i > 0, egui::Button::new("⏶")).on_hover_text(up_tip).clicked() {
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
        ui.add_space(6.0);

        if ui
            .add_sized([ui.available_width(), 32.0], egui::Button::new("↺  Reset window"))
            .clicked()
        {
            self.apply_opacity(w.hwnd, 255);
            if get_crop(w.hwnd).is_some() {
                let r = set_crop(w.hwnd, None);
                self.report(r);
            }
            if self.topmost {
                self.pin(w.hwnd, false);
            }
        }

        if !self.status.is_empty() {
            ui.add_space(6.0);
            egui::Frame::none()
                .fill(DANGER.gamma_multiply(0.15))
                .rounding(8.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(format!("⚠  {}", self.status)).color(DANGER).size(12.5));
                });
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
            .frame(egui::Frame::none().fill(PANEL).inner_margin(egui::Margin::symmetric(16.0, 12.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Window Manager").size(18.0).strong());
                    ui.label(RichText::new(format!("{} windows", self.windows.len())).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("⟳  Refresh").clicked() {
                            self.refresh();
                        }
                    });
                });
            });

        egui::SidePanel::right("details")
            .exact_width(300.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(BG).inner_margin(14.0))
            .show(ctx, |ui| self.details(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 14.0, right: 0.0, top: 14.0, bottom: 14.0 }))
            .show(ctx, |ui| self.window_list(ui));
    }
}

fn main() -> eframe::Result<()> {
    let background = std::env::args().any(|a| a == BACKGROUND_ARG);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Window Manager")
            .with_taskbar(false)
            .with_inner_size([760.0, 540.0])
            .with_min_inner_size([620.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native("Window Manager", options, Box::new(move |cc| Ok(Box::new(App::new(cc, background)))))
}
