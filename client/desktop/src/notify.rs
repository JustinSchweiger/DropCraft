//! Benachrichtigungen: Änderungen in freigegebenen Ordnern zusammenfassen und
//! in die Liste der App legen (siehe `inbox`). Außerdem die App-ID und die
//! Startmenü-Verknüpfung unter Windows.

use std::collections::BTreeMap;

use dropcraft_core::{Api, Event, EventKind};

/// Fasst Änderungen anderer (Gäste, andere Geräte) in Freigaben mit
/// eingeschalteter Benachrichtigung zusammen und zeigt je Freigabe eine Meldung.
pub fn shared_folder_changes(api: &Api, own_device: &str, events: &[Event]) {
    if !crate::config::Config::load().notifications {
        return;
    }
    let relevant: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Downloaded | EventKind::NewOnline | EventKind::DeletedLocal))
        .filter(|e| e.by.as_deref().map(|b| b != own_device).unwrap_or(false))
        .collect();
    if relevant.is_empty() {
        return;
    }
    let Ok(shares) = api.list_shares() else { return };
    // Freigabe-Pfad -> (neu, gelöscht, wer)
    let mut groups: BTreeMap<String, (usize, usize, Vec<String>)> = BTreeMap::new();
    for e in relevant {
        let Some(sh) = shares
            .iter()
            .filter(|s| s.notify && (e.path == s.path || e.path.starts_with(&format!("{}/", s.path))))
            .max_by_key(|s| s.path.len())
        else {
            continue;
        };
        let g = groups.entry(sh.path.clone()).or_default();
        if e.kind == EventKind::DeletedLocal {
            g.1 += 1;
        } else {
            g.0 += 1;
        }
        if let Some(b) = &e.by {
            if !g.2.contains(b) {
                g.2.push(b.clone());
            }
        }
    }
    for (path, (added, removed, who)) in groups {
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        show(&format!("DropCraft · {name}"), &summary(added, removed, &who));
    }
}

pub fn summary(added: usize, removed: usize, who: &[String]) -> String {
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(if added == 1 { crate::i18n::t("1 new file", "1 neue Datei").to_string() } else { tf!("{added} new files", "{added} neue Dateien") });
    }
    if removed > 0 {
        parts.push(if removed == 1 { crate::i18n::t("1 file deleted", "1 Datei gelöscht").to_string() } else { tf!("{removed} files deleted", "{removed} Dateien gelöscht") });
    }
    let mut s = parts.join(", ");
    if !who.is_empty() {
        s.push_str(&tf!(" · by {}", " · von {}", who.join(", ")));
    }
    s
}

/// Eigene App-ID: Taskleiste und Startmenü ordnen Fenster und Verknüpfung
/// damit „DropCraft“ zu.
#[cfg(windows)]
pub const APP_ID: &str = "DropCraft.App";

/// Beim Start aufrufen: App-ID setzen, bei Windows bekannt machen und die
/// Startmenü-Verknüpfung anlegen.
pub fn init() {
    #[cfg(windows)]
    {
        unsafe {
            let id: Vec<u16> = APP_ID.encode_utf16().chain([0]).collect();
            let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(windows::core::PCWSTR(id.as_ptr()));
        }
        std::thread::spawn(|| {
            register_app_id();
            if let Err(e) = ensure_shortcut() {
                crate::applog::line(&format!("Startmenü-Verknüpfung: {e}"));
            }
        });
    }
}

#[cfg(windows)]
fn register_app_id() {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let rev = crate::icons::LOGO_REV;
    let icon = crate::config::app_dir().join(format!("dropcraft-{rev}.ico"));
    if !icon.exists() {
        let _ = std::fs::create_dir_all(crate::config::app_dir());
        let _ = std::fs::write(&icon, crate::icons::ico_bytes(&[16, 32, 48, 256], crate::icons::logo_rgba));
        let _ = std::fs::remove_file(crate::config::app_dir().join("dropcraft.ico"));
    }
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok((key, _)) = hkcu.create_subkey(format!("Software\\Classes\\AppUserModelId\\{APP_ID}")) {
        let _ = key.set_value("DisplayName", &"DropCraft");
        let _ = key.set_value("IconUri", &icon.display().to_string());
        let _ = key.set_value("IconBackgroundColor", &"FF0B0E12");
    }
}

/// Startmenü-Verknüpfung „DropCraft“ mit unserer App-ID (der von Microsoft
/// vorgesehene Weg für Benachrichtigungen ohne Installer).
#[cfg(windows)]
fn ensure_shortcut() -> Result<(), String> {
    use windows::{
        core::{Interface, HSTRING, PWSTR},
        Win32::{
            Storage::EnhancedStorage::PKEY_AppUserModel_ID,
            System::{
                Com::{CoCreateInstance, CoInitializeEx, IPersistFile, StructuredStorage::PROPVARIANT, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED},
                Variant::VT_LPWSTR,
            },
            UI::Shell::{IShellLinkW, PropertiesSystem::IPropertyStore, ShellLink},
        },
    };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let appdata = std::env::var_os("APPDATA").ok_or("APPDATA fehlt")?;
    let dir = std::path::PathBuf::from(appdata).join("Microsoft\\Windows\\Start Menu\\Programs");
    let lnk = dir.join("DropCraft.lnk");
    // Nur neu schreiben, wenn das Programm woanders liegt als beim letzten Mal.
    let stamp = crate::config::app_dir().join("shortcut.txt");
    if lnk.exists() && std::fs::read_to_string(&stamp).ok().as_deref() == Some(exe.display().to_string().as_str()) {
        return Ok(());
    }
    let _ = std::fs::create_dir_all(&dir);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        link.SetPath(&HSTRING::from(exe.as_os_str())).map_err(|e| e.to_string())?;
        if let Some(d) = exe.parent() {
            let _ = link.SetWorkingDirectory(&HSTRING::from(d.as_os_str()));
        }
        let _ = link.SetDescription(&HSTRING::from("DropCraft"));
        let store: IPropertyStore = link.cast().map_err(|e| e.to_string())?;
        let mut id: Vec<u16> = APP_ID.encode_utf16().chain([0]).collect();
        let mut pv = PROPVARIANT::default();
        (*pv.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*pv.Anonymous.Anonymous).Anonymous.pwszVal = PWSTR(id.as_mut_ptr());
        store.SetValue(&PKEY_AppUserModel_ID, &pv).map_err(|e| e.to_string())?;
        store.Commit().map_err(|e| e.to_string())?;
        // `pv` zeigt auf unseren Puffer: nicht von Windows freigeben lassen.
        std::mem::forget(pv);
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true).map_err(|e| e.to_string())?;
        drop(id);
    }
    let _ = std::fs::write(stamp, exe.display().to_string());
    Ok(())
}

/// Meldung in die Liste der App (Glocke im Popup, Punkt am Tray-Symbol).
pub fn show(title: &str, body: &str) {
    crate::inbox::push(title, body);
}

#[cfg(test)]
mod tests {
    #[test]
    fn texts() {
        crate::i18n::apply("de");
        assert_eq!(super::summary(3, 0, &["Gast: Anna".into()]), "3 neue Dateien · von Gast: Anna");
        assert_eq!(super::summary(1, 2, &[]), "1 neue Datei, 2 Dateien gelöscht");
    }
}
