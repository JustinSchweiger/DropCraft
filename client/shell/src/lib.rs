//! Windows-11-Kontextmenü: Eintrag „DropCraft“ mit Untermenü für Dateien und
//! Ordner in den Sync-Ordnern (wie bei Dropbox ganz oben, nicht erst unter
//! „Weitere Optionen anzeigen“).
//!
//! Der Explorer zeigt dort nur `IExplorerCommand`-Handler aus App-Paketen. Die
//! DLL wird deshalb in `DropCraft.exe` eingebettet, liegt im Programmordner und
//! ist über ein kleines, lokal signiertes Paket registriert (siehe
//! `desktop/src/shellmenu.rs`). Alle Befehle starten `DropCraft.exe` aus
//! demselben Ordner; die App erledigt den Rest.
#![cfg(windows)]
#![allow(non_snake_case)]

use std::{
    ffi::c_void,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use windows::{
    core::{implement, Interface, Ref, Result, BOOL, GUID, HRESULT, PCWSTR, PWSTR},
    Win32::{
        Foundation::{CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_NOTIMPL, E_POINTER, HINSTANCE, S_FALSE, S_OK},
        System::{
            Com::{CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl},
            LibraryLoader::GetModuleFileNameW,
            SystemServices::DLL_PROCESS_ATTACH,
        },
        UI::Shell::{
            IEnumExplorerCommand, IEnumExplorerCommand_Impl, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray, SHStrDupW, ECF_DEFAULT,
            ECF_HASSUBCOMMANDS, ECS_ENABLED, ECS_HIDDEN, SIGDN_FILESYSPATH,
        },
    },
};

/// Muss mit der CLSID im Paket-Manifest übereinstimmen (`shellmenu.rs`).
pub const CLSID_DROPCRAFT_MENU: GUID = GUID::from_u128(0x5b2f1d3e_6c0a_4e7b_9c35_2e8d1a7f4c61);

static MODULE: AtomicUsize = AtomicUsize::new(0);

#[no_mangle]
extern "system" fn DllMain(module: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(module.0 as usize, Ordering::SeqCst);
    }
    BOOL(1)
}

/// `DropCraft.exe` neben dieser DLL.
fn app_exe() -> Option<PathBuf> {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(Some(windows::Win32::Foundation::HMODULE(MODULE.load(Ordering::SeqCst) as *mut c_void)), &mut buf) } as usize;
    if n == 0 {
        return None;
    }
    let dll = PathBuf::from(String::from_utf16_lossy(&buf[..n]));
    Some(dll.parent()?.join("DropCraft.exe"))
}

fn config() -> serde_json::Value {
    let Some(appdata) = std::env::var_os("APPDATA") else { return serde_json::Value::Null };
    let Ok(text) = std::fs::read_to_string(Path::new(&appdata).join("DropCraft").join("config.json")) else { return serde_json::Value::Null };
    serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
}

/// Deutsch, wenn in der App so eingestellt (sonst Englisch).
fn german() -> bool {
    config()["language"].as_str() == Some("de")
}

/// Sync-Ordner aller Konten aus `%APPDATA%\DropCraft\config.json`.
fn sync_dirs() -> Vec<String> {
    let v = config();
    v["accounts"]
        .as_array()
        .map(|a| a.iter().filter_map(|acc| acc["sync_dir"].as_str().map(norm)).collect())
        .unwrap_or_default()
}

fn norm(p: &str) -> String {
    p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Wo liegt der Pfad? `None` = außerhalb aller Sync-Ordner.
#[derive(PartialEq)]
enum Place {
    Root,
    Inside,
}

fn place(path: &str) -> Option<Place> {
    let p = norm(path);
    sync_dirs().iter().find_map(|d| {
        if p == *d {
            Some(Place::Root)
        } else if p.starts_with(d.as_str()) && p[d.len()..].starts_with('\\') {
            Some(Place::Inside)
        } else {
            None
        }
    })
}

/// Pfad der einzigen Auswahl (bei mehreren: `None`).
fn single_path(items: Ref<IShellItemArray>) -> Option<String> {
    let arr = items.as_ref()?;
    unsafe {
        if arr.GetCount().ok()? != 1 {
            return None;
        }
        let p = arr.GetItemAt(0).ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

fn dup(s: &str) -> Result<PWSTR> {
    let w: Vec<u16> = s.encode_utf16().chain([0]).collect();
    unsafe { SHStrDupW(PCWSTR(w.as_ptr())) }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Root,
    CopyLink,
    ShareOptions,
    OpenWeb,
}

const SUBCOMMANDS: [Kind; 3] = [Kind::CopyLink, Kind::ShareOptions, Kind::OpenWeb];

impl Kind {
    fn title(self) -> &'static str {
        let de = german();
        match self {
            Kind::Root => "DropCraft",
            Kind::CopyLink if de => "DropCraft-Link kopieren",
            Kind::CopyLink => "Copy DropCraft link",
            Kind::ShareOptions if de => "Freigeben …",
            Kind::ShareOptions => "Share …",
            Kind::OpenWeb if de => "Im Browser anzeigen",
            Kind::OpenWeb => "View in browser",
        }
    }

    fn arg(self) -> &'static str {
        match self {
            Kind::Root => "",
            Kind::CopyLink => "--copy-link",
            Kind::ShareOptions => "--share",
            Kind::OpenWeb => "--open-web",
        }
    }

    fn guid(self) -> GUID {
        match self {
            Kind::Root => CLSID_DROPCRAFT_MENU,
            Kind::CopyLink => GUID::from_u128(0x5b2f1d3e_6c0a_4e7b_9c35_2e8d1a7f4c62),
            Kind::ShareOptions => GUID::from_u128(0x5b2f1d3e_6c0a_4e7b_9c35_2e8d1a7f4c63),
            Kind::OpenWeb => GUID::from_u128(0x5b2f1d3e_6c0a_4e7b_9c35_2e8d1a7f4c64),
        }
    }
}

#[implement(IExplorerCommand)]
struct Command {
    kind: Kind,
}

impl IExplorerCommand_Impl for Command_Impl {
    fn GetTitle(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        dup(self.kind.title())
    }

    fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        match (self.kind, app_exe()) {
            (Kind::Root, Some(exe)) => dup(&format!("{},0", exe.display())),
            _ => Err(E_NOTIMPL.into()),
        }
    }

    fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(self.kind.guid())
    }

    fn GetState(&self, items: Ref<IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        let visible = match single_path(items).as_deref().and_then(place) {
            None => false,
            // Den Sync-Ordner selbst kann man nicht freigeben, nur im Browser zeigen.
            Some(Place::Root) => matches!(self.kind, Kind::Root | Kind::OpenWeb),
            Some(Place::Inside) => true,
        };
        Ok(if visible { ECS_ENABLED.0 as u32 } else { ECS_HIDDEN.0 as u32 })
    }

    fn Invoke(&self, items: Ref<IShellItemArray>, _bind: Ref<IBindCtx>) -> Result<()> {
        if self.kind == Kind::Root {
            return Ok(());
        }
        if let (Some(path), Some(exe)) = (single_path(items), app_exe()) {
            let _ = std::process::Command::new(exe).arg(self.kind.arg()).arg(path).spawn();
        }
        Ok(())
    }

    fn GetFlags(&self) -> Result<u32> {
        Ok(if self.kind == Kind::Root { ECF_HASSUBCOMMANDS.0 as u32 } else { ECF_DEFAULT.0 as u32 })
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        if self.kind == Kind::Root {
            Ok(SubCommands { pos: AtomicUsize::new(0) }.into())
        } else {
            Err(E_NOTIMPL.into())
        }
    }
}

#[implement(IEnumExplorerCommand)]
struct SubCommands {
    pos: AtomicUsize,
}

impl IEnumExplorerCommand_Impl for SubCommands_Impl {
    fn Next(&self, celt: u32, out: *mut Option<IExplorerCommand>, fetched: *mut u32) -> HRESULT {
        if out.is_null() {
            return E_POINTER;
        }
        let mut n = 0u32;
        while n < celt {
            let i = self.pos.fetch_add(1, Ordering::SeqCst);
            let Some(kind) = SUBCOMMANDS.get(i) else {
                self.pos.store(SUBCOMMANDS.len(), Ordering::SeqCst);
                break;
            };
            unsafe { out.add(n as usize).write(Some(Command { kind: *kind }.into())) };
            n += 1;
        }
        if !fetched.is_null() {
            unsafe { *fetched = n };
        }
        if n == celt {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, celt: u32) -> Result<()> {
        self.pos.fetch_add(celt as usize, Ordering::SeqCst);
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        self.pos.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn Clone(&self) -> Result<IEnumExplorerCommand> {
        Ok(SubCommands { pos: AtomicUsize::new(self.pos.load(Ordering::SeqCst)) }.into())
    }
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Ref<windows::core::IUnknown>, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let cmd: IExplorerCommand = Command { kind: Kind::Root }.into();
        unsafe { cmd.query(riid, ppv).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// # Safety
/// Von COM aufgerufen.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(clsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if ppv.is_null() || clsid.is_null() {
        return E_POINTER;
    }
    *ppv = std::ptr::null_mut();
    if *clsid != CLSID_DROPCRAFT_MENU {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = Factory.into();
    factory.query(riid, ppv)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    // Bleibt geladen, solange der Explorer-Ersatzprozess läuft (klein, harmlos).
    S_FALSE
}
