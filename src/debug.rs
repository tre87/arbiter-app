//! The debug log, shared by every module that has something worth logging from a place
//! whose stderr nobody sees (hook subcommands, the reader thread). Switched on in
//! Settings, or by `ARBITER_CLAUDE_DEBUG` for development.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// What Settings said, in the app's own process: `UNSET` everywhere else (the hook
/// subcommands), which read `MARKER` instead.
static SETTING: AtomicU8 = AtomicU8::new(UNSET);
const UNSET: u8 = 0;
const OFF: u8 = 1;
const ON: u8 = 2;

/// In the data dir while the setting is on. A hook subcommand is a process of its own,
/// started by the agent from a shell that may predate the setting, so a file is what
/// reaches it.
const MARKER: &str = "debug-log.on";
const LOG_NAME: &str = "arbiter-debug.log";
/// Past this the log is moved to `<name>.old.log` and begun again: one long session
/// with it on wrote 165 MB.
const LOG_CAP: u64 = 32 << 20;

/// `ARBITER_CLAUDE_DEBUG`, the developers' switch: on whatever Settings say, and
/// logging to the temp dir, which the shell → agent → subcommand chain shares and the
/// live tests point at a folder of their own. The name predates other agents.
fn forced() -> bool {
    static FORCED: OnceLock<bool> = OnceLock::new();
    *FORCED.get_or_init(|| std::env::var_os("ARBITER_CLAUDE_DEBUG").is_some())
}

/// Whether the debug log is on, for callers that would otherwise pay to build a
/// message that `debug_log` throws away.
pub fn debug_enabled() -> bool {
    if forced() {
        return true;
    }
    match SETTING.load(Ordering::Relaxed) {
        ON => true,
        OFF => false,
        _ => {
            static MARKED: OnceLock<bool> = OnceLock::new();
            *MARKED.get_or_init(|| crate::shell::app_data_dir().is_some_and(|d| d.join(MARKER).exists()))
        }
    }
}

/// The app: Settings switched the log on or off (and at startup, what they say).
pub fn set_enabled(on: bool) {
    SETTING.store(if on { ON } else { OFF }, Ordering::Relaxed);
    let Some(marker) = crate::shell::app_data_dir().map(|d| d.join(MARKER)) else { return };
    if on {
        let _ = std::fs::write(marker, b"");
    } else {
        let _ = std::fs::remove_file(marker);
    }
}

/// Where the log is written: next to the executable, so it sits with the installed
/// app, or in the data dir when that folder cannot be written to. `None` with nowhere
/// to write. Probing for that creates the file, so this is only asked once the log is on.
pub fn log_path() -> Option<&'static Path> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        if forced() {
            return Some(std::env::temp_dir().join("arbiter-claude-debug.log"));
        }
        let writable = |p: &Path| std::fs::OpenOptions::new().create(true).append(true).open(p).is_ok();
        let beside = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join(LOG_NAME)));
        beside
            .filter(|p| writable(p))
            .or_else(|| crate::shell::app_data_dir().map(|d| d.join(LOG_NAME)))
    })
    .as_deref()
}

/// Append a line to the log (`log_path`) when it is on.
pub fn debug_log(msg: &str) {
    if !debug_enabled() {
        return;
    }
    let Some(path) = log_path() else { return };
    let open = || std::fs::OpenOptions::new().create(true).append(true).open(path);
    let Ok(mut f) = open() else { return };
    if f.metadata().is_ok_and(|m| m.len() > LOG_CAP) {
        drop(f);
        let _ = std::fs::rename(path, path.with_extension("old.log"));
        let Ok(fresh) = open() else { return };
        f = fresh;
    }
    let _ = writeln!(f, "[{}] {msg}", now_ms());
}

/// A recorder file stops growing past this.
const RECORDING_CAP: u64 = 8 << 20;

/// Per-pane recordings for writing an agent's `ScreenProfile` from what it really draws:
/// `ARBITER_RECORD_PTY=1` appends every output chunk to `<temp>/arbiter-pty-<pane>.jsonl`
/// (`{"t":ms,"data":"…"}`), and `ARBITER_SCREEN_DUMP=1` appends the visible rows to
/// `<temp>/arbiter-screen-<pane>.log` whenever the screen reading, the lifecycle or a
/// terminal signal changes. Both hold whatever was on screen, secrets included, so they
/// are for a debugging session only. The variables are read once, at the pane's start.
pub struct PaneRecorder {
    pty: Option<(std::fs::File, u64)>,
    screen: Option<(std::fs::File, u64)>,
    last: Option<(crate::agent_screen::ScreenState, crate::agent_state::Lifecycle)>,
}

impl PaneRecorder {
    pub fn new(pane: u64) -> Self {
        let open = |var: &str, name: String| {
            std::env::var_os(var)?;
            let f = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join(name)).ok()?;
            Some((f, 0))
        };
        Self {
            pty: open("ARBITER_RECORD_PTY", format!("arbiter-pty-{pane}.jsonl")),
            screen: open("ARBITER_SCREEN_DUMP", format!("arbiter-screen-{pane}.log")),
            last: None,
        }
    }

    pub fn wants_screen(&self) -> bool {
        self.screen.is_some()
    }

    pub fn chunk(&mut self, bytes: &[u8]) {
        let Some((f, written)) = self.pty.as_mut().filter(|(_, w)| *w < RECORDING_CAP) else { return };
        let line = serde_json::json!({ "t": now_ms(), "data": String::from_utf8_lossy(bytes) }).to_string();
        if writeln!(f, "{line}").is_ok() {
            *written += line.len() as u64 + 1;
        }
    }

    /// Record `rows` if what was read from them, or the signals, say something new.
    pub fn screen(
        &mut self,
        kind: crate::agent::AgentKind,
        rows: impl FnOnce() -> Vec<String>,
        state: crate::agent_screen::ScreenState,
        lifecycle: crate::agent_state::Lifecycle,
        signals: &[crate::term_signal::TermSignal],
    ) {
        let Some((f, written)) = self.screen.as_mut().filter(|(_, w)| *w < RECORDING_CAP) else { return };
        if self.last == Some((state, lifecycle)) && signals.is_empty() {
            return;
        }
        self.last = Some((state, lifecycle));
        let mut out = format!("[{}] {kind:?} {lifecycle:?} {state:?} signals={signals:?}\n", now_ms());
        for row in rows() {
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out.push('\n');
        if f.write_all(out.as_bytes()).is_ok() {
            *written += out.len() as u64;
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
