//! Per-pane Claude status, updated event-driven (no polling) by a single `notify`
//! watcher over the capture + hook dirs — the same model the webview used and
//! the native git footer uses.
//!
//! Each `Session` owns an `Arc<ClaudeHandle>` (shared with the watcher via a
//! global registry of `Weak` handles) holding its live `ClaudeStatus`; `view()`
//! reads it each frame. Captures (`<data>/claude-sessions/<sid>.json`) bind to a
//! pane by cwd and fill the stats; hook signals (`<data>/claude-hooks/<sid>.json`)
//! flip the lifecycle (Stop→ready, Permission/elicitation→attention).

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};

pub type Watcher = Debouncer<RecommendedWatcher>;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Lifecycle {
    #[default]
    Closed,
    Ready,
    Working,
    Attention,
}

/// Live Claude status for one pane (stats from the Tier-2 capture + lifecycle).
#[derive(Clone, Default)]
pub struct ClaudeStatus {
    pub lifecycle: Lifecycle,
    pub model: Option<String>,
    pub context_size: Option<u64>,
    pub used_percent: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
    /// A capture has bound (stats are populated).
    pub has_stats: bool,
}

/// The watcher's view of one session: how to match it (cwd + alive flag + bound
/// session id), the captured stats, and the timestamps the lifecycle is derived
/// from. The lifecycle is *computed* (not stored) so the reader thread (activity/
/// menu) and the watcher thread (hooks) never fight over a single field — each
/// just stamps its latest event time.
pub struct ClaudeHandle {
    pub shell_pid: Option<u32>,
    pub cwd: Arc<Mutex<Option<String>>>,
    pub claude_running: Arc<AtomicBool>,
    stats: Mutex<ClaudeStatus>,
    /// Claude session id once a capture binds it (so hooks route here).
    session_id: Mutex<Option<String>>,
    /// Highest hook nonce applied (so a repeated signal still fires once).
    last_nonce: Mutex<u128>,
    /// Last-event times (ms since epoch); 0 = never.
    activity_ms: AtomicU64, // spinner / "esc to interrupt" (working)
    stop_ms: AtomicU64,     // Stop hook (turn end)
    /// A text menu/approval prompt is currently on the visible screen — level-
    /// triggered by the reader's grid scan, so it clears the instant the prompt
    /// leaves (the user escapes/answers). Covers AskUserQuestion / plan / proceed.
    menu_on_screen: AtomicBool,
    /// A permission/elicitation hook fired (edge-triggered) — cleared when Claude
    /// resumes (activity) or the turn ends (Stop). Covers tool-permission prompts
    /// that don't show a grid marker.
    hook_attention: AtomicBool,
}

/// Working reverts to ready after this long without a detected spinner frame.
/// Must comfortably exceed the gap BETWEEN detected frames: Claude's ✻ bloom
/// passes through `·` frames (not in our star range) and animates slower while
/// "thinking", so a too-short TTL makes working flicker on/off between frames.
/// The web used 2s; matching it. The turn-end stays instant regardless — the Stop
/// hook (+ post-stop guard) clears working immediately; this only bounds the
/// no-hook fallback.
const WORKING_TTL_MS: u64 = 2000;
/// After a Stop hook, treat the turn as over: ignore a trailing spinner frame (the
/// final redraw) and force ready for this long, so the turn-end can't flicker
/// working→ready→working.
const STOP_SUPPRESS_MS: u64 = 700;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl ClaudeHandle {
    pub fn new(
        shell_pid: Option<u32>,
        cwd: Arc<Mutex<Option<String>>>,
        claude_running: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            shell_pid,
            cwd,
            claude_running,
            stats: Mutex::new(ClaudeStatus::default()),
            session_id: Mutex::new(None),
            last_nonce: Mutex::new(0),
            activity_ms: AtomicU64::new(0),
            stop_ms: AtomicU64::new(0),
            menu_on_screen: AtomicBool::new(false),
            hook_attention: AtomicBool::new(false),
        })
    }

    /// Reader: Claude's working spinner is on screen. Also resolves any pending
    /// permission attention — Claude has resumed, so it's working, not waiting.
    pub fn note_activity(&self) {
        let now = now_ms();
        // A spinner frame inside the post-Stop window is the turn's FINAL redraw —
        // ignore it so it can't revive "working" after Stop already ended the turn.
        // (A genuinely new turn's frames land well after the window.)
        if now.saturating_sub(self.stop_ms.load(Ordering::Relaxed)) < STOP_SUPPRESS_MS {
            return;
        }
        self.activity_ms.store(now, Ordering::Relaxed);
        self.hook_attention.store(false, Ordering::Relaxed);
    }

    /// Reader: whether a menu/approval prompt is currently on the visible screen.
    pub fn set_menu(&self, on: bool) {
        self.menu_on_screen.store(on, Ordering::Relaxed);
    }

    /// Reader: a menu/prompt just LEFT the screen (answered or escaped) → resolve
    /// any hook-set attention. AskUserQuestion fires a permission/elicitation hook
    /// but escaping it produces no spinner/Stop to clear that hook, so it would
    /// hang amber. Called only on the on→off edge, so a markerless prompt (which
    /// never sets a menu) is never cleared prematurely.
    pub fn clear_hook_attention(&self) {
        self.hook_attention.store(false, Ordering::Relaxed);
    }

    /// The bound Claude session id (set once a capture matches this pane), for
    /// `claude --resume` on restore.
    pub fn session_id(&self) -> Option<String> {
        self.session_id.lock().unwrap().clone()
    }

    /// The session id ONLY if `claude --resume` will actually find it — i.e. its
    /// transcript exists on disk. A freshly-launched Claude has a session id (shown
    /// in the statusline) but no conversation, so no transcript, so resuming it
    /// errors ("no conversation found"); we return `None` and let restore launch a
    /// clean `claude`. This is the webview's check: the transcript lives at
    /// `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`.
    pub fn resumable_session(&self) -> Option<String> {
        let sid = self.session_id.lock().unwrap().clone()?;
        let cwd = self.cwd.lock().unwrap().clone()?;
        let transcript = dirs::home_dir()?
            .join(".claude")
            .join("projects")
            .join(encode_project_dir(&cwd))
            .join(format!("{sid}.jsonl"));
        transcript.is_file().then_some(sid)
    }

    /// Derived lifecycle: the most recent signal wins; activity counts as
    /// "working" only while fresh, then reverts to ready.
    fn lifecycle(&self) -> Lifecycle {
        // Attention is level-based: a prompt on screen, or an unresolved hook.
        if self.menu_on_screen.load(Ordering::Relaxed)
            || self.hook_attention.load(Ordering::Relaxed)
        {
            return Lifecycle::Attention;
        }
        let act = self.activity_ms.load(Ordering::Relaxed);
        let stop = self.stop_ms.load(Ordering::Relaxed);
        let now = now_ms();
        // Just stopped: clean turn-end — ignore a trailing spinner frame so it
        // doesn't flicker working→ready→working.
        if stop != 0 && now.saturating_sub(stop) < STOP_SUPPRESS_MS {
            return Lifecycle::Ready;
        }
        // Working while activity is fresh and more recent than the last turn-end.
        if act > stop && now.saturating_sub(act) < WORKING_TTL_MS {
            return Lifecycle::Working;
        }
        Lifecycle::Ready
    }

    /// Snapshot for the view: stats + the currently-derived lifecycle.
    pub fn snapshot(&self) -> ClaudeStatus {
        let mut s = self.stats.lock().unwrap().clone();
        s.lifecycle = self.lifecycle();
        s
    }
}

/// Claude stores each session's transcript under `~/.claude/projects/<dir>/`,
/// where `<dir>` is the cwd with every non-alphanumeric (and non-`-`) char
/// replaced by `-`. (Mirrors the webview's `encode_project_dir`.)
fn encode_project_dir(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect()
}

static REGISTRY: Mutex<Vec<Weak<ClaudeHandle>>> = Mutex::new(Vec::new());

/// Set by the watcher when a Claude session newly binds to a pane, so the UI knows
/// to persist the layout (capturing "Claude is running here, resume id X") without
/// the watcher needing access to the window state. The UI clears it on save.
pub static SAVE_DIRTY: AtomicBool = AtomicBool::new(false);

/// Register a session handle for the watcher to update.
pub fn register(handle: &Arc<ClaudeHandle>) {
    let mut reg = REGISTRY.lock().unwrap();
    reg.retain(|w| w.strong_count() > 0); // prune closed panes
    reg.push(Arc::downgrade(handle));
}

fn live_handles() -> Vec<Arc<ClaudeHandle>> {
    REGISTRY.lock().unwrap().iter().filter_map(Weak::upgrade).collect()
}

/// Start the capture + hook watcher. Returns the debouncer (keep it alive for
/// the app's lifetime).
pub fn start_watcher() -> Option<Watcher> {
    let data_dir = crate::shell::app_data_dir()?;
    let capture_dir = data_dir.join(crate::claude_shim::CAPTURE_SUBDIR);
    let hooks_dir = data_dir.join(crate::claude_shim::HOOKS_SUBDIR);
    let _ = std::fs::create_dir_all(&capture_dir);
    let _ = std::fs::create_dir_all(&hooks_dir);

    let (cap, hk) = (capture_dir.clone(), hooks_dir.clone());
    let mut deb = new_debouncer(Duration::from_millis(80), move |res: DebounceEventResult| {
        if res.is_ok() {
            process_captures(&cap);
            process_hooks(&hk);
        }
    })
    .ok()?;
    deb.watcher().watch(&capture_dir, RecursiveMode::NonRecursive).ok()?;
    deb.watcher().watch(&hooks_dir, RecursiveMode::NonRecursive).ok()?;
    // Process once up front so existing captures bind immediately.
    process_captures(&capture_dir);
    process_hooks(&hooks_dir);
    Some(deb)
}

/// Re-read captures and route each to the pane whose cwd matches (and where
/// Claude is running), filling its stats + binding its session id.
fn process_captures(dir: &Path) {
    let handles = live_handles();
    for c in crate::claude_shim::read_captures(dir) {
        let Some(h) = handles.iter().find(|h| {
            h.claude_running.load(Ordering::Relaxed)
                && h.cwd.lock().unwrap().as_deref() == Some(c.cwd.as_str())
        }) else {
            continue;
        };
        {
            // Bind the session id; flag a save only when it's NEWLY bound (not on
            // every statusline refresh), so the restored layout knows to resume it.
            let mut sid = h.session_id.lock().unwrap();
            if sid.as_deref() != Some(c.session_id.as_str()) {
                *sid = Some(c.session_id.clone());
                SAVE_DIRTY.store(true, Ordering::Relaxed);
            }
        }
        let mut st = h.stats.lock().unwrap();
        st.model = c.model.clone();
        st.context_size = c.context_size;
        st.used_percent = c.used_percent;
        st.input_tokens = c.input_tokens;
        st.output_tokens = c.output_tokens;
        st.cache_read = c.cache_read;
        st.cache_write = c.cache_write;
        st.cost_usd = c.cost_usd;
        st.has_stats = true;
    }
}

/// Re-read hook signals and apply them to the pane whose bound session id matches.
fn process_hooks(dir: &Path) {
    let handles = live_handles();
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let sid = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let signal = v.get("signal").and_then(|s| s.as_str()).unwrap_or("");
        let nonce = v.get("nonce").and_then(|n| n.as_u64()).unwrap_or(0) as u128;
        let Some(h) = handles
            .iter()
            .find(|h| h.session_id.lock().unwrap().as_deref() == Some(sid.as_str()))
        else {
            continue;
        };
        let mut last = h.last_nonce.lock().unwrap();
        if nonce <= *last {
            continue;
        }
        *last = nonce;
        match signal {
            "attention" => h.hook_attention.store(true, Ordering::Relaxed),
            "stop" => {
                h.stop_ms.store(now_ms(), Ordering::Relaxed);
                h.hook_attention.store(false, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::encode_project_dir;

    #[test]
    fn encodes_cwd_like_claude() {
        // Matches the real on-disk dir under ~/.claude/projects/.
        assert_eq!(
            encode_project_dir("/Users/tor/Private/Source/arbiter-app"),
            "-Users-tor-Private-Source-arbiter-app"
        );
        assert_eq!(encode_project_dir("/a/b-c.d_e"), "-a-b-c-d-e");
    }
}
