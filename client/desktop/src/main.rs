// Unter Windows ohne Konsolenfenster starten.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[macro_use]
mod i18n;
mod actions;
mod app;
#[cfg(all(windows, feature = "cloud-files"))]
mod cloudfiles;
mod config;
mod folder_icon;
#[allow(dead_code)]
mod icons;
mod installer;
mod notify;
#[cfg(windows)]
mod shellmenu;
mod theme;
mod tray;
mod updater;
mod worker;

use std::{
    net::{TcpListener, TcpStream},
    sync::Arc,
};

/// Lokaler Port als „nur eine Instanz“-Sperre. Ein zweiter Start meldet sich
/// dort und holt damit das laufende Fenster nach vorne.
pub(crate) const INSTANCE_PORT: u16 = 47819;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    // Sprache zuerst: auch Installer-Fragen und Meldungen folgen ihr.
    i18n::apply(&config::Config::load().language);
    // Für das macOS-Bundle: `DropCraft --export-iconset <Ordner>.iconset`
    if let Some(dir) = args.iter().position(|a| a == "--export-iconset").and_then(|i| args.get(i + 1)) {
        if let Err(e) = icons::export_iconset(std::path::Path::new(dir)) {
            eprintln!("{e}");
        }
        return Ok(());
    }
    // Für Web-App und iOS: `DropCraft --export-png <Datei> <Größe> [--full-bleed]`
    if let Some(i) = args.iter().position(|a| a == "--export-png") {
        let (Some(path), Some(size)) = (args.get(i + 1), args.get(i + 2).and_then(|s| s.parse::<u32>().ok())) else {
            eprintln!("--export-png <file> <size> [--full-bleed]");
            return Ok(());
        };
        let rgba = if args.iter().any(|a| a == "--full-bleed") { icons::logo_full_bleed_rgba(size) } else { icons::logo_rgba(size) };
        if let Err(e) = std::fs::write(path, icons::png_bytes(&rgba, size)) {
            eprintln!("{e}");
        }
        return Ok(());
    }
    // Aus dem Explorer-Menü: ohne Fenster erledigen und beenden.
    if let Some(i) = args.iter().position(|a| a == "--copy-link" || a == "--open-web") {
        if let Some(p) = args.get(i + 1) {
            notify::init();
            if args[i] == "--copy-link" {
                actions::copy_link(std::path::Path::new(p));
            } else {
                actions::open_web(std::path::Path::new(p));
            }
        }
        return Ok(());
    }
    // Installieren/Deinstallieren (Windows); fragt beim Start aus „Downloads“ o. Ä.
    if let installer::Next::Exit = installer::handle_args(&args) {
        return Ok(());
    }
    // Beim Windows-Start und nach einem (meist automatischen) Update nur ins
    // Tray, ohne Popup.
    let autostart = args.iter().any(|a| a == "--autostart" || a == "--after-update");
    let share = args.iter().position(|a| a == "--share").and_then(|i| args.get(i + 1)).map(std::path::PathBuf::from);
    // Nach einem Update: warten, bis die alte Instanz beendet ist.
    if args.iter().any(|a| a == "--after-update") {
        for _ in 0..50 {
            match TcpListener::bind(("127.0.0.1", INSTANCE_PORT)) {
                Ok(l) => {
                    drop(l);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(300)),
            }
        }
    }
    updater::cleanup();
    installer::on_start();
    notify::init();
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
        // Unter Windows ein rahmenloses Popup über der Taskleiste (unter
        // macOS unter der Menüleiste), das erst beim Klick aufs Symbol erscheint.
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
