//! Eigenes Ordnersymbol für den Sync-Ordner im Explorer: das App-Logo.

use std::path::{Path, PathBuf};

use crate::{
    config,
    icons::{ico_bytes, logo_rgba, LOGO_REV},
};

/// Pfad der .ico für Sync-Ordner (auch für die Cloud-Ordner-Anmeldung);
/// legt sie bei Bedarf an. Der Name enthält die Logo-Version, damit Windows
/// nach einem neuen Logo nicht das alte aus dem Symbol-Cache zeigt.
pub fn ico_path() -> PathBuf {
    let ico = config::app_dir().join(format!("folder-{LOGO_REV}.ico"));
    if !ico.exists() {
        let _ = std::fs::create_dir_all(config::app_dir());
        let _ = std::fs::write(&ico, ico_bytes(&[16, 20, 24, 32, 40, 48, 64, 256], logo_rgba));
        let _ = std::fs::remove_file(config::app_dir().join("folder.ico"));
        for rev in 2..LOGO_REV {
            let _ = std::fs::remove_file(config::app_dir().join(format!("folder-{rev}.ico")));
        }
    }
    ico
}

/// Schreibt das Symbol und setzt es für den Ordner (nur Windows).
pub fn apply(dir: &Path) {
    let ico = ico_path();
    #[cfg(windows)]
    apply_windows(dir, &ico);
    #[cfg(target_os = "macos")]
    apply_macos(dir);
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = dir;
}

/// Finder: Symbol über NSWorkspace setzen.
#[cfg(target_os = "macos")]
fn apply_macos(dir: &Path) {
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSImage, NSWorkspace, NSWorkspaceIconCreationOptions};
    use objc2_foundation::{NSData, NSString};
    let png = crate::icons::png_bytes(&logo_rgba(512), 512);
    let data = NSData::with_bytes(&png);
    let Some(img) = NSImage::initWithData(NSImage::alloc(), &data) else { return };
    let path = NSString::from_str(&dir.display().to_string());
    let ws = NSWorkspace::sharedWorkspace();
    let _ = ws.setIcon_forFile_options(Some(&img), &path, NSWorkspaceIconCreationOptions(0));
}

#[cfg(windows)]
fn apply_windows(dir: &Path, ico: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Storage::FileSystem::{
            GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
            FILE_ATTRIBUTE_SYSTEM, INVALID_FILE_ATTRIBUTES,
        },
        UI::Shell::{SHChangeNotify, SHCNE_UPDATEITEM, SHCNF_PATHW},
    };

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    let ini = dir.join("desktop.ini");
    let content = format!(
        "[.ShellClassInfo]\r\nIconResource={},0\r\nInfoTip={}\r\n[ViewState]\r\nMode=\r\nVid=\r\nFolderType=Generic\r\n",
        ico.display(),
        crate::i18n::t("DropCraft sync folder", "DropCraft-Sync-Ordner")
    );
    // UTF-16 mit BOM, damit Umlaute im Pfad funktionieren.
    let mut bytes = vec![0xFF, 0xFE];
    for u in content.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    if std::fs::read(&ini).ok().as_deref() == Some(bytes.as_slice()) {
        return; // schon gesetzt
    }
    let ini_w = wide(&ini);
    let dir_w = wide(dir);
    unsafe {
        // Versteckte Systemdateien lassen sich sonst nicht überschreiben.
        if GetFileAttributesW(ini_w.as_ptr()) != INVALID_FILE_ATTRIBUTES {
            SetFileAttributesW(ini_w.as_ptr(), FILE_ATTRIBUTE_NORMAL);
        }
    }
    if std::fs::write(&ini, &bytes).is_err() {
        return;
    }
    unsafe {
        SetFileAttributesW(ini_w.as_ptr(), FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM);
        // Der Explorer liest desktop.ini nur bei Ordnern mit Schreibschutz-Attribut.
        let attrs = GetFileAttributesW(dir_w.as_ptr());
        if attrs != INVALID_FILE_ATTRIBUTES {
            SetFileAttributesW(dir_w.as_ptr(), attrs | FILE_ATTRIBUTE_READONLY);
        }
        SHChangeNotify(SHCNE_UPDATEITEM as i32, SHCNF_PATHW, dir_w.as_ptr() as *const _, std::ptr::null());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_ico_for_inspection() {
        let bytes = crate::icons::ico_bytes(&[16, 32, 48, 256], crate::icons::logo_rgba);
        assert_eq!(&bytes[..4], &[0, 0, 1, 0]);
        std::fs::write(std::env::temp_dir().join("dropcraft-folder.ico"), bytes).unwrap();
    }
}
