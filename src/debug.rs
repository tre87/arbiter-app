//! `ARBITER_CLAUDE_DEBUG` diagnostics, shared by every module that has something worth
//! logging from a place whose stderr nobody sees (hook subcommands, the reader thread).

use std::io::Write;

/// Whether `ARBITER_CLAUDE_DEBUG` is set, for callers that would otherwise pay to build a
/// message that `debug_log` throws away. The name predates other agents and is kept so
/// existing setups keep logging.
pub fn debug_enabled() -> bool {
    std::env::var_os("ARBITER_CLAUDE_DEBUG").is_some()
}

/// Append a line to `<temp>/arbiter-claude-debug.log` when `ARBITER_CLAUDE_DEBUG` is set.
/// Off by default: hooks and the reader call this often enough to grow it without bound.
/// The temp dir is shared down the shell → agent → subcommand chain, so every step lands
/// in one findable file.
pub fn debug_log(msg: &str) {
    if !debug_enabled() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = std::env::temp_dir().join("arbiter-claude-debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{ts}] {msg}");
    }
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
