//! Installation unter Windows: dieselbe `DropCraft.exe` ist Programm und
//! Installer. Wird sie von woanders gestartet (z. B. aus „Downloads“), bietet
//! sie an, sich nach `%LOCALAPPDATA%\Programs\DropCraft` zu kopieren – ohne
//! Adminrechte, damit das Selbst-Update (siehe `updater`) die Datei später
//! austauschen kann. Dazu kommen ein Eintrag unter „Apps“ mit Deinstallation,
//! die Startmenü-Verknüpfung und „Mit Windows starten“.
//!
//! Befehlszeile:
//!   DropCraft.exe --install            ohne Rückfrage installieren und starten
//!   DropCraft.exe --uninstall [--quiet] deinstallieren

/// Was `main` nach der Installationsprüfung tun soll.
pub enum Next {
    /// Normal weiterstarten (installierte oder portable Kopie).
    Run,
    /// Programm beenden (installiert und neu gestartet, abgebrochen, deinstalliert).
    Exit,
}

#[cfg(not(windows))]
pub fn handle_args(_args: &[String]) -> Next {
    Next::Run
}

#[cfg(not(windows))]
pub fn on_start() {}

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use std::{
        io::Write,
        net::{TcpListener, TcpStream},
        os::windows::process::CommandExt,
        path::{Path, PathBuf},
        process::Command,
        time::Duration,
    };

    use winreg::{
        enums::{HKEY_CURRENT_USER, KEY_WRITE},
        RegKey,
    };

    use super::Next;
    use crate::{i18n::t, updater, INSTANCE_PORT};

    const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\DropCraft";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// `%LOCALAPPDATA%\Programs\DropCraft`
    pub fn install_dir() -> PathBuf {
        dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Programs").join("DropCraft")
    }

    pub fn installed_exe() -> PathBuf {
        install_dir().join("DropCraft.exe")
    }

    fn same_file(a: &Path, b: &Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(x), Ok(y)) => x == y,
            _ => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
        }
    }

    /// Läuft gerade die installierte Kopie?
    pub fn is_installed_copy() -> bool {
        std::env::current_exe().map(|e| same_file(&e, &installed_exe())).unwrap_or(false)
    }

    /// Version der installierten Kopie laut „Apps“-Eintrag.
    fn installed_version() -> Option<String> {
        if !installed_exe().exists() {
            return None;
        }
        let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(UNINSTALL_KEY).ok()?;
        key.get_value::<String, _>("DisplayVersion").ok()
    }

    /// Wird ganz am Anfang von `main` aufgerufen.
    pub fn handle_args(args: &[String]) -> Next {
        let has = |f: &str| args.iter().any(|a| a == f);
        if has("--uninstall") {
            uninstall(has("--quiet"));
            return Next::Exit;
        }
        if has("--setup-explorer-menu") {
            setup_menu_interactive(false);
            return Next::Exit;
        }
        if has("--install") {
            return match install(false) {
                Ok(()) => Next::Exit,
                Err(e) => {
                    message(&tf!("DropCraft could not be installed:\n\n{e}", "DropCraft konnte nicht installiert werden:\n\n{e}"), MB_OK | MB_ICONERROR);
                    Next::Exit
                }
            };
        }
        // Automatische Starts (Windows-Anmeldung, Explorer-Rechtsklick,
        // nach einem Update) und die installierte Kopie fragen nie.
        if has("--autostart") || has("--share") || has("--after-update") || is_installed_copy() {
            return Next::Run;
        }
        let this = updater::version();
        let (text, flags) = match installed_version() {
            None => (
                tf!(
                    "Install DropCraft {this} on this PC?\n\n\
                     It is copied to\n{}\nand gets an entry in the Start menu and under “Apps”, \
                     starts with Windows and keeps itself up to date. No admin rights needed.\n\n\
                     Yes = install\nNo = just run it from here this time",
                    "DropCraft {this} auf diesem PC installieren?\n\n\
                     Das Programm wird nach\n{}\nkopiert, bekommt einen Eintrag im Startmenü und unter „Apps“, \
                     startet mit Windows und hält sich selbst aktuell. Adminrechte sind nicht nötig.\n\n\
                     Ja = installieren\nNein = nur dieses Mal von hier starten",
                    install_dir().display()
                ),
                MB_YESNOCANCEL | MB_ICONQUESTION,
            ),
            Some(v) => (
                tf!(
                    "DropCraft is already installed (version {v}).\nThis file is version {this}.\n\n\
                     Yes = replace the installed version with this one\nNo = start the installed version",
                    "DropCraft ist bereits installiert (Version {v}).\nDiese Datei ist Version {this}.\n\n\
                     Ja = installierte Version durch diese ersetzen\nNein = installierte Version starten"
                ),
                MB_YESNOCANCEL | MB_ICONQUESTION | if updater::newer(&v, this) || v == this { MB_DEFBUTTON2 } else { 0 },
            ),
        };
        match message(&text, flags) {
            IDYES => {
                if let Err(e) = install(true) {
                    message(&tf!("DropCraft could not be installed:\n\n{e}", "DropCraft konnte nicht installiert werden:\n\n{e}"), MB_OK | MB_ICONERROR);
                }
                Next::Exit
            }
            IDNO if installed_version().is_some() => {
                let _ = Command::new(installed_exe()).spawn();
                Next::Exit
            }
            IDNO => Next::Run,
            _ => Next::Exit,
        }
    }

    /// Kopiert dieses Programm in den Programmordner, trägt es bei Windows ein
    /// und startet die installierte Kopie. `interactive`: auch das
    /// Explorer-Menü anbieten (Windows fragt dafür nach Adminrechten).
    pub fn install(interactive: bool) -> Result<(), String> {
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        let target = installed_exe();
        stop_running();
        if !same_file(&me, &target) {
            std::fs::create_dir_all(install_dir()).map_err(|e| format!("{}: {e}", install_dir().display()))?;
            let tmp = install_dir().join(format!(".dropcraft-install-{}", std::process::id()));
            std::fs::copy(&me, &tmp).map_err(|e| tf!("Copy to {}: {e}", "Kopieren nach {}: {e}", tmp.display()))?;
            // Eine eben beendete Instanz gibt die Datei manchmal erst kurz danach frei.
            let mut last = None;
            for _ in 0..40 {
                match std::fs::rename(&tmp, &target) {
                    Ok(()) => {
                        last = None;
                        break;
                    }
                    Err(e) => {
                        last = Some(e);
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
            }
            if let Some(e) = last {
                let _ = std::fs::remove_file(&tmp);
                return Err(tf!("Replace {}: {e}", "{} ersetzen: {e}", target.display()));
            }
        }
        write_uninstall_entry(&target).map_err(|e| tf!("Entry under “Apps”: {e}", "Eintrag unter „Apps“: {e}"))?;
        if crate::shellmenu::supported() {
            if crate::shellmenu::is_set_up() {
                let _ = crate::shellmenu::ensure_dll(&install_dir());
            } else if interactive {
                setup_menu_interactive(true);
            }
        }
        // Startmenü-Verknüpfung, Explorer-Rechtsklick und „Mit Windows starten“
        // richtet die installierte Kopie beim Start selbst ein (siehe `on_start`).
        Command::new(&target).spawn().map_err(|e| tf!("Start: {e}", "Starten: {e}"))?;
        Ok(())
    }

    /// Fragt nach und richtet das Windows-11-Explorer-Menü ein.
    fn setup_menu_interactive(ask: bool) {
        if !crate::shellmenu::supported() {
            message(t("The Explorer menu needs Windows 11.", "Das Explorer-Menü gibt es erst ab Windows 11."), MB_OK | MB_ICONINFORMATION);
            return;
        }
        if !installed_exe().exists() {
            message(t("Please install DropCraft first.", "Bitte DropCraft zuerst installieren."), MB_OK | MB_ICONINFORMATION);
            return;
        }
        if ask
            && message(
                t(
                    "Also add “DropCraft” to the Explorer right-click menu?\n\n\
                     Files and folders in your sync folders then offer “Copy DropCraft link”, “Share …” and \
                     “View in browser” directly, without going through “Show more options”.\n\n\
                     For this, DropCraft signs a small package with a certificate of its own. Windows asks once for \
                     admin rights to trust that certificate. You can also do this later in Settings.",
                    "„DropCraft“ auch direkt ins Rechtsklick-Menü des Explorers aufnehmen?\n\n\
                     Dateien und Ordner in deinen Sync-Ordnern bieten dann direkt „DropCraft-Link kopieren“, \
                     „Freigeben …“ und „Im Browser anzeigen“ – ohne Umweg über „Weitere Optionen anzeigen“.\n\n\
                     Dafür signiert DropCraft ein kleines Paket mit einem eigenen Zertifikat. Windows fragt einmal \
                     nach Adminrechten, um diesem Zertifikat zu vertrauen. Später geht das auch in den Einstellungen.",
                ),
                MB_YESNO | MB_ICONQUESTION,
            ) != IDYES
        {
            return;
        }
        match crate::shellmenu::setup(&install_dir()) {
            Ok(()) => {
                message(
                    t(
                        "The Explorer menu is set up. Right-click a file in your DropCraft folder › “DropCraft”.",
                        "Das Explorer-Menü ist eingerichtet. Rechtsklick auf eine Datei in deinem DropCraft-Ordner › „DropCraft“.",
                    ),
                    MB_OK | MB_ICONINFORMATION,
                );
            }
            Err(e) => {
                message(
                    &tf!("The Explorer menu could not be set up:\n\n{e}", "Das Explorer-Menü konnte nicht eingerichtet werden:\n\n{e}"),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
    }

    fn write_uninstall_entry(exe: &Path) -> std::io::Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(UNINSTALL_KEY)?;
        let exe_s = exe.display().to_string();
        key.set_value("DisplayName", &"DropCraft")?;
        key.set_value("DisplayIcon", &format!("{exe_s},0"))?;
        key.set_value("Publisher", &"DropCraft")?;
        key.set_value("URLInfoAbout", &"https://dropcraft.tech")?;
        key.set_value("InstallLocation", &install_dir().display().to_string())?;
        key.set_value("UninstallString", &format!("\"{exe_s}\" --uninstall"))?;
        key.set_value("QuietUninstallString", &format!("\"{exe_s}\" --uninstall --quiet"))?;
        key.set_value("NoModify", &1u32)?;
        key.set_value("NoRepair", &1u32)?;
        if key.get_value::<String, _>("InstallDate").is_err() {
            key.set_value("InstallDate", &chrono::Local::now().format("%Y%m%d").to_string())?;
        }
        refresh_entry(&key, exe)
    }

    /// Version und Größe aktuell halten (ändern sich mit jedem Selbst-Update).
    fn refresh_entry(key: &RegKey, exe: &Path) -> std::io::Result<()> {
        key.set_value("DisplayVersion", &updater::version())?;
        let kb = std::fs::metadata(exe).map(|m| (m.len() / 1024) as u32).unwrap_or(0);
        key.set_value("EstimatedSize", &kb)
    }

    /// Beim Start der App: „Apps“-Eintrag der installierten Kopie auffrischen.
    pub fn on_start() {
        if !is_installed_copy() {
            return;
        }
        // Explorer-Menü-DLL nach Updates austauschen (im Hintergrund).
        std::thread::spawn(|| crate::shellmenu::refresh(&install_dir()));
        if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(UNINSTALL_KEY, KEY_WRITE) {
            let _ = refresh_entry(&key, &installed_exe());
        } else {
            let _ = write_uninstall_entry(&installed_exe());
        }
    }

    /// Beendet eine laufende DropCraft-Instanz (egal, von wo sie gestartet wurde).
    fn stop_running() {
        let addr = ("127.0.0.1", INSTANCE_PORT);
        let port_free = || TcpListener::bind(addr).map(drop).is_ok();
        if port_free() {
            return;
        }
        if let Ok(mut s) = TcpStream::connect(addr) {
            let _ = s.write_all(b"QUIT\n");
            let _ = s.shutdown(std::net::Shutdown::Write);
        }
        for _ in 0..25 {
            if port_free() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // Ältere Versionen kennen „QUIT“ nicht: Prozess am Port suchen und beenden.
        if let Some(pid) = pid_on_port(INSTANCE_PORT) {
            if pid != std::process::id() {
                let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).creation_flags(CREATE_NO_WINDOW).status();
            }
        }
        for _ in 0..25 {
            if port_free() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn pid_on_port(port: u16) -> Option<u32> {
        let out = Command::new("netstat").args(["-ano", "-p", "TCP"]).creation_flags(CREATE_NO_WINDOW).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let local = format!("127.0.0.1:{port}");
        text.lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>())
            .find(|c| c.len() >= 5 && c[1] == local && c[2].ends_with(":0"))
            .and_then(|c| c[4].parse().ok())
    }

    /// Deinstallieren. Die Dateien in den Sync-Ordnern bleiben immer erhalten.
    pub fn uninstall(quiet: bool) {
        if !quiet
            && message(
                t(
                    "Uninstall DropCraft?\n\nYour files in the sync folders stay.",
                    "DropCraft deinstallieren?\n\nDeine Dateien in den Sync-Ordnern bleiben erhalten.",
                ),
                MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
            ) != IDOK
        {
            return;
        }
        stop_running();
        let has_accounts = !crate::config::Config::load().accounts.is_empty();
        let remove_all = !quiet
            && has_accounts
            && message(
                &tf!(
                    "Keep accounts and settings?\n\n\
                     Yes = keep: after reinstalling, everything continues without signing in again.\n\
                     No = sign out the accounts and delete the settings ({}).{}",
                    "Konten und Einstellungen behalten?\n\n\
                     Ja = behalten: Nach einer erneuten Installation geht es ohne neue Anmeldung weiter.\n\
                     Nein = Konten abmelden und Einstellungen löschen ({}).{}",
                    crate::config::app_dir().display(),
                    if !cfg!(feature = "cloud-files") {
                        ""
                    } else {
                        t(
                            "\n\nOnline-only files (cloud icon) can no longer be opened in the folder afterwards, but they stay on the server.",
                            "\n\nNur-online-Dateien (Wolkensymbol) lassen sich danach im Ordner nicht mehr öffnen, sie bleiben aber auf dem Server.",
                        )
                    }
                ),
                MB_YESNO | MB_ICONQUESTION,
            ) == IDNO;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(run) = hkcu.open_subkey_with_flags("Software\\Microsoft\\Windows\\CurrentVersion\\Run", KEY_WRITE) {
            let _ = run.delete_value("DropCraft");
        }
        crate::config::register_context_menu(&[]);
        crate::shellmenu::remove();
        let _ = hkcu.delete_subkey_all(format!("Software\\Classes\\AppUserModelId\\{}", crate::notify::APP_ID));
        let _ = hkcu.delete_subkey_all(UNINSTALL_KEY);
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let _ = std::fs::remove_file(PathBuf::from(appdata).join("Microsoft\\Windows\\Start Menu\\Programs\\DropCraft.lnk"));
        }
        let _ = std::fs::remove_file(crate::config::app_dir().join("shortcut.txt"));

        if remove_all {
            #[cfg(feature = "cloud-files")]
            crate::cloudfiles::unregister_all();
            let _ = std::fs::remove_dir_all(crate::config::app_dir());
        }

        // Der Programmordner enthält diese laufende .exe: nach dem Beenden löschen.
        let dir = install_dir();
        if dir.exists() {
            if is_installed_copy() {
                let _ = Command::new("cmd")
                    .raw_arg(format!("/c ping -n 6 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display()))
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn();
            } else {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        if !quiet {
            message(t("DropCraft was uninstalled.", "DropCraft wurde deinstalliert."), MB_OK | MB_ICONINFORMATION);
        }
    }

    // ------------------------------------------------------------ Meldungen

    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDNO, IDOK, IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_OKCANCEL,
        MB_SETFOREGROUND, MB_YESNO, MB_YESNOCANCEL, MESSAGEBOX_RESULT, MESSAGEBOX_STYLE,
    };

    fn message(text: &str, flags: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
        use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (text, title) = (w(text), w("DropCraft"));
        unsafe {
            // Gestochen scharf statt hochskaliert (winit setzt später dasselbe).
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), flags | MB_SETFOREGROUND)
        }
    }
}
