//! Taskleisten-Symbol, App-Icon und Fenster ein-/ausblenden.

#[cfg_attr(not(windows), allow(unused_imports))]
use std::sync::{
    atomic::{AtomicIsize, Ordering},
    Arc,
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

fn line(px: &mut [u8], size: u32, a: (f32, f32), b: (f32, f32), thick: f32, col: [u8; 4]) {
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

/// Fensterhandle (nur Windows), um das Fenster auch dann ein-/auszublenden,
/// wenn egui gerade nicht zeichnet.
#[derive(Clone, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct WindowHandle(pub Arc<AtomicIsize>);

impl WindowHandle {
    pub fn set_from<W: raw_window_handle::HasWindowHandle>(&self, w: &W) {
        #[cfg(windows)]
        if let Ok(h) = w.window_handle() {
            if let raw_window_handle::RawWindowHandle::Win32(h) = h.as_raw() {
                self.0.store(h.hwnd.get(), Ordering::SeqCst);
            }
        }
        #[cfg(not(windows))]
        let _ = w;
    }

    pub fn show(&self, ctx: &egui::Context) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW};
            let hwnd = self.0.load(Ordering::SeqCst);
            if hwnd != 0 {
                unsafe {
                    ShowWindow(hwnd as _, SW_SHOW);
                    ShowWindow(hwnd as _, SW_RESTORE);
                    SetForegroundWindow(hwnd as _);
                }
            }
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    pub fn hide(&self, ctx: &egui::Context) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
            let hwnd = self.0.load(Ordering::SeqCst);
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
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: tray_icon::MouseButtonState::Up, .. } = e {
            w1.show(&c1);
        }
    }));
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == ids.open {
            win.show(&ctx);
        } else if e.id == ids.pause {
            on_pause();
            ctx.request_repaint();
        } else if e.id == ids.quit {
            on_quit();
        }
    }));
}
