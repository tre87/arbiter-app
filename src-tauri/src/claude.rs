use notify::{Config as NotifyConfig, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use sysinfo::{ProcessRefreshKind, System, UpdateKind};
use tauri::{AppHandle, Emitter, State};

use crate::pty::PtySession;

/// State for an active Claude process detected under a pane's shell.
#[derive(Clone)]
pub struct ClaudeEntry {
    jsonl: PathBuf,
}

// Tracks active Claude processes: pane_id → ClaudeEntry
pub struct ClaudeMonitor(pub Arc<Mutex<HashMap<String, ClaudeEntry>>>);

// Frontend-registered expected Claude session id per pane (for `claude --resume`).
// Lets the JSONL adoption logic pick the *correct* pane when several panes are
// waiting for adoption simultaneously, instead of racing on whichever empty
// pane the HashMap iterator yields first.
pub struct ExpectedClaudeSessions(pub Arc<Mutex<HashMap<String, String>>>);

#[derive(Serialize, Clone)]
pub struct ClaudeSessionStatus {
    session_id: String,
    model_id: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    folder: Option<String>,
    branch: Option<String>,
}

/// Frontend tells us the Claude session id it's about to resume in a pane,
/// so the JSONL watcher can adopt the matching file into the *correct* pane
/// (rather than the first empty one it finds).
#[tauri::command]
pub fn set_expected_claude_session(
    session_id: String,
    claude_session_id: String,
    expected: State<ExpectedClaudeSessions>,
) {
    expected.0.lock().unwrap().insert(session_id, claude_session_id);
}

/// Frontend detected Claude exit via shell-activity (OSC 133 idle) before
/// the backend's PID-based exit watcher could fire.  Clear the monitor entry
/// so the pane is no longer considered "tracked" and future JSONL files can
/// be adopted into it.
#[tauri::command]
pub fn clear_claude_monitor(session_id: String, monitor: State<ClaudeMonitor>) {
    monitor.0.lock().unwrap().remove(&session_id);
}

// ── Claude process monitoring (file-system events + per-PID exit watch) ─────

/// Walk up the parent chain of `pid` and return true if `ancestor` is found.
fn is_descendant(sys: &System, pid: u32, ancestor: u32) -> bool {
    let mut cur = pid;
    loop {
        if cur == ancestor { return true; }
        if cur == 0 { break; }
        match sys.process(sysinfo::Pid::from_u32(cur)).and_then(|p| p.parent()) {
            Some(p) => cur = p.as_u32(),
            None => break,
        }
    }
    false
}

static SHARED_SYSTEM: std::sync::OnceLock<SharedSystem> = std::sync::OnceLock::new();
fn shared_system() -> &'static SharedSystem {
    SHARED_SYSTEM.get_or_init(SharedSystem::new)
}

/// Shared sysinfo System instance used by every Claude-descendant scan.
/// Building a fresh `System::new()` + full process refresh costs 1–5 ms and
/// was happening once per pane per second under the old implementation. A
/// single shared System with a 250 ms minimum refresh gate collapses that
/// cost regardless of pane count.
struct SharedSystem {
    sys: Mutex<(System, std::time::Instant)>,
}

impl SharedSystem {
    fn new() -> Self {
        Self {
            sys: Mutex::new((
                System::new(),
                std::time::Instant::now() - std::time::Duration::from_secs(60),
            )),
        }
    }

    /// Run `f` against a recently-refreshed sysinfo snapshot. If another
    /// caller refreshed within `max_age`, that snapshot is reused.
    fn with<T>(&self, max_age: std::time::Duration, f: impl FnOnce(&System) -> T) -> T {
        let mut guard = self.sys.lock().unwrap();
        let (sys, last) = &mut *guard;
        if last.elapsed() >= max_age {
            sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::new().with_cmd(UpdateKind::Always),
            );
            *last = std::time::Instant::now();
        }
        f(sys)
    }
}

fn is_claude_process(proc: &sysinfo::Process) -> bool {
    let name = proc.name().to_string_lossy().to_lowercase();
    if name.starts_with("claude") { return true; }
    if !name.contains("node") { return false; }
    proc.cmd().iter().any(|s| s.to_string_lossy().to_lowercase().contains("claude"))
}

/// Find the most recently started Claude process that is a descendant of the
/// given shell. Returns (claude_pid, start_time) or None. Operates against a
/// caller-provided sysinfo snapshot so callers can amortise the refresh cost.
fn find_claude_in(sys: &System, shell_pid: u32) -> Option<(u32, u64)> {
    let mut best: Option<(u32, u64)> = None;
    for (pid, proc) in sys.processes() {
        if !is_claude_process(proc) { continue; }
        if !is_descendant(sys, pid.as_u32(), shell_pid) { continue; }
        let start_time = proc.start_time();
        if best.as_ref().map_or(true, |(_, t)| start_time > *t) {
            best = Some((pid.as_u32(), start_time));
        }
    }
    best
}

/// Encode a working directory the way the Claude CLI does for its
/// `~/.claude/projects/<encoded>/` directory: every character that isn't
/// `[a-zA-Z0-9-]` becomes `-`. Verified against existing project dirs:
///   `C:\Users\TRE\source\arbiter-app` → `C--Users-TRE-source-arbiter-app`.
/// This lets us tie a JSONL to a pane via its parent directory alone, without
/// having to read the file's contents (Claude doesn't write `cwd` lines until
/// the first user turn, so content-based matching is unreliable for fresh
/// sessions that haven't received a message yet).
fn encode_project_dir(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}

/// True if `jsonl_path`'s parent directory matches the encoded form of
/// `pane_cwd`. Case-insensitive on Windows; case-sensitive elsewhere is fine
/// because both inputs come from the same source.
fn jsonl_parent_matches_cwd(jsonl_path: &std::path::Path, pane_cwd: &str) -> bool {
    let Some(parent) = jsonl_path.parent() else { return false };
    let Some(parent_name) = parent.file_name().and_then(|n| n.to_str()) else { return false };
    let expected = encode_project_dir(pane_cwd);
    parent_name.eq_ignore_ascii_case(&expected)
}

/// Look up a pane's current cwd in the sessions map.
fn pane_cwd(
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    pane_id: &str,
) -> Option<String> {
    let s = sessions.lock().unwrap();
    let session = s.get(pane_id)?;
    let cwd = session.cwd.lock().unwrap().clone();
    cwd
}

/// Parse the last assistant message from a JSONL file for model/token data.
fn parse_jsonl_status(path: &std::path::Path) -> Option<ClaudeSessionStatus> {
    let content = std::fs::read_to_string(path).ok()?;
    let session_id = path.file_stem()?.to_str()?.to_string();

    let mut cwd: Option<String>      = None;
    let mut branch: Option<String>   = None;
    let mut model_id: Option<String> = None;
    let mut input_tokens: Option<u64>  = None;
    let mut output_tokens: Option<u64> = None;
    let mut cache_creation: Option<u64> = None;
    let mut cache_read: Option<u64>     = None;

    for line in content.lines() {
        if line.is_empty() { continue; }
        let entry = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => v,
            Err(e) => {
                // Partial/corrupt lines happen at the trailing edge when Claude
                // is mid-write. Log at debug level so they're not silent but
                // don't spam on every parse.
                eprintln!("parse_jsonl_status: skipping malformed line in {}: {e}", path.display());
                continue;
            }
        };

        if let Some(c) = entry.get("cwd").and_then(|v| v.as_str()) {
            cwd = Some(c.to_string());
        }
        if let Some(b) = entry.get("gitBranch").and_then(|v| v.as_str()) {
            branch = Some(b.to_string());
        }
        if entry.pointer("/message/role").and_then(|v| v.as_str()) == Some("assistant") {
            if let Some(m) = entry.pointer("/message/model").and_then(|v| v.as_str()) {
                model_id = Some(m.to_string());
            }
            if let Some(u) = entry.pointer("/message/usage") {
                input_tokens   = u.get("input_tokens").and_then(|v| v.as_u64()).or(input_tokens);
                output_tokens  = u.get("output_tokens").and_then(|v| v.as_u64()).or(output_tokens);
                cache_creation = u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).or(cache_creation);
                cache_read     = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).or(cache_read);
            }
        }
    }

    let folder = cwd.as_deref().map(|p| {
        p.replace('\\', "/").split('/').filter(|s| !s.is_empty())
            .last().unwrap_or(p).to_string()
    });

    Some(ClaudeSessionStatus { session_id, model_id, input_tokens, output_tokens,
        cache_creation_input_tokens: cache_creation, cache_read_input_tokens: cache_read,
        folder, branch })
}

/// Block the current thread until the process with `pid` exits.
///
/// Windows uses `WaitForSingleObject(INFINITE)` on a process handle — zero CPU,
/// event-driven, wakes the instant the kernel marks the process signalled.
///
/// Unix falls back to a cheap single-PID `sysinfo` refresh loop: the kernel
/// alternatives (`pidfd_open` on Linux ≥5.3, `kqueue` + `EVFILT_PROC` on macOS)
/// would require `libc` and more unsafe code; the per-PID refresh is a single
/// `readlink /proc/<pid>` on Linux and a single sysctl on macOS, so the poll
/// stays negligible in practice.
#[cfg(windows)]
fn wait_for_pid_exit(pid: u32) {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, WAIT_FAILED};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, INFINITE};
    // PROCESS_ACCESS_RIGHTS::SYNCHRONIZE — defined as a raw constant so this
    // works across windows-sys feature layouts.
    const SYNCHRONIZE: u32 = 0x00100000;
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, FALSE, pid);
        if handle.is_null() {
            // Process may already be gone, or access denied — fall back to a
            // short sysinfo poll rather than returning immediately so callers
            // don't get a spurious "exited" event right at startup.
            wait_for_pid_exit_polling(pid);
            return;
        }
        if WaitForSingleObject(handle, INFINITE) == WAIT_FAILED {
            // If the wait failed, fall through to polling so we still
            // eventually detect exit.
            CloseHandle(handle);
            wait_for_pid_exit_polling(pid);
            return;
        }
        CloseHandle(handle);
    }
}

#[cfg(not(windows))]
fn wait_for_pid_exit(pid: u32) {
    wait_for_pid_exit_polling(pid);
}

fn wait_for_pid_exit_polling(pid: u32) {
    // Uses its own System because this is a per-PID refresh (cheap on Linux:
    // single readlink(/proc/<pid>); cheap on macOS: single sysctl), so there's
    // no benefit to batching with the shared full-process-list cache.
    let sysinfo_pid = sysinfo::Pid::from_u32(pid);
    let mut sys = System::new();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[sysinfo_pid]),
            true,
            ProcessRefreshKind::new(),
        );
        if sys.process(sysinfo_pid).is_none() { break; }
    }
}

/// Spawn a thread that blocks on the Claude process handle until it exits,
/// then emits `claude-exited-{pane_id}`. No polling on Windows.
fn spawn_exit_watcher(
    app:      AppHandle,
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    expected: Arc<Mutex<HashMap<String, String>>>,
    pane_id:  String,
    claude_pid: u32,
) {
    std::thread::spawn(move || {
        wait_for_pid_exit(claude_pid);
        clear_tracked(&sessions, &pane_id);
        monitor.lock().unwrap().remove(&pane_id);
        expected.lock().unwrap().remove(&pane_id);
        app.emit(&format!("claude-exited-{}", pane_id), ()).ok();
    });
}

/// Fallback exit watcher: when `find_claude_pid_for_shell_bounded` can't find
/// Claude's PID, poll every 2s to check whether a Claude descendant still exists
/// under the shell. If not found on two consecutive checks, Claude has exited.
fn spawn_polling_exit_watcher(
    app:      AppHandle,
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    expected: Arc<Mutex<HashMap<String, String>>>,
    pane_id:  String,
    shell_pid: u32,
) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            // Monitor entry was cleared externally (e.g. clear_claude_monitor)
            if !monitor.lock().unwrap().contains_key(&pane_id) { return; }

            let claude = shared_system().with(std::time::Duration::from_millis(250), |sys| {
                find_claude_in(sys, shell_pid)
            });
            if let Some((claude_pid, _)) = claude {
                wait_for_pid_exit(claude_pid);
                clear_tracked(&sessions, &pane_id);
                monitor.lock().unwrap().remove(&pane_id);
                expected.lock().unwrap().remove(&pane_id);
                app.emit(&format!("claude-exited-{}", pane_id), ()).ok();
                return;
            }

            // No Claude descendant — confirm with a second check after 2s
            // to avoid racing with process startup.
            std::thread::sleep(std::time::Duration::from_secs(2));
            if !monitor.lock().unwrap().contains_key(&pane_id) { return; }
            let still_gone = shared_system()
                .with(std::time::Duration::from_millis(250), |sys| {
                    find_claude_in(sys, shell_pid).is_none()
                });
            if still_gone {
                clear_tracked(&sessions, &pane_id);
                monitor.lock().unwrap().remove(&pane_id);
                expected.lock().unwrap().remove(&pane_id);
                app.emit(&format!("claude-exited-{}", pane_id), ()).ok();
                return;
            }
        }
    });
}

fn set_tracked(sessions: &Arc<Mutex<HashMap<String, PtySession>>>, pane_id: &str) {
    if let Some(s) = sessions.lock().unwrap().get(pane_id) {
        s.claude_tracked.store(true, Ordering::Relaxed);
    }
}

fn clear_tracked(sessions: &Arc<Mutex<HashMap<String, PtySession>>>, pane_id: &str) {
    if let Some(s) = sessions.lock().unwrap().get(pane_id) {
        s.claude_tracked.store(false, Ordering::Relaxed);
    }
}

/// Empty-status stub used at "Claude started" events before the JSONL has
/// been parsed. Keeps the struct construction in one place.
fn stub_status(session_id: String) -> ClaudeSessionStatus {
    ClaudeSessionStatus {
        session_id,
        model_id: None, input_tokens: None, output_tokens: None,
        cache_creation_input_tokens: None, cache_read_input_tokens: None,
        folder: None, branch: None,
    }
}

/// Return a pane directory's JSONL files sorted most-recent first.
fn list_recent_jsonl(project_dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(project_dir) else { return Vec::new() };
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { return None; }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((path, modified))
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1));
    files.into_iter().map(|(p, _)| p).collect()
}

fn claude_home() -> Option<PathBuf> {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    Some(PathBuf::from(home).join(".claude"))
}

/// Per-PTY process monitor: polls ~1s for Claude descendants under the shell
/// PID. When Claude appears, emits `claude-started`, adopts a matching JSONL
/// if one already exists (for the "Claude started but hasn't written yet"
/// window), and blocks the thread on the Claude exit handle. When Claude
/// exits, emits `claude-exited` and loops to detect relaunches.
///
/// Polling here is intentional: see the justifying comment at the call site
/// in `create_session`. Cost stays bounded via `SharedSystem` (one full
/// process refresh shared across panes, min 250 ms between refreshes).
pub fn spawn_pane_monitor(
    app:      AppHandle,
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    expected: Arc<Mutex<HashMap<String, String>>>,
    session_id: String,
) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));

        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));

            // Session closed? stop monitoring.
            let shell_pid = {
                let s = sessions.lock().unwrap();
                match s.get(&session_id) {
                    Some(sess) => sess.shell_pid,
                    None => return,
                }
            };
            let Some(shell_pid) = shell_pid else { continue };

            // Already tracked (file watcher adopted it or we did on a prior
            // iteration)? Don't compete with the existing exit watcher.
            if monitor.lock().unwrap().contains_key(&session_id) { continue; }

            // Is a Claude descendant running under this shell right now?
            let claude = shared_system().with(std::time::Duration::from_millis(250), |sys| {
                find_claude_in(sys, shell_pid)
            });
            let Some((claude_pid, _)) = claude else { continue };

            // Claude just appeared — adopt a JSONL if one already exists.
            let cwd = pane_cwd(&sessions, &session_id);
            let mut adopted = false;
            if let Some(ref cwd) = cwd {
                if let Some(project_dir) = claude_home().map(|h| h.join("projects").join(encode_project_dir(cwd))) {
                    if project_dir.is_dir() {
                        for path in list_recent_jsonl(&project_dir).into_iter().take(5) {
                            let already = monitor.lock().unwrap().values().any(|e| e.jsonl == path);
                            if already { continue; }
                            monitor.lock().unwrap()
                                .insert(session_id.clone(), ClaudeEntry { jsonl: path.clone() });
                            expected.lock().unwrap().remove(&session_id);
                            set_tracked(&sessions, &session_id);
                            let stem = path.file_stem().and_then(|s| s.to_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| session_id.clone());
                            app.emit(&format!("claude-started-{}", session_id), stub_status(stem)).ok();
                            if let Some(status) = parse_jsonl_status(&path) {
                                app.emit(&format!("claude-status-{}", session_id), &status).ok();
                            }
                            adopted = true;
                            break;
                        }
                    }
                }
            }

            if !adopted {
                // No JSONL found (yet). Emit `claude-started` so the UI
                // transitions out of 'closed'; the file watcher will emit
                // `claude-status` once the JSONL appears. Don't insert a
                // monitor entry — the file watcher owns JSONL adoption.
                app.emit(&format!("claude-started-{}", session_id), stub_status(String::new())).ok();
            }

            // Block this monitor thread on the Claude exit handle. When it
            // returns, emit `claude-exited` and loop back to detect relaunch.
            // No child thread / no channel — the monitor thread has no other
            // work while Claude is alive, so there's nothing to parallelise.
            wait_for_pid_exit(claude_pid);
            clear_tracked(&sessions, &session_id);
            monitor.lock().unwrap().remove(&session_id);
            // Clear any stale `expected` entry: if a `claude --resume` died
            // before its JSONL was written, the resume id would otherwise
            // linger and skew future adoption matching.
            expected.lock().unwrap().remove(&session_id);
            app.emit(&format!("claude-exited-{}", session_id), ()).ok();
        }
    });
}

/// Find the most recently started Claude process under `shell_pid`, looking up
/// to `attempts` times with a 150ms delay between tries. Returns the PID only.
/// Called at JSONL-adoption time to obtain a PID for the blocking exit watcher.
fn find_claude_pid_for_shell_bounded(shell_pid: u32, attempts: u32) -> Option<u32> {
    for i in 0..attempts {
        let found = shared_system().with(std::time::Duration::from_millis(100), |sys| {
            find_claude_in(sys, shell_pid).map(|(pid, _)| pid)
        });
        if found.is_some() { return found; }
        if i + 1 < attempts {
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }
    None
}

/// Look up a pane's shell PID from the sessions map.
fn pane_shell_pid(
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    pane_id: &str,
) -> Option<u32> {
    sessions.lock().unwrap().get(pane_id).and_then(|s| s.shell_pid)
}

/// Adopt a JSONL into the matching pane and emit the appropriate event.
/// Used by the file-system watcher on Create/Modify of an untracked JSONL.
fn try_adopt_jsonl(
    app:      &AppHandle,
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  &Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    expected: &Arc<Mutex<HashMap<String, String>>>,
    jsonl_path: &std::path::Path,
) -> MatchResult {
    // Already tracked? Just emit a status update.
    let already = monitor.lock().unwrap().iter()
        .find(|(_, e)| e.jsonl == jsonl_path)
        .map(|(id, _)| id.clone());
    if let Some(pane_id) = already {
        if let Some(status) = parse_jsonl_status(jsonl_path) {
            app.emit(&format!("claude-status-{}", pane_id), &status).ok();
        }
        return MatchResult::Matched(pane_id);
    }

    let pane_id = match match_jsonl_to_pane(jsonl_path, sessions, monitor, expected) {
        MatchResult::Matched(id) => id,
        other => return other,
    };

    // Adopt: write the JSONL into the pane's monitor entry, clear any expected entry.
    monitor.lock().unwrap()
        .insert(pane_id.clone(), ClaudeEntry { jsonl: jsonl_path.to_path_buf() });
    expected.lock().unwrap().remove(&pane_id);
    set_tracked(sessions, &pane_id);

    // Emit "started" so the frontend footer appears, then a status update with
    // whatever the JSONL has so far (may be partial — cwd line isn't written
    // until the first user turn).
    let stem = jsonl_path.file_stem().and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| pane_id.clone());
    app.emit(&format!("claude-started-{}", pane_id), stub_status(stem)).ok();
    if let Some(status) = parse_jsonl_status(jsonl_path) {
        app.emit(&format!("claude-status-{}", pane_id), &status).ok();
    }

    // Find the Claude PID under this pane's shell and spawn a blocking exit
    // watcher so we get a `claude-exited` event the instant the process dies.
    if let Some(shell_pid) = pane_shell_pid(sessions, &pane_id) {
        let app2      = app.clone();
        let sessions2 = sessions.clone();
        let monitor2  = monitor.clone();
        let expected2 = expected.clone();
        let pane_id2  = pane_id.clone();
        std::thread::spawn(move || {
            if let Some(claude_pid) = find_claude_pid_for_shell_bounded(shell_pid, 12) {
                spawn_exit_watcher(app2, sessions2, monitor2, expected2, pane_id2, claude_pid);
            } else {
                spawn_polling_exit_watcher(app2, sessions2, monitor2, expected2, pane_id2, shell_pid);
            }
        });
    }

    MatchResult::Matched(pane_id)
}

/// Result of JSONL-to-pane matching / adoption.
enum MatchResult {
    /// No CWD-matching candidates at all — nothing to retry.
    NoMatch,
    /// CWD candidates exist but PID check failed (process not visible yet).
    /// The caller should schedule a bounded retry.
    RetryNeeded,
    /// Single unambiguous match (or already-tracked pane).
    Matched(String),
}

/// Single source of truth for "which pane should this JSONL belong to?".
/// Called by the global file-system watcher when an untracked JSONL appears.
///
/// Match priority:
/// 1. **Expected**: a pane registered this JSONL's filename stem via
///    `set_expected_claude_session` (resume case). Cwd is sanity-checked.
/// 2. **Fresh detection**: among panes whose cwd's encoded form matches the
///    JSONL's parent directory AND which aren't already tracked, find those
///    whose shell has a Claude descendant *right now*. If exactly one matches,
///    adopt it.
fn match_jsonl_to_pane(
    jsonl_path: &std::path::Path,
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  &Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    expected: &Arc<Mutex<HashMap<String, String>>>,
) -> MatchResult {
    let Some(stem) = jsonl_path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()) else {
        return MatchResult::NoMatch;
    };

    // 1) Expected match (resumed sessions).
    {
        let exp = expected.lock().unwrap();
        for (pane_id, exp_id) in exp.iter() {
            if exp_id != &stem { continue; }
            if let Some(p_cwd) = pane_cwd(sessions, pane_id) {
                if !jsonl_parent_matches_cwd(jsonl_path, &p_cwd) { continue; }
            }
            return MatchResult::Matched(pane_id.clone());
        }
    }

    // 2) Fresh-session match by parent-dir encoding.
    let tracked: std::collections::HashSet<String> =
        monitor.lock().unwrap().keys().cloned().collect();
    let exp_panes: std::collections::HashSet<String> =
        expected.lock().unwrap().keys().cloned().collect();

    let candidates: Vec<(String, Option<u32>)> = {
        let s = sessions.lock().unwrap();
        s.iter()
            .filter(|(id, _)| !tracked.contains(*id) && !exp_panes.contains(*id))
            .filter_map(|(id, sess)| {
                let cwd = sess.cwd.lock().unwrap().clone()?;
                if jsonl_parent_matches_cwd(jsonl_path, &cwd) {
                    Some((id.clone(), sess.shell_pid))
                } else {
                    None
                }
            })
            .collect()
    };

    if candidates.is_empty() { return MatchResult::NoMatch; }

    // Verify via PID scan which candidate actually has Claude running.
    let matches: Vec<String> = shared_system().with(std::time::Duration::from_millis(100), |sys| {
        candidates.iter()
            .filter_map(|(pane_id, shell_pid)| {
                let shell_pid = (*shell_pid)?;
                find_claude_in(sys, shell_pid).map(|_| pane_id.clone())
            })
            .collect()
    });
    if matches.len() == 1 {
        MatchResult::Matched(matches.into_iter().next().unwrap())
    } else if matches.is_empty() {
        // CWD candidates exist but Claude process not visible in sysinfo yet.
        MatchResult::RetryNeeded
    } else {
        // Multiple matches — ambiguous, refuse to guess.
        MatchResult::NoMatch
    }
}

/// Schedule bounded background retries of `try_adopt_jsonl` for a JSONL that
/// had CWD candidates but no visible Claude process yet. Used by both the
/// file-watcher and (prior to its removal) on-demand frontend scans.
fn schedule_adoption_retry(
    app:      AppHandle,
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    expected: Arc<Mutex<HashMap<String, String>>>,
    path:     PathBuf,
) {
    std::thread::spawn(move || {
        for delay_ms in [500, 1500] {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            if monitor.lock().unwrap().values().any(|e| e.jsonl == path) { return; }
            if matches!(
                try_adopt_jsonl(&app, &sessions, &monitor, &expected, &path),
                MatchResult::Matched(_)
            ) { return; }
        }
    });
}

/// Start the file-system watcher on `~/.claude/projects/`.
/// Fires OS-level events (inotify / FSEvents / ReadDirectoryChangesW) — no polling.
pub fn start_claude_watcher(
    app:      AppHandle,
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    expected: Arc<Mutex<HashMap<String, String>>>,
) {
    let Some(projects_dir) = claude_home().map(|h| h.join("projects")) else {
        eprintln!("start_claude_watcher: neither HOME nor USERPROFILE is set; Claude session tracking disabled");
        app.emit("backend-degraded", "claude_watcher: HOME/USERPROFILE missing").ok();
        return;
    };

    std::thread::spawn(move || {
        if let Err(e) = std::fs::create_dir_all(&projects_dir) {
            eprintln!("start_claude_watcher: cannot create {}: {e}", projects_dir.display());
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match RecommendedWatcher::new(tx, NotifyConfig::default()) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("start_claude_watcher: notify::Watcher::new failed: {e}");
                app.emit("backend-degraded", format!("claude_watcher: watcher init failed: {e}")).ok();
                return;
            }
        };
        if let Err(e) = watcher.watch(&projects_dir, RecursiveMode::Recursive) {
            eprintln!("start_claude_watcher: cannot watch {}: {e}", projects_dir.display());
            app.emit("backend-degraded", format!("claude_watcher: watch failed: {e}")).ok();
            return;
        }
        let _watcher = watcher; // keep alive

        for result in &rx {
            let event = match result {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("start_claude_watcher: notify event error: {e}");
                    continue;
                }
            };

            // Only care about .jsonl files
            let paths: Vec<PathBuf> = event.paths.into_iter()
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
                .collect();
            if paths.is_empty() { continue; }
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_)
                | EventKind::Other | EventKind::Any => {
                    for path in paths {
                        let result = try_adopt_jsonl(&app, &sessions, &monitor, &expected, &path);
                        if matches!(result, MatchResult::RetryNeeded) {
                            schedule_adoption_retry(
                                app.clone(), sessions.clone(), monitor.clone(),
                                expected.clone(), path,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    });
}
