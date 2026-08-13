//! Build script: embeds the application icon and product metadata in Windows executables.
//! Other platforms require no action here; macOS uses the app bundle and Linux uses .desktop.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon/Playmate.ico");
    #[cfg(windows)]
    {
        // build.rs runs on the host, so embed resources only for a Windows target.
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("../../assets/icon/Playmate.ico");
            res.set("ProductName", "Playmate");
            res.set("FileDescription", "Playmate - LAN co-op FC/NES emulator");
            if let Err(e) = res.compile() {
                println!("cargo:warning=failed to embed Windows resources: {e}");
            }
        }
    }
}
