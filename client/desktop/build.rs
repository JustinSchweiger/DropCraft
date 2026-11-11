// Erzeugt das Programmsymbol (Würfel-Logo) und bettet es unter Windows in die
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
    std::fs::write(&ico, ico_bytes(&[16, 24, 32, 48, 64, 256], logo_rgba)).unwrap();
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap());
    res.set("ProductName", "DropCraft");
    res.set("FileDescription", "DropCraft");
    if let Err(e) = res.compile() {
        // Ohne Resource-Compiler trotzdem bauen, nur ohne Symbol.
        println!("cargo:warning=Programmsymbol nicht eingebettet: {e}");
    }
}
