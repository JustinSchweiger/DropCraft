// Erzeugt das Programmsymbol (Tropfen-Logo) und bettet es unter Windows in die
// DropCraft.exe ein, damit sie im Explorer und in der Taskleiste das Logo zeigt.
#[allow(dead_code)]
mod icons {
    include!("src/icons.rs");
}
use icons::*;

fn main() {
    println!("cargo:rerun-if-changed=src/icons.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("dropcraft.ico");
    std::fs::write(&ico, ico_bytes(&[16, 20, 24, 32, 40, 48, 64, 256], logo_rgba)).unwrap();
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap());
    res.set("ProductName", "DropCraft");
    res.set("FileDescription", "DropCraft");
    if let Err(e) = res.compile() {
        // Ohne Resource-Compiler trotzdem bauen, nur ohne Symbol.
        println!("cargo:warning=Programmsymbol nicht eingebettet: {e}");
    }
    build_shell_dll(&out);
}

/// Baut die Explorer-Menü-DLL (`client/shell`) in einem eigenen Zielordner und
/// legt sie als `DropCraftShell.dll` in OUT_DIR, damit `shellmenu.rs` sie per
/// include_bytes! einbetten kann. Scheitert das, bleibt die Datei leer und die
/// App baut trotzdem (nur ohne Windows-11-Menü).
fn build_shell_dll(out: &std::path::Path) {
    use std::{path::PathBuf, process::Command};
    println!("cargo:rerun-if-changed=../shell/src");
    println!("cargo:rerun-if-changed=../shell/Cargo.toml");
    let dest = out.join("DropCraftShell.dll");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../shell/Cargo.toml");
    let target = std::env::var("TARGET").unwrap();
    let target_dir = out.join("shell-target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--release", "--manifest-path"])
        .arg(&manifest)
        .args(["--target", &target, "--target-dir"])
        .arg(&target_dir)
        .env_remove("CARGO_TARGET_DIR")
        .status();
    let built = target_dir.join(&target).join("release").join("dropcraft_shell.dll");
    match status {
        Ok(s) if s.success() && built.exists() => {
            std::fs::copy(&built, &dest).unwrap();
        }
        other => {
            println!("cargo:warning=Explorer-Menü-DLL nicht gebaut ({other:?}) – die App hat dann kein Windows-11-Menü");
            std::fs::write(&dest, b"").unwrap();
        }
    }
}
