//! Windows-11-Kontextmenü „DropCraft“ (ganz oben, wie bei Dropbox).
//!
//! Der Explorer zeigt dort nur Einträge aus signierten App-Paketen. Weil
//! DropCraft kein gekauftes Zertifikat hat, baut die App das kleine Paket
//! (nur Manifest und Logos, die DLL bleibt im Programmordner) auf jedem PC
//! selbst, signiert es mit einem frisch erzeugten Zertifikat und lässt Windows
//! diesem Zertifikat einmalig vertrauen (eine Admin-Abfrage). Der private
//! Schlüssel wird gleich danach gelöscht: Niemand kann damit weitere Pakete
//! signieren.
//!
//! Die DLL (`client/shell`) steckt in der .exe und wird bei jedem Start bzw.
//! nach Updates aktualisiert; das Paket selbst ändert sich nur, wenn sich
//! `PACKAGE_VERSION` ändert.

use std::{
    os::windows::process::CommandExt,
    path::Path,
    process::Command,
};

/// Die DLL, von build.rs aus `client/shell` gebaut (leer, falls das scheiterte).
const SHELL_DLL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/DropCraftShell.dll"));
pub const DLL_NAME: &str = "DropCraftShell.dll";

const PACKAGE_NAME: &str = "DropCraft.ExplorerMenu";
/// Erhöhen, wenn sich das Manifest unten ändert (dann wird neu eingerichtet).
const PACKAGE_VERSION: &str = "1.0.0.0";
const PUBLISHER: &str = "CN=DropCraft Explorer Menu";
/// Muss zu `CLSID_DROPCRAFT_MENU` in `client/shell/src/lib.rs` passen.
const CLSID: &str = "5B2F1D3E-6C0A-4E7B-9C35-2E8D1A7F4C61";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Gibt es das Menü in diesem Build und unter diesem Windows?
pub fn supported() -> bool {
    !SHELL_DLL.is_empty() && windows_build() >= 22000
}

fn windows_build() -> u32 {
    use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion")
        .and_then(|k| k.get_value::<String, _>("CurrentBuildNumber"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Installierte Paketversion (für diesen Benutzer), falls eingerichtet.
pub fn installed_version() -> Option<String> {
    use windows::{
        core::{PCWSTR, PWSTR},
        Win32::Storage::Packaging::Appx::{GetPackagesByPackageFamily, PackageFamilyNameFromId, PACKAGE_ID},
    };
    let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (mut name, mut publisher) = (w(PACKAGE_NAME), w(PUBLISHER));
    let id = PACKAGE_ID {
        name: PWSTR(name.as_mut_ptr()),
        publisher: PWSTR(publisher.as_mut_ptr()),
        processorArchitecture: 9, // x64
        ..Default::default()
    };
    unsafe {
        let mut len = 0u32;
        let _ = PackageFamilyNameFromId(&id, &mut len, None);
        let mut family = vec![0u16; len as usize];
        if PackageFamilyNameFromId(&id, &mut len, Some(PWSTR(family.as_mut_ptr()))).0 != 0 {
            return None;
        }
        let (mut count, mut buf_len) = (0u32, 0u32);
        let _ = GetPackagesByPackageFamily(PCWSTR(family.as_ptr()), &mut count, None, &mut buf_len, None);
        if count == 0 {
            return None;
        }
        let mut names = vec![PWSTR::null(); count as usize];
        let mut buf = vec![0u16; buf_len as usize];
        if GetPackagesByPackageFamily(PCWSTR(family.as_ptr()), &mut count, Some(names.as_mut_ptr()), &mut buf_len, Some(PWSTR(buf.as_mut_ptr()))).0 != 0 {
            return None;
        }
        // Vollständiger Name: Name_Version_Architektur__PublisherId
        let full = names.first()?.to_string().ok()?;
        full.split('_').nth(1).map(str::to_string)
    }
}

pub fn is_set_up() -> bool {
    installed_version().as_deref() == Some(PACKAGE_VERSION)
}

/// Legt die DLL in den Ordner bzw. ersetzt eine ältere. Eine gerade vom
/// Explorer geladene DLL wird umbenannt (wie die .exe beim Update).
pub fn ensure_dll(dir: &Path) -> Result<(), String> {
    if SHELL_DLL.is_empty() {
        return Err(crate::i18n::t("This build has no Explorer menu", "Dieser Build enthält kein Explorer-Menü").into());
    }
    let dll = dir.join(DLL_NAME);
    if std::fs::read(&dll).ok().as_deref() == Some(SHELL_DLL) {
        return Ok(());
    }
    let tmp = dir.join(format!("{DLL_NAME}.new"));
    std::fs::write(&tmp, SHELL_DLL).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if dll.exists() && std::fs::remove_file(&dll).is_err() {
        let old = dir.join(format!("{DLL_NAME}.{}.old", std::process::id()));
        std::fs::rename(&dll, &old).map_err(|e| format!("{}: {e}", dll.display()))?;
    }
    std::fs::rename(&tmp, &dll).map_err(|e| format!("{}: {e}", dll.display()))
}

/// Beim Start der installierten App: DLL aktuell halten, Reste entfernen.
pub fn refresh(dir: &Path) {
    if !supported() || !is_set_up() {
        return;
    }
    let _ = ensure_dll(dir);
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with(DLL_NAME) && n.ends_with(".old") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Richtet das Menü ein (blockiert, zeigt die Admin-Abfrage von Windows).
pub fn setup(dir: &Path) -> Result<(), String> {
    if !supported() {
        return Err(crate::i18n::t("The Explorer menu needs Windows 11.", "Das Explorer-Menü gibt es erst ab Windows 11.").into());
    }
    ensure_dll(dir)?;
    let work = std::env::temp_dir().join(format!("dropcraft-menu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let res = setup_in(dir, &work);
    let _ = std::fs::remove_dir_all(&work);
    res
}

fn setup_in(dir: &Path, work: &Path) -> Result<(), String> {
    let msix = work.join("DropCraft-ExplorerMenu.msix");
    let cer = work.join("DropCraft-ExplorerMenu.cer");
    pack(&msix)?;
    // 1. Zertifikat erzeugen (mit privatem Schlüssel, nur vorübergehend).
    let thumb = powershell(&format!(
        "$c = New-SelfSignedCertificate -Type Custom -Subject '{PUBLISHER}' -FriendlyName 'DropCraft Explorer-Menü' \
         -KeyUsage DigitalSignature -KeyExportPolicy NonExportable -NotAfter (Get-Date).AddYears(50) \
         -CertStoreLocation Cert:\\CurrentUser\\My -TextExtension @('2.5.29.37={{text}}1.3.6.1.5.5.7.3.3','2.5.29.19={{text}}'); \
         Export-Certificate -Cert $c -FilePath '{}' | Out-Null; $c.Thumbprint",
        cer.display()
    ))?;
    let thumb = thumb.trim().to_string();
    // 2. Signieren, 3. privaten Schlüssel sofort löschen.
    let signed = sign(&msix, &thumb);
    let _ = powershell(&format!("Remove-Item 'Cert:\\CurrentUser\\My\\{thumb}' -DeleteKey -ErrorAction SilentlyContinue"));
    signed?;
    // 4. Windows dem Zertifikat vertrauen lassen (Admin-Abfrage), 5. Paket anmelden.
    powershell(&format!(
        "$ErrorActionPreference = 'Stop'; \
         try {{ $p = Start-Process -FilePath certutil.exe -ArgumentList '-addstore','TrustedPeople','\"{}\"' -Verb RunAs -WindowStyle Hidden -Wait -PassThru }} \
         catch {{ throw '{cancelled}' }}; \
         if ($p.ExitCode -ne 0) {{ throw \"{cert_failed} (certutil $($p.ExitCode)).\" }}; \
         Get-AppxPackage -Name '{PACKAGE_NAME}' | Remove-AppxPackage; \
         Add-AppxPackage -Path '{}' -ExternalLocation '{}'",
        cer.display(),
        msix.display(),
        dir.display(),
        cancelled = crate::i18n::t("The admin approval was cancelled.", "Die Admin-Bestätigung wurde abgebrochen."),
        cert_failed = crate::i18n::t("Could not add the certificate", "Zertifikat konnte nicht eingetragen werden"),
    ))?;
    Ok(())
}

/// Entfernt das Paket (Deinstallation). Das vertrauenswürdige Zertifikat
/// bleibt; ohne privaten Schlüssel ist es nutzlos.
pub fn remove() {
    let _ = powershell(&format!("Get-AppxPackage -Name '{PACKAGE_NAME}' | Remove-AppxPackage"));
}

fn powershell(script: &str) -> Result<String, String> {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"])
        .arg(format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8; {script}"))
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("PowerShell: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        // Nur die eigentliche Meldung, ohne PowerShell-Positionsangaben.
        let msg = err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("unbekannter Fehler").to_string();
        Err(msg)
    }
}

fn manifest() -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
         xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
         xmlns:uap10="http://schemas.microsoft.com/appx/manifest/uap/windows10/10"
         xmlns:desktop4="http://schemas.microsoft.com/appx/manifest/desktop/windows10/4"
         xmlns:desktop5="http://schemas.microsoft.com/appx/manifest/desktop/windows10/5"
         xmlns:com="http://schemas.microsoft.com/appx/manifest/com/windows10"
         xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
         IgnorableNamespaces="uap uap10 desktop4 desktop5 com rescap">
  <Identity Name="{PACKAGE_NAME}" Publisher="{PUBLISHER}" Version="{PACKAGE_VERSION}" ProcessorArchitecture="x64"/>
  <Properties>
    <DisplayName>DropCraft Explorer-Menü</DisplayName>
    <PublisherDisplayName>DropCraft</PublisherDisplayName>
    <Logo>Assets\StoreLogo.png</Logo>
    <uap10:AllowExternalContent>true</uap10:AllowExternalContent>
  </Properties>
  <Resources><Resource Language="de-de"/></Resources>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.22000.0" MaxVersionTested="10.0.26100.0"/>
  </Dependencies>
  <Capabilities>
    <rescap:Capability Name="runFullTrust"/>
    <rescap:Capability Name="unvirtualizedResources"/>
  </Capabilities>
  <Applications>
    <Application Id="DropCraft" Executable="DropCraft.exe" uap10:TrustLevel="mediumIL" uap10:RuntimeBehavior="win32App">
      <uap:VisualElements DisplayName="DropCraft" Description="DropCraft Explorer-Menü" BackgroundColor="transparent"
                          Square150x150Logo="Assets\Square150.png" Square44x44Logo="Assets\Square44.png" AppListEntry="none"/>
      <Extensions>
        <desktop4:Extension Category="windows.fileExplorerContextMenus">
          <desktop4:FileExplorerContextMenus>
            <desktop5:ItemType Type="*"><desktop5:Verb Id="DropCraft" Clsid="{CLSID}"/></desktop5:ItemType>
            <desktop5:ItemType Type="Directory"><desktop5:Verb Id="DropCraft" Clsid="{CLSID}"/></desktop5:ItemType>
            <desktop5:ItemType Type="Directory\Background"><desktop5:Verb Id="DropCraft" Clsid="{CLSID}"/></desktop5:ItemType>
          </desktop4:FileExplorerContextMenus>
        </desktop4:Extension>
        <com:Extension Category="windows.comServer">
          <com:ComServer>
            <com:SurrogateServer DisplayName="DropCraft Explorer-Menü">
              <com:Class Id="{CLSID}" Path="{DLL_NAME}" ThreadingModel="STA"/>
            </com:SurrogateServer>
          </com:ComServer>
        </com:Extension>
      </Extensions>
    </Application>
  </Applications>
</Package>
"#
    )
}

/// Baut das .msix (Manifest + Logos) mit der Appx-Packaging-API von Windows.
fn pack(out: &Path) -> Result<(), String> {
    use windows::{
        core::HSTRING,
        Win32::{
            Storage::{FileSystem::FILE_ATTRIBUTE_NORMAL, Packaging::Appx::*},
            System::Com::{CoCreateInstance, CoInitializeEx, CreateUri, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_CREATE, STGM_SHARE_EXCLUSIVE, STGM_WRITE, Uri_CREATE_CANONICALIZE},
            UI::Shell::{SHCreateMemStream, SHCreateStreamOnFileEx},
        },
    };
    fn err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
        move |e| format!("Paket bauen ({what}): {e}")
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let factory: IAppxFactory = CoCreateInstance(&AppxFactory, None, CLSCTX_INPROC_SERVER).map_err(err("Factory"))?;
        let stream = SHCreateStreamOnFileEx(&HSTRING::from(out.as_os_str()), (STGM_CREATE | STGM_WRITE | STGM_SHARE_EXCLUSIVE).0, FILE_ATTRIBUTE_NORMAL.0, true, None)
            .map_err(err("Datei"))?;
        let sha256 = CreateUri(&HSTRING::from("http://www.w3.org/2001/04/xmlenc#sha256"), Uri_CREATE_CANONICALIZE, None).map_err(err("Hash"))?;
        let settings = APPX_PACKAGE_SETTINGS { forceZip32: true.into(), hashMethod: std::mem::ManuallyDrop::new(Some(sha256)) };
        let writer = factory.CreatePackageWriter(&stream, &settings).map_err(err("Writer"))?;
        let _ = std::mem::ManuallyDrop::into_inner(settings.hashMethod);
        for (name, size) in [("Assets\\StoreLogo.png", 50u32), ("Assets\\Square44.png", 44), ("Assets\\Square150.png", 150)] {
            let png = crate::icons::png_bytes(&crate::icons::logo_rgba(size), size);
            let data = SHCreateMemStream(Some(&png)).ok_or("Speicher-Stream")?;
            writer.AddPayloadFile(&HSTRING::from(name), &HSTRING::from("image/png"), APPX_COMPRESSION_OPTION_NORMAL, &data).map_err(err(name))?;
        }
        let manifest = manifest();
        let m = SHCreateMemStream(Some(manifest.as_bytes())).ok_or("Speicher-Stream")?;
        writer.Close(&m).map_err(err("Manifest"))?;
    }
    Ok(())
}

/// Für `SignerSignEx2` bei App-Paketen (aus mssign32.h, fehlt im windows-Crate).
#[repr(C)]
struct SignerSignEx2Params {
    flags: u32,
    subject_info: *const windows::Win32::Security::Cryptography::SIGNER_SUBJECT_INFO,
    signing_cert: *const windows::Win32::Security::Cryptography::SIGNER_CERT,
    signature_info: *const windows::Win32::Security::Cryptography::SIGNER_SIGNATURE_INFO,
    provider_info: *const std::ffi::c_void,
    timestamp_flags: u32,
    algorithm_oid: *const u8,
    timestamp_url: *const u16,
    crypt_attrs: *const std::ffi::c_void,
    sip_data: *const std::ffi::c_void,
    signer_context: *mut *mut windows::Win32::Security::Cryptography::SIGNER_CONTEXT,
    crypto_policy: *const std::ffi::c_void,
    reserved: *const std::ffi::c_void,
}

#[repr(C)]
struct AppxSipClientData {
    signer_params: *const SignerSignEx2Params,
    appx_sip_state: *mut std::ffi::c_void,
}

/// Signiert das Paket mit dem Zertifikat (Fingerabdruck) aus „Eigene Zertifikate“.
fn sign(msix: &Path, thumbprint: &str) -> Result<(), String> {
    use windows::{
        core::{HSTRING, PCSTR, PCWSTR},
        Win32::Security::Cryptography::*,
    };
    let hash: Vec<u8> = (0..thumbprint.len() / 2).filter_map(|i| u8::from_str_radix(&thumbprint[i * 2..i * 2 + 2], 16).ok()).collect();
    if hash.len() != 20 {
        return Err(format!("Zertifikat nicht erstellt ({thumbprint})"));
    }
    unsafe {
        let my: Vec<u16> = "MY".encode_utf16().chain([0]).collect();
        let store = CertOpenStore(CERT_STORE_PROV_SYSTEM_W, CERT_QUERY_ENCODING_TYPE(0), None, CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_CURRENT_USER), Some(my.as_ptr() as *const _))
            .map_err(|e| format!("Zertifikatspeicher: {e}"))?;
        let blob = CRYPT_INTEGER_BLOB { cbData: 20, pbData: hash.as_ptr() as *mut u8 };
        let cert = CertFindCertificateInStore(store, X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, 0, CERT_FIND_SHA1_HASH, Some(&blob as *const _ as *const _), None);
        if cert.is_null() {
            let _ = CertCloseStore(Some(store), 0);
            return Err("Zertifikat nicht gefunden".into());
        }
        let path = HSTRING::from(msix.as_os_str());
        let mut file_info = SIGNER_FILE_INFO { cbSize: size_of::<SIGNER_FILE_INFO>() as u32, pwszFileName: PCWSTR(path.as_ptr()), ..Default::default() };
        let mut index = 0u32;
        let subject = SIGNER_SUBJECT_INFO {
            cbSize: size_of::<SIGNER_SUBJECT_INFO>() as u32,
            pdwIndex: &mut index,
            dwSubjectChoice: SIGNER_SUBJECT_FILE,
            Anonymous: SIGNER_SUBJECT_INFO_0 { pSignerFileInfo: &mut file_info },
        };
        let mut store_info = SIGNER_CERT_STORE_INFO { cbSize: size_of::<SIGNER_CERT_STORE_INFO>() as u32, pSigningCert: cert, dwCertPolicy: SIGNER_CERT_POLICY_CHAIN, hCertStore: HCERTSTORE::default() };
        let signer_cert = SIGNER_CERT { cbSize: size_of::<SIGNER_CERT>() as u32, dwCertChoice: SIGNER_CERT_STORE, Anonymous: SIGNER_CERT_0 { pCertStoreInfo: &mut store_info }, ..Default::default() };
        let signature = SIGNER_SIGNATURE_INFO { cbSize: size_of::<SIGNER_SIGNATURE_INFO>() as u32, algidHash: CALG_SHA_256, dwAttrChoice: SIGNER_NO_ATTR, ..Default::default() };
        let mut context: *mut SIGNER_CONTEXT = std::ptr::null_mut();
        let mut sip = AppxSipClientData { signer_params: std::ptr::null(), appx_sip_state: std::ptr::null_mut() };
        let params = SignerSignEx2Params {
            flags: 0,
            subject_info: &subject,
            signing_cert: &signer_cert,
            signature_info: &signature,
            provider_info: std::ptr::null(),
            timestamp_flags: 0,
            algorithm_oid: std::ptr::null(),
            timestamp_url: std::ptr::null(),
            crypt_attrs: std::ptr::null(),
            sip_data: &sip as *const _ as *const _,
            signer_context: &mut context,
            crypto_policy: std::ptr::null(),
            reserved: std::ptr::null(),
        };
        sip.signer_params = &params;
        let res = SignerSignEx2(
            SIGNER_SIGN_FLAGS(0),
            &subject,
            &signer_cert,
            &signature,
            None,
            None,
            PCSTR::null(),
            PCWSTR::null(),
            None,
            Some(&sip as *const _ as *const _),
            &mut context,
            None,
            None,
        );
        if !sip.appx_sip_state.is_null() {
            // IUnknown::Release
            let unk: windows::core::IUnknown = windows::core::Interface::from_raw(sip.appx_sip_state);
            drop(unk);
        }
        if !context.is_null() {
            let _ = SignerFreeSignerContext(context);
        }
        let _ = CertFreeCertificateContext(Some(cert));
        let _ = CertCloseStore(Some(store), 0);
        res.map_err(|e| format!("Signieren: {e}"))
    }
}
