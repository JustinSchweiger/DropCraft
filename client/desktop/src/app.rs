use std::{
    net::TcpListener,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc, Mutex,
    },
};

use dropcraft_core::{sync::human_size, Api, EventKind};
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::{
    config::{self, Config},
    theme::{self, *},
    tray,
    worker::{self, Cmd, Phase, Shared, SharedRef},
};

type LoginResult = Arc<Mutex<Option<Result<String, String>>>>;

struct SetupForm {
    server: String,
    username: String,
    password: String,
    device: String,
    sync_dir: PathBuf,
    autostart: bool,
    busy: bool,
    error: Option<String>,
    result: LoginResult,
}

impl SetupForm {
    fn from(cfg: Option<&Config>) -> Self {
        Self {
            server: cfg.map(|c| c.server.clone()).unwrap_or_else(|| "https://".into()),
            username: cfg.map(|c| c.username.clone()).unwrap_or_default(),
            password: String::new(),
            device: cfg.map(|c| c.device_name.clone()).unwrap_or_else(Config::default_device_name),
            sync_dir: cfg.map(|c| c.sync_dir.clone()).unwrap_or_else(Config::default_sync_dir),
            autostart: cfg.map(|c| c.autostart).unwrap_or(true),
            busy: false,
            error: None,
            result: Arc::new(Mutex::new(None)),
        }
    }
}

enum Screen {
    Setup(SetupForm),
    Main,
}

/// Wird mit dem Taskleisten-Menü geteilt.
#[derive(Clone)]
struct Control {
    cmd: Arc<Mutex<Option<Sender<Cmd>>>>,
    paused: Arc<AtomicBool>,
}

impl Control {
    fn send(&self, c: Cmd) {
        if let Some(tx) = self.cmd.lock().unwrap().as_ref() {
            let _ = tx.send(c);
        }
    }
    fn toggle_pause(&self) {
        let p = !self.paused.load(Ordering::SeqCst);
        self.paused.store(p, Ordering::SeqCst);
        self.send(Cmd::SetPaused(p));
        if let Some(mut cfg) = Config::load() {
            cfg.paused = p;
            let _ = cfg.save();
        }
    }
}

pub struct App {
    screen: Screen,
    cfg: Option<Config>,
    shared: SharedRef,
    control: Control,
    win: tray::WindowHandle,
    _tray: Option<tray::Tray>,
    tray_pause: Option<tray_icon::menu::MenuItem>,
    quitting: Arc<AtomicBool>,
    hide_on_first_frame: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, listener: TcpListener, autostart: bool) -> Self {
        theme::apply(&cc.egui_ctx);
        let win = tray::WindowHandle::default();
        win.set_from(cc);

        let cfg = Config::load();
        let control = Control { cmd: Arc::new(Mutex::new(None)), paused: Arc::new(AtomicBool::new(cfg.as_ref().map(|c| c.paused).unwrap_or(false))) };
        let quitting = Arc::new(AtomicBool::new(false));

        // Zweiter Start der App holt nur das vorhandene Fenster nach vorne.
        {
            let (win, ctx) = (win.clone(), cc.egui_ctx.clone());
            std::thread::spawn(move || {
                for _ in listener.incoming().flatten() {
                    win.show(&ctx);
                }
            });
        }

        // Unter Linux braucht das Tray-Icon eine GTK-Schleife; die App ist für
        // Windows gedacht, daher dort nur ohne Tray.
        let mut tray_obj = None;
        let mut tray_pause = None;
        if cfg!(not(target_os = "linux")) {
            if let Some((t, ids, pause_item)) = tray::create(if control.paused.load(Ordering::SeqCst) { "Fortsetzen" } else { "Pausieren" }) {
                let c = control.clone();
                let q = quitting.clone();
                tray::install_handlers(ids, win.clone(), cc.egui_ctx.clone(), move || c.toggle_pause(), move || {
                    q.store(true, Ordering::SeqCst);
                    std::process::exit(0);
                });
                tray_obj = Some(t);
                tray_pause = Some(pause_item);
            }
        }

        let shared: SharedRef = Arc::new(Mutex::new(Shared::default()));
        let screen = match &cfg {
            Some(c) if c.is_logged_in() => {
                *control.cmd.lock().unwrap() = Some(worker::spawn(c.clone(), shared.clone(), cc.egui_ctx.clone()));
                Screen::Main
            }
            other => Screen::Setup(SetupForm::from(other.as_ref())),
        };
        let logged_in = matches!(screen, Screen::Main);

        Self {
            screen,
            cfg,
            shared,
            control,
            win,
            _tray: tray_obj,
            tray_pause,
            quitting,
            hide_on_first_frame: autostart && logged_in,
        }
    }

    fn start_worker(&mut self, ctx: &egui::Context) {
        if let Some(cfg) = &self.cfg {
            *self.shared.lock().unwrap() = Shared::default();
            self.control.paused.store(cfg.paused, Ordering::SeqCst);
            *self.control.cmd.lock().unwrap() = Some(worker::spawn(cfg.clone(), self.shared.clone(), ctx.clone()));
        }
    }

    fn logout(&mut self) {
        self.control.send(Cmd::Stop);
        *self.control.cmd.lock().unwrap() = None;
        if let Some(cfg) = &mut self.cfg {
            if let Some(token) = cfg.token.take() {
                let server = cfg.server.clone();
                std::thread::spawn(move || {
                    if let Ok(api) = Api::new(&server, Some(token)) {
                        let _ = api.logout();
                    }
                });
            }
            let _ = cfg.save();
        }
        worker::reset_state();
        self.screen = Screen::Setup(SetupForm::from(self.cfg.as_ref()));
    }

    // ------------------------------------------------------------ Einrichtung

    fn setup_ui(&mut self, ui: &mut egui::Ui) {
        let Screen::Setup(form) = &mut self.screen else { return };

        // Ergebnis der Anmeldung abholen.
        let done = form.result.lock().unwrap().take();
        if let Some(res) = done {
            form.busy = false;
            match res {
                Ok(token) => {
                    let cfg = Config {
                        server: normalize_server(&form.server),
                        username: form.username.trim().to_string(),
                        device_name: form.device.trim().to_string(),
                        sync_dir: form.sync_dir.clone(),
                        token: Some(token),
                        paused: false,
                        autostart: form.autostart,
                    };
                    if let Err(e) = std::fs::create_dir_all(&cfg.sync_dir).and_then(|_| cfg.save()) {
                        form.error = Some(format!("Konnte nicht speichern: {e}"));
                    } else {
                        config::set_autostart(cfg.autostart);
                        self.cfg = Some(cfg);
                        self.screen = Screen::Main;
                        self.start_worker(ui.ctx());
                        return;
                    }
                }
                Err(e) => form.error = Some(e),
            }
        }

        header_band(ui, "MIT SERVER VERBINDEN");
        egui::Frame::new().inner_margin(16).show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
                ui.painter().rect_filled(rect, 8.0, BG);
                paint_cube(ui.painter(), rect.shrink(9.0), ACCENT);
                ui.vertical(|ui| {
                    ui.label(RichText::new("Willkommen bei DropCraft").size(17.0));
                    ui.label(RichText::new("Dein eigener Cloud-Speicher").color(TEXT_DIM));
                });
            });
            ui.add_space(12.0);

            egui::Grid::new("setup").num_columns(2).spacing([12.0, 10.0]).min_col_width(100.0).show(ui, |ui| {
                ui.label(RichText::new("Server").color(TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut form.server).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label(RichText::new("Benutzername").color(TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut form.username).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label(RichText::new("Passwort").color(TEXT_DIM));
                let pw = ui.add(egui::TextEdit::singleline(&mut form.password).password(true).desired_width(f32::INFINITY));
                if pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !form.busy {
                    start_login(form, ui.ctx());
                }
                ui.end_row();
                ui.label(RichText::new("Gerätename").color(TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut form.device).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label(RichText::new("Sync-Ordner").color(TEXT_DIM));
                ui.horizontal(|ui| {
                    if ui.button("Ändern …").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_directory(&form.sync_dir).pick_folder() {
                            form.sync_dir = dir;
                        }
                    }
                    ui.add(egui::Label::new(form.sync_dir.display().to_string()).truncate());
                });
                ui.end_row();
                if cfg!(windows) {
                    ui.label(RichText::new("Beim Start").color(TEXT_DIM));
                    ui.horizontal(|ui| {
                        toggle(ui, &mut form.autostart);
                        ui.label("Mit Windows starten");
                    });
                    ui.end_row();
                }
            });

            if let Some(e) = &form.error {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    paint_icon(ui, Icon::Warning, DANGER);
                    ui.label(RichText::new(e).color(TEXT));
                });
            }
        });

        footer_band(ui, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = !form.busy && !form.username.trim().is_empty() && !form.password.is_empty() && normalize_server(&form.server).len() > 8;
                let btn = egui::Button::new(theme::button_text(if form.busy { "Verbinde …" } else { "Verbinden" }).color(BG)).fill(ACCENT);
                if ui.add_enabled(ready, btn).clicked() {
                    start_login(form, ui.ctx());
                }
                if form.busy {
                    ui.spinner();
                }
            });
        });
    }

    // ------------------------------------------------------------ Hauptansicht

    fn main_ui(&mut self, ui: &mut egui::Ui) {
        let (phase, used, quota, last_sync, activity) = {
            let sh = self.shared.lock().unwrap();
            (sh.phase.clone(), sh.used, sh.quota, sh.last_sync, sh.activity.iter().take(50).cloned().collect::<Vec<_>>())
        };
        let paused = self.control.paused.load(Ordering::SeqCst);
        let mut do_logout = false;

        // Kopfbereich
        let head = egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                paint_cube(ui.painter(), rect, ACCENT);
                ui.label(RichText::new("DROPCRAFT").size(12.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if quota > 0 {
                        ui.label(RichText::new(format!("{} von {}", human_size(used), human_size(quota))).size(11.0).color(TEXT_DIM));
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let (icon, color, text) = match &phase {
                    Phase::Starting => (Icon::Sync, ACCENT, "Startet …".to_string()),
                    Phase::Syncing => (Icon::Sync, ACCENT, "Synchronisiere …".to_string()),
                    Phase::UpToDate => (
                        Icon::Check,
                        OK,
                        match last_sync {
                            Some(t) => format!("Alles synchronisiert · {}", t.format("%H:%M")),
                            None => "Alles synchronisiert".into(),
                        },
                    ),
                    Phase::Paused => (Icon::Pause, TEXT_DIM, "Pausiert".into()),
                    Phase::Offline(_) => (Icon::Offline, WARN, "Server nicht erreichbar".into()),
                    Phase::Error(e) => (Icon::Warning, DANGER, e.clone()),
                    Phase::NeedsLogin => (Icon::Warning, WARN, "Bitte neu anmelden".into()),
                };
                if matches!(phase, Phase::Syncing | Phase::Starting) {
                    ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                } else {
                    paint_icon(ui, icon, color);
                }
                let r = ui.add(egui::Label::new(RichText::new(text).color(TEXT)).truncate());
                if let Phase::Offline(m) = &phase {
                    r.on_hover_text(m);
                }
            });
            if quota > 0 {
                ui.add_space(6.0);
                let frac = (used as f32 / quota as f32).clamp(0.0, 1.0);
                let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), Sense::hover());
                ui.painter().rect_filled(rect, 2.0, RAISED);
                let mut fill = rect;
                fill.set_width(rect.width() * frac);
                ui.painter().rect_filled(fill, 2.0, if frac > 0.95 { DANGER } else { ACCENT });
            }
        });
        ui.painter().hline(ui.max_rect().x_range(), head.response.rect.bottom(), Stroke::new(1.0, BORDER));

        if phase == Phase::NeedsLogin {
            egui::Frame::new().inner_margin(16).show(ui, |ui| {
                ui.label("Deine Anmeldung ist abgelaufen oder wurde auf einem anderen Gerät beendet.");
                if ui.button(theme::button_text("Neu anmelden")).clicked() {
                    do_logout = true;
                }
            });
        }

        // Footer zuerst reservieren, damit die Liste den Rest füllt.
        let footer_h = 52.0;
        let list_h = (ui.available_height() - footer_h).max(60.0);

        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 4 }).show(ui, |ui| {
            ui.label(theme::label("Zuletzt"));
        });
        egui::ScrollArea::vertical().max_height(list_h - 34.0).auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 0)).show(ui, |ui| {
                for ev in &activity {
                    activity_row(ui, ev);
                }
            });
        });

        // Footer
        let footer_rect = Rect::from_min_max(Pos2::new(ui.max_rect().left(), ui.max_rect().bottom() - footer_h), ui.max_rect().max);
        ui.painter().rect_filled(footer_rect, 0.0, BG);
        ui.painter().hline(footer_rect.x_range(), footer_rect.top(), Stroke::new(1.0, BORDER));
        let mut footer_ui = ui.new_child(egui::UiBuilder::new().max_rect(footer_rect.shrink(8.0)).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let ui = &mut footer_ui;
        if ui.add(egui::Button::new(theme::button_text("Ordner öffnen")).min_size(Vec2::new(0.0, 36.0))).clicked() {
            if let Some(cfg) = &self.cfg {
                open_folder(&cfg.sync_dir);
            }
        }
        let pause_text = if paused { "Fortsetzen" } else { "Pausieren" };
        if ui.add(egui::Button::new(theme::button_text(pause_text)).min_size(Vec2::new(0.0, 36.0))).clicked() {
            self.control.toggle_pause();
        }
        if let Some(item) = &self.tray_pause {
            item.set_text(pause_text);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_button(theme::button_text("Konto"), |ui| {
                if let Some(cfg) = &mut self.cfg {
                    ui.label(RichText::new(format!("{} · {}", cfg.username, cfg.server.trim_start_matches("https://"))).color(TEXT_DIM));
                    ui.label(RichText::new(cfg.sync_dir.display().to_string()).color(TEXT_DIM).size(11.0));
                    ui.separator();
                    if ui.button("Jetzt synchronisieren").clicked() {
                        self.control.send(Cmd::SyncNow);
                        ui.close();
                    }
                    if cfg!(windows) {
                        ui.horizontal(|ui| {
                            if toggle(ui, &mut cfg.autostart).changed() {
                                config::set_autostart(cfg.autostart);
                                let _ = cfg.save();
                            }
                            ui.label("Mit Windows starten");
                        });
                    }
                    ui.separator();
                    if ui.button(RichText::new("Abmelden").color(DANGER)).clicked() {
                        do_logout = true;
                        ui.close();
                    }
                }
            });
        });

        if do_logout {
            self.logout();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.hide_on_first_frame {
            self.hide_on_first_frame = false;
            self.win.hide(&ctx);
        }
        // Schließen = in die Taskleiste minimieren.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting.load(Ordering::SeqCst) && cfg!(not(target_os = "linux")) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.win.hide(&ctx);
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(SURFACE)).show(ui, |ui| match self.screen {
            Screen::Setup(_) => self.setup_ui(ui),
            Screen::Main => self.main_ui(ui),
        });

        // Status regelmäßig auffrischen („vor x Min.“), auch ohne Ereignisse.
        ctx.request_repaint_after(std::time::Duration::from_secs(30));
    }
}

// ------------------------------------------------------------ Hilfsfunktionen

fn normalize_server(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("https://{s}")
    }
}

fn start_login(form: &mut SetupForm, ctx: &egui::Context) {
    form.busy = true;
    form.error = None;
    let (server, user, pw, dev) = (normalize_server(&form.server), form.username.trim().to_string(), form.password.clone(), form.device.trim().to_string());
    let result = form.result.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = match Api::login(&server, &user, &pw, &dev) {
            Ok(api) => Ok(api.token().unwrap_or_default().to_string()),
            Err(dropcraft_core::ApiError::Unauthorized) => Err("Benutzername oder Passwort falsch".to_string()),
            Err(e) => Err(e.to_string()),
        };
        *result.lock().unwrap() = Some(r);
        ctx.request_repaint();
    });
}

fn open_folder(p: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(p).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(p).spawn();
}

fn header_band(ui: &mut egui::Ui, title: &str) {
    let r = egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).size(12.0).strong().color(TEXT));
    });
    ui.painter().hline(ui.max_rect().x_range(), r.response.rect.bottom(), Stroke::new(1.0, BORDER));
}

fn footer_band(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let h = 48.0;
    let rect = Rect::from_min_max(Pos2::new(ui.max_rect().left(), ui.max_rect().bottom() - h), ui.max_rect().max);
    ui.painter().rect_filled(rect, 0.0, BG);
    ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, BORDER));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(12.0, 8.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    add(&mut child);
}

fn activity_row(ui: &mut egui::Ui, ev: &dropcraft_core::Event) {
    let (icon, color, verb) = match ev.kind {
        EventKind::Uploaded => (Icon::Up, ACCENT, "Hochgeladen"),
        EventKind::Downloaded => (Icon::Down, OK, "Heruntergeladen"),
        EventKind::DeletedLocal => (Icon::Trash, TEXT_DIM, "Hier gelöscht"),
        EventKind::DeletedRemote => (Icon::Trash, TEXT_DIM, "Gelöscht"),
        EventKind::Conflict => (Icon::Warning, WARN, "Konflikt"),
        EventKind::Error => (Icon::Warning, DANGER, "Fehler"),
    };
    let name = ev.path.rsplit('/').next().unwrap_or(&ev.path);
    let mut detail = format!("{verb} · {}", ev.at.format("%d.%m. %H:%M"));
    if !ev.detail.is_empty() {
        detail = format!("{verb} · {} · {}", ev.detail, ev.at.format("%d.%m. %H:%M"));
    }
    ui.horizontal(|ui| {
        ui.set_min_height(44.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
        ui.painter().rect_filled(rect, 4.0, RAISED);
        paint_icon_at(ui.painter(), rect.shrink(6.0), icon, color);
        ui.vertical(|ui| {
            ui.add(egui::Label::new(RichText::new(name).color(TEXT)).truncate()).on_hover_text(&ev.path);
            ui.add(egui::Label::new(RichText::new(detail).size(11.0).color(TEXT_DIM)).truncate());
        });
    });
}

/// Ein-/Aus-Schalter (statt Checkbox).
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = Vec2::new(34.0, 18.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let p = ui.painter();
    let track = if *on { ACCENT_DIM } else { RAISED };
    p.rect(rect, 9.0, track, Stroke::new(1.0, if *on { ACCENT_DIM } else { BORDER }), egui::StrokeKind::Inside);
    let x = egui::lerp(rect.left() + 9.0..=rect.right() - 9.0, t);
    p.circle_filled(Pos2::new(x, rect.center().y), 7.0, if *on { ACCENT } else { TEXT_DIM });
    resp
}

#[derive(Clone, Copy)]
enum Icon {
    Check,
    Sync,
    Pause,
    Warning,
    Offline,
    Up,
    Down,
    Trash,
}

fn paint_icon(ui: &mut egui::Ui, icon: Icon, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
    paint_icon_at(ui.painter(), rect, icon, color);
}

fn paint_icon_at(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    let s = Stroke::new(1.5, color);
    let at = |x: f32, y: f32| Pos2::new(r.left() + r.width() * x, r.top() + r.height() * y);
    match icon {
        Icon::Check => {
            p.circle_stroke(r.center(), r.width() * 0.42, s);
            p.line(vec![at(0.32, 0.52), at(0.45, 0.65), at(0.68, 0.38)], s);
        }
        Icon::Sync => {
            p.circle_stroke(r.center(), r.width() * 0.38, s);
            p.line(vec![at(0.72, 0.12), at(0.86, 0.3), at(0.66, 0.36)], s);
        }
        Icon::Pause => {
            p.line_segment([at(0.36, 0.22), at(0.36, 0.78)], Stroke::new(2.0, color));
            p.line_segment([at(0.64, 0.22), at(0.64, 0.78)], Stroke::new(2.0, color));
        }
        Icon::Warning => {
            p.add(egui::Shape::closed_line(vec![at(0.5, 0.1), at(0.94, 0.88), at(0.06, 0.88)], s));
            p.line_segment([at(0.5, 0.38), at(0.5, 0.6)], s);
            p.circle_filled(at(0.5, 0.74), 1.0, color);
        }
        Icon::Offline => {
            p.circle_stroke(r.center(), r.width() * 0.4, s);
            p.line_segment([at(0.2, 0.2), at(0.8, 0.8)], s);
        }
        Icon::Up => {
            p.line_segment([at(0.5, 0.12), at(0.5, 0.66)], s);
            p.line(vec![at(0.28, 0.34), at(0.5, 0.12), at(0.72, 0.34)], s);
            p.line(vec![at(0.12, 0.66), at(0.12, 0.88), at(0.88, 0.88), at(0.88, 0.66)], s);
        }
        Icon::Down => {
            p.line_segment([at(0.5, 0.12), at(0.5, 0.66)], s);
            p.line(vec![at(0.28, 0.44), at(0.5, 0.66), at(0.72, 0.44)], s);
            p.line(vec![at(0.12, 0.66), at(0.12, 0.88), at(0.88, 0.88), at(0.88, 0.66)], s);
        }
        Icon::Trash => {
            p.line_segment([at(0.12, 0.24), at(0.88, 0.24)], s);
            p.line(vec![at(0.38, 0.24), at(0.38, 0.1), at(0.62, 0.1), at(0.62, 0.24)], s);
            p.line(vec![at(0.24, 0.24), at(0.3, 0.9), at(0.7, 0.9), at(0.76, 0.24)], s);
        }
    }
}

/// Das Würfel-Logo, als Linien gezeichnet.
fn paint_cube(p: &egui::Painter, r: Rect, color: Color32) {
    let s = Stroke::new(1.6, color);
    let c = r.center();
    let h = r.height() * 0.46;
    let w = h * 0.866;
    let pt = |x: f32, y: f32| Pos2::new(c.x + x, c.y + y);
    let (top, ul, ur, mid, ll, lr, bot) = (pt(0.0, -h), pt(-w, -h / 2.0), pt(w, -h / 2.0), pt(0.0, 0.0), pt(-w, h / 2.0), pt(w, h / 2.0), pt(0.0, h));
    p.add(egui::Shape::closed_line(vec![top, ur, lr, bot, ll, ul], s));
    p.line(vec![ul, mid, ur], s);
    p.line_segment([mid, bot], s);
}
