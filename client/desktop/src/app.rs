use std::{
    collections::VecDeque,
    net::TcpListener,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use dropcraft_core::{sync::human_size, Api, ApiError, EventKind, Progress, UserInfo};
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::{
    config::{self, Account, Config},
    theme::{self, *},
    tray,
    worker::{self, Cmd, Phase, Shared, SharedRef},
};

// ================================================================ Laufzeit pro Konto

struct AccountRt {
    acc: Account,
    shared: SharedRef,
    cmd: Option<Sender<Cmd>>,
    paused: Arc<AtomicBool>,
    speed: SpeedMeter,
}

impl AccountRt {
    fn start(acc: Account, ctx: &egui::Context) -> Self {
        let shared: SharedRef = Arc::new(Mutex::new(Shared::default()));
        let cmd = acc.token.as_ref().map(|_| worker::spawn(acc.clone(), shared.clone(), ctx.clone()));
        let paused = Arc::new(AtomicBool::new(acc.paused));
        Self { acc, shared, cmd, paused, speed: SpeedMeter::default() }
    }

    fn send(&self, c: Cmd) {
        if let Some(tx) = &self.cmd {
            let _ = tx.send(c);
        }
    }

    fn stop(&mut self) {
        self.send(Cmd::Stop);
        self.cmd = None;
    }
}

/// Für das Taskleisten-Menü: alle Konten pausieren/fortsetzen.
#[derive(Clone, Default)]
struct Controls(Arc<Mutex<Vec<(Sender<Cmd>, Arc<AtomicBool>, String)>>>);

impl Controls {
    fn rebuild(&self, rts: &[AccountRt]) {
        *self.0.lock().unwrap() =
            rts.iter().filter_map(|r| r.cmd.clone().map(|tx| (tx, r.paused.clone(), r.acc.id.clone()))).collect();
    }

    fn any_running(&self) -> bool {
        self.0.lock().unwrap().iter().any(|(_, p, _)| !p.load(Ordering::SeqCst))
    }

    fn toggle_all(&self) {
        let pause = self.any_running();
        let list = self.0.lock().unwrap();
        for (tx, p, _) in list.iter() {
            p.store(pause, Ordering::SeqCst);
            let _ = tx.send(Cmd::SetPaused(pause));
        }
        let mut cfg = Config::load();
        for a in &mut cfg.accounts {
            if list.iter().any(|(_, _, id)| id == &a.id) {
                a.paused = pause;
            }
        }
        let _ = cfg.save();
    }
}

/// Übertragungsgeschwindigkeit über die letzten Sekunden.
#[derive(Default)]
struct SpeedMeter {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedMeter {
    fn update(&mut self, total: u64) -> f64 {
        let now = Instant::now();
        self.samples.push_back((now, total));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > Duration::from_secs(4) {
            self.samples.pop_front();
        }
        let (t0, b0) = self.samples[0];
        let dt = now.duration_since(t0).as_secs_f64();
        if dt < 0.5 {
            return 0.0;
        }
        (total.saturating_sub(b0)) as f64 / dt
    }
}

// ================================================================ Formulare

type Pending<T> = Arc<Mutex<Option<T>>>;

struct AccountForm {
    /// Bestehendes Konto neu anmelden (statt neues anlegen).
    replace: Option<String>,
    server: String,
    username: String,
    password: String,
    device: String,
    sync_dir: PathBuf,
    dir_touched: bool,
    first: bool,
    cancelable: bool,
    busy: bool,
    error: Option<String>,
    result: Pending<Result<String, String>>,
}

impl AccountForm {
    fn new(cfg: &Config) -> Self {
        let first = cfg.accounts.is_empty();
        Self {
            replace: None,
            server: "https://".into(),
            username: String::new(),
            password: String::new(),
            device: cfg.accounts.first().map(|a| a.device_name.clone()).unwrap_or_else(Config::default_device_name),
            sync_dir: Config::default_sync_dir("", first),
            dir_touched: false,
            first,
            cancelable: !first,
            busy: false,
            error: None,
            result: Arc::new(Mutex::new(None)),
        }
    }

    fn relogin(acc: &Account) -> Self {
        Self {
            replace: Some(acc.id.clone()),
            server: acc.server.clone(),
            username: acc.username.clone(),
            password: String::new(),
            device: acc.device_name.clone(),
            sync_dir: acc.sync_dir.clone(),
            dir_touched: true,
            first: false,
            cancelable: true,
            busy: false,
            error: None,
            result: Arc::new(Mutex::new(None)),
        }
    }
}

enum AdminMsg {
    Users(Vec<UserInfo>),
    Done(String),
    Failed(String),
}

struct AdminView {
    account_id: String,
    users: Vec<UserInfo>,
    loading: bool,
    info: Option<(bool, String)>,
    new_name: String,
    new_pw: String,
    new_gb: String,
    edit: Option<(i64, String, String)>,
    confirm_delete: Option<i64>,
    inbox: Pending<AdminMsg>,
}

enum Screen {
    Main,
    Form(AccountForm),
    Admin(AdminView),
}

// ================================================================ App

pub struct App {
    cfg: Config,
    rts: Vec<AccountRt>,
    selected: usize,
    screen: Screen,
    controls: Controls,
    win: tray::WindowHandle,
    _tray: Option<tray::Tray>,
    tray_pause: Option<tray_icon::menu::MenuItem>,
    had_focus: bool,
    confirm_remove: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, listener: TcpListener, autostart: bool) -> Self {
        theme::apply(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let win = tray::WindowHandle::default();
        win.set_from(cc);

        let cfg = Config::load();
        let rts: Vec<AccountRt> = cfg.accounts.iter().map(|a| AccountRt::start(a.clone(), &ctx)).collect();
        let controls = Controls::default();
        controls.rebuild(&rts);

        // Zweiter Start der App öffnet nur das Popup der laufenden.
        {
            let (win, ctx) = (win.clone(), ctx.clone());
            std::thread::spawn(move || {
                for _ in listener.incoming().flatten() {
                    win.show(&ctx, None);
                }
            });
        }

        // Unter Linux braucht das Tray-Icon eine GTK-Schleife; die App ist
        // für Windows gedacht, dort läuft alles über das Taskleisten-Symbol.
        let mut tray_obj = None;
        let mut tray_pause = None;
        if cfg!(not(target_os = "linux")) {
            if let Some((t, ids, pause_item)) = tray::create("Pausieren") {
                let c = controls.clone();
                tray::install_handlers(ids, win.clone(), ctx.clone(), move || c.toggle_all(), || std::process::exit(0));
                tray_obj = Some(t);
                tray_pause = Some(pause_item);
            }
        }

        let screen = if cfg.accounts.is_empty() { Screen::Form(AccountForm::new(&cfg)) } else { Screen::Main };
        if !autostart || cfg.accounts.is_empty() {
            win.show(&ctx, None);
        }

        Self {
            cfg,
            rts,
            selected: 0,
            screen,
            controls,
            win,
            _tray: tray_obj,
            tray_pause,
            had_focus: false,
            confirm_remove: false,
        }
    }

    fn save(&mut self) {
        for (a, rt) in self.cfg.accounts.iter_mut().zip(&self.rts) {
            a.paused = rt.paused.load(Ordering::SeqCst);
        }
        let _ = self.cfg.save();
    }

    fn remove_account(&mut self, idx: usize) {
        if idx >= self.rts.len() {
            return;
        }
        let mut rt = self.rts.remove(idx);
        rt.stop();
        if let Some(token) = rt.acc.token.clone() {
            let server = rt.acc.server.clone();
            std::thread::spawn(move || {
                if let Ok(api) = Api::new(&server, Some(token)) {
                    let _ = api.logout();
                }
            });
        }
        let _ = std::fs::remove_dir_all(rt.acc.state_dir());
        self.cfg.accounts.retain(|a| a.id != rt.acc.id);
        self.save();
        self.controls.rebuild(&self.rts);
        self.selected = self.selected.min(self.rts.len().saturating_sub(1));
        if self.rts.is_empty() {
            self.screen = Screen::Form(AccountForm::new(&self.cfg));
        }
    }

    fn open_admin(&mut self, ctx: &egui::Context) {
        let Some(rt) = self.rts.get(self.selected) else { return };
        let mut view = AdminView {
            account_id: rt.acc.id.clone(),
            users: Vec::new(),
            loading: false,
            info: None,
            new_name: String::new(),
            new_pw: String::new(),
            new_gb: "50".into(),
            edit: None,
            confirm_delete: None,
            inbox: Arc::new(Mutex::new(None)),
        };
        admin_reload(&mut view, &rt.acc, ctx);
        self.screen = Screen::Admin(view);
    }

    // ------------------------------------------------------------ Konto-Formular

    fn form_ui(&mut self, ui: &mut egui::Ui) {
        let Screen::Form(form) = &mut self.screen else { return };

        let done = form.result.lock().unwrap().take();
        if let Some(res) = done {
            form.busy = false;
            match res {
                Ok(token) => {
                    let acc = Account {
                        id: form.replace.clone().unwrap_or_else(config::new_account_id),
                        server: normalize_server(&form.server),
                        username: form.username.trim().to_string(),
                        device_name: form.device.trim().to_string(),
                        sync_dir: form.sync_dir.clone(),
                        token: Some(token),
                        paused: false,
                    };
                    if let Err(e) = std::fs::create_dir_all(&acc.sync_dir) {
                        form.error = Some(format!("Ordner konnte nicht angelegt werden: {e}"));
                        return;
                    }
                    let ctx = ui.ctx().clone();
                    if let Some(i) = self.rts.iter().position(|r| r.acc.id == acc.id) {
                        self.rts[i].stop();
                        self.rts[i] = AccountRt::start(acc.clone(), &ctx);
                        self.cfg.accounts[i] = acc;
                        self.selected = i;
                    } else {
                        if self.cfg.accounts.is_empty() {
                            config::set_autostart(self.cfg.autostart);
                        }
                        self.rts.push(AccountRt::start(acc.clone(), &ctx));
                        self.cfg.accounts.push(acc);
                        self.selected = self.rts.len() - 1;
                    }
                    self.save();
                    self.controls.rebuild(&self.rts);
                    self.screen = Screen::Main;
                    return;
                }
                Err(e) => form.error = Some(e),
            }
        }

        let title = if form.replace.is_some() { "NEU ANMELDEN" } else if form.first { "MIT SERVER VERBINDEN" } else { "KONTO HINZUFÜGEN" };
        header_band(ui, title);
        let mut cancel = false;
        let mut submit = false;
        egui::Frame::new().inner_margin(16).show(ui, |ui| {
            if form.first {
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
            }
            let locked = form.replace.is_some();
            egui::Grid::new("form").num_columns(2).spacing([12.0, 10.0]).min_col_width(100.0).show(ui, |ui| {
                ui.label(RichText::new("Server").color(TEXT_DIM));
                ui.add_enabled(!locked, egui::TextEdit::singleline(&mut form.server).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label(RichText::new("Benutzername").color(TEXT_DIM));
                let r = ui.add_enabled(!locked, egui::TextEdit::singleline(&mut form.username).desired_width(f32::INFINITY));
                if r.changed() && !form.dir_touched {
                    form.sync_dir = Config::default_sync_dir(&form.username, form.first);
                }
                ui.end_row();
                ui.label(RichText::new("Passwort").color(TEXT_DIM));
                let pw = ui.add(egui::TextEdit::singleline(&mut form.password).password(true).desired_width(f32::INFINITY));
                if pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.end_row();
                ui.label(RichText::new("Gerätename").color(TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut form.device).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label(RichText::new("Sync-Ordner").color(TEXT_DIM));
                ui.horizontal(|ui| {
                    if ui.add_enabled(!locked, egui::Button::new("Ändern …")).clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_directory(form.sync_dir.parent().unwrap_or(&form.sync_dir)).pick_folder() {
                            form.sync_dir = dir;
                            form.dir_touched = true;
                        }
                    }
                    ui.add(egui::Label::new(form.sync_dir.display().to_string()).truncate());
                });
                ui.end_row();
                if form.first && cfg!(windows) {
                    ui.label(RichText::new("Beim Start").color(TEXT_DIM));
                    ui.horizontal(|ui| {
                        toggle(ui, &mut self.cfg.autostart);
                        ui.label("Mit Windows starten");
                    });
                    ui.end_row();
                }
            });
            if let Some(e) = &form.error {
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    paint_icon(ui, Icon::Warning, DANGER);
                    ui.label(RichText::new(e).color(TEXT));
                });
            }
        });

        footer_band(ui, 48.0, |ui| {
            if form.cancelable && ui.button(theme::button_text("Abbrechen")).clicked() {
                cancel = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = !form.busy && !form.username.trim().is_empty() && !form.password.is_empty() && normalize_server(&form.server).len() > 8;
                let btn = egui::Button::new(theme::button_text(if form.busy { "Verbinde …" } else { "Verbinden" }).color(BG)).fill(ACCENT);
                if ui.add_enabled(ready, btn).clicked() {
                    submit = true;
                }
                if form.busy {
                    ui.spinner();
                }
            });
        });

        if submit && !form.busy && !form.password.is_empty() {
            // Prüfungen, bevor wir den Server fragen.
            let server = normalize_server(&form.server);
            let user = form.username.trim().to_string();
            if let Some(other) = self.cfg.overlapping_account(&form.sync_dir, form.replace.as_deref()) {
                form.error = Some(format!("Der Ordner überschneidet sich mit dem Ordner von „{}“. Bitte einen eigenen Ordner wählen.", other.label()));
            } else if form.replace.is_none()
                && self.cfg.accounts.iter().any(|a| a.server == server && a.username.eq_ignore_ascii_case(&user))
            {
                form.error = Some("Dieses Konto ist schon eingerichtet.".into());
            } else {
                start_login(form, ui.ctx());
            }
        }
        if cancel {
            self.screen = Screen::Main;
        }
    }

    // ------------------------------------------------------------ Benutzerverwaltung

    fn admin_ui(&mut self, ui: &mut egui::Ui) {
        let Screen::Admin(view) = &mut self.screen else { return };
        let Some(rt) = self.rts.iter().find(|r| r.acc.id == view.account_id) else {
            self.screen = Screen::Main;
            return;
        };
        let acc = rt.acc.clone();
        let ctx = ui.ctx().clone();

        let msg = view.inbox.lock().unwrap().take();
        if let Some(msg) = msg {
            view.loading = false;
            match msg {
                AdminMsg::Users(u) => view.users = u,
                AdminMsg::Done(m) => {
                    view.info = Some((true, m));
                    admin_reload(view, &acc, &ctx);
                }
                AdminMsg::Failed(m) => view.info = Some((false, m)),
            }
        }

        header_band(ui, &format!("BENUTZER · {}", acc.host().to_uppercase()));
        let mut back = false;
        let footer_h = 48.0;
        let body_h = ui.available_height() - footer_h;
        egui::ScrollArea::vertical().max_height(body_h).auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(16).show(ui, |ui| {
                ui.label(theme::label("Neuer Benutzer"));
                ui.add_space(4.0);
                egui::Grid::new("new_user").num_columns(2).spacing([12.0, 8.0]).min_col_width(90.0).show(ui, |ui| {
                    ui.label(RichText::new("Name").color(TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut view.new_name).desired_width(f32::INFINITY));
                    ui.end_row();
                    ui.label(RichText::new("Passwort").color(TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut view.new_pw).password(true).hint_text("mind. 10 Zeichen").desired_width(f32::INFINITY));
                    ui.end_row();
                    ui.label(RichText::new("Speicher").color(TEXT_DIM));
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut view.new_gb).desired_width(60.0));
                        ui.label("GB");
                    });
                    ui.end_row();
                });
                ui.add_space(6.0);
                let gb = parse_gb(&view.new_gb);
                let valid = view.new_name.trim().len() >= 3 && view.new_pw.len() >= 10 && gb.is_some() && !view.loading;
                if ui.add_enabled(valid, egui::Button::new(theme::button_text("Anlegen").color(BG)).fill(ACCENT)).clicked() {
                    let (name, pw, gb) = (view.new_name.trim().to_string(), view.new_pw.clone(), gb.unwrap_or(0.0));
                    view.new_name.clear();
                    view.new_pw.clear();
                    admin_run(view, &acc, &ctx, move |api| {
                        api.create_user(&name, &pw, gb, false).map(|_| format!("„{name}“ angelegt ({} GB)", fmt_gb(gb)))
                    });
                }

                if let Some((ok, m)) = &view.info {
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        if *ok {
                            paint_icon(ui, Icon::Check, OK);
                        } else {
                            paint_icon(ui, Icon::Warning, DANGER);
                        }
                        ui.label(m);
                    });
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.label(theme::label("Alle Benutzer"));
                    if view.loading {
                        ui.spinner();
                    }
                });
                ui.add_space(4.0);
                let users = view.users.clone();
                for u in &users {
                    user_row(ui, u, view, &acc, &ctx);
                }
            });
        });

        footer_band(ui, footer_h, |ui| {
            if ui.button(theme::button_text("Zurück")).clicked() {
                back = true;
            }
        });
        if back {
            self.screen = Screen::Main;
        }
    }

    // ------------------------------------------------------------ Hauptansicht

    fn main_ui(&mut self, ui: &mut egui::Ui) {
        if self.rts.is_empty() {
            self.screen = Screen::Form(AccountForm::new(&self.cfg));
            return;
        }
        self.selected = self.selected.min(self.rts.len() - 1);
        let ctx = ui.ctx().clone();

        // Kopf mit Konto-Reitern
        let head = egui::Frame::new().fill(BG).inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 10 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                paint_cube(ui.painter(), rect, ACCENT);
                ui.label(RichText::new("DROPCRAFT").size(12.0).strong());
            });
            if self.rts.len() > 1 {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for i in 0..self.rts.len() {
                        let phase = self.rts[i].shared.lock().unwrap().phase.clone();
                        let paused = self.rts[i].paused.load(Ordering::SeqCst);
                        let color = phase_color(&phase, paused);
                        let sel = i == self.selected;
                        let resp = account_tab(ui, &self.rts[i].acc.username, color, sel);
                        if resp.clicked() {
                            self.selected = i;
                            self.confirm_remove = false;
                        }
                        resp.on_hover_text(self.rts[i].acc.label());
                    }
                });
            }
        });
        ui.painter().hline(ui.max_rect().x_range(), head.response.rect.bottom(), Stroke::new(1.0, BORDER));

        let rt = &mut self.rts[self.selected];
        let (phase, used, quota, last_sync, is_admin, activity, progress) = {
            let sh = rt.shared.lock().unwrap();
            (
                sh.phase.clone(),
                sh.used,
                sh.quota,
                sh.last_sync,
                sh.is_admin,
                sh.activity.iter().take(50).cloned().collect::<Vec<_>>(),
                sh.progress.as_ref().map(|p| p.lock().unwrap().clone()),
            )
        };
        // Ereignisse des laufenden Durchgangs oben anzeigen.
        let activity: Vec<_> = progress
            .as_ref()
            .map(|p| p.events.iter().rev().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .chain(activity)
            .take(50)
            .collect();
        let paused = rt.paused.load(Ordering::SeqCst);
        let speed = progress.as_ref().map(|p| rt.speed.update(p.bytes_total())).unwrap_or(0.0);

        // Status + Speicher
        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 4 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (icon, color, text) = phase_text(&phase, last_sync);
                if matches!(phase, Phase::Syncing | Phase::Starting) {
                    ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                } else {
                    paint_icon(ui, icon, color);
                }
                let r = ui.add(egui::Label::new(RichText::new(text).color(TEXT)).truncate());
                if let Phase::Offline(m) | Phase::Error(m) = &phase {
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
                ui.label(RichText::new(format!("{} von {} belegt", human_size(used), human_size(quota))).size(11.0).color(TEXT_DIM));
            }
        });

        if phase == Phase::NeedsLogin {
            let acc = rt.acc.clone();
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 6)).show(ui, |ui| {
                ui.label("Die Anmeldung ist abgelaufen oder wurde auf einem anderen Gerät beendet.");
                if ui.button(theme::button_text("Neu anmelden")).clicked() {
                    self.screen = Screen::Form(AccountForm::relogin(&acc));
                }
            });
            if !matches!(self.screen, Screen::Main) {
                return;
            }
        }

        // Live-Statistik
        if let Some(p) = &progress {
            if p.is_active() {
                ctx.request_repaint_after(Duration::from_millis(250));
                stats_panel(ui, p, speed);
            }
        }

        let footer_h = 52.0;
        let list_h = (ui.available_height() - footer_h).max(60.0);
        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 10, bottom: 4 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::label("Zuletzt"));
                if let Some(p) = &progress {
                    if p.bytes_up + p.bytes_down > 0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("Seit Start: {} hoch · {} runter", human_size(p.bytes_up as i64), human_size(p.bytes_down as i64)))
                                    .size(11.0)
                                    .color(TEXT_DIM),
                            );
                        });
                    }
                }
            });
        });
        egui::ScrollArea::vertical().max_height(list_h - 30.0).auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 0)).show(ui, |ui| {
                for ev in &activity {
                    activity_row(ui, ev);
                }
            });
        });

        // Footer
        let mut action: Option<&str> = None;
        let multi = self.rts.len() > 1;
        footer_band(ui, footer_h, |ui| {
            if ui.add(egui::Button::new(theme::button_text("Ordner öffnen")).min_size(Vec2::new(0.0, 36.0))).clicked() {
                action = Some("open");
            }
            let pause_text = if paused { "Fortsetzen" } else { "Pausieren" };
            if ui.add(egui::Button::new(theme::button_text(pause_text)).min_size(Vec2::new(0.0, 36.0))).clicked() {
                action = Some("pause");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(theme::button_text("Konto"), |ui| {
                    let acc = &self.rts[self.selected].acc;
                    ui.label(RichText::new(acc.label()).color(TEXT));
                    ui.label(RichText::new(acc.sync_dir.display().to_string()).color(TEXT_DIM).size(11.0));
                    ui.separator();
                    if ui.button("Jetzt synchronisieren").clicked() {
                        action = Some("sync");
                        ui.close();
                    }
                    if is_admin && ui.button("Benutzer verwalten …").clicked() {
                        action = Some("admin");
                        ui.close();
                    }
                    if ui.button("Konto hinzufügen …").clicked() {
                        action = Some("add");
                        ui.close();
                    }
                    if cfg!(windows) {
                        ui.horizontal(|ui| {
                            if toggle(ui, &mut self.cfg.autostart).changed() {
                                action = Some("autostart");
                            }
                            ui.label("Mit Windows starten");
                        });
                    }
                    ui.separator();
                    if !self.confirm_remove {
                        if ui.button(RichText::new("Konto entfernen …").color(DANGER)).clicked() {
                            self.confirm_remove = true;
                        }
                    } else {
                        ui.label(RichText::new("Die Dateien im Ordner bleiben erhalten.").size(11.0).color(TEXT_DIM));
                        if ui.button(RichText::new("Wirklich entfernen").color(DANGER)).clicked() {
                            action = Some("remove");
                            ui.close();
                        }
                    }
                    if ui.button("Beenden").clicked() {
                        std::process::exit(0);
                    }
                });
            });
        });

        match action {
            Some("open") => open_folder(&self.rts[self.selected].acc.sync_dir),
            Some("pause") => {
                let rt = &self.rts[self.selected];
                let p = !rt.paused.load(Ordering::SeqCst);
                rt.paused.store(p, Ordering::SeqCst);
                rt.send(Cmd::SetPaused(p));
                self.save();
            }
            Some("sync") => self.rts[self.selected].send(Cmd::SyncNow),
            Some("admin") => self.open_admin(&ctx),
            Some("add") => self.screen = Screen::Form(AccountForm::new(&self.cfg)),
            Some("autostart") => {
                config::set_autostart(self.cfg.autostart);
                self.save();
            }
            Some("remove") => {
                self.confirm_remove = false;
                self.remove_account(self.selected);
            }
            _ => {}
        }

        if let Some(item) = &self.tray_pause {
            let any_running = self.rts.iter().any(|r| !r.paused.load(Ordering::SeqCst));
            item.set_text(match (multi, any_running) {
                (true, true) => "Alle pausieren",
                (true, false) => "Alle fortsetzen",
                (false, true) => "Pausieren",
                (false, false) => "Fortsetzen",
            });
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let popup_mode = cfg!(not(target_os = "linux"));

        // Popup: beim Wegklicken oder mit Esc verschwinden (nicht während
        // des Kontoformulars, dort öffnen sich Ordner-Dialoge).
        if popup_mode && !matches!(self.screen, Screen::Form(_)) {
            let focused = ctx.input(|i| i.viewport().focused);
            match focused {
                Some(true) => self.had_focus = true,
                Some(false) if self.had_focus => {
                    self.had_focus = false;
                    self.confirm_remove = false;
                    self.win.hide(&ctx);
                }
                _ => {}
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.win.hide(&ctx);
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && popup_mode {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.win.hide(&ctx);
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(SURFACE)).show(ui, |ui| match self.screen {
            Screen::Form(_) => self.form_ui(ui),
            Screen::Admin(_) => self.admin_ui(ui),
            Screen::Main => self.main_ui(ui),
        });

        // Rahmen ums rahmenlose Fenster
        let screen = ctx.content_rect();
        ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("frame")))
            .rect_stroke(screen, 0.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);

        ctx.request_repaint_after(Duration::from_secs(30));
    }
}

// ================================================================ Bausteine

fn stats_panel(ui: &mut egui::Ui, p: &Progress, speed: f64) {
    egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 10, bottom: 6 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(theme::label("Wird synchronisiert"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if speed > 0.0 {
                    ui.label(RichText::new(format!("{}/s", human_size(speed as i64))).color(ACCENT).strong());
                }
            });
        });
        ui.add_space(4.0);
        egui::Frame::new().fill(RAISED).corner_radius(4).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(c) = &p.current {
                ui.horizontal(|ui| {
                    paint_icon(ui, if c.upload { Icon::Up } else { Icon::Down }, if c.upload { ACCENT } else { OK });
                    let name = c.path.rsplit('/').next().unwrap_or(&c.path);
                    ui.add(egui::Label::new(RichText::new(name).color(TEXT)).truncate()).on_hover_text(&c.path);
                });
                let frac = if c.total > 0 { (c.done as f32 / c.total as f32).clamp(0.0, 1.0) } else { 1.0 };
                let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), Sense::hover());
                ui.painter().rect_filled(rect, 2.0, SURFACE);
                let mut fill = rect;
                fill.set_width(rect.width() * frac);
                ui.painter().rect_filled(fill, 2.0, if c.upload { ACCENT } else { OK });
                let status = if c.done == 0 && c.upload {
                    "Wird geprüft …".to_string()
                } else {
                    format!("{} von {} · {} %", human_size(c.done as i64), human_size(c.total as i64), (frac * 100.0) as u32)
                };
                ui.label(RichText::new(status).size(11.0).color(TEXT_DIM));
            }
            let files = p.remaining_files();
            let bytes = p.remaining_bytes();
            let mut line = format!("Noch {} {} · {}", files, if files == 1 { "Datei" } else { "Dateien" }, human_size(bytes as i64));
            if speed > 1.0 && bytes > 0 {
                line.push_str(&format!(" · {}", eta(bytes as f64 / speed)));
            }
            ui.label(RichText::new(line).color(TEXT));
            for q in p.queue.iter().take(3) {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    paint_icon_at(ui.painter(), r, if q.upload { Icon::Up } else { Icon::Down }, TEXT_DIM);
                    let name = q.path.rsplit('/').next().unwrap_or(&q.path);
                    ui.add(egui::Label::new(RichText::new(format!("{name} · {}", human_size(q.size as i64))).size(11.0).color(TEXT_DIM)).truncate());
                });
            }
            if p.queue.len() > 3 {
                ui.label(RichText::new(format!("und {} weitere", p.queue.len() - 3)).size(11.0).color(TEXT_DIM));
            }
        });
    });
}

fn user_row(ui: &mut egui::Ui, u: &UserInfo, view: &mut AdminView, acc: &Account, ctx: &egui::Context) {
    egui::Frame::new().fill(RAISED).corner_radius(4).inner_margin(10).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(&u.username).color(TEXT).strong());
            if u.is_admin {
                ui.label(RichText::new("ADMIN").size(10.0).strong().color(ACCENT));
            }
        });
        let frac = if u.quota_bytes > 0 { (u.used_bytes as f32 / u.quota_bytes as f32).clamp(0.0, 1.0) } else { 0.0 };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), Sense::hover());
        ui.painter().rect_filled(rect, 2.0, SURFACE);
        let mut fill = rect;
        fill.set_width(rect.width() * frac);
        ui.painter().rect_filled(fill, 2.0, if frac > 0.95 { DANGER } else { ACCENT });
        ui.label(RichText::new(format!("{} von {} belegt", human_size(u.used_bytes), human_size(u.quota_bytes))).size(11.0).color(TEXT_DIM));

        let editing = view.edit.as_ref().map(|e| e.0) == Some(u.id);
        if editing {
            let (_, gb, pw) = view.edit.as_mut().unwrap();
            egui::Grid::new(("edit", u.id)).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new("Speicher").color(TEXT_DIM));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(gb).desired_width(60.0));
                    ui.label("GB");
                });
                ui.end_row();
                ui.label(RichText::new("Neues Passwort").color(TEXT_DIM));
                ui.add(egui::TextEdit::singleline(pw).password(true).hint_text("leer = unverändert").desired_width(f32::INFINITY));
                ui.end_row();
            });
            let gb_val = parse_gb(gb);
            let pw_val = if pw.is_empty() { None } else { Some(pw.clone()) };
            let valid = gb_val.is_some() && pw_val.as_ref().map(|p| p.len() >= 10).unwrap_or(true);
            ui.horizontal(|ui| {
                if ui.add_enabled(valid, egui::Button::new(theme::button_text("Speichern").color(BG)).fill(ACCENT)).clicked() {
                    let (id, name) = (u.id, u.username.clone());
                    let q = gb_val;
                    view.edit = None;
                    admin_run(view, acc, ctx, move |api| api.update_user(id, q, pw_val.as_deref()).map(|_| format!("„{name}“ gespeichert")));
                }
                if ui.button(theme::button_text("Abbrechen")).clicked() {
                    view.edit = None;
                }
            });
        } else if !u.is_admin {
            ui.horizontal(|ui| {
                if ui.small_button("Bearbeiten").clicked() {
                    view.edit = Some((u.id, fmt_gb(u.quota_bytes as f64 / 1024f64.powi(3)), String::new()));
                    view.confirm_delete = None;
                }
                if view.confirm_delete == Some(u.id) {
                    if ui.small_button(RichText::new("Wirklich löschen (inkl. aller Dateien)").color(DANGER)).clicked() {
                        let (id, name) = (u.id, u.username.clone());
                        view.confirm_delete = None;
                        admin_run(view, acc, ctx, move |api| api.delete_user(id).map(|_| format!("„{name}“ gelöscht")));
                    }
                } else if ui.small_button(RichText::new("Löschen").color(DANGER)).clicked() {
                    view.confirm_delete = Some(u.id);
                }
            });
        }
    });
    ui.add_space(4.0);
}

fn admin_api(acc: &Account) -> Result<Api, String> {
    Api::new(&acc.server, acc.token.clone()).map_err(|e| e.to_string())
}

fn admin_reload(view: &mut AdminView, acc: &Account, ctx: &egui::Context) {
    view.loading = true;
    let inbox = view.inbox.clone();
    let ctx = ctx.clone();
    let api = admin_api(acc);
    std::thread::spawn(move || {
        let msg = match api.and_then(|a| a.list_users().map_err(api_msg)) {
            Ok(u) => AdminMsg::Users(u),
            Err(e) => AdminMsg::Failed(e),
        };
        *inbox.lock().unwrap() = Some(msg);
        ctx.request_repaint();
    });
}

fn admin_run(view: &mut AdminView, acc: &Account, ctx: &egui::Context, f: impl FnOnce(&Api) -> Result<String, ApiError> + Send + 'static) {
    view.loading = true;
    view.info = None;
    let inbox = view.inbox.clone();
    let ctx = ctx.clone();
    let api = admin_api(acc);
    std::thread::spawn(move || {
        let msg = match api.and_then(|a| f(&a).map_err(api_msg)) {
            Ok(m) => AdminMsg::Done(m),
            Err(e) => AdminMsg::Failed(e),
        };
        *inbox.lock().unwrap() = Some(msg);
        ctx.request_repaint();
    });
}

fn api_msg(e: ApiError) -> String {
    match e {
        ApiError::Conflict(m) | ApiError::Server { message: m, .. } if !m.is_empty() => m,
        ApiError::Unauthorized => "Keine Berechtigung – bitte neu anmelden".into(),
        other => other.to_string(),
    }
}

fn parse_gb(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").parse::<f64>().ok().filter(|v| *v > 0.0 && *v < 100_000.0)
}

fn fmt_gb(v: f64) -> String {
    if (v - v.round()).abs() < 0.01 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}").replace('.', ",")
    }
}

fn eta(secs: f64) -> String {
    let s = secs.round() as u64;
    if s < 60 {
        "unter 1 Min.".into()
    } else if s < 3600 {
        format!("ca. {} Min.", s.div_ceil(60))
    } else {
        format!("ca. {} Std. {} Min.", s / 3600, (s % 3600) / 60)
    }
}

fn phase_color(p: &Phase, paused: bool) -> Color32 {
    if paused {
        return TEXT_DIM;
    }
    match p {
        Phase::UpToDate => OK,
        Phase::Syncing | Phase::Starting => ACCENT,
        Phase::Paused => TEXT_DIM,
        Phase::Offline(_) | Phase::NeedsLogin => WARN,
        Phase::Error(_) => DANGER,
    }
}

fn phase_text(phase: &Phase, last_sync: Option<chrono::DateTime<chrono::Local>>) -> (Icon, Color32, String) {
    match phase {
        Phase::Starting => (Icon::Sync, ACCENT, "Startet …".into()),
        Phase::Syncing => (Icon::Sync, ACCENT, "Synchronisiere …".into()),
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
    }
}

fn account_tab(ui: &mut egui::Ui, name: &str, dot: Color32, selected: bool) -> egui::Response {
    let font = egui::FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(name.to_string(), font, TEXT);
    let size = Vec2::new(galley.size().x + 32.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if selected {
        ACCENT_DIM
    } else if resp.hovered() {
        RAISED_HOVER
    } else {
        RAISED
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    if selected {
        ui.painter().rect_stroke(rect, 4.0, Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
    }
    ui.painter().circle_filled(Pos2::new(rect.left() + 13.0, rect.center().y), 4.0, dot);
    ui.painter().galley(Pos2::new(rect.left() + 24.0, rect.center().y - galley.size().y / 2.0), galley, TEXT);
    resp
}

fn normalize_server(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("https://{s}")
    }
}

fn start_login(form: &mut AccountForm, ctx: &egui::Context) {
    form.busy = true;
    form.error = None;
    let (server, user, pw, dev) = (normalize_server(&form.server), form.username.trim().to_string(), form.password.clone(), form.device.trim().to_string());
    let result = form.result.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = match Api::login(&server, &user, &pw, &dev) {
            Ok(api) => Ok(api.token().unwrap_or_default().to_string()),
            Err(ApiError::Unauthorized) => Err("Benutzername oder Passwort falsch".to_string()),
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

fn footer_band(ui: &mut egui::Ui, h: f32, add: impl FnOnce(&mut egui::Ui)) {
    let rect = Rect::from_min_max(Pos2::new(ui.max_rect().left(), ui.max_rect().bottom() - h), ui.max_rect().max);
    ui.painter().rect_filled(rect, 0.0, BG);
    ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, BORDER));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(10.0, 8.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
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
    let when = if ev.at.date_naive() == chrono::Local::now().date_naive() { ev.at.format("%H:%M").to_string() } else { ev.at.format("%d.%m. %H:%M").to_string() };
    let detail = if ev.detail.is_empty() { format!("{verb} · {when}") } else { format!("{verb} · {} · {when}", ev.detail) };
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
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(34.0, 18.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let p = ui.painter();
    p.rect(rect, 9.0, if *on { ACCENT_DIM } else { RAISED }, Stroke::new(1.0, if *on { ACCENT_DIM } else { BORDER }), egui::StrokeKind::Inside);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(parse_gb("50"), Some(50.0));
        assert_eq!(parse_gb("2,5"), Some(2.5));
        assert_eq!(parse_gb("0"), None);
        assert_eq!(parse_gb("abc"), None);
        assert_eq!(fmt_gb(100.0), "100");
        assert_eq!(fmt_gb(2.5), "2,5");
        assert_eq!(eta(30.0), "unter 1 Min.");
        assert_eq!(eta(125.0), "ca. 3 Min.");
        assert_eq!(eta(3700.0), "ca. 1 Std. 1 Min.");
        assert_eq!(normalize_server("cloud.example.de/"), "https://cloud.example.de");
    }
}
