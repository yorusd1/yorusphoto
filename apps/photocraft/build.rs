//! Windows only: embed the app icon and version info (VERSIONINFO) into `photocraft.exe`.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `PHOTOCRAFT_REQUIRE_WINRES=1` (set by the
//! release workflow) turns it into an error.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/yorusphoto/yorusphoto.ico");
    println!("cargo:rerun-if-env-changed=PHOTOCRAFT_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/yorusphoto/yorusphoto.ico")
        .set("ProductName", "yorusphoto")
        .set("FileDescription", "yorusphoto image editor")
        .set("CompanyName", "Yorus")
        .set("LegalCopyright", "Copyright (c) the PhotoCraft authors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "yorusphoto.exe")
        .set("InternalName", "yorusphoto");
    if let Err(e) = res.compile() {
        if std::env::var_os("PHOTOCRAFT_REQUIRE_WINRES").is_some() {
            panic!("embedding Windows resources failed: {e}");
        }
        println!("cargo:warning=photocraft.exe built without icon/version resources: {e}");
    }
}
