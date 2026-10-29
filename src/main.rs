#![windows_subsystem = "windows"]

use eframe::egui::{self, Color32, RichText, Rounding, Sense, Stroke, Vec2};
use std::collections::HashMap;
use std::ffi::c_void;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{BOOL, CloseHandle, COLORREF, E_FAIL, HWND, LPARAM, TRUE};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PWSTR;

const ACCENT: Color32 = Color32::from_rgb(99, 140, 255);
const BG: Color32 = Color32::from_rgb(22, 24, 30);
const PANEL: Color32 = Color32::from_rgb(30, 33, 41);
const CARD: Color32 = Color32::from_rgb(38, 42, 52);
const HOVER: Color32 = Color32::from_rgb(46, 51, 63);
const MUTED: Color32 = Color32::from_rgb(140, 147, 163);
const DANGER: Color32 = Color32::from_rgb(255, 110, 110);
const REFRESH_EVERY: Duration = Duration::from_secs(2);

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

// Greys out Close in the system menu, which disables the X button and Alt+F4 on
// standard windows. Apps that draw their own title bar ignore it.
fn is_close_locked(h: isize) -> bool {
    unsafe {
        let menu = GetSystemMenu(hwnd(h), false);
        if menu.is_invalid() {
            return false;
        }
        let state = GetMenuState(menu, SC_CLOSE, MF_BYCOMMAND);
        state != u32::MAX && state & MF_GRAYED.0 != 0
    }
}

fn set_close_locked(h: isize, on: bool) -> windows::core::Result<()> {
    unsafe {
        let menu = GetSystemMenu(hwnd(h), false);
        if menu.is_invalid() {
            return Err(windows::core::Error::new(E_FAIL, "This window has no system menu"));
        }
        let flags = MF_BYCOMMAND | if on { MF_GRAYED } else { MF_ENABLED };
        if EnableMenuItem(menu, SC_CLOSE, flags).0 == -1 {
            return Err(windows::core::Error::new(E_FAIL, "This window has no Close command"));
        }
        // Repaint the caption so the X button reflects the new state.
        SetWindowPos(
            hwnd(h),
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )
    }
}

fn bring_to_front(h: isize) {
    unsafe {
        if IsIconic(hwnd(h)).as_bool() {
            let _ = ShowWindow(hwnd(h), SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd(h));
    }
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
    names: HashMap<u32, String>,
    windows: Vec<WinInfo>,
    last_refresh: Instant,
    filter: String,
    selected: Option<WinInfo>,
    opacity: u8,
    topmost: bool,
    close_locked: bool,
    status: String,
    screen_security: bool,
    screen_security_applied: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        setup_style(&cc.egui_ctx);
        let mut names = HashMap::new();
        let windows = list_windows(&mut names);
        Self {
            names,
            windows,
            last_refresh: Instant::now(),
            filter: String::new(),
            selected: None,
            opacity: 255,
            topmost: false,
            close_locked: false,
            status: String::new(),
            screen_security: true,
            screen_security_applied: false,
        }
    }

    fn refresh(&mut self) {
        self.windows = list_windows(&mut self.names);
        self.last_refresh = Instant::now();
    }

    fn select(&mut self, w: WinInfo) {
        self.opacity = get_opacity(w.hwnd);
        self.topmost = is_topmost(w.hwnd);
        self.close_locked = is_close_locked(w.hwnd);
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
                if is_topmost(w.hwnd) {
                    right = badge(&painter, ui, right, rect.center().y, "📌 TOP", Color32::from_rgb(255, 196, 90));
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

        let mut top = self.topmost;
        let mut top_changed = false;
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
        });
        if top_changed {
            self.topmost = top;
            let r = set_topmost(w.hwnd, top);
            self.report(r);
        }
        ui.add_space(6.0);

        let mut lock = self.close_locked;
        let mut lock_changed = false;
        card(ui, &mut |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new("Prevent close").strong());
                    ui.label(
                        RichText::new("Disable X and Alt+F4 (not for custom title bars)")
                            .color(MUTED)
                            .size(12.0),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    lock_changed = toggle(ui, &mut lock).changed();
                });
            });
        });
        if lock_changed {
            let r = set_close_locked(w.hwnd, lock);
            if r.is_ok() {
                self.close_locked = lock;
            }
            self.report(r);
        }
        ui.add_space(6.0);

        if ui
            .add_sized([ui.available_width(), 32.0], egui::Button::new("↺  Reset window"))
            .clicked()
        {
            self.apply_opacity(w.hwnd, 255);
            if self.topmost {
                self.topmost = false;
                let r = set_topmost(w.hwnd, false);
                self.report(r);
            }
            if self.close_locked {
                self.close_locked = false;
                let r = set_close_locked(w.hwnd, false);
                self.report(r);
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

        // Applied on the first frame since the native window must exist first
        if !self.screen_security_applied {
            self.screen_security_applied = true;
            let r = set_screen_security(self.screen_security);
            self.report(r);
        }

        if let Some(w) = &self.selected {
            if !is_alive(w.hwnd) {
                self.selected = None;
                self.status.clear();
                self.refresh();
            } else {
                // Pick up changes made outside this tool
                self.topmost = is_topmost(w.hwnd);
                self.close_locked = is_close_locked(w.hwnd);
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
                        ui.add_space(8.0);
                        let mut on = self.screen_security;
                        if toggle(ui, &mut on)
                            .on_hover_text("Hide this window from screenshots and screen sharing")
                            .changed()
                        {
                            self.screen_security = on;
                            let r = set_screen_security(on);
                            self.report(r);
                        }
                        ui.label(RichText::new("🔒 Screen security").color(MUTED));
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Window Manager")
            .with_inner_size([760.0, 540.0])
            .with_min_inner_size([620.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native("Window Manager", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
