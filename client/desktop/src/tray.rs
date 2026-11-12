//! Taskleisten-Symbol, App-Icon und Fenster ein-/ausblenden.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicIsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use crate::icons::{disc, line, logo_rgba};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    MouseButton, TrayIconBuilder, TrayIconEvent,
};

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
        #[cfg(target_os = "macos")]
        if let Ok(h) = w.window_handle() {
            if let raw_window_handle::RawWindowHandle::AppKit(h) = h.as_raw() {
                self.hwnd.store(h.ns_view.as_ptr() as isize, Ordering::SeqCst);
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
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
        #[cfg(target_os = "macos")]
        {
            let view = self.hwnd.load(Ordering::SeqCst);
            if view != 0 {
                let ctx2 = ctx.clone();
                let near_mouse = anchor.is_some();
                mac::on_main(move || {
                    mac::show(view, near_mouse);
                    ctx2.request_repaint();
                });
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
        #[cfg(target_os = "macos")]
        {
            let view = self.hwnd.load(Ordering::SeqCst);
            if view != 0 {
                mac::on_main(move || mac::hide(view));
                return;
            }
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }
}

/// macOS: Fenster direkt über AppKit zeigen/verstecken (wie Win32 unter
/// Windows), Menüleisten-Symbol auf dem Hauptthread.
#[cfg(target_os = "macos")]
pub mod mac {
    use objc2::{rc::Retained, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEvent, NSScreen, NSView, NSWindow};
    use objc2_foundation::NSPoint;

    /// Führt `f` auf dem Hauptthread aus (sofort, wenn wir schon dort sind).
    pub fn on_main(f: impl FnOnce() + Send + 'static) {
        if MainThreadMarker::new().is_some() {
            f();
        } else {
            dispatch::Queue::main().exec_async(f);
        }
    }

    fn window(view: isize) -> Option<Retained<NSWindow>> {
        if view == 0 || MainThreadMarker::new().is_none() {
            return None;
        }
        // SAFETY: Zeiger stammt aus dem AppKit-Fensterhandle von winit und
        // lebt so lange wie das Fenster (die ganze Laufzeit der App).
        let view: &NSView = unsafe { &*(view as *const NSView) };
        view.window()
    }

    /// Kein Dock-Symbol, nur Menüleiste.
    pub fn accessory() {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }

    /// Popup unter der Menüleiste zeigen: unter dem Mauszeiger (Klick aufs
    /// Symbol) oder oben rechts.
    pub fn show(view: isize, near_mouse: bool) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let Some(win) = window(view) else { return };
        let mouse = NSEvent::mouseLocation();
        let inside = |s: &NSScreen| {
            let f = s.frame();
            mouse.x >= f.origin.x && mouse.x <= f.origin.x + f.size.width && mouse.y >= f.origin.y && mouse.y <= f.origin.y + f.size.height
        };
        let screen = NSScreen::screens(mtm).iter().find(|s| inside(s)).or_else(|| NSScreen::mainScreen(mtm));
        if let Some(s) = screen {
            let vf = s.visibleFrame();
            let w = win.frame().size.width;
            let (min_x, max_x) = (vf.origin.x + 8.0, vf.origin.x + vf.size.width - w - 8.0);
            let x = if near_mouse { (mouse.x - w / 2.0).clamp(min_x, max_x.max(min_x)) } else { max_x };
            win.setFrameTopLeftPoint(NSPoint::new(x, vf.origin.y + vf.size.height - 6.0));
        }
        let app = NSApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        win.makeKeyAndOrderFront(None);
    }

    pub fn hide(view: isize) {
        if let Some(win) = window(view) {
            win.orderOut(None);
        }
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

/// Zustand des Taskleisten-Symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayState {
    Idle,
    Syncing,
    Paused,
    Problem,
}

pub struct TrayStatus {
    pub state: TrayState,
    pub tooltip: String,
    pub pause_label: &'static str,
}

/// Taskleisten-Symbol: Logo plus Abzeichen unten rechts. Beim Synchronisieren
/// dreht sich ein Pfeil im Abzeichen (`frame` 0..8).
pub fn tray_rgba(state: TrayState, frame: u32) -> Vec<u8> {
    let size = 32u32;
    let mut px = logo_rgba(size);
    let s = size as f32;
    let c = (s * 0.74, s * 0.74);
    let r = s * 0.25;
    let ink = [0x0b, 0x0e, 0x12, 0xff];
    let t = s / 14.0;
    match state {
        TrayState::Idle => {}
        TrayState::Syncing => {
            disc(&mut px, size, c, r + 1.5, ink);
            disc(&mut px, size, c, r, [0x19, 0xe3, 0xf0, 0xff]);
            // Bogen mit Pfeilspitze, um frame*45° gedreht
            let rot = frame as f32 * std::f32::consts::FRAC_PI_4;
            let rr = r * 0.55;
            let steps = 10;
            let span = std::f32::consts::PI * 1.4;
            let pt = |a: f32| (c.0 + rr * (a + rot).cos(), c.1 + rr * (a + rot).sin());
            for k in 0..steps {
                let a0 = span * k as f32 / steps as f32;
                let a1 = span * (k + 1) as f32 / steps as f32;
                line(&mut px, size, pt(a0), pt(a1), t, ink);
            }
            let tip = pt(span);
            let back = pt(span - 0.6);
            let n = (tip.0 - back.0, tip.1 - back.1);
            line(&mut px, size, tip, (tip.0 - n.0 - n.1 * 0.8, tip.1 - n.1 + n.0 * 0.8), t, ink);
            line(&mut px, size, tip, (tip.0 - n.0 + n.1 * 0.8, tip.1 - n.1 - n.0 * 0.8), t, ink);
        }
        TrayState::Paused => {
            disc(&mut px, size, c, r + 1.5, ink);
            disc(&mut px, size, c, r, [0x95, 0xa2, 0xb0, 0xff]);
            line(&mut px, size, (c.0 - r * 0.3, c.1 - r * 0.45), (c.0 - r * 0.3, c.1 + r * 0.45), t * 1.2, ink);
            line(&mut px, size, (c.0 + r * 0.3, c.1 - r * 0.45), (c.0 + r * 0.3, c.1 + r * 0.45), t * 1.2, ink);
        }
        TrayState::Problem => {
            disc(&mut px, size, c, r + 1.5, ink);
            disc(&mut px, size, c, r, [0xfb, 0xbf, 0x24, 0xff]);
            line(&mut px, size, (c.0, c.1 - r * 0.55), (c.0, c.1 + r * 0.1), t * 1.2, ink);
            disc(&mut px, size, (c.0, c.1 + r * 0.45), t * 0.7, ink);
        }
    }
    px
}

/// Startet das Taskleisten-Symbol in einem eigenen Thread mit eigener
/// Nachrichtenschleife. So kann es sich drehen und reagieren, auch wenn das
/// Popup versteckt ist (dann läuft egui nicht).
#[cfg(not(target_os = "macos"))]
pub fn spawn(
    status: impl Fn() -> TrayStatus + Send + 'static,
    win: WindowHandle,
    ctx: egui::Context,
    on_pause: impl Fn() + Send + Sync + 'static,
    on_quit: impl Fn() + Send + Sync + 'static,
) {
    let _ = std::thread::Builder::new().name("tray".into()).spawn(move || {
        let open = MenuItem::new("DropCraft öffnen", true, None);
        let pause = MenuItem::new("Pausieren", true, None);
        let quit = MenuItem::new("Beenden", true, None);
        let menu = Menu::new();
        if menu.append_items(&[&open, &pause, &PredefinedMenuItem::separator(), &quit]).is_err() {
            return;
        }
        let ids = TrayIds { open: open.id().clone(), pause: pause.id().clone(), quit: quit.id().clone() };
        install_handlers(ids, win, ctx, on_pause, on_quit);

        let icon = |st: TrayState, f: u32| tray_icon::Icon::from_rgba(tray_rgba(st, f), 32, 32).ok();
        let Some(tray) = TrayIconBuilder::new()
            .with_tooltip("DropCraft")
            .with_icon(match icon(TrayState::Idle, 0) {
                Some(i) => i,
                None => return,
            })
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .ok()
        else {
            return;
        };

        let mut shown = (TrayState::Idle, 0u32);
        let mut frame = 0u32;
        let mut tooltip = String::new();
        let mut pause_label = "";
        let mut last = Instant::now() - Duration::from_secs(1);
        loop {
            pump_messages(100);
            if last.elapsed() < Duration::from_millis(125) {
                continue;
            }
            last = Instant::now();
            let st = status();
            let f = if st.state == TrayState::Syncing {
                frame = (frame + 1) % 8;
                frame
            } else {
                0
            };
            if (st.state, f) != shown {
                let _ = tray.set_icon(icon(st.state, f));
                shown = (st.state, f);
            }
            if st.tooltip != tooltip {
                let _ = tray.set_tooltip(Some(&st.tooltip));
                tooltip = st.tooltip;
            }
            if st.pause_label != pause_label {
                pause.set_text(st.pause_label);
                pause_label = st.pause_label;
            }
        }
    });
}

#[cfg(target_os = "macos")]
thread_local! {
    static MAC_TRAY: std::cell::RefCell<Option<(tray_icon::TrayIcon, MenuItem)>> = const { std::cell::RefCell::new(None) };
}

/// macOS: Das Menüleisten-Symbol muss auf dem Hauptthread entstehen (hier:
/// beim Start der App). Ein Hilfsthread berechnet den Zustand und schickt
/// Änderungen an den Hauptthread.
#[cfg(target_os = "macos")]
pub fn spawn(
    status: impl Fn() -> TrayStatus + Send + 'static,
    win: WindowHandle,
    ctx: egui::Context,
    on_pause: impl Fn() + Send + Sync + 'static,
    on_quit: impl Fn() + Send + Sync + 'static,
) {
    let open = MenuItem::new("DropCraft öffnen", true, None);
    let pause = MenuItem::new("Pausieren", true, None);
    let quit = MenuItem::new("Beenden", true, None);
    let menu = Menu::new();
    if menu.append_items(&[&open, &pause, &PredefinedMenuItem::separator(), &quit]).is_err() {
        return;
    }
    let ids = TrayIds { open: open.id().clone(), pause: pause.id().clone(), quit: quit.id().clone() };
    install_handlers(ids, win, ctx, on_pause, on_quit);
    let Some(icon) = tray_icon::Icon::from_rgba(tray_rgba(TrayState::Idle, 0), 32, 32).ok() else { return };
    let Ok(tray) = TrayIconBuilder::new()
        .with_tooltip("DropCraft")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
    else {
        return;
    };
    MAC_TRAY.with(|t| *t.borrow_mut() = Some((tray, pause)));

    let _ = std::thread::Builder::new().name("tray".into()).spawn(move || {
        let mut shown = (TrayState::Idle, 0u32);
        let mut frame = 0u32;
        let mut tooltip = String::new();
        let mut pause_label = "";
        loop {
            std::thread::sleep(Duration::from_millis(125));
            let st = status();
            let f = if st.state == TrayState::Syncing {
                frame = (frame + 1) % 8;
                frame
            } else {
                0
            };
            let new_icon = ((st.state, f) != shown).then(|| tray_rgba(st.state, f));
            shown = (st.state, f);
            let new_tip = (st.tooltip != tooltip).then(|| st.tooltip.clone());
            tooltip = st.tooltip;
            let new_label = (st.pause_label != pause_label).then_some(st.pause_label);
            pause_label = st.pause_label;
            if new_icon.is_none() && new_tip.is_none() && new_label.is_none() {
                continue;
            }
            dispatch::Queue::main().exec_async(move || {
                MAC_TRAY.with(|t| {
                    if let Some((tray, pause)) = &*t.borrow() {
                        if let Some(px) = new_icon {
                            let _ = tray.set_icon(tray_icon::Icon::from_rgba(px, 32, 32).ok());
                        }
                        if let Some(tip) = new_tip {
                            let _ = tray.set_tooltip(Some(&tip));
                        }
                        if let Some(l) = new_label {
                            pause.set_text(l);
                        }
                    }
                });
            });
        }
    });
}

struct TrayIds {
    open: MenuId,
    pause: MenuId,
    quit: MenuId,
}

/// Klick-Ereignisse verarbeiten, auch wenn das Fenster versteckt ist.
fn install_handlers(
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

#[cfg(windows)]
fn pump_messages(timeout_ms: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, QS_ALLINPUT,
    };
    unsafe {
        MsgWaitForMultipleObjects(0, std::ptr::null(), 0, timeout_ms, QS_ALLINPUT);
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn pump_messages(timeout_ms: u32) {
    std::thread::sleep(Duration::from_millis(timeout_ms as u64));
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

    #[test]
    fn tray_icons_render() {
        for st in [TrayState::Idle, TrayState::Syncing, TrayState::Paused, TrayState::Problem] {
            for f in 0..8 {
                let px = tray_rgba(st, f);
                assert_eq!(px.len(), 32 * 32 * 4);
                let _ = std::fs::write(std::env::temp_dir().join(format!("tray-{st:?}-{f}.rgba")), &px);
            }
        }
    }
}
