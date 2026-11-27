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

use dropcraft_core::{sync::human_size, Api, ApiError, BandwidthRef, EventKind, Progress, Share, ShareSettings};

use crate::i18n::t;
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
    fn start(acc: Account, ctx: &egui::Context, bw: &BandwidthRef) -> Self {
        let shared: SharedRef = Arc::new(Mutex::new(Shared::default()));
        let paused = Arc::new(AtomicBool::new(acc.paused));
        let cmd = acc.token.as_ref().map(|_| worker::spawn(acc.clone(), shared.clone(), ctx.clone(), paused.clone(), bw.clone()));
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

/// Für das Taskleisten-Symbol: Zustand aller Konten, alle pausieren/fortsetzen.
#[derive(Clone, Default)]
struct Controls(Arc<Mutex<Vec<Ctl>>>, Arc<Mutex<SpeedMeter>>);

struct Ctl {
    tx: Sender<Cmd>,
    paused: Arc<AtomicBool>,
    id: String,
    shared: SharedRef,
}

impl Controls {
    fn rebuild(&self, rts: &[AccountRt]) {
        *self.0.lock().unwrap() = rts
            .iter()
            .filter_map(|r| {
                r.cmd.clone().map(|tx| Ctl { tx, paused: r.paused.clone(), id: r.acc.id.clone(), shared: r.shared.clone() })
            })
            .collect();
    }

    fn any_running(&self) -> bool {
        self.0.lock().unwrap().iter().any(|c| !c.paused.load(Ordering::SeqCst))
    }

    fn toggle_all(&self) {
        let pause = self.any_running();
        let list = self.0.lock().unwrap();
        for c in list.iter() {
            c.paused.store(pause, Ordering::SeqCst);
            let _ = c.tx.send(Cmd::SetPaused(pause));
        }
        let mut cfg = Config::load();
        for a in &mut cfg.accounts {
            if list.iter().any(|c| c.id == a.id) {
                a.paused = pause;
            }
        }
        let _ = cfg.save();
    }

    /// Gesamtzustand für das Taskleisten-Symbol.
    /// Keine Übertragung läuft (gut für einen Neustart nach Update).
    fn idle(&self) -> bool {
        self.0.lock().unwrap().iter().all(|c| {
            let sh = c.shared.lock().unwrap();
            sh.progress.as_ref().map(|p| !p.lock().unwrap().is_active()).unwrap_or(true)
        })
    }

    fn tray_status(&self) -> tray::TrayStatus {
        let list = self.0.lock().unwrap();
        let multi = list.len() > 1;
        let mut files = 0usize;
        let mut bytes = 0u64;
        let mut transferred = 0u64;
        let mut problem: Option<String> = None;
        let mut all_paused = !list.is_empty();
        for c in list.iter() {
            let paused = c.paused.load(Ordering::SeqCst);
            all_paused &= paused;
            let sh = c.shared.lock().unwrap();
            if let Some(p) = &sh.progress {
                let p = p.lock().unwrap();
                files += p.remaining_files();
                bytes += p.remaining_bytes();
                transferred += p.bytes_total();
            }
            if !paused && problem.is_none() {
                problem = match &sh.phase {
                    Phase::Offline(_) => Some(crate::i18n::t("Server not reachable", "Server nicht erreichbar").into()),
                    Phase::NeedsLogin => Some(crate::i18n::t("Please sign in again", "Bitte neu anmelden").into()),
                    Phase::Error(e) => Some(e.clone()),
                    _ => None,
                };
            }
        }
        let any_running = list.iter().any(|c| !c.paused.load(Ordering::SeqCst));
        let pause_label = match (multi, any_running) {
            (true, true) => crate::i18n::t("Pause all", "Alle pausieren"),
            (true, false) => crate::i18n::t("Resume all", "Alle fortsetzen"),
            (false, true) => crate::i18n::t("Pause", "Pausieren"),
            (false, false) => crate::i18n::t("Resume", "Fortsetzen"),
        };
        let speed = self.1.lock().unwrap().update(transferred);
        let (state, tooltip) = if files > 0 {
            let mut t = format!("DropCraft – {files} {} · {}", if files == 1 { crate::i18n::t("file", "Datei") } else { crate::i18n::t("files", "Dateien") }, human_size(bytes as i64));
            if speed > 1.0 {
                t.push_str(&format!("\n{}/s · {}", human_size(speed as i64), eta(bytes as f64 / speed)));
            }
            (tray::TrayState::Syncing, t)
        } else if let Some(p) = problem {
            (tray::TrayState::Problem, format!("DropCraft – {p}"))
        } else if all_paused && !list.is_empty() {
            (tray::TrayState::Paused, crate::i18n::t("DropCraft – paused", "DropCraft – pausiert").to_string())
        } else {
            (tray::TrayState::Idle, crate::i18n::t("DropCraft – everything synced", "DropCraft – alles synchronisiert").to_string())
        };
        let unread = crate::inbox::unread();
        let tooltip = if unread > 0 { format!("{tooltip}\n{}", tf!("{unread} new notifications", "{unread} neue Benachrichtigungen")) } else { tooltip };
        tray::TrayStatus { state, tooltip, pause_label, open_label: crate::i18n::t("Open DropCraft", "DropCraft öffnen"), quit_label: crate::i18n::t("Quit", "Beenden"), unread: unread > 0 }
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

enum SharesMsg {
    List(Vec<Share>),
    /// Link und Token der neuen Freigabe
    Created(String, String),
    Updated(String),
    Deleted,
    Failed(String),
}

struct SettingsForm {
    up: String,
    down: String,
}

struct SharesView {
    account_id: String,
    shares: Vec<Share>,
    loading: bool,
    info: Option<(bool, String)>,
    /// Aufgeklappte Freigabe (Token)
    expanded: Option<String>,
    confirm_delete: Option<String>,
    /// Passwort-Eingabe für die Freigabe mit diesem Token
    pw_edit: Option<(String, String)>,
    inbox: Pending<SharesMsg>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Activity,
    Shares,
    Account,
    Settings,
}

enum Screen {
    Main,
    Form(AccountForm),
}

// ================================================================ App

pub struct App {
    cfg: Config,
    bw: BandwidthRef,
    rts: Vec<AccountRt>,
    selected: usize,
    screen: Screen,
    tab: Tab,
    shares: Option<SharesView>,
    settings: SettingsForm,
    controls: Controls,
    win: tray::WindowHandle,
    had_focus: bool,
    share_requests: Arc<Mutex<Vec<PathBuf>>>,
    confirm_remove: bool,
    update: crate::updater::StatusRef,
    /// Liste der Benachrichtigungen (Glocke) statt des Tab-Inhalts
    notes_open: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, listener: TcpListener, autostart: bool, initial_share: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let win = tray::WindowHandle::default();
        win.set_from(cc);
        #[cfg(target_os = "macos")]
        tray::mac::accessory();

        let cfg = Config::load();
        let bw = BandwidthRef::default();
        cfg.apply_bandwidth(&bw);
        let rts: Vec<AccountRt> = cfg.accounts.iter().map(|a| AccountRt::start(a.clone(), &ctx, &bw)).collect();
        let controls = Controls::default();
        controls.rebuild(&rts);

        // Zweiter Start der App öffnet das Popup der laufenden; mit
        // „--share <Pfad>“ (Explorer-Rechtsklick) wird ein Link erstellt.
        let share_requests: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(initial_share.into_iter().collect()));
        {
            let (win, ctx, reqs) = (win.clone(), ctx.clone(), share_requests.clone());
            std::thread::spawn(move || {
                for mut stream in listener.incoming().flatten() {
                    let mut line = String::new();
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = std::io::Read::read_to_string(&mut stream, &mut line);
                    let line = line.trim_end();
                    // Installer/Deinstallation: laufende Instanz beenden.
                    if line == "QUIT" {
                        std::process::exit(0);
                    }
                    // Meldung aus einem anderen Prozess (z. B. „Link kopieren“)
                    if let Some(json) = line.strip_prefix("NOTE ") {
                        crate::inbox::receive(json);
                        continue;
                    }
                    if line == "AUTOSTART" {
                        crate::applog::line("Zweiter Autostart ignoriert");
                        continue;
                    }
                    if let Some(p) = line.strip_prefix("SHARE ") {
                        reqs.lock().unwrap().push(PathBuf::from(p));
                    }
                    // "SHOW", "SHARE …" oder leer (ältere Versionen)
                    crate::applog::line(&format!("Popup zeigen: zweiter Start ({})", if line.is_empty() { "ohne Angabe" } else { line }));
                    win.show(&ctx, None);
                }
            });
        }

        // Unter Linux braucht das Tray-Icon eine GTK-Schleife; die App ist
        // für Windows gedacht, dort läuft alles über das Taskleisten-Symbol.
        if cfg!(not(target_os = "linux")) {
            let (c1, c2) = (controls.clone(), controls.clone());
            tray::spawn(move || c1.tray_status(), win.clone(), ctx.clone(), move || c2.toggle_all(), || std::process::exit(0));
        }
        config::register_context_menu(&cfg.accounts);
        // Run-Eintrag auf diese Programmdatei setzen (sie kann seit dem letzten
        // Start verschoben oder installiert worden sein).
        if cfg.autostart && !cfg.accounts.is_empty() {
            config::set_autostart(true);
        }

        let update = crate::updater::StatusRef::default();
        {
            let c = controls.clone();
            crate::updater::spawn_checker(update.clone(), ctx.clone(), || Config::load().auto_update, move || c.idle());
        }

        let screen = if cfg.accounts.is_empty() { Screen::Form(AccountForm::new(&cfg)) } else { Screen::Main };
        crate::inbox::init_app(&ctx);
        if !autostart || cfg.accounts.is_empty() || !share_requests.lock().unwrap().is_empty() {
            crate::applog::line(&format!("Popup zeigen: Start (autostart={autostart}, Konten={})", cfg.accounts.len()));
            win.show(&ctx, None);
        }

        Self {
            cfg,
            bw,
            rts,
            selected: 0,
            screen,
            tab: Tab::Activity,
            shares: None,
            settings: SettingsForm { up: String::new(), down: String::new() },
            controls,
            win,
            had_focus: false,
            share_requests,
            confirm_remove: false,
            update,
            notes_open: false,
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
        rt.send(Cmd::Remove);
        rt.cmd = None;
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
        config::register_context_menu(&self.cfg.accounts);
        self.selected = self.selected.min(self.rts.len().saturating_sub(1));
        if self.rts.is_empty() {
            self.screen = Screen::Form(AccountForm::new(&self.cfg));
        }
    }

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
                        form.error = Some(tf!("Could not create the folder: {e}", "Ordner konnte nicht angelegt werden: {e}"));
                        return;
                    }
                    let ctx = ui.ctx().clone();
                    if let Some(i) = self.rts.iter().position(|r| r.acc.id == acc.id) {
                        self.rts[i].stop();
                        self.rts[i] = AccountRt::start(acc.clone(), &ctx, &self.bw);
                        self.cfg.accounts[i] = acc;
                        self.selected = i;
                    } else {
                        if self.cfg.accounts.is_empty() {
                            config::set_autostart(self.cfg.autostart);
                        }
                        self.rts.push(AccountRt::start(acc.clone(), &ctx, &self.bw));
                        self.cfg.accounts.push(acc);
                        self.selected = self.rts.len() - 1;
                    }
                    self.save();
                    self.controls.rebuild(&self.rts);
                    config::register_context_menu(&self.cfg.accounts);
                    self.screen = Screen::Main;
                    return;
                }
                Err(e) => form.error = Some(e),
            }
        }

        let title = if form.replace.is_some() { t("SIGN IN AGAIN", "NEU ANMELDEN") } else if form.first { t("CONNECT TO SERVER", "MIT SERVER VERBINDEN") } else { t("ADD ACCOUNT", "KONTO HINZUFÜGEN") };
        header_band(ui, title);
        let mut cancel = false;
        let mut submit = false;
        egui::Frame::new().inner_margin(16).show(ui, |ui| {
            if form.first {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
                    let logo = logo_texture(ui.ctx());
                    ui.painter().image(logo.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(t("Welcome to DropCraft", "Willkommen bei DropCraft")).size(17.0));
                        ui.label(RichText::new(t("Your own cloud storage", "Dein eigener Cloud-Speicher")).color(TEXT_DIM));
                    });
                });
                ui.add_space(12.0);
            }
            let locked = form.replace.is_some();
            egui::Grid::new("form").num_columns(2).spacing([12.0, 10.0]).min_col_width(100.0).show(ui, |ui| {
                ui.label(RichText::new("Server").color(TEXT_DIM));
                ui.add_enabled(!locked, egui::TextEdit::singleline(&mut form.server).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label(RichText::new(t("Username", "Benutzername")).color(TEXT_DIM));
                let r = ui.add_enabled(!locked, egui::TextEdit::singleline(&mut form.username).desired_width(f32::INFINITY));
                if r.changed() && !form.dir_touched {
                    form.sync_dir = Config::default_sync_dir(&form.username, form.first);
                }
                ui.end_row();
                ui.label(RichText::new(t("Password", "Passwort")).color(TEXT_DIM));
                let pw = ui.add(egui::TextEdit::singleline(&mut form.password).password(true).desired_width(f32::INFINITY));
                if pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.end_row();
                ui.label(RichText::new(t("Device name", "Gerätename")).color(TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut form.device).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label(RichText::new(t("Sync folder", "Sync-Ordner")).color(TEXT_DIM));
                ui.horizontal(|ui| {
                    if ui.add_enabled(!locked, egui::Button::new(t("Change …", "Ändern …"))).clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_directory(form.sync_dir.parent().unwrap_or(&form.sync_dir)).pick_folder() {
                            form.sync_dir = dir;
                            form.dir_touched = true;
                        }
                    }
                    ui.add(egui::Label::new(form.sync_dir.display().to_string()).truncate());
                });
                ui.end_row();
                if form.first && cfg!(windows) {
                    ui.label(RichText::new(t("On startup", "Beim Start")).color(TEXT_DIM));
                    ui.horizontal(|ui| {
                        toggle(ui, &mut self.cfg.autostart);
                        ui.label(t("Start with Windows", "Mit Windows starten"));
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
            if form.cancelable && ui.button(theme::button_text(t("Cancel", "Abbrechen"))).clicked() {
                cancel = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = !form.busy && !form.username.trim().is_empty() && !form.password.is_empty() && normalize_server(&form.server).len() > 8;
                let btn = egui::Button::new(theme::button_text(if form.busy { t("Connecting …", "Verbinde …") } else { t("Connect", "Verbinden") }).color(BG)).fill(ACCENT);
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
                form.error = Some(tf!("The folder overlaps with the folder of “{}”. Please choose a separate folder.", "Der Ordner überschneidet sich mit dem Ordner von „{}“. Bitte einen eigenen Ordner wählen.", other.label()));
            } else if form.replace.is_none()
                && self.cfg.accounts.iter().any(|a| a.server == server && a.username.eq_ignore_ascii_case(&user))
            {
                form.error = Some(t("This account is already set up.", "Dieses Konto ist schon eingerichtet.").into());
            } else {
                start_login(form, ui.ctx());
            }
        }
        if cancel {
            self.screen = Screen::Main;
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        if tab != self.tab || tab == Tab::Shares {
            // Beim Öffnen frisch laden.
            match tab {
                Tab::Shares => self.shares = None,
                _ => {}
            }
        }
        if tab == Tab::Settings {
            let f = |v: f64| if v > 0.0 { fmt_gb(v) } else { String::new() };
            self.settings = SettingsForm { up: f(self.cfg.up_limit_mbps), down: f(self.cfg.down_limit_mbps) };
        }
        self.confirm_remove = false;
        self.tab = tab;
    }

    /// Lädt Freigaben bzw. Benutzer für das gewählte Konto, wenn nötig.
    fn ensure_views(&mut self, ctx: &egui::Context) {
        let Some(rt) = self.rts.get(self.selected) else { return };
        let id = rt.acc.id.clone();
        if self.tab == Tab::Shares && self.shares.as_ref().map(|v| v.account_id != id).unwrap_or(true) {
            let mut v = SharesView {
                account_id: id.clone(),
                shares: Vec::new(),
                loading: false,
                info: None,
                expanded: None,
                confirm_delete: None,
                pw_edit: None,
                inbox: Arc::new(Mutex::new(None)),
            };
            shares_reload(&mut v, &rt.acc, ctx);
            self.shares = Some(v);
        }
    }

    /// Freigabe-Anfrage aus dem Explorer (Rechtsklick) oder dem Dialog.
    fn share_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.screen = Screen::Main;
        let found = self.rts.iter().enumerate().find_map(|(i, r)| remote_path(&r.acc.sync_dir, &path).map(|rp| (i, rp)));
        if let Some((i, _)) = &found {
            self.selected = *i;
        }
        self.tab = Tab::Shares;
        if self.shares.as_ref().map(|v| v.account_id != self.rts[self.selected].acc.id).unwrap_or(true) {
            self.shares = None;
        }
        self.ensure_views(ctx);
        let acc = self.rts[self.selected].acc.clone();
        let Some(view) = self.shares.as_mut() else { return };
        match found {
            Some((_, rp)) => shares_create(view, &acc, ctx, rp),
            None => view.info = Some((false, tf!("“{}” is not in a DropCraft folder.", "„{}“ liegt in keinem DropCraft-Ordner.", path.display()))),
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
        self.ensure_views(&ctx);

        let full = ui.max_rect();
        let head_r = Rect::from_min_size(full.min, Vec2::new(full.width(), 52.0));
        let tab_r = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - 56.0), full.max);
        let body_r = Rect::from_min_max(Pos2::new(full.left(), head_r.bottom()), Pos2::new(full.right(), tab_r.top()));

        let mut action: Option<Action> = None;
        {
            let mut h = ui.new_child(egui::UiBuilder::new().max_rect(head_r));
            self.header_ui(&mut h, &mut action);
        }
        {
            let mut b = ui.new_child(egui::UiBuilder::new().max_rect(body_r).layout(egui::Layout::top_down(egui::Align::Min)));
            b.set_clip_rect(body_r);
            if self.notes_open {
                self.notes_panel(&mut b);
            } else {
                match self.tab {
                Tab::Activity => self.activity_tab(&mut b),
                Tab::Shares => self.shares_tab(&mut b),
                Tab::Account => self.account_tab(&mut b, &mut action),
                Tab::Settings => self.settings_tab(&mut b),
                }
            }
        }
        {
            let mut t = ui.new_child(egui::UiBuilder::new().max_rect(tab_r));
            if let Some(nt) = tab_bar(&mut t, if self.notes_open { None } else { Some(self.tab) }) {
                self.notes_open = false;
                self.set_tab(nt);
            }
        }
        ui.allocate_rect(full, Sense::hover());

        match action {
            Some(Action::Select(i)) => {
                self.selected = i;
                self.shares = None;
                self.confirm_remove = false;
            }
            Some(Action::Add) => self.screen = Screen::Form(AccountForm::new(&self.cfg)),
            Some(Action::Relogin) => self.screen = Screen::Form(AccountForm::relogin(&self.rts[self.selected].acc)),
            Some(Action::OpenFolder) => open_folder(&self.rts[self.selected].acc.sync_dir),
            Some(Action::TogglePause) => {
                let rt = &self.rts[self.selected];
                let p = !rt.paused.load(Ordering::SeqCst);
                rt.paused.store(p, Ordering::SeqCst);
                rt.send(Cmd::SetPaused(p));
                self.save();
            }
            Some(Action::SyncNow) => self.rts[self.selected].send(Cmd::SyncNow),
            Some(Action::Remove) => {
                self.confirm_remove = false;
                self.remove_account(self.selected);
                self.shares = None;
            }
            Some(Action::Quit) => std::process::exit(0),
            None => {}
        }
    }

    fn header_ui(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        let r = ui.max_rect();
        ui.painter().rect_filled(r, 0.0, BG);
        ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, BORDER));
        let inner = r.shrink2(Vec2::new(12.0, 9.0));
        let pause_r = Rect::from_center_size(Pos2::new(inner.right() - 16.0, inner.center().y), Vec2::splat(32.0));
        let folder_r = pause_r.translate(Vec2::new(-40.0, 0.0));
        let bell_r = folder_r.translate(Vec2::new(-40.0, 0.0));
        let acc_r = Rect::from_min_max(inner.min, Pos2::new(bell_r.left() - 8.0, inner.max.y));

        let rt = &self.rts[self.selected];
        let phase = rt.shared.lock().unwrap().phase.clone();
        let paused = rt.paused.load(Ordering::SeqCst);
        let resp = ui.interact(acc_r, ui.scope_id().with("account_switch"), Sense::click());
        let p = ui.painter();
        p.rect(acc_r, 4.0, if resp.hovered() { RAISED } else { SURFACE }, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
        let av = Rect::from_min_size(Pos2::new(acc_r.left() + 4.0, acc_r.center().y - 13.0), Vec2::splat(26.0));
        p.rect_filled(av, 4.0, ACCENT_DIM);
        let initial = rt.acc.username.chars().next().unwrap_or('?').to_uppercase().to_string();
        p.text(av.center(), egui::Align2::CENTER_CENTER, initial, egui::FontId::proportional(13.0), ACCENT);
        let tx = av.right() + 8.0;
        let max_w = acc_r.right() - 40.0 - tx;
        let name = p.layout_no_wrap(rt.acc.username.clone(), egui::FontId::proportional(13.0), TEXT);
        let host = p.layout_no_wrap(rt.acc.host().to_string(), egui::FontId::proportional(10.5), TEXT_DIM);
        let clip = p.with_clip_rect(Rect::from_min_max(Pos2::new(tx, acc_r.top()), Pos2::new(tx + max_w, acc_r.bottom())));
        clip.galley(Pos2::new(tx, acc_r.center().y - 15.0), name, TEXT);
        clip.galley(Pos2::new(tx, acc_r.center().y + 1.0), host, TEXT_DIM);
        p.circle_filled(Pos2::new(acc_r.right() - 30.0, acc_r.center().y), 4.0, phase_color(&phase, paused));
        paint_icon_at(p, Rect::from_center_size(Pos2::new(acc_r.right() - 14.0, acc_r.center().y), Vec2::splat(12.0)), Icon::Chevron, TEXT_DIM);
        let (status_icon, _, status_text) = phase_text(&phase, None);
        let _ = status_icon;
        let resp = resp.on_hover_text(format!("{} · {}", rt.acc.label(), if paused { t("Paused", "Pausiert").to_string() } else { status_text }));

        egui::Popup::menu(&resp).width(acc_r.width()).show(|ui| {
            for i in 0..self.rts.len() {
                let rt = &self.rts[i];
                let ph = rt.shared.lock().unwrap().phase.clone();
                let col = phase_color(&ph, rt.paused.load(Ordering::SeqCst));
                ui.horizontal(|ui| {
                    let (d, _) = ui.allocate_exact_size(Vec2::new(10.0, 20.0), Sense::hover());
                    ui.painter().circle_filled(d.center(), 4.0, col);
                    if ui.selectable_label(i == self.selected, rt.acc.label()).clicked() {
                        *action = Some(Action::Select(i));
                        ui.close();
                    }
                });
            }
            ui.separator();
            if ui.button(t("Sync now", "Jetzt synchronisieren")).clicked() {
                *action = Some(Action::SyncNow);
                ui.close();
            }
            if ui.button(t("Add account …", "Konto hinzufügen …")).clicked() {
                *action = Some(Action::Add);
                ui.close();
            }
            ui.separator();
            if ui.button(t("Quit DropCraft", "DropCraft beenden")).clicked() {
                *action = Some(Action::Quit);
            }
        });

        let unread = crate::inbox::unread();
        let bell_tip = if unread > 0 { tf!("{unread} new notifications", "{unread} neue Benachrichtigungen") } else { t("Notifications", "Benachrichtigungen").to_string() };
        if icon_button(ui, bell_r, Icon::Bell, &bell_tip, self.notes_open, 0).clicked() {
            self.notes_open = !self.notes_open;
        }
        if unread > 0 {
            let c = Pos2::new(bell_r.right() - 5.0, bell_r.top() + 5.0);
            let text = if unread > 9 { "9+".to_string() } else { unread.to_string() };
            let w = if text.len() > 1 { 18.0 } else { 15.0 };
            let badge = Rect::from_center_size(c, Vec2::new(w, 15.0));
            ui.painter().rect_filled(badge, 7.5, DANGER);
            ui.painter().text(badge.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(9.5), Color32::WHITE);
        }
        if icon_button(ui, folder_r, Icon::Folder, t("Open folder", "Ordner öffnen"), false, 0).clicked() {
            *action = Some(Action::OpenFolder);
        }
        let (pi, tip) = if paused { (Icon::Play, t("Resume", "Fortsetzen")) } else { (Icon::Pause, t("Pause", "Pausieren")) };
        if icon_button(ui, pause_r, pi, tip, paused, 0).clicked() {
            *action = Some(Action::TogglePause);
        }
    }

    // ------------------------------------------------------------ Benachrichtigungen

    fn notes_panel(&mut self, ui: &mut egui::Ui) {
        // Wer die Liste sieht, hat alles gelesen.
        crate::inbox::mark_all_read();
        let notes = crate::inbox::list();
        let full = ui.max_rect();
        let mut clear_all = false;
        let mut clear_link = |ui: &mut egui::Ui| {
            if ui.link(RichText::new(t("Clear all", "Alle löschen")).size(12.0).color(ACCENT)).clicked() {
                clear_all = true;
            }
        };
        section_label(ui, t("Notifications", "Benachrichtigungen"), if notes.is_empty() { None } else { Some(&mut clear_link) });
        if clear_all {
            crate::inbox::clear();
            return;
        }
        if notes.is_empty() {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 24)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    paint_icon(ui, Icon::Bell, TEXT_DIM);
                    ui.add_space(6.0);
                    ui.label(RichText::new(t("No notifications", "Keine Benachrichtigungen")).color(TEXT_DIM));
                });
            });
            return;
        }
        let mut remove = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for n in &notes {
                let top = ui.cursor().top();
                egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 40, top: 10, bottom: 10 }).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(&n.title).color(TEXT).strong());
                    if !n.body.is_empty() {
                        ui.add(egui::Label::new(RichText::new(&n.body).color(TEXT_DIM)).wrap());
                    }
                    ui.label(RichText::new(note_time(n.at)).size(10.5).color(TEXT_DIM));
                });
                let bottom = ui.cursor().top();
                let x = Rect::from_center_size(Pos2::new(full.right() - 22.0, top + 22.0), Vec2::splat(26.0));
                let resp = ui.interact(x, ui.scope_id().with(("note-x", n.id)), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(x, 4.0, RAISED_HOVER);
                }
                paint_icon_at(ui.painter(), Rect::from_center_size(x.center(), Vec2::splat(12.0)), Icon::Close, if resp.hovered() { TEXT } else { TEXT_DIM });
                if resp.on_hover_text(t("Clear", "Löschen")).clicked() {
                    remove = Some(n.id);
                }
                ui.painter().hline(full.x_range(), bottom, Stroke::new(1.0, BORDER));
            }
        });
        if let Some(id) = remove {
            crate::inbox::remove(id);
        }
    }

    // ------------------------------------------------------------ Aktivität

    fn activity_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Hinweis auf ein verfügbares Update
        let status = self.update.lock().unwrap().clone();
        if matches!(status, crate::updater::Status::Available(_) | crate::updater::Status::Failed(_)) {
            let text = match &status {
                crate::updater::Status::Available(l) => tf!("Update to version {} available", "Update auf Version {} verfügbar", l.version),
                crate::updater::Status::Failed(e) => tf!("Update failed: {e}", "Update fehlgeschlagen: {e}"),
                _ => String::new(),
            };
            egui::Frame::new().fill(ACCENT_DIM).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(&text).color(TEXT)).truncate());
                    if let crate::updater::Status::Available(l) = &status {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new(theme::button_text(t("Install", "Installieren")).color(BG)).fill(ACCENT)).clicked() {
                                let (upd, c, l) = (self.update.clone(), ctx.clone(), l.clone());
                                std::thread::spawn(move || crate::updater::run_install(&upd, &c, &l));
                            }
                        });
                    }
                });
            });
        } else if let crate::updater::Status::Downloading(p) = status {
            egui::Frame::new().fill(ACCENT_DIM).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(tf!("Downloading update … {} %", "Update wird geladen … {} %", (p * 100.0) as u32)).color(TEXT));
            });
        }
        let rt = &mut self.rts[self.selected];
        let (phase, used, quota, last_sync, activity, progress) = {
            let sh = rt.shared.lock().unwrap();
            (
                sh.phase.clone(),
                sh.used,
                sh.quota,
                sh.last_sync,
                sh.activity.iter().take(100).cloned().collect::<Vec<_>>(),
                sh.progress.as_ref().map(|p| p.lock().unwrap().clone()),
            )
        };
        let activity: Vec<_> = progress
            .as_ref()
            .map(|p| p.events.iter().rev().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .chain(activity)
            .take(100)
            .collect();
        let paused = rt.paused.load(Ordering::SeqCst);
        let speed = progress.as_ref().map(|p| rt.speed.update(p.bytes_total())).unwrap_or(0.0);

        let full = ui.max_rect();
        let quota_h = if quota > 0 { 46.0 } else { 0.0 };

        // Status
        egui::Frame::new().inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let active = progress.as_ref().map(|p| p.is_active()).unwrap_or(false);
            if phase == Phase::NeedsLogin {
                ui.horizontal(|ui| {
                    paint_icon(ui, Icon::Warning, WARN);
                    ui.label(RichText::new(t("Please sign in again", "Bitte neu anmelden")).size(15.0).color(WARN));
                });
                ui.label(RichText::new(t("Your sign-in expired or was ended on another device.", "Die Anmeldung ist abgelaufen oder wurde auf einem anderen Gerät beendet.")).color(TEXT_DIM));
                if ui.add(egui::Button::new(theme::button_text(t("Sign in again", "Neu anmelden")).color(BG)).fill(ACCENT)).clicked() {
                    self.screen = Screen::Form(AccountForm::relogin(&rt.acc));
                }
            } else if let (true, Some(p)) = (active && !paused, &progress) {
                ctx.request_repaint_after(Duration::from_millis(250));
                transfer_panel(ui, p, speed);
            } else {
                let (icon, color, text) = if paused { (Icon::Pause, TEXT_DIM, t("Paused", "Pausiert").to_string()) } else { phase_text(&phase, last_sync) };
                ui.horizontal(|ui| {
                    paint_icon(ui, icon, color);
                    let r = ui.add(egui::Label::new(RichText::new(text).size(15.0).color(if color == OK { TEXT } else { color })).truncate());
                    if let Phase::Offline(m) | Phase::Error(m) = &phase {
                        r.on_hover_text(m);
                    }
                });
            }
        });
        let y = ui.cursor().top();
        ui.painter().hline(full.x_range(), y, Stroke::new(1.0, BORDER));

        egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 12, top: 10, bottom: 4 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::label(t("Recent", "Zuletzt")));
                if let Some(p) = &progress {
                    if p.bytes_up + p.bytes_down > 0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(tf!("{} up · {} down", "{} hoch · {} runter", human_size(p.bytes_up as i64), human_size(p.bytes_down as i64))).size(10.5).color(TEXT_DIM));
                        });
                    }
                }
            });
        });
        let list_h = (full.bottom() - quota_h - ui.cursor().top()).max(40.0);
        egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
            for group in group_activity(&activity) {
                activity_row(ui, &group);
            }
        });

        if quota > 0 {
            let r = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - quota_h), full.max);
            let p = ui.painter();
            p.rect_filled(r, 0.0, BG);
            p.hline(r.x_range(), r.top(), Stroke::new(1.0, BORDER));
            let inner = r.shrink2(Vec2::new(12.0, 10.0));
            p.text(inner.left_top(), egui::Align2::LEFT_TOP, t("Storage", "Speicher"), egui::FontId::proportional(10.5), TEXT_DIM);
            p.text(inner.right_top(), egui::Align2::RIGHT_TOP, tf!("{} of {}", "{} von {}", human_size(used), human_size(quota)), egui::FontId::proportional(10.5), TEXT_DIM);
            let bar = Rect::from_min_size(Pos2::new(inner.left(), inner.bottom() - 4.0), Vec2::new(inner.width(), 4.0));
            quota_bar(p, bar, used, quota, RAISED);
        }
    }

    // ------------------------------------------------------------ Freigaben

    fn shares_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let acc = self.rts[self.selected].acc.clone();
        let mut pick: Option<bool> = None; // Some(true)=Ordner, Some(false)=Datei
        {
            let Some(view) = self.shares.as_mut() else { return };
            let msg = view.inbox.lock().unwrap().take();
            if let Some(msg) = msg {
                view.loading = false;
                match msg {
                    SharesMsg::List(l) => view.shares = l,
                    SharesMsg::Created(url, token) => {
                        let copied = copy_to_clipboard(&url);
                        view.info = Some((true, if copied { t("Link created and copied", "Link erstellt und kopiert").into() } else { t("Link created", "Link erstellt").into() }));
                        view.expanded = Some(token);
                        shares_reload(view, &acc, &ctx);
                    }
                    SharesMsg::Updated(m) => {
                        view.info = Some((true, m));
                        shares_reload(view, &acc, &ctx);
                    }
                    SharesMsg::Deleted => {
                        view.info = Some((true, t("Link ended – it no longer works", "Freigabe beendet – der Link funktioniert nicht mehr").into()));
                        shares_reload(view, &acc, &ctx);
                    }
                    SharesMsg::Failed(m) => view.info = Some((false, m)),
                }
            }

            let full = ui.max_rect();
            egui::Frame::new().inner_margin(12).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let n = view.shares.len();
                    ui.label(theme::label(&format!("{n} {}", if n == 1 { t("active link", "aktiver Link") } else { t("active links", "aktive Links") })));
                    if view.loading {
                        ui.add(egui::Spinner::new().size(12.0).color(TEXT_DIM));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let file_r = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover()).0;
                        if icon_button(ui, file_r, Icon::File, t("Share file …", "Datei freigeben …"), false, 0).clicked() {
                            pick = Some(false);
                        }
                        if ui.add(egui::Button::new(theme::button_text(t("Share folder", "Ordner freigeben")).color(BG)).fill(ACCENT).min_size(Vec2::new(0.0, 30.0))).clicked() {
                            pick = Some(true);
                        }
                    });
                });
                if let Some((ok, m)) = &view.info {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        paint_icon(ui, if *ok { Icon::Check } else { Icon::Warning }, if *ok { OK } else { DANGER });
                        ui.label(RichText::new(m).color(TEXT_DIM));
                    });
                }
            });
            ui.painter().hline(full.x_range(), ui.cursor().top(), Stroke::new(1.0, BORDER));

            let api = Api::new(&acc.server, None).ok();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if view.shares.is_empty() && !view.loading {
                    egui::Frame::new().inner_margin(12).show(ui, |ui| {
                        ui.label(RichText::new(t("Nothing shared yet. Choose a folder, or right-click a file in Explorer › DropCraft › Copy DropCraft link.", "Noch nichts freigegeben. Ordner auswählen oder im Explorer per Rechtsklick „DropCraft-Link kopieren“.")).color(TEXT_DIM));
                    });
                }
                for sh in view.shares.clone() {
                    let url = api.as_ref().map(|a| a.share_url(&sh.token)).unwrap_or_default();
                    share_row(ui, view, &sh, &url, &acc, &ctx);
                }
            });
        }
        if let Some(folder) = pick {
            let start = acc.sync_dir.clone();
            let dlg = rfd::FileDialog::new().set_directory(&start);
            let chosen = if folder { dlg.pick_folder() } else { dlg.pick_file() };
            self.had_focus = false;
            if let Some(p) = chosen {
                self.share_path(p, &ctx);
            }
        }
    }

    // ------------------------------------------------------------ Konto

    fn account_tab(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        let rt = &self.rts[self.selected];
        let acc = rt.acc.clone();
        let (used, quota, is_admin) = {
            let sh = rt.shared.lock().unwrap();
            (sh.used, sh.quota, sh.is_admin)
        };
        let full = ui.max_rect();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 12, top: 14, bottom: 12 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tf!("{} used", "{} belegt", human_size(used))).size(15.0));
                    if quota > 0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(tf!("of {}", "von {}", human_size(quota))).size(10.5).color(TEXT_DIM));
                        });
                    }
                });
                if quota > 0 {
                    let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 6.0), Sense::hover());
                    quota_bar(ui.painter(), bar, used, quota, BG);
                }
                ui.add(egui::Label::new(RichText::new(tf!("Folder: {} · Device: {}", "Ordner: {} · Gerät: {}", acc.sync_dir.display(), acc.device_name)).size(10.5).color(TEXT_DIM)).wrap());
            });
            ui.painter().hline(full.x_range(), ui.cursor().top(), Stroke::new(1.0, BORDER));
            if list_row(ui, Icon::Sync, t("Sync now", "Jetzt synchronisieren"), TEXT, TEXT_DIM).clicked() {
                *action = Some(Action::SyncNow);
            }

            // Konto und Verwaltung laufen auf der Website des Servers.
            let manage = if is_admin { t("Manage account and server …", "Konto und Server verwalten …") } else { t("Manage account …", "Konto verwalten …") };
            if list_row(ui, Icon::User, manage, ACCENT, ACCENT).on_hover_text(t("Password, devices, links – opens in the browser", "Passwort, Geräte, Links – öffnet sich im Browser")).clicked() {
                open_account_site(&acc, is_admin);
            }

            section_label(ui, t("Accounts in this app", "Konten in der App"), None);
            if list_row(ui, Icon::Plus, t("Add another account", "Weiteres Konto hinzufügen"), ACCENT, ACCENT).clicked() {
                *action = Some(Action::Add);
            }
            if !self.confirm_remove {
                if list_row(ui, Icon::Logout, t("Sign out and remove account", "Abmelden und Konto entfernen"), DANGER, DANGER).clicked() {
                    self.confirm_remove = true;
                }
            } else {
                egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.label(RichText::new(tf!("Really remove “{}”? The files in the folder stay.", "„{}“ wirklich entfernen? Die Dateien im Ordner bleiben erhalten.", acc.label())).color(TEXT));
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(theme::button_text(t("Remove", "Entfernen")).color(TEXT)).fill(DANGER)).clicked() {
                            *action = Some(Action::Remove);
                        }
                        if ui.button(theme::button_text(t("Cancel", "Abbrechen"))).clicked() {
                            self.confirm_remove = false;
                        }
                    });
                });
            }
            if list_row(ui, Icon::Power, t("Quit DropCraft", "DropCraft beenden"), TEXT, TEXT_DIM).clicked() {
                *action = Some(Action::Quit);
            }
            if phase_needs_login(&self.rts[self.selected]) && list_row(ui, Icon::Warning, t("Sign in again", "Neu anmelden"), WARN, WARN).clicked() {
                *action = Some(Action::Relogin);
            }
        });
    }

    // ------------------------------------------------------------ Einstellungen

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        egui::ScrollArea::vertical().max_height(full.height() - 34.0).auto_shrink([false, false]).show(ui, |ui| self.settings_body(ui));
        let r = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - 34.0), full.max);
        ui.painter().rect_filled(r, 0.0, BG);
        ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, BORDER));
        ui.painter().text(
            Pos2::new(r.left() + 12.0, r.center().y),
            egui::Align2::LEFT_CENTER,
            tf!("Changes apply immediately · DropCraft {}", "Änderungen gelten sofort · DropCraft {}", crate::updater::version()),
            egui::FontId::proportional(10.5),
            TEXT_DIM,
        );
    }

    fn settings_body(&mut self, ui: &mut egui::Ui) {
        let mut changed_bw = false;
        let mut changed = false;
        section_label(ui, t("Bandwidth", "Bandbreite"), None);
        for (label, val) in [("Upload", &mut self.settings.up), ("Download", &mut self.settings.down)] {
            setting_row(ui, label, None, |ui| {
                ui.label(RichText::new("MB/s").size(10.5).color(TEXT_DIM));
                if ui.add(egui::TextEdit::singleline(val).hint_text(t("unlimited", "unbegrenzt")).desired_width(96.0)).changed() {
                    changed_bw = true;
                }
            });
        }
        section_label(ui, t("General", "Allgemein"), None);
        let mut de = crate::i18n::german();
        setting_row(ui, t("Language", "Sprache"), None, |ui| {
            if ui.selectable_label(de, "Deutsch").clicked() {
                de = true;
            }
            if ui.selectable_label(!de, "English").clicked() {
                de = false;
            }
        });
        if de != crate::i18n::german() {
            self.cfg.language = if de { "de".into() } else { "en".into() };
            crate::i18n::apply(&self.cfg.language);
            // Klassischer Rechtsklick-Eintrag in der neuen Sprache
            config::register_context_menu(&self.cfg.accounts);
            changed = true;
        }
        if cfg!(any(windows, target_os = "macos")) {
            let mut v = self.cfg.autostart;
            setting_row(ui, if cfg!(windows) { t("Start with Windows", "Mit Windows starten") } else { t("Start at login", "Bei der Anmeldung starten") }, None, |ui| {
                if toggle(ui, &mut v).changed() {
                    changed = true;
                }
            });
            if v != self.cfg.autostart {
                self.cfg.autostart = v;
                config::set_autostart(v);
            }
        }
        let mut n = self.cfg.notifications;
        setting_row(ui, t("Notifications", "Benachrichtigungen"), Some(t("for changes in shared folders", "bei Änderungen in Freigaben")), |ui| {
            if toggle(ui, &mut n).changed() {
                changed = true;
            }
        });
        self.cfg.notifications = n;
        setting_row(ui, t("Test notification", "Test-Benachrichtigung"), Some(t("appears under the bell", "erscheint unter der Glocke")), |ui| {
            if ui.link(RichText::new(t("Send", "Senden")).color(ACCENT)).clicked() {
                crate::notify::show("DropCraft", t("This is what notifications look like.", "So sehen Benachrichtigungen aus."));
            }
        });
        // Windows 11: Eintrag „DropCraft“ direkt im Rechtsklick-Menü des Explorers.
        #[cfg(windows)]
        if crate::shellmenu::supported() && crate::installer::is_installed_copy() {
            let ready = crate::shellmenu::is_set_up();
            let sub = if ready { t("set up", "eingerichtet") } else { t("one-time admin approval", "einmalige Admin-Bestätigung") };
            setting_row(ui, t("Explorer menu", "Explorer-Menü"), Some(sub), |ui| {
                let label = if ready { t("Set up again", "Neu einrichten") } else { t("Set up", "Einrichten") };
                if ui.link(RichText::new(label).color(ACCENT)).clicked() {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(exe).arg("--setup-explorer-menu").spawn();
                    }
                }
            });
        }
        if crate::updater::SUPPORTED {
            section_label(ui, "Updates", None);
            let mut au = self.cfg.auto_update;
            setting_row(ui, t("Install automatically", "Automatisch installieren"), Some(t("otherwise only notify", "sonst nur benachrichtigen")), |ui| {
                if toggle(ui, &mut au).changed() {
                    changed = true;
                }
            });
            self.cfg.auto_update = au;
            let status = self.update.lock().unwrap().clone();
            let ctx = ui.ctx().clone();
            let upd = self.update.clone();
            use crate::updater::Status;
            let sub = match &status {
                Status::Idle => String::new(),
                Status::Checking => t("Checking …", "Suche …").into(),
                Status::UpToDate => t("Up to date", "Aktuell").into(),
                Status::Available(l) => tf!("Version {} available", "Version {} verfügbar", l.version),
                Status::Downloading(p) => tf!("Downloading … {} %", "Lädt … {} %", (p * 100.0) as u32),
                Status::Restarting => t("Restarting …", "Startet neu …").into(),
                Status::Failed(e) | Status::CheckFailed(e) => e.clone(),
            };
            let title = format!("Version {}", crate::updater::version());
            setting_row(ui, &title, (!sub.is_empty()).then_some(sub.as_str()), |ui| match &status {
                Status::Available(l) => {
                    if ui.add(egui::Button::new(theme::button_text(t("Install", "Installieren")).color(BG)).fill(ACCENT)).on_hover_text(&l.notes).clicked() {
                        let l = l.clone();
                        std::thread::spawn(move || crate::updater::run_install(&upd, &ctx, &l));
                    }
                }
                Status::Checking | Status::Downloading(_) | Status::Restarting => {
                    ui.add(egui::Spinner::new().size(14.0).color(TEXT_DIM));
                }
                _ => {
                    if ui.link(RichText::new(t("Check for updates", "Nach Updates suchen")).color(ACCENT)).clicked() {
                        std::thread::spawn(move || crate::updater::check_now(&upd, &ctx));
                    }
                }
            });
        }
        if changed_bw {
            let parse = |s: &str| s.trim().replace(',', ".").parse::<f64>().ok().filter(|v| *v > 0.0).unwrap_or(0.0);
            self.cfg.up_limit_mbps = parse(&self.settings.up);
            self.cfg.down_limit_mbps = parse(&self.settings.down);
            self.cfg.apply_bandwidth(&self.bw);
            changed = true;
        }
        if changed {
            self.save();
        }
        ui.add_space(12.0);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let popup_mode = cfg!(not(target_os = "linux"));
        // Fenster-Handle nachholen, falls es beim Start noch fehlte.
        self.win.ensure_handle(frame);
        // Ist das Fenster sichtbar, obwohl es versteckt sein soll (z. B. von
        // Windows beim Anmelden gezeigt)? Dann wieder verstecken.
        if popup_mode && self.win.shown_without_us() {
            crate::applog::line("Fenster war ohne Aufforderung sichtbar – versteckt");
            self.win.hide(&ctx);
            return;
        }

        // Freigabe-Anfragen aus dem Explorer-Rechtsklick
        let reqs: Vec<PathBuf> = std::mem::take(&mut *self.share_requests.lock().unwrap());
        for p in reqs {
            if !self.rts.is_empty() {
                self.share_path(p, &ctx);
            }
        }

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

enum Action {
    Select(usize),
    Add,
    Relogin,
    OpenFolder,
    TogglePause,
    SyncNow,
    Remove,
    Quit,
}

fn phase_needs_login(rt: &AccountRt) -> bool {
    rt.shared.lock().unwrap().phase == Phase::NeedsLogin
}

fn tab_bar(ui: &mut egui::Ui, current: Option<Tab>) -> Option<Tab> {
    let r = ui.max_rect();
    ui.painter().rect_filled(r, 0.0, BG);
    ui.painter().hline(r.x_range(), r.top() + 0.5, Stroke::new(1.0, BORDER));
    let tabs = [(Tab::Activity, Icon::Pulse, t("Activity", "Aktivität")), (Tab::Shares, Icon::Link, t("Shares", "Freigaben")), (Tab::Account, Icon::User, t("Account", "Konto")), (Tab::Settings, Icon::Gear, t("Settings", "Einstellungen"))];
    let w = r.width() / tabs.len() as f32;
    let mut out = None;
    for (i, (tab, icon, label)) in tabs.into_iter().enumerate() {
        let cell = Rect::from_min_size(Pos2::new(r.left() + i as f32 * w, r.top()), Vec2::new(w, r.height()));
        let resp = ui.interact(cell, ui.scope_id().with(("tab", i)), Sense::click());
        let on = Some(tab) == current;
        let c = if on {
            ACCENT
        } else if resp.hovered() {
            TEXT
        } else {
            TEXT_DIM
        };
        let p = ui.painter();
        if on {
            p.rect_filled(Rect::from_min_size(Pos2::new(cell.left() + w * 0.22, cell.top()), Vec2::new(w * 0.56, 2.0)), 0.0, ACCENT);
        }
        paint_icon_at(p, Rect::from_center_size(Pos2::new(cell.center().x, cell.top() + 20.0), Vec2::splat(18.0)), icon, c);
        p.text(Pos2::new(cell.center().x, cell.top() + 40.0), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(10.5), c);
        if resp.clicked() {
            out = Some(tab);
        }
    }
    out
}

fn icon_button(ui: &mut egui::Ui, rect: Rect, icon: Icon, tip: &str, active: bool, salt: impl std::hash::Hash + std::fmt::Debug) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(("ib", tip, salt)), Sense::click());
    let fill = if active {
        ACCENT_DIM
    } else if resp.hovered() {
        RAISED_HOVER
    } else {
        RAISED
    };
    ui.painter().rect(rect, 4.0, fill, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
    paint_icon_at(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(16.0)), icon, if active { ACCENT } else { TEXT });
    resp.on_hover_text(tip)
}

fn quota_bar(p: &egui::Painter, bar: Rect, used: i64, quota: i64, track: Color32) {
    let frac = if quota > 0 { (used as f32 / quota as f32).clamp(0.0, 1.0) } else { 0.0 };
    p.rect_filled(bar, bar.height() / 2.0, track);
    let mut fill = bar;
    fill.set_width(bar.width() * frac);
    p.rect_filled(fill, bar.height() / 2.0, if frac > 0.95 { DANGER } else { ACCENT });
}

fn section_label(ui: &mut egui::Ui, text: &str, right: Option<&mut dyn FnMut(&mut egui::Ui)>) {
    let full = ui.max_rect();
    egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 12, top: 14, bottom: 6 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(theme::label(text));
            if let Some(f) = right {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| f(ui));
            }
        });
    });
    ui.painter().hline(full.x_range(), ui.cursor().top(), Stroke::new(1.0, BORDER));
}

/// Ganze Zeile zum Anklicken (Konto-Tab).
fn list_row(ui: &mut egui::Ui, icon: Icon, text: &str, color: Color32, icon_color: Color32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, 0.0, RAISED);
    }
    p.hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, RAISED));
    paint_icon_at(p, Rect::from_center_size(Pos2::new(r.left() + 20.0, r.center().y), Vec2::splat(16.0)), icon, icon_color);
    p.text(Pos2::new(r.left() + 38.0, r.center().y), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(13.0), color);
    resp
}

/// Einstellungszeile: Text links, Bedienelement rechts.
fn setting_row(ui: &mut egui::Ui, text: &str, sub: Option<&str>, add: impl FnOnce(&mut egui::Ui)) {
    let h = if sub.is_some() { 50.0 } else { 44.0 };
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    let p = ui.painter();
    p.hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, RAISED));
    if let Some(sub) = sub {
        p.text(Pos2::new(r.left() + 12.0, r.center().y - 8.0), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(13.0), TEXT);
        p.text(Pos2::new(r.left() + 12.0, r.center().y + 9.0), egui::Align2::LEFT_CENTER, sub, egui::FontId::proportional(10.5), TEXT_DIM);
    } else {
        p.text(Pos2::new(r.left() + 12.0, r.center().y), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(13.0), TEXT);
    }
    let right = Rect::from_min_max(Pos2::new(r.center().x, r.top()), Pos2::new(r.right() - 12.0, r.bottom()));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(right).layout(egui::Layout::right_to_left(egui::Align::Center)));
    add(&mut c);
}

/// Kleines Etikett in der Freigabe-Zeile.
fn chip(ui: &mut egui::Ui, icon: Icon, text: &str, fg: Color32, bg: Color32) {
    let font = egui::FontId::proportional(10.5);
    let g = ui.painter().layout_no_wrap(text.to_string(), font, fg);
    let (r, _) = ui.allocate_exact_size(Vec2::new(g.size().x + 23.0, 18.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, 4.0, bg);
    paint_icon_at(p, Rect::from_center_size(Pos2::new(r.left() + 10.0, r.center().y), Vec2::splat(10.0)), icon, fg);
    p.galley(Pos2::new(r.left() + 18.0, r.center().y - g.size().y / 2.0), g, fg);
}

fn share_row(ui: &mut egui::Ui, view: &mut SharesView, sh: &Share, url: &str, acc: &Account, ctx: &egui::Context) {
    let open = view.expanded.as_deref() == Some(sh.token.as_str());
    let width = ui.available_width();
    let (row, resp) = ui.allocate_exact_size(Vec2::new(width, 56.0), Sense::click());
    let p = ui.painter().clone();
    if resp.hovered() && !open {
        p.rect_filled(row, 0.0, RAISED);
    }
    let name = sh.path.rsplit('/').next().unwrap_or(&sh.path).to_string();
    let is_file = name.contains('.') && !sh.path.ends_with('/');
    let ic = Rect::from_min_size(Pos2::new(row.left() + 12.0, row.center().y - 16.0), Vec2::splat(32.0));
    p.rect_filled(ic, 4.0, BG);
    paint_icon_at(&p, Rect::from_center_size(ic.center(), Vec2::splat(16.0)), if is_file { Icon::File } else { Icon::Folder }, ACCENT);
    let chev = Rect::from_center_size(Pos2::new(row.right() - 22.0, row.center().y), Vec2::new(24.0, 32.0));
    let copy = Rect::from_center_size(Pos2::new(chev.left() - 18.0, row.center().y), Vec2::splat(32.0));
    let text_r = Rect::from_min_max(Pos2::new(ic.right() + 10.0, row.top() + 8.0), Pos2::new(copy.left() - 8.0, row.bottom() - 6.0));
    let mut t = ui.new_child(egui::UiBuilder::new().max_rect(text_r).layout(egui::Layout::top_down(egui::Align::Min)));
    t.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
    t.add(egui::Label::new(RichText::new(&name).color(TEXT)).truncate().selectable(false)).on_hover_text(&sh.path);
    t.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if sh.allow_upload {
            chip(ui, Icon::Upload, "Upload", ACCENT, ACCENT_DIM);
        }
        if sh.has_password {
            chip(ui, Icon::Lock, crate::i18n::t("Password", "Passwort"), TEXT_DIM, RAISED);
        }
        if let Some(at) = sh.expires_at {
            let left = at - chrono::Utc::now().timestamp_millis();
            let (txt, c) = if left <= 0 {
                (crate::i18n::t("expired", "abgelaufen").to_string(), DANGER)
            } else if left < 86_400_000 {
                (crate::i18n::t("expires today", "läuft heute ab").to_string(), WARN)
            } else {
                (tf!("{} days left", "noch {} Tage", left / 86_400_000), TEXT_DIM)
            };
            chip(ui, Icon::Clock, &txt, c, RAISED);
        }
        if sh.notify {
            chip(ui, Icon::Bell, "", TEXT_DIM, RAISED);
        }
    });
    if icon_button(ui, copy, Icon::Copy, crate::i18n::t("Copy link", "Link kopieren"), false, &sh.token).clicked() && copy_to_clipboard(url) {
        view.info = Some((true, tf!("Link for “{name}” copied", "Link für „{name}“ kopiert")));
    }
    paint_icon_at(&p, Rect::from_center_size(chev.center(), Vec2::splat(14.0)), if open { Icon::Chevron } else { Icon::ChevronRight }, TEXT_DIM);
    if resp.clicked() {
        view.expanded = if open { None } else { Some(sh.token.clone()) };
        view.pw_edit = None;
        view.confirm_delete = None;
    }

    if open {
        egui::Frame::new().outer_margin(egui::Margin { left: 12, right: 12, top: 0, bottom: 12 }).inner_margin(egui::Margin::symmetric(12, 6)).fill(RAISED).corner_radius(4).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(url).size(10.5).color(ACCENT)).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.link(RichText::new(crate::i18n::t("Open", "Öffnen")).size(11.0).color(ACCENT)).clicked() {
                        open_url(url);
                    }
                });
            });
            let line = |ui: &mut egui::Ui| {
                let y = ui.cursor().top();
                ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, BORDER));
            };
            line(ui);
            let row = |ui: &mut egui::Ui, icon: Icon, text: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
                ui.horizontal(|ui| {
                    ui.set_min_height(32.0);
                    paint_icon(ui, icon, TEXT_DIM);
                    ui.label(RichText::new(text).color(TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| add(ui));
                });
            };
            let mut upd: Option<ShareSettings> = None;
            row(ui, Icon::Upload, crate::i18n::t("Guests may upload", "Gäste dürfen hochladen"), &mut |ui| {
                let mut v = sh.allow_upload;
                if toggle(ui, &mut v).changed() {
                    upd = Some(ShareSettings { allow_upload: Some(v), ..Default::default() });
                }
            });
            row(ui, Icon::Bell, crate::i18n::t("Notify", "Benachrichtigen"), &mut |ui| {
                let mut v = sh.notify;
                if toggle(ui, &mut v).changed() {
                    upd = Some(ShareSettings { notify: Some(v), ..Default::default() });
                }
            });
            let editing = view.pw_edit.as_ref().map(|(t, _)| t == &sh.token).unwrap_or(false);
            let mut start_edit = false;
            row(ui, Icon::Lock, crate::i18n::t("Password", "Passwort"), &mut |ui| {
                if editing {
                    return;
                }
                if sh.has_password && ui.link(RichText::new(crate::i18n::t("Remove", "Entfernen")).size(12.0).color(DANGER)).clicked() {
                    upd = Some(ShareSettings { password: Some(String::new()), ..Default::default() });
                }
                if ui.link(RichText::new(if sh.has_password { crate::i18n::t("Change", "Ändern") } else { crate::i18n::t("Set", "Festlegen") }).size(12.0).color(ACCENT)).clicked() {
                    start_edit = true;
                }
            });
            if start_edit {
                view.pw_edit = Some((sh.token.clone(), String::new()));
            }
            let mut save_pw: Option<String> = None;
            let mut cancel_pw = false;
            if let Some((t, pw)) = view.pw_edit.as_mut() {
                if t == &sh.token {
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(pw).password(true).hint_text(crate::i18n::t("at least 4 characters", "mind. 4 Zeichen")).desired_width(150.0));
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (ui.add_enabled(pw.len() >= 4, egui::Button::new(crate::i18n::t("Save", "Speichern"))).clicked() || enter) && pw.len() >= 4 {
                            save_pw = Some(pw.clone());
                        }
                        if ui.button(crate::i18n::t("Cancel", "Abbrechen")).clicked() {
                            cancel_pw = true;
                        }
                    });
                }
            }
            if cancel_pw {
                view.pw_edit = None;
            }
            if let Some(pw) = save_pw {
                view.pw_edit = None;
                upd = Some(ShareSettings { password: Some(pw), ..Default::default() });
            }
            row(ui, Icon::Clock, crate::i18n::t("Valid", "Gültig"), &mut |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (label, days) in [(crate::i18n::t("30 d", "30 T"), 30i64), (crate::i18n::t("7 d", "7 T"), 7), (crate::i18n::t("1 d", "1 T"), 1), (crate::i18n::t("Always", "Immer"), 0)] {
                    let sel = days == 0 && sh.expires_at.is_none();
                    let b = egui::Button::new(RichText::new(label).size(11.0).color(if sel { ACCENT } else { TEXT_DIM }))
                        .fill(if sel { ACCENT_DIM } else { BG })
                        .min_size(Vec2::new(36.0, 24.0))
                        .corner_radius(0);
                    if ui.add(b).clicked() {
                        let at = if days == 0 { 0 } else { chrono::Utc::now().timestamp_millis() + days * 86_400_000 };
                        upd = Some(ShareSettings { expires_at: Some(at), ..Default::default() });
                    }
                }
            });
            if let Some(at) = sh.expires_at {
                ui.label(RichText::new(expiry_text(at)).size(10.5).color(TEXT_DIM));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if view.confirm_delete.as_deref() == Some(sh.token.as_str()) {
                    if ui.add(egui::Button::new(RichText::new(crate::i18n::t("Really end", "Wirklich beenden")).color(TEXT)).fill(DANGER)).clicked() {
                        view.confirm_delete = None;
                        view.expanded = None;
                        shares_delete(view, acc, ctx, sh.token.clone());
                    }
                    if ui.button(crate::i18n::t("Cancel", "Abbrechen")).clicked() {
                        view.confirm_delete = None;
                    }
                } else if ui.add(egui::Button::new(RichText::new(crate::i18n::t("End link", "Freigabe beenden")).color(DANGER)).fill(BG)).clicked() {
                    view.confirm_delete = Some(sh.token.clone());
                }
            }));
            if let Some(s) = upd {
                shares_update(view, acc, ctx, sh.token.clone(), s);
            }
        });
    }
    let y = ui.cursor().top();
    ui.painter().hline(ui.max_rect().x_range(), y - 0.5, Stroke::new(1.0, BORDER));
}

fn transfer_panel(ui: &mut egui::Ui, p: &Progress, speed: f64) {
    let files = p.remaining_files();
    let bytes = p.remaining_bytes();
    ui.label(
        RichText::new(tf!("Syncing {} {}", "{} {} werden synchronisiert", files, if files == 1 { t("file", "Datei") } else { t("files", "Dateien") }))
            .size(15.0)
            .color(ACCENT),
    );
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if p.uploading() || !p.downloading() {
            paint_small(ui, Icon::Up);
        }
        if p.downloading() {
            paint_small(ui, Icon::Down);
        }
        ui.label(RichText::new(if speed > 1.0 { format!("{}/s", human_size(speed as i64)) } else { "–".into() }).size(10.5).color(TEXT_DIM));
        ui.add_space(10.0);
        paint_small(ui, Icon::Clock);
        let rest = if speed > 1.0 && bytes > 0 { eta(bytes as f64 / speed) } else { human_size(bytes as i64) };
        ui.label(RichText::new(rest).size(10.5).color(TEXT_DIM));
    });
    ui.add_space(2.0);
    // Laufende Übertragungen (mehrere gleichzeitig), dann die Warteschlange.
    for c in &p.active {
        let frac = if c.total > 0 { (c.done as f32 / c.total as f32).clamp(0.0, 1.0) } else { 1.0 };
        let name = c.path.rsplit('/').next().unwrap_or(&c.path);
        let detail = if c.done == 0 && c.upload { t("checking", "wird geprüft").to_string() } else { format!("{} / {}", human_size(c.done as i64), human_size(c.total as i64)) };
        transfer_row(ui, name, &c.path, &detail, frac, c.upload);
    }
    let shown_queue = 4usize.saturating_sub(p.active.len()).max(1);
    for q in p.queue.iter().take(shown_queue) {
        let name = q.path.rsplit('/').next().unwrap_or(&q.path);
        transfer_row(ui, name, &q.path, &human_size(q.size as i64), 0.0, q.upload);
    }
    if p.queue.len() > shown_queue {
        ui.label(RichText::new(tf!("and {} more", "und {} weitere", p.queue.len() - shown_queue)).size(10.5).color(TEXT_DIM));
    }
}

fn paint_small(ui: &mut egui::Ui, icon: Icon) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    paint_icon_at(ui.painter(), r, icon, TEXT_DIM);
}

fn transfer_row(ui: &mut egui::Ui, name: &str, path: &str, detail: &str, frac: f32, upload: bool) {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());
    let _ = resp.on_hover_text(path);
    let p = ui.painter();
    let ic = Rect::from_min_size(Pos2::new(r.left(), r.center().y - 16.0), Vec2::splat(32.0));
    p.rect_filled(ic, 4.0, RAISED);
    paint_icon_at(p, Rect::from_center_size(ic.center(), Vec2::splat(16.0)), file_icon(name), TEXT_DIM);
    let dir_r = Rect::from_center_size(Pos2::new(r.right() - 7.0, r.center().y), Vec2::splat(14.0));
    paint_icon_at(p, dir_r, if upload { Icon::Up } else { Icon::Down }, ACCENT);
    let tx = ic.right() + 10.0;
    let tw = dir_r.left() - 10.0 - tx;
    let dg = p.layout_no_wrap(detail.to_string(), egui::FontId::proportional(10.5), TEXT_DIM);
    let dw = dg.size().x;
    p.galley(Pos2::new(tx + tw - dw, r.center().y - 13.0), dg, TEXT_DIM);
    let clip = p.with_clip_rect(Rect::from_min_max(Pos2::new(tx, r.top()), Pos2::new(tx + tw - dw - 8.0, r.bottom())));
    clip.text(Pos2::new(tx, r.center().y - 6.0), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(13.0), TEXT);
    let bar = Rect::from_min_size(Pos2::new(tx, r.center().y + 7.0), Vec2::new(tw, 4.0));
    p.rect_filled(bar, 2.0, BG);
    let mut fill = bar;
    fill.set_width(bar.width() * frac);
    p.rect_filled(fill, 2.0, ACCENT);
}

fn file_icon(name: &str) -> Icon {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "bmp" => Icon::Image,
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "mp3" | "wav" | "flac" | "m4a" | "ogg" => Icon::Media,
        _ => Icon::File,
    }
}

fn shares_spawn(view: &mut SharesView, acc: &Account, ctx: &egui::Context, f: impl FnOnce(&Api) -> Result<SharesMsg, ApiError> + Send + 'static) {
    view.loading = true;
    let inbox = view.inbox.clone();
    let ctx = ctx.clone();
    let api = admin_api(acc);
    std::thread::spawn(move || {
        let msg = match api.and_then(|a| f(&a).map_err(api_msg)) {
            Ok(m) => m,
            Err(e) => SharesMsg::Failed(e),
        };
        *inbox.lock().unwrap() = Some(msg);
        ctx.request_repaint();
    });
}

fn shares_reload(view: &mut SharesView, acc: &Account, ctx: &egui::Context) {
    shares_spawn(view, acc, ctx, |api| api.list_shares().map(SharesMsg::List));
}

fn shares_create(view: &mut SharesView, acc: &Account, ctx: &egui::Context, remote: String) {
    view.info = None;
    shares_spawn(view, acc, ctx, move |api| match api.create_share(&remote) {
        Ok(s) => Ok(SharesMsg::Created(api.share_url(&s.token), s.token.clone())),
        Err(ApiError::NotFound) => Ok(SharesMsg::Failed(t("Not on the server yet – please wait until it is uploaded.", "Noch nicht auf dem Server – bitte warten, bis es hochgeladen ist.").into())),
        Err(e) => Err(e),
    });
}

fn shares_update(view: &mut SharesView, acc: &Account, ctx: &egui::Context, token: String, settings: ShareSettings) {
    shares_spawn(view, acc, ctx, move |api| api.update_share(&token, &settings).map(|_| SharesMsg::Updated(t("Saved", "Gespeichert").into())));
}

fn shares_delete(view: &mut SharesView, acc: &Account, ctx: &egui::Context, token: String) {
    shares_spawn(view, acc, ctx, move |api| api.delete_share(&token).map(|_| SharesMsg::Deleted));
}

/// Lokaler Pfad im Sync-Ordner -> Server-Pfad. None, wenn außerhalb.
pub(crate) fn remote_path(sync_dir: &std::path::Path, p: &std::path::Path) -> Option<String> {
    let rel: PathBuf = match p.strip_prefix(sync_dir) {
        Ok(r) => r.to_path_buf(),
        // Unter Windows Groß-/Kleinschreibung ignorieren.
        Err(_) if cfg!(windows) => {
            let d = sync_dir.to_string_lossy().to_lowercase();
            let d = d.trim_end_matches(['\\', '/']);
            let full = p.to_string_lossy().to_string();
            let lower = full.to_lowercase();
            // Nur ganze Ordnernamen: "DropCraft" passt nicht auf "DropCraft ocv".
            let boundary = lower.get(d.len()..).map(|r| r.is_empty() || r.starts_with(['\\', '/'])).unwrap_or(false);
            if !lower.starts_with(d) || !boundary {
                return None;
            }
            PathBuf::from(full.get(d.len()..)?.trim_start_matches(['\\', '/']))
        }
        Err(_) => return None,
    };
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    let r = parts?.join("/");
    (!r.is_empty()).then_some(r)
}

pub(crate) fn copy_to_clipboard(text: &str) -> bool {
    arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())).is_ok()
}

pub(crate) fn open_url(url: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Öffnet die Konto-Website des Servers, gleich angemeldet.
fn open_account_site(acc: &Account, admin: bool) {
    let (server, token) = (acc.server.clone(), acc.token.clone());
    std::thread::spawn(move || {
        let url = Api::new(&server, token)
            .and_then(|api| api.web_login_url(if admin { "admin" } else { "" }))
            .unwrap_or_else(|_| format!("{}/account", server.trim_end_matches('/')));
        open_url(&url);
    });
}

fn admin_api(acc: &Account) -> Result<Api, String> {
    Api::new(&acc.server, acc.token.clone()).map_err(|e| e.to_string())
}

fn api_msg(e: ApiError) -> String {
    match e {
        ApiError::Conflict(m) | ApiError::Server { message: m, .. } if !m.is_empty() => m,
        ApiError::Unauthorized => t("Not allowed – please sign in again", "Keine Berechtigung – bitte neu anmelden").into(),
        other => other.to_string(),
    }
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
        t("under 1 min", "unter 1 Min.").into()
    } else if s < 3600 {
        tf!("about {} min", "ca. {} Min.", s.div_ceil(60))
    } else {
        tf!("about {} h {} min", "ca. {} Std. {} Min.", s / 3600, (s % 3600) / 60)
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
        Phase::Starting => (Icon::Sync, ACCENT, t("Starting …", "Startet …").into()),
        Phase::Syncing => (Icon::Sync, ACCENT, t("Syncing …", "Synchronisiere …").into()),
        Phase::UpToDate => (
            Icon::Check,
            OK,
            match last_sync {
                Some(t) => tf!("Everything synced · {}", "Alles synchronisiert · {}", t.format("%H:%M")),
                None => t("Everything synced", "Alles synchronisiert").into(),
            },
        ),
        Phase::Paused => (Icon::Pause, TEXT_DIM, t("Paused", "Pausiert").into()),
        Phase::Offline(_) => (Icon::Offline, WARN, t("Server not reachable", "Server nicht erreichbar").into()),
        Phase::Error(e) => (Icon::Warning, DANGER, e.clone()),
        Phase::NeedsLogin => (Icon::Warning, WARN, t("Please sign in again", "Bitte neu anmelden").into()),
    }
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
            Err(ApiError::Unauthorized) => Err(t("Wrong username or password", "Benutzername oder Passwort falsch").to_string()),
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

/// Fasst Ereignisse derselben Datei zusammen (neuestes zuerst). Die Reihenfolge
/// richtet sich nach dem jeweils neuesten Ereignis.
fn group_activity(events: &[dropcraft_core::Event]) -> Vec<Vec<&dropcraft_core::Event>> {
    let mut groups: Vec<Vec<&dropcraft_core::Event>> = Vec::new();
    let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for ev in events {
        match index.get(ev.path.as_str()) {
            Some(&i) => groups[i].push(ev),
            None => {
                index.insert(ev.path.as_str(), groups.len());
                groups.push(vec![ev]);
            }
        }
    }
    groups
}

fn event_style(kind: EventKind) -> (Icon, Color32, &'static str) {
    match kind {
        EventKind::Uploaded => (Icon::Up, ACCENT, t("uploaded", "hochgeladen")),
        EventKind::Downloaded => (Icon::Down, OK, t("downloaded", "heruntergeladen")),
        EventKind::NewOnline => (Icon::Cloud, TEXT_DIM, t("online only", "nur online")),
        EventKind::DeletedLocal => (Icon::Trash, TEXT_DIM, t("deleted here", "hier gelöscht")),
        EventKind::DeletedRemote => (Icon::Trash, TEXT_DIM, t("deleted", "gelöscht")),
        EventKind::Conflict => (Icon::Warning, WARN, t("conflict", "Konflikt")),
        EventKind::Error => (Icon::Warning, DANGER, t("error", "Fehler")),
    }
}

fn event_time(ev: &dropcraft_core::Event) -> String {
    if ev.at.date_naive() == chrono::Local::now().date_naive() {
        ev.at.format("%H:%M").to_string()
    } else {
        ev.at.format("%d.%m. %H:%M").to_string()
    }
}

/// Eine Zeile pro Datei; mehrere Änderungen zeigt der Hover-Text einzeln.
fn activity_row(ui: &mut egui::Ui, group: &[&dropcraft_core::Event]) {
    let ev = group[0];
    let (icon, color, verb) = event_style(ev.kind);
    let name = ev.path.rsplit('/').next().unwrap_or(&ev.path);
    let when = if ev.at.date_naive() == chrono::Local::now().date_naive() { ev.at.format("%H:%M").to_string() } else { ev.at.format("%d.%m.").to_string() };
    let who = match &ev.by {
        Some(by) if matches!(ev.kind, EventKind::Downloaded | EventKind::NewOnline | EventKind::DeletedLocal) => by.clone(),
        _ => verb.to_string(),
    };
    let detail = format!("{who} · {when}");
    let mut hover = ev.path.clone();
    for e in group {
        let (_, _, v) = event_style(e.kind);
        hover.push_str(&format!("\n{}  {v}", event_time(e)));
        if !e.detail.is_empty() {
            hover.push_str(&format!(" · {}", e.detail));
        }
        if let Some(by) = &e.by {
            hover.push_str(&tf!(" · by {by}", " · von {by}"));
        }
    }
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::hover());
    let _ = resp.on_hover_text(hover);
    let p = ui.painter();
    p.hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, RAISED));
    paint_icon_at(p, Rect::from_center_size(Pos2::new(r.left() + 19.0, r.center().y), Vec2::splat(14.0)), icon, color);
    let dg = p.layout_no_wrap(detail, egui::FontId::proportional(10.5), TEXT_DIM);
    let dw = dg.size().x;
    let mut dx = r.right() - 12.0 - dw;
    p.galley(Pos2::new(dx, r.center().y - dg.size().y / 2.0), dg, TEXT_DIM);
    // Mehrere Änderungen: kleines Zähl-Etikett vor der Zeit
    if group.len() > 1 {
        let cg = p.layout_no_wrap(format!("{}×", group.len()), egui::FontId::proportional(10.5), TEXT_DIM);
        let badge = Rect::from_center_size(Pos2::new(dx - 8.0 - (cg.size().x + 10.0) / 2.0, r.center().y), Vec2::new(cg.size().x + 10.0, 16.0));
        p.rect_filled(badge, 4.0, RAISED);
        p.galley(Pos2::new(badge.left() + 5.0, badge.center().y - cg.size().y / 2.0), cg, TEXT_DIM);
        dx = badge.left();
    }
    let tx = r.left() + 36.0;
    let clip = p.with_clip_rect(Rect::from_min_max(Pos2::new(tx, r.top()), Pos2::new(dx - 10.0, r.bottom())));
    clip.text(Pos2::new(tx, r.center().y), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(13.0), TEXT);
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
    Play,
    Warning,
    Offline,
    Up,
    Down,
    Trash,
    Folder,
    File,
    Image,
    Media,
    Chevron,
    ChevronRight,
    Pulse,
    Link,
    User,
    Gear,
    Copy,
    Upload,
    Lock,
    Clock,
    Bell,
    Plus,
    Logout,
    Power,
    Cloud,
    Close,
}

fn paint_icon(ui: &mut egui::Ui, icon: Icon, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
    paint_icon_at(ui.painter(), rect, icon, color);
}

/// Linien-Symbole im 16er-Raster (Stil wie Phosphor).
fn paint_icon_at(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    let k = r.width() / 16.0;
    let s = Stroke::new((1.4 * k).max(1.1), color);
    let at = |x: f32, y: f32| Pos2::new(r.left() + x * k, r.top() + y * k);
    let poly = |pts: &[(f32, f32)]| pts.iter().map(|&(x, y)| at(x, y)).collect::<Vec<_>>();
    let closed = |pts: &[(f32, f32)]| egui::Shape::closed_line(poly(pts), s);
    let line = |pts: &[(f32, f32)]| egui::Shape::line(poly(pts), s);
    let circle = |x: f32, y: f32, rad: f32| egui::Shape::circle_stroke(at(x, y), rad * k, s);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32, rr: f32| egui::Shape::rect_stroke(Rect::from_min_max(at(x0, y0), at(x1, y1)), rr * k, s, egui::StrokeKind::Middle);
    let arc = |cx: f32, cy: f32, rad: f32, a0: f32, a1: f32| {
        let n = 16;
        let pts: Vec<Pos2> = (0..=n).map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            at(cx + rad * a.cos(), cy + rad * a.sin())
        }).collect();
        egui::Shape::line(pts, s)
    };
    use std::f32::consts::PI;
    let shapes: Vec<egui::Shape> = match icon {
        Icon::Check => vec![circle(8.0, 8.0, 6.5), line(&[(5.0, 8.3), (7.2, 10.5), (11.0, 6.0)])],
        Icon::Sync => vec![
            arc(8.0, 8.0, 5.0, PI * 1.1, PI * 1.9),
            line(&[(12.6, 3.0), (12.8, 5.6), (10.2, 5.8)]),
            arc(8.0, 8.0, 5.0, PI * 0.1, PI * 0.9),
            line(&[(3.4, 13.0), (3.2, 10.4), (5.8, 10.2)]),
        ],
        Icon::Pause => vec![line(&[(5.5, 3.5), (5.5, 12.5)]), line(&[(10.5, 3.5), (10.5, 12.5)])],
        Icon::Play => vec![closed(&[(5.0, 3.0), (13.0, 8.0), (5.0, 13.0)])],
        Icon::Warning => vec![
            closed(&[(8.0, 1.8), (14.8, 14.0), (1.2, 14.0)]),
            line(&[(8.0, 6.2), (8.0, 9.6)]),
            egui::Shape::circle_filled(at(8.0, 11.8), 0.9 * k, color),
        ],
        Icon::Offline => vec![circle(8.0, 8.0, 6.3), line(&[(3.5, 3.5), (12.5, 12.5)])],
        Icon::Up => vec![line(&[(8.0, 13.0), (8.0, 3.0)]), line(&[(4.0, 7.0), (8.0, 3.0), (12.0, 7.0)])],
        Icon::Down => vec![line(&[(8.0, 3.0), (8.0, 13.0)]), line(&[(4.0, 9.0), (8.0, 13.0), (12.0, 9.0)])],
        Icon::Trash => vec![
            line(&[(2.5, 4.5), (13.5, 4.5)]),
            line(&[(6.0, 4.5), (6.0, 2.5), (10.0, 2.5), (10.0, 4.5)]),
            line(&[(4.0, 4.5), (4.8, 13.5), (11.2, 13.5), (12.0, 4.5)]),
        ],
        Icon::Folder => vec![closed(&[(1.8, 3.5), (6.0, 3.5), (7.5, 5.2), (14.2, 5.2), (14.2, 12.8), (1.8, 12.8)])],
        Icon::File => vec![closed(&[(3.5, 1.5), (9.5, 1.5), (12.5, 4.5), (12.5, 14.5), (3.5, 14.5)]), line(&[(9.5, 1.5), (9.5, 4.5), (12.5, 4.5)])],
        Icon::Image => vec![rect(2.0, 3.0, 14.0, 13.0, 1.0), circle(6.0, 6.5, 1.1), line(&[(2.5, 12.0), (6.0, 8.5), (8.5, 11.0), (10.5, 9.0), (13.5, 12.0)])],
        Icon::Media => vec![rect(2.0, 3.0, 14.0, 13.0, 1.0), closed(&[(6.5, 6.0), (10.5, 8.0), (6.5, 10.0)])],
        Icon::Chevron => vec![line(&[(4.0, 6.0), (8.0, 10.0), (12.0, 6.0)])],
        Icon::ChevronRight => vec![line(&[(6.0, 4.0), (10.0, 8.0), (6.0, 12.0)])],
        Icon::Pulse => vec![line(&[(1.5, 8.0), (4.5, 8.0), (6.5, 3.5), (9.5, 12.5), (11.5, 8.0), (14.5, 8.0)])],
        Icon::Link => vec![
            line(&[(6.3, 9.7), (9.7, 6.3)]),
            line(&[(7.5, 4.6), (8.9, 3.2), (11.0, 2.6), (13.0, 3.6), (13.4, 5.0), (12.8, 7.0), (11.4, 8.4)]),
            line(&[(8.5, 11.4), (7.1, 12.8), (5.0, 13.4), (3.0, 12.4), (2.6, 11.0), (3.2, 9.0), (4.6, 7.6)]),
        ],
        Icon::User => vec![circle(8.0, 5.5, 2.8), arc(8.0, 14.0, 5.2, PI, PI * 2.0)],
        Icon::Gear => {
            let mut v = vec![circle(8.0, 8.0, 2.3), circle(8.0, 8.0, 5.0)];
            for i in 0..8 {
                let a = i as f32 * PI / 4.0;
                v.push(line(&[(8.0 + 5.0 * a.cos(), 8.0 + 5.0 * a.sin()), (8.0 + 6.8 * a.cos(), 8.0 + 6.8 * a.sin())]));
            }
            v
        }
        Icon::Copy => vec![rect(5.5, 5.5, 13.5, 13.5, 1.0), line(&[(10.5, 5.5), (10.5, 2.5), (2.5, 2.5), (2.5, 10.5), (5.5, 10.5)])],
        Icon::Upload => vec![line(&[(8.0, 10.5), (8.0, 2.5)]), line(&[(5.0, 5.5), (8.0, 2.5), (11.0, 5.5)]), line(&[(2.5, 10.5), (2.5, 13.5), (13.5, 13.5), (13.5, 10.5)])],
        Icon::Lock => vec![rect(3.5, 7.0, 12.5, 13.5, 1.0), arc(8.0, 5.2, 2.5, PI, PI * 2.0), line(&[(5.5, 5.2), (5.5, 7.0)]), line(&[(10.5, 5.2), (10.5, 7.0)])],
        Icon::Clock => vec![circle(8.0, 8.0, 6.0), line(&[(8.0, 4.5), (8.0, 8.0), (10.5, 9.5)])],
        Icon::Bell => vec![
            arc(8.0, 7.0, 4.0, PI, PI * 2.0),
            line(&[(4.0, 7.0), (4.0, 11.5), (3.0, 12.5), (13.0, 12.5), (12.0, 11.5), (12.0, 7.0)]),
            line(&[(6.8, 14.2), (9.2, 14.2)]),
        ],
        Icon::Plus => vec![line(&[(8.0, 3.0), (8.0, 13.0)]), line(&[(3.0, 8.0), (13.0, 8.0)])],
        Icon::Close => vec![line(&[(3.5, 3.5), (12.5, 12.5)]), line(&[(12.5, 3.5), (3.5, 12.5)])],
        Icon::Logout => vec![line(&[(6.5, 2.5), (2.5, 2.5), (2.5, 13.5), (6.5, 13.5)]), line(&[(10.0, 5.0), (13.0, 8.0), (10.0, 11.0)]), line(&[(13.0, 8.0), (6.0, 8.0)])],
        Icon::Power => vec![arc(8.0, 8.6, 5.4, -PI * 0.3, PI * 1.3), line(&[(8.0, 2.0), (8.0, 7.5)])],
        Icon::Cloud => vec![
            arc(5.0, 9.5, 3.0, PI * 0.5, PI * 1.45),
            arc(8.3, 7.0, 4.0, PI * 1.05, PI * 1.85),
            arc(11.8, 9.6, 2.9, PI * 1.5, PI * 2.5),
            line(&[(5.0, 12.5), (11.8, 12.5)]),
        ],
    };
    for sh in shapes {
        p.add(sh);
    }
}

/// Das App-Logo als Textur (einmal gezeichnet, dann aus dem Speicher).
fn logo_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let id = egui::Id::unique("dropcraft-logo");
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return t;
    }
    let n = 128;
    let img = egui::ColorImage::from_rgba_unmultiplied([n, n], &crate::icons::logo_rgba(n as u32));
    let t = ctx.load_texture("dropcraft-logo", img, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        crate::i18n::apply("de");
        assert_eq!(fmt_gb(100.0), "100");
        assert_eq!(fmt_gb(2.5), "2,5");
        assert_eq!(eta(30.0), "unter 1 Min.");
        assert_eq!(eta(125.0), "ca. 3 Min.");
        assert_eq!(eta(3700.0), "ca. 1 Std. 1 Min.");
        assert_eq!(normalize_server("cloud.example.de/"), "https://cloud.example.de");
        let root = std::path::Path::new("/home/j/DropCraft");
        assert_eq!(remote_path(root, std::path::Path::new("/home/j/DropCraft/Fotos/Urlaub")).as_deref(), Some("Fotos/Urlaub"));
        assert_eq!(remote_path(root, std::path::Path::new("/home/j/Andere")), None);
        assert_eq!(remote_path(root, root), None);
        assert_eq!(remote_path(root, std::path::Path::new("/home/j/DropCraft ocv/x.txt")), None);
    }
}

/// Zeitpunkt einer Benachrichtigung: heute nur Uhrzeit, sonst Datum.
fn note_time(at_ms: i64) -> String {
    let Some(d) = chrono::DateTime::from_timestamp_millis(at_ms) else { return String::new() };
    let d = d.with_timezone(&chrono::Local);
    if d.date_naive() == chrono::Local::now().date_naive() {
        d.format("%H:%M").to_string()
    } else if crate::i18n::german() {
        d.format("%d.%m.%Y, %H:%M").to_string()
    } else {
        d.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// "bis 18.11.2026, 23:57 · noch 6 Tage" bzw. "abgelaufen".
fn expiry_text(at_ms: i64) -> String {
    let Some(d) = chrono::DateTime::from_timestamp_millis(at_ms) else { return String::new() };
    let left = at_ms - chrono::Utc::now().timestamp_millis();
    if left <= 0 {
        return t("expired", "abgelaufen").into();
    }
    let hours = left / 3_600_000;
    let rest = match hours {
        0 => t("under 1 hour", "unter 1 Stunde").to_string(),
        1 => t("1 hour left", "noch 1 Stunde").to_string(),
        2..=47 => tf!("{hours} hours left", "noch {hours} Stunden"),
        _ => tf!("{} days left", "noch {} Tage", hours / 24),
    };
    tf!("until {} · {rest}", "bis {} · {rest}", d.with_timezone(&chrono::Local).format("%d.%m.%Y, %H:%M"))
}
