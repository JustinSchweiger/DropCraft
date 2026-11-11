//! Taskleisten-Symbol, App-Icon und Fenster ein-/ausblenden.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicIsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

/// Das Würfel-Logo als RGBA-Bild, im Code gezeichnet.
pub fn logo_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let bg = [0x0b, 0x0e, 0x12, 0xff];
    let cyan = [0x19, 0xe3, 0xf0, 0xff];
    let r = s * 0.2;
    // Abgerundetes Quadrat als Hintergrund.
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = (r - fx).max(fx - (s - r)).max(0.0);
            let dy = (r - fy).max(fy - (s - r)).max(0.0);
            if dx * dx + dy * dy <= r * r {
                let i = ((y * size + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&bg);
            }
        }
    }
    // Isometrischer Würfel.
    let c = s / 2.0;
    let h = s * 0.34;
    let w = h * 0.866;
    let top = (c, c - h);
    let ul = (c - w, c - h / 2.0);
    let ur = (c + w, c - h / 2.0);
    let mid = (c, c);
    let ll = (c - w, c + h / 2.0);
    let lr = (c + w, c + h / 2.0);
    let bot = (c, c + h);
    let t = (s / 16.0).max(1.2);
    for (a, b) in [(top, ul), (top, ur), (ul, ll), (ur, lr), (ll, bot), (lr, bot), (ul, mid), (ur, mid), (mid, bot)] {
        line(&mut px, size, a, b, t, cyan);
    }
    px
}

pub(crate) fn line(px: &mut [u8], size: u32, a: (f32, f32), b: (f32, f32), thick: f32, col: [u8; 4]) {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let tt = (((fx - a.0) * vx + (fy - a.1) * vy) / len2).clamp(0.0, 1.0);
            let (qx, qy) = (a.0 + tt * vx - fx, a.1 + tt * vy - fy);
            let d = (qx * qx + qy * qy).sqrt();
            let cover = (thick / 2.0 + 0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                let i = ((y * size + x) * 4) as usize;
                for k in 0..3 {
                    px[i + k] = (px[i + k] as f32 * (1.0 - cover) + col[k] as f32 * cover) as u8;
                }
                px[i + 3] = px[i + 3].max((255.0 * cover) as u8);
            }
        }
    }
}

pub struct Tray {
    _icon: TrayIcon,
}

pub struct TrayIds {
    pub open: MenuId,
    pub pause: MenuId,
    pub quit: MenuId,
}

/// Fenster-Steuerung. Unter Windows direkt über die Win32-API, weil egui
/// bei verstecktem Fenster nicht zeichnet und Befehle sonst liegen blieben.
#[derive(Clone, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct WindowHandle {
    hwnd: Arc<AtomicIsize>,
    visible: Arc<AtomicBool>,
    last_hide: Arc<Mutex<Option<Instant>>>,
}

/// Position/Größe des Tray-Symbols in physischen Pixeln.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Anchor {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl WindowHandle {
    pub fn set_from<W: raw_window_handle::HasWindowHandle>(&self, w: &W) {
        #[cfg(windows)]
        if let Ok(h) = w.window_handle() {
            if let raw_window_handle::RawWindowHandle::Win32(h) = h.as_raw() {
                self.hwnd.store(h.hwnd.get(), Ordering::SeqCst);
                round_corners(h.hwnd.get());
            }
        }
        #[cfg(not(windows))]
        let _ = w;
    }

    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::SeqCst)
    }

    /// Wurde das Popup gerade erst durch Wegklicken geschlossen? Dann soll
    /// der Klick aufs Tray-Symbol es nicht sofort wieder öffnen.
    fn hidden_just_now(&self) -> bool {
        self.last_hide.lock().unwrap().map(|t| t.elapsed() < Duration::from_millis(400)).unwrap_or(false)
    }

    /// Klick aufs Tray-Symbol: Popup ein-/ausblenden.
    pub fn toggle(&self, ctx: &egui::Context, anchor: Option<Anchor>) {
        if self.is_visible() {
            self.hide(ctx);
        } else if !self.hidden_just_now() {
            self.show(ctx, anchor);
        }
    }

    /// Zeigt das Popup über dem Tray-Symbol (bzw. unten rechts).
    pub fn show(&self, ctx: &egui::Context, anchor: Option<Anchor>) {
        self.visible.store(true, Ordering::SeqCst);
        #[cfg(windows)]
        {
            let hwnd = self.hwnd.load(Ordering::SeqCst);
            if hwnd != 0 {
                show_popup_win32(hwnd, anchor);
                ctx.request_repaint();
                return;
            }
        }
        let _ = anchor;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    pub fn hide(&self, ctx: &egui::Context) {
        self.visible.store(false, Ordering::SeqCst);
        *self.last_hide.lock().unwrap() = Some(Instant::now());
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
            let hwnd = self.hwnd.load(Ordering::SeqCst);
            if hwnd != 0 {
                unsafe {
                    ShowWindow(hwnd as _, SW_HIDE);
                }
                return;
            }
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }
}

#[cfg(windows)]
fn show_popup_win32(hwnd: isize, anchor: Option<Anchor>) {
    use windows_sys::Win32::{
        Foundation::RECT,
        UI::WindowsAndMessaging::{
            GetWindowRect, SetForegroundWindow, SetWindowPos, SystemParametersInfoW, HWND_TOPMOST, SPI_GETWORKAREA, SWP_NOSIZE,
            SWP_SHOWWINDOW,
        },
    };
    unsafe {
        let mut wr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetWindowRect(hwnd as _, &mut wr);
        let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
        let mut work = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut RECT as *mut _, 0);
        let margin = 12;
        let (x, y) = popup_position((work.left, work.top, work.right, work.bottom), (w, h), anchor, margin);
        SetWindowPos(hwnd as _, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_SHOWWINDOW);
        SetForegroundWindow(hwnd as _);
    }
}

/// Berechnet die Popup-Position: über (oder unter) dem Tray-Symbol,
/// innerhalb des Arbeitsbereichs. Ohne Symbol: unten rechts.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn popup_position(work: (i32, i32, i32, i32), size: (i32, i32), anchor: Option<Anchor>, margin: i32) -> (i32, i32) {
    let (left, top, right, bottom) = work;
    let (w, h) = size;
    let clamp_x = |x: i32| x.max(left + margin).min((right - w - margin).max(left));
    match anchor {
        Some(a) => {
            let x = clamp_x(a.x + a.w / 2 - w / 2);
            // Taskleiste oben: Symbol liegt über dem Arbeitsbereich.
            let y = if a.y + a.h <= top + 4 { top + margin } else { bottom - h - margin };
            (x, y)
        }
        None => (clamp_x(right - w - margin), bottom - h - margin),
    }
}

#[cfg(windows)]
fn round_corners(hwnd: isize) {
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
    let pref = DWMWCP_ROUND;
    unsafe {
        // Ab Windows 11; ältere Versionen ignorieren das.
        DwmSetWindowAttribute(
            hwnd as _,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &pref as *const _ as *const _,
            std::mem::size_of_val(&pref) as u32,
        );
    }
}

pub fn create(pause_label: &str) -> Option<(Tray, TrayIds, MenuItem)> {
    let open = MenuItem::new("DropCraft öffnen", true, None);
    let pause = MenuItem::new(pause_label, true, None);
    let quit = MenuItem::new("Beenden", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &pause, &PredefinedMenuItem::separator(), &quit]).ok()?;
    let ids = TrayIds { open: open.id().clone(), pause: pause.id().clone(), quit: quit.id().clone() };
    let icon = tray_icon::Icon::from_rgba(logo_rgba(32), 32, 32).ok()?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("DropCraft")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .ok()?;
    Some((Tray { _icon: tray }, ids, pause))
}

/// Klick-Ereignisse verarbeiten, auch wenn das Fenster versteckt ist.
pub fn install_handlers(
    ids: TrayIds,
    win: WindowHandle,
    ctx: egui::Context,
    on_pause: impl Fn() + Send + Sync + 'static,
    on_quit: impl Fn() + Send + Sync + 'static,
) {
    let (w1, c1) = (win.clone(), ctx.clone());
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: tray_icon::MouseButtonState::Up, rect, .. } = e {
            let anchor = Anchor { x: rect.position.x as i32, y: rect.position.y as i32, w: rect.size.width as i32, h: rect.size.height as i32 };
            w1.toggle(&c1, Some(anchor));
        }
    }));
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == ids.open {
            win.show(&ctx, None);
        } else if e.id == ids.pause {
            on_pause();
            ctx.request_repaint();
        } else if e.id == ids.quit {
            on_quit();
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_above_tray_icon() {
        // 1920x1080, Taskleiste unten (Arbeitsbereich endet bei 1032)
        let work = (0, 0, 1920, 1032);
        let a = Anchor { x: 1700, y: 1040, w: 32, h: 32 };
        assert_eq!(popup_position(work, (380, 580), Some(a), 12), (1526, 440));
        // Symbol ganz rechts: Popup bleibt im Bildschirm
        let a = Anchor { x: 1900, y: 1040, w: 20, h: 32 };
        assert_eq!(popup_position(work, (380, 580), Some(a), 12).0, 1920 - 380 - 12);
        // Taskleiste oben
        let work = (0, 40, 1920, 1080);
        let a = Anchor { x: 1700, y: 4, w: 32, h: 32 };
        assert_eq!(popup_position(work, (380, 580), Some(a), 12).1, 52);
        // Ohne Symbol: unten rechts
        assert_eq!(popup_position((0, 0, 1920, 1032), (380, 580), None, 12), (1528, 440));
    }
}
