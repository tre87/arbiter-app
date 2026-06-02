//! Tier-2 Claude context capture (cross-platform).
//!
//! Claude's exact context-window usage (window size, used %, per-component
//! tokens) is NOT in the transcript JSONL — it is only handed to a configured
//! `statusLine` command on stdin. To surface it in Arbiter's footer, we
//! intercept `claude` launches inside Arbiter's own PTYs:
//!
//!   1. `shell.rs` prepends an Arbiter `bin/` dir (written here) to PATH for the
//!      spawned shell, so `claude` — and any alias that resolves it via PATH,
//!      e.g. `cc` — hits our launcher.
//!   2. The launcher `exec`s the REAL claude with `--settings <file>` (generated
//!      here in Rust) that points `statusLine` at `arbiter claude-statusline`.
//!   3. Claude pipes its session JSON to that command on every render; we write
//!      it to `<capture-dir>/<session_id>.json` and call through to the user's
//!      original status line so it still renders.
//!
//! All JSON is built/parsed here in Rust — no `jq` or external tooling. The
//! launcher scripts contain no logic, only a delegated `exec`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Absolute path to the real `claude` binary (resolved from PATH at spawn, with
/// our shim dir excluded so it never recurses into itself).
pub const REAL_CLAUDE_ENV: &str = "ARBITER_REAL_CLAUDE";
/// Path to the generated settings file the launcher passes via `--settings`.
pub const SETTINGS_ENV: &str = "ARBITER_CLAUDE_SETTINGS";
/// Directory where the capture subcommand writes `<session_id>.json`.
pub const CAPTURE_DIR_ENV: &str = "ARBITER_CAPTURE_DIR";
/// The user's original `statusLine.command`, if any, to call through to.
pub const ORIG_STATUSLINE_ENV: &str = "ARBITER_ORIG_STATUSLINE";

/// Subdir (under app-data) that holds the shim `bin/` and generated settings.
const SHIM_SUBDIR: &str = "claude-shim";
/// Subdir (under app-data) that holds per-session capture files.
pub const CAPTURE_SUBDIR: &str = "claude-sessions";

// ── Capture subcommand (`arbiter claude-statusline`) ─────────────────────────

/// Entry point for `arbiter claude-statusline`, invoked by Claude as its
/// statusLine command (via our injected `--settings`). Reads Claude's status
/// JSON from stdin, persists it for the capture watcher keyed by `session_id`,
/// then forwards to the user's original status line so it still renders.
/// Runs headless and returns — `main` must exit without starting the GUI.
pub fn run_statusline_capture() {
    let mut buf = Vec::new();
    if std::io::stdin().read_to_end(&mut buf).is_err() {
        return;
    }

    if let (Ok(dir), Some(session_id)) = (std::env::var(CAPTURE_DIR_ENV), extract_session_id(&buf)) {
        write_capture(Path::new(&dir), &session_id, &buf);
    }

    // Call through to the user's original status line so it still shows while
    // they verify the footer against it (and until they remove it).
    if let Ok(orig) = std::env::var(ORIG_STATUSLINE_ENV) {
        if !orig.trim().is_empty() {
            forward_to_original(&orig, &buf);
        }
    }
}

/// Parse just the `session_id` string from Claude's status JSON.
fn extract_session_id(buf: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(buf).ok()?;
    let id = v.get("session_id")?.as_str()?;
    // session_id is a UUID; accept only path-safe chars as defence in depth.
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None;
    }
    Some(id.to_string())
}

/// Atomically write the captured JSON to `<dir>/<session_id>.json`.
fn write_capture(dir: &Path, session_id: &str, buf: &[u8]) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let final_path = dir.join(format!("{session_id}.json"));
    let tmp_path = dir.join(format!("{session_id}.json.tmp"));
    if std::fs::write(&tmp_path, buf).is_ok() {
        let _ = std::fs::rename(&tmp_path, &final_path);
    }
}

/// Run the user's original `statusLine.command` with the same stdin and let its
/// stdout become ours, so Claude renders it unchanged.
fn forward_to_original(orig: &str, stdin_bytes: &[u8]) {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(orig);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(orig);
        c
    };

    cmd.stdin(std::process::Stdio::piped());
    // stdout/stderr inherit our handles → their output reaches Claude.
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_bytes);
        // Drop closes stdin so the child sees EOF.
    }
    let _ = child.wait();
}

// ── Spawn-time setup (called from shell.rs) ──────────────────────────────────

/// Everything `shell.rs` needs to wire the spawned shell for interception.
pub struct ShimSetup {
    /// Prepend to PATH so `claude`/`cc` resolve to our launcher.
    pub bin_dir: PathBuf,
    /// Absolute path to the real claude binary (`REAL_CLAUDE_ENV`).
    pub real_claude: PathBuf,
    /// Generated settings file (`SETTINGS_ENV`).
    pub settings_file: PathBuf,
    /// Per-session capture dir (`CAPTURE_DIR_ENV`).
    pub capture_dir: PathBuf,
    /// User's original status-line command, if any (`ORIG_STATUSLINE_ENV`).
    pub orig_statusline: Option<String>,
}

/// Prepare the shim dir, launcher(s), and settings file under `data_dir`.
/// Returns `None` (interception skipped, shell behaves normally) when the real
/// `claude` can't be resolved from `original_path` or any file write fails.
pub fn setup(data_dir: &Path, original_path: &str, arbiter_bin: &Path) -> Option<ShimSetup> {
    let shim_dir = data_dir.join(SHIM_SUBDIR);
    let bin_dir = shim_dir.join("bin");
    let capture_dir = data_dir.join(CAPTURE_SUBDIR);

    let real_claude = find_real_claude(original_path, &bin_dir)?;

    std::fs::create_dir_all(&bin_dir).ok()?;
    std::fs::create_dir_all(&capture_dir).ok()?;

    let settings_file = shim_dir.join("settings.json");
    write_settings(&settings_file, arbiter_bin)?;
    write_launchers(&bin_dir)?;

    Some(ShimSetup {
        bin_dir,
        real_claude,
        settings_file,
        capture_dir,
        orig_statusline: read_user_statusline(),
    })
}

/// Resolve the real `claude` by scanning `original_path`, skipping `bin_dir`
/// (our launcher) so we never point the launcher at itself.
fn find_real_claude(original_path: &str, bin_dir: &Path) -> Option<PathBuf> {
    // Executable name candidates by platform. On Windows, npm installs both a
    // `claude.cmd` shim and a bare `claude` (sh) — prefer the .cmd/.exe.
    #[cfg(windows)]
    let names: &[&str] = &["claude.exe", "claude.cmd", "claude.bat", "claude"];
    #[cfg(not(windows))]
    let names: &[&str] = &["claude"];

    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in original_path.split(sep) {
        if dir.is_empty() {
            continue;
        }
        let dir = Path::new(dir);
        if dir == bin_dir {
            continue;
        }
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Write the launcher(s) into `bin_dir`. The launcher contains no logic — it
/// `exec`s the real claude with our `--settings`, forwarding all args.
fn write_launchers(bin_dir: &Path) -> Option<()> {
    // POSIX launcher (zsh/bash/Git Bash). `exec` replaces the process so the
    // TTY, signals, and exit code pass through transparently. If the real-claude
    // env var ever fails to propagate, fall back to a PATH scan that skips this
    // shim dir — so a missing var degrades to "no stats", never a broken
    // `claude`. Pure POSIX builtins, no external tools. The var names here must
    // match REAL_CLAUDE_ENV / SETTINGS_ENV above.
    const POSIX_LAUNCHER: &str = r#"#!/bin/sh
# Generated by Arbiter — intercepts `claude` to capture Claude context usage.
_real="$ARBITER_REAL_CLAUDE"
if [ -z "$_real" ] || [ ! -x "$_real" ]; then
	_self=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd)
	_save=$IFS; IFS=:
	for _d in $PATH; do
		[ "$_d" = "$_self" ] && continue
		[ -x "$_d/claude" ] && { _real="$_d/claude"; break; }
	done
	IFS=$_save
fi
[ -z "$_real" ] && { echo "arbiter: real 'claude' not found" >&2; exit 127; }
if [ -n "$ARBITER_CLAUDE_SETTINGS" ]; then
	exec "$_real" --settings "$ARBITER_CLAUDE_SETTINGS" "$@"
fi
exec "$_real" "$@"
"#;
    let posix_path = bin_dir.join("claude");
    std::fs::write(&posix_path, POSIX_LAUNCHER).ok()?;
    make_executable(&posix_path);

    // Windows native shells (PowerShell/cmd) resolve `claude` via PATHEXT to a
    // `.cmd`. `call` handles the target being another `.cmd` (npm shim). Guards
    // against an unset real-claude var so we never invoke an empty path.
    #[cfg(windows)]
    {
        const WINDOWS_LAUNCHER: &str = "@echo off\r\n\
if not defined ARBITER_REAL_CLAUDE (echo arbiter: ARBITER_REAL_CLAUDE not set 1>&2 & exit /b 127)\r\n\
if defined ARBITER_CLAUDE_SETTINGS (\r\n\
call \"%ARBITER_REAL_CLAUDE%\" --settings \"%ARBITER_CLAUDE_SETTINGS%\" %*\r\n\
) else (\r\n\
call \"%ARBITER_REAL_CLAUDE%\" %*\r\n\
)\r\n\
exit /b %ERRORLEVEL%\r\n";
        std::fs::write(bin_dir.join("claude.cmd"), WINDOWS_LAUNCHER).ok()?;
    }
    Some(())
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}
#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

/// Write the settings file that points `statusLine` at our capture subcommand.
/// Built with serde_json so the binary path is correctly escaped.
fn write_settings(path: &Path, arbiter_bin: &Path) -> Option<()> {
    // statusLine.command is run by Claude via a shell, so quote the binary path.
    let command = format!("\"{}\" claude-statusline", arbiter_bin.display());
    let settings = serde_json::json!({
        "statusLine": { "type": "command", "command": command, "padding": 0 }
    });
    let json = serde_json::to_string_pretty(&settings).ok()?;
    std::fs::write(path, json).ok()?;
    Some(())
}

/// Read the user's configured `statusLine.command` (for call-through), honoring
/// `CLAUDE_CONFIG_DIR` and falling back to `~/.claude`.
fn read_user_statusline() -> Option<String> {
    let settings_path = claude_config_dir()?.join("settings.json");
    let data = std::fs::read_to_string(settings_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v.get("statusLine")?.get("command")?.as_str().map(str::to_string)
}

/// Resolve Claude's config dir: `$CLAUDE_CONFIG_DIR` if set, else `~/.claude`.
fn claude_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    Some(PathBuf::from(home).join(".claude"))
}
