//! Build script: on Windows, puts the application icon and the version
//! details inside `avero.exe` (seen in Explorer, the taskbar and the
//! file's "Properties › Details"). Nothing to do on other systems.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/logo/avero.ico");
    // `cfg(windows)` is the machine running this script; the target is
    // checked too, since only a Windows executable takes these resources.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_resources();
    }
}

#[cfg(windows)]
fn windows_resources() {
    let mut resources = winresource::WindowsResource::new();
    // The file and product versions come from Cargo (CARGO_PKG_VERSION).
    resources
        .set_icon("assets/logo/avero.ico")
        .set("ProductName", "Avero")
        .set("FileDescription", "Avero")
        .set("OriginalFilename", "avero.exe")
        .set("LegalCopyright", "GPL-3.0-or-later");
    // The Windows SDK resource compiler (rc.exe) comes with the MSVC
    // build tools; without it the build stops here with the reason.
    if let Err(e) = resources.compile() {
        panic!("cannot embed the Windows icon and version details: {e}");
    }
}
