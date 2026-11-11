// Unter Windows ohne Konsolenfenster starten.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
#[cfg(all(windows, feature = "cloud-files"))]
mod cloudfiles;
mod config;
mod folder_icon;
#[allow(dead_code)]
mod icons;
mod theme;
mod tray;
mod worker;

use std::{
    net::{TcpListener, TcpStream},
    sync::Arc,
};

/// Lokaler Port als „nur eine Instanz“-Sperre. Ein zweiter Start meldet sich
/// dort und holt damit das laufende Fenster nach vorne.
const INSTANCE_PORT: u16 = 47819;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let autostart = args.iter().any(|a| a == "--autostart");
    let share = args.iter().position(|a| a == "--share").and_then(|i| args.get(i + 1)).map(std::path::PathBuf::from);
    let listener = match TcpListener::bind(("127.0.0.1", INSTANCE_PORT)) {
        Ok(l) => l,
        Err(_) => {
            // Läuft schon: Fenster zeigen bzw. Freigabe weiterreichen.
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", INSTANCE_PORT)) {
                if let Some(p) = &share {
                    let _ = std::io::Write::write_all(&mut s, format!("SHARE {}\n", p.display()).as_bytes());
                }
            }
            return Ok(());
        }
    };

    let icon = egui::IconData { rgba: icons::logo_rgba(64), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        // Unter Windows ein rahmenloses Popup über der Taskleiste, das erst
        // beim Klick aufs Tray-Symbol erscheint.
        viewport: egui::ViewportBuilder::default()
            .with_title("DropCraft")
            .with_inner_size([380.0, 600.0])
            .with_resizable(false)
            .with_decorations(false)
            .with_taskbar(false)
            .with_always_on_top()
            .with_visible(cfg!(target_os = "linux"))
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "DropCraft",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, listener, autostart, share)))),
    )
}
