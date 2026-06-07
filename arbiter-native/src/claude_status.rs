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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

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
/// session id) and where to write its status.
pub struct ClaudeHandle {
    pub shell_pid: Option<u32>,
    pub cwd: Arc<Mutex<Option<String>>>,
    pub claude_running: Arc<AtomicBool>,
    pub status: Mutex<ClaudeStatus>,
    /// Claude session id once a capture binds it (so hooks route here).
    pub session_id: Mutex<Option<String>>,
    /// Highest hook nonce applied (so a repeated signal still fires once).
    pub last_nonce: Mutex<u128>,
}

static REGISTRY: Mutex<Vec<Weak<ClaudeHandle>>> = Mutex::new(Vec::new());

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
        *h.session_id.lock().unwrap() = Some(c.session_id.clone());
        let mut st = h.status.lock().unwrap();
        st.model = c.model.clone();
        st.context_size = c.context_size;
        st.used_percent = c.used_percent;
        st.input_tokens = c.input_tokens;
        st.output_tokens = c.output_tokens;
        st.cache_read = c.cache_read;
        st.cache_write = c.cache_write;
        st.cost_usd = c.cost_usd;
        st.has_stats = true;
        if st.lifecycle == Lifecycle::Closed {
            st.lifecycle = Lifecycle::Ready;
        }
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
        let mut st = h.status.lock().unwrap();
        st.lifecycle = match signal {
            "attention" => Lifecycle::Attention,
            "stop" => Lifecycle::Ready,
            _ => st.lifecycle,
        };
    }
}
