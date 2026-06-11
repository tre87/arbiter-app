//! Tier-2 Claude context capture (cross-platform). Ported from the webview's
//! `src-tauri/src/claude_shim.rs` — it was already Tauri-free.
//!
//! Claude's exact context-window usage (window size, used %, per-component
//! tokens) is NOT in the transcript JSONL — it is only handed to a configured
//! `statusLine` command on stdin. To surface it, we intercept `claude` launches
//! inside Arbiter's own PTYs:
//!
//!   1. `shell.rs` prepends an Arbiter `bin/` dir (written here) to PATH for the
//!      spawned shell, so `claude` — and any alias that resolves it via PATH —
//!      hits our launcher.
//!   2. The launcher `exec`s the REAL claude with `--settings <file>` (generated
//!      here) that points `statusLine` at `<arbiter-bin> claude-statusline` and
//!      the Notification/PermissionRequest/Stop hooks at `<arbiter-bin> claude-hook`.
//!   3. Claude pipes its session JSON to those commands; we write it to
//!      `<capture-dir>/<session_id>.json` (and hook signals to `<hooks-dir>`),
//!      then call through to the user's original status line so it still renders.
//!
//! All JSON is built/parsed here in Rust. The launcher scripts contain no logic,
//! only a delegated `exec`.

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
/// Directory where the hook subcommand writes per-session attention/stop signals.
pub const HOOKS_DIR_ENV: &str = "ARBITER_HOOKS_DIR";
/// Unique-per-pane id set on each spawned shell. It rides through
/// shell→claude→statusLine/hook subcommand, so a capture/hook can be keyed to the
/// EXACT pane that launched Claude — robust under many simultaneous launches and
/// when several panes share a cwd (cwd alone can't disambiguate).
pub const PANE_ID_ENV: &str = "ARBITER_PANE_ID";

/// Subdir (under app-data) that holds the shim `bin/` and generated settings.
const SHIM_SUBDIR: &str = "claude-shim";
/// Subdir (under app-data) that holds per-session capture files.
pub const CAPTURE_SUBDIR: &str = "claude-sessions";
/// Subdir (under app-data) that holds per-session hook signal files.
pub const HOOKS_SUBDIR: &str = "claude-hooks";

/// Append a diagnostic line to `<temp>/arbiter-claude-debug.log` when
/// `ARBITER_CLAUDE_DEBUG` is set. Off by default (it would otherwise grow
/// unbounded — the statusLine fires on every render). The temp dir is shared
/// down the shell→claude→subcommand chain, so all steps land in one findable
/// file even though the subcommands' stderr is invisible.
pub fn debug_log(msg: &str) {
    if std::env::var_os("ARBITER_CLAUDE_DEBUG").is_none() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = std::env::temp_dir().join("arbiter-claude-debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

// ── Capture subcommand (`arbiter claude-statusline`) ─────────────────────────

/// Entry point for `claude-statusline`, invoked by Claude as its statusLine
/// command (via our injected `--settings`). Reads Claude's status JSON from
/// stdin, persists it keyed by `session_id`, then forwards to the user's
/// original status line so it still renders. Runs headless and returns.
pub fn run_statusline_capture() {
    let mut buf = Vec::new();
    if std::io::stdin().read_to_end(&mut buf).is_err() {
        return;
    }

    let sid = extract_session_id(&buf);
    // Key the capture by our pane id (so it binds to the EXACT pane that launched
    // Claude — robust when several panes share a cwd / launch at once). Falls back
    // to the session id when Claude wasn't launched in one of our shells.
    let key = std::env::var(PANE_ID_ENV)
        .ok()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        .or_else(|| sid.clone());
    debug_log(&format!(
        "statusline: invoked key={:?} session={:?} CAPTURE_DIR={:?} bytes={}",
        key,
        sid,
        std::env::var(CAPTURE_DIR_ENV).ok(),
        buf.len(),
    ));
    if let (Ok(dir), Some(key)) = (std::env::var(CAPTURE_DIR_ENV), key) {
        write_capture(Path::new(&dir), &key, &buf);
        debug_log(&format!("statusline: wrote capture to {dir}/{key}.json"));
    }

    if let Ok(orig) = std::env::var(ORIG_STATUSLINE_ENV) {
        if !orig.trim().is_empty() {
            forward_to_original(&orig, &buf);
        }
    }
}

/// Entry point for `claude-hook`, invoked by Claude's Notification /
/// PermissionRequest / Stop hooks. Reads the hook JSON from stdin and writes a
/// per-session signal file (`<HOOKS_DIR>/<session_id>.json`) the watcher routes
/// to the matching pane. Permission prompts/elicitation → `attention`; turn end
/// (`Stop`) → `stop`.
pub fn run_hook_signal() {
    let mut buf = Vec::new();
    if std::io::stdin().read_to_end(&mut buf).is_err() {
        return;
    }
    let v: serde_json::Value = match serde_json::from_slice(&buf) {
        Ok(v) => v,
        Err(_) => return,
    };
    let Some(session_id) = v.get("session_id").and_then(|s| s.as_str()) else { return };
    if session_id.is_empty()
        || !session_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return;
    }
    let event = v.get("hook_event_name").and_then(|s| s.as_str()).unwrap_or("");
    let ntype = v.get("notification_type").and_then(|s| s.as_str()).unwrap_or("");
    let signal = match event {
        "Stop" => "stop",
        "PermissionRequest" => "attention",
        "Notification" => match ntype {
            // `idle_prompt` is intentionally excluded — it fires after ~60s idle
            // and would turn a merely-idle pane amber (we keep idle grey).
            //
            // `elicitation_dialog` (AskUserQuestion) is ALSO excluded: it's detected
            // level-based from the rendered menu (`VtTerm::visible_menu`), which
            // clears the instant the user escapes/answers. The hook is edge-only and
            // would get STUCK on escape (no spinner/Stop follows to clear it).
            "permission_prompt" => "attention",
            _ => return,
        },
        _ => return,
    };

    let Ok(dir) = std::env::var(HOOKS_DIR_ENV) else { return };
    let dir = Path::new(&dir);
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // A nonce makes every write distinct so the watcher fires even on a repeat.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let body = format!("{{\"signal\":\"{signal}\",\"nonce\":{nonce}}}");
    let final_path = dir.join(format!("{session_id}.json"));
    let tmp_path = dir.join(format!("{session_id}.json.tmp"));
    if std::fs::write(&tmp_path, body).is_ok() {
        let _ = std::fs::rename(&tmp_path, &final_path);
    }
}

/// Parse just the `session_id` from Claude's status JSON.
fn extract_session_id(buf: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(buf).ok()?;
    let id = v.get("session_id")?.as_str()?;
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

/// Run the user's original `statusLine.command` with the same stdin so Claude
/// renders it unchanged.
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
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_bytes);
    }
    let _ = child.wait();
}

// ── Reading captures back (the footer's Tier-2 stats) ────────────────────────

/// Parsed Claude statusLine capture: stats + the cwd/session_id used to bind it
/// to a pane. `used_percent`/token usage are None/0 until the first turn.
#[derive(Clone, Debug)]
pub struct Capture {
    pub session_id: String,
    pub cwd: String,
    /// The capture file's stem — the pane id (`PANE_ID_ENV`) when Claude ran in one
    /// of our shells, so a capture binds to the exact pane; else the session id
    /// (legacy / Claude launched outside our shell). The primary bind key.
    pub key: String,
    /// The capture file's last-modified time — fallback to pick the LIVE session
    /// when binding by cwd (the live one is written most recently).
    pub mtime: std::time::SystemTime,
    pub model: Option<String>,
    pub context_size: Option<u64>,
    pub used_percent: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
}

/// Parse one capture JSON (the shape Claude's statusLine emits).
pub fn parse_capture(bytes: &[u8]) -> Option<Capture> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    let cwd = v
        .get("cwd")
        .and_then(|c| c.as_str())
        .or_else(|| v.pointer("/workspace/current_dir").and_then(|c| c.as_str()))?
        .to_string();
    let cw = v.get("context_window");
    let usage = cw.and_then(|c| c.get("current_usage"));
    let tok = |k: &str| usage.and_then(|u| u.get(k)).and_then(|n| n.as_u64()).unwrap_or(0);
    Some(Capture {
        session_id,
        cwd,
        key: String::new(),                       // filled in by read_captures (file stem)
        mtime: std::time::SystemTime::UNIX_EPOCH, // filled in by read_captures
        model: v.pointer("/model/display_name").and_then(|m| m.as_str()).map(str::to_string),
        context_size: cw.and_then(|c| c.get("context_window_size")).and_then(|n| n.as_u64()),
        used_percent: cw.and_then(|c| c.get("used_percentage")).and_then(|n| n.as_f64()),
        input_tokens: tok("input_tokens"),
        output_tokens: tok("output_tokens"),
        cache_write: tok("cache_creation_input_tokens"),
        cache_read: tok("cache_read_input_tokens"),
        cost_usd: v.pointer("/cost/total_cost_usd").and_then(|n| n.as_f64()).unwrap_or(0.0),
    })
}

/// Read + parse every `<sid>.json` in the capture dir.
pub fn read_captures(dir: &Path) -> Vec<Capture> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(bytes) = std::fs::read(&p) {
                    if let Some(mut c) = parse_capture(&bytes) {
                        c.key = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                        c.mtime = entry
                            .metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        out.push(c);
                    }
                }
            }
        }
    }
    out
}

// ── Spawn-time setup (called from shell.rs) ──────────────────────────────────

/// Everything `shell.rs` needs to wire the spawned shell for interception.
pub struct ShimSetup {
    pub bin_dir: PathBuf,
    pub real_claude: Option<PathBuf>,
    pub settings_file: PathBuf,
    pub capture_dir: PathBuf,
    pub hooks_dir: PathBuf,
    pub orig_statusline: Option<String>,
}

/// Prepare the shim dir, launcher(s), and settings file under `data_dir`.
/// Returns `None` only if a required file write fails.
pub fn setup(data_dir: &Path, original_path: &str, arbiter_bin: &Path) -> Option<ShimSetup> {
    let shim_dir = data_dir.join(SHIM_SUBDIR);
    let bin_dir = shim_dir.join("bin");
    let capture_dir = data_dir.join(CAPTURE_SUBDIR);
    let hooks_dir = data_dir.join(HOOKS_SUBDIR);

    // May be None when the GUI's PATH lacks claude (Finder-launched .app); not
    // fatal — the launcher falls back to a PATH scan in the spawned shell.
    let real_claude = find_real_claude(original_path, &bin_dir);

    std::fs::create_dir_all(&bin_dir).ok()?;
    std::fs::create_dir_all(&capture_dir).ok()?;
    std::fs::create_dir_all(&hooks_dir).ok()?;

    let settings_file = shim_dir.join("settings.json");
    write_settings(&settings_file, arbiter_bin)?;
    write_launchers(&bin_dir)?;

    debug_log(&format!(
        "setup: bin_dir={} real_claude={:?} settings={} arbiter_bin={}",
        bin_dir.display(),
        real_claude,
        settings_file.display(),
        arbiter_bin.display(),
    ));

    Some(ShimSetup {
        bin_dir,
        real_claude,
        settings_file,
        capture_dir,
        hooks_dir,
        orig_statusline: read_user_statusline(),
    })
}

/// Resolve the real `claude` by scanning `original_path`, skipping `bin_dir`.
fn find_real_claude(original_path: &str, bin_dir: &Path) -> Option<PathBuf> {
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

/// Write the launcher(s) into `bin_dir`. The launcher `exec`s the real claude
/// with our `--settings`, forwarding all args; no logic.
fn write_launchers(bin_dir: &Path) -> Option<()> {
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

/// Write the settings file pointing `statusLine` at our capture subcommand and
/// the attention hooks at our hook subcommand. Claude merges this with the
/// user's own settings/hooks (loaded via `--settings`).
fn write_settings(path: &Path, arbiter_bin: &Path) -> Option<()> {
    // Commands run via a shell; quote the path. On Windows that shell may be Git
    // Bash, where backslashes are escapes — normalise to forward slashes (valid
    // for cmd/PowerShell/Git Bash alike).
    #[cfg(windows)]
    let bin = arbiter_bin.display().to_string().replace('\\', "/");
    #[cfg(not(windows))]
    let bin = arbiter_bin.display().to_string();
    let status_command = format!("\"{bin}\" claude-statusline");
    let hook_command = format!("\"{bin}\" claude-hook");
    let hook_entry = serde_json::json!({
        "hooks": [{ "type": "command", "command": hook_command }]
    });
    let settings = serde_json::json!({
        "statusLine": { "type": "command", "command": status_command, "padding": 0 },
        "hooks": {
            "Notification": [hook_entry.clone()],
            "PermissionRequest": [hook_entry.clone()],
            "Stop": [hook_entry],
        }
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
pub fn claude_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    Some(PathBuf::from(home).join(".claude"))
}
