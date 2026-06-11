//! Build script: embed the app icon into the Windows `.exe` so Explorer and the
//! NSIS Start-Menu shortcut display it. cargo-packager bundles the binary as-is
//! and never touches its resources, so the icon has to be a Win32 resource
//! compiled in here at build time. No-op on macOS/Linux.
//!
//! `#[cfg(windows)]` keys off the build HOST (build scripts run on the host); our
//! CI builds each Windows target on a Windows runner (native, host == target), so
//! this fires for both x64 and arm64 and is skipped on the macOS runners.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icons/icon.ico");
        if let Err(e) = res.compile() {
            // Don't fail the build over the icon — warn and ship without it.
            println!("cargo:warning=failed to embed Windows app icon: {e}");
        }
    }
}
