//! Build script: embed the app icon into the Windows `.exe` so Explorer and the
//! NSIS Start-Menu shortcut display it. cargo-packager bundles the binary as-is
//! and never touches its resources, so the icon has to be a Win32 resource
//! compiled in here at build time. No-op on macOS/Linux.
//!
//! `#[cfg(windows)]` keys off the build HOST (build scripts run on the host); our
//! CI builds each Windows target on a Windows runner (native, host == target), so
//! this fires for both x64 and arm64 and is skipped on the macOS runners.
fn main() {
    build_facts();
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

/// What `arbiter about` reports about the build: the commit (with `+` when the tree was
/// dirty), its date, the profile, the target and the compiler. Each falls back to
/// "unknown" rather than failing the build, e.g. from a source tarball without `.git`.
fn build_facts() {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let mut sha = git(&["rev-parse", "--short=9", "HEAD"]).unwrap_or_else(|| "unknown".into());
    if git(&["status", "--porcelain", "--untracked-files=no"]).is_some() {
        sha.push('+');
    }
    let date = git(&["log", "-1", "--format=%cs"]).unwrap_or_else(|| "unknown".into());
    let rustc = std::env::var("RUSTC")
        .ok()
        .and_then(|rustc| std::process::Command::new(rustc).arg("-V").output().ok())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ARBITER_GIT_SHA={sha}");
    println!("cargo:rustc-env=ARBITER_GIT_DATE={date}");
    println!("cargo:rustc-env=ARBITER_PROFILE={}", std::env::var("PROFILE").unwrap_or_default());
    println!("cargo:rustc-env=ARBITER_TARGET={}", std::env::var("TARGET").unwrap_or_default());
    println!("cargo:rustc-env=ARBITER_RUSTC={rustc}");
    // Re-run when the checked-out commit moves, so the commit shown is the one built.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}
