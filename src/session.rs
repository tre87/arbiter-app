//! Core terminal session — the seed of the Tauri-free backend the native app
//! drives directly (no IPC, no events bus). A PTY + headless VT term + a reader
//! thread that feeds the grid and parses OSC-7 (cwd) / OSC-133 (shell busy/idle)
//! / BEL into shared state. Ported from `src-tauri/src/pty.rs`, minus the
//! webview/xterm streaming, flow control and Claude monitoring (those follow as
//! features land). cwd/shell-idle are tracked here and read by the UI; later
//! they'll drive the footer + status, and `core` grows claude/git/shim.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::term::VtTerm;

/// Bumped on each OSC-133 idle→busy edge; the Claude monitor waits on it.
type CmdEpoch = Arc<(Mutex<u64>, Condvar)>;
/// Native FS watcher for the current repo (refreshes git on external edits).
type GitWatcher = Debouncer<RecommendedWatcher>;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// UI redraw hook: a PTY reader calls this after feeding new output so the UI can
/// redraw *on output* instead of polling the grid every frame. The iced shell wires
/// it to a redraw message at startup; it's a no-op until then (early output is covered
/// by the startup fast tick). Keeps this lib iced-agnostic.
static UI_WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Register the UI redraw hook (called once by the shell).
pub fn set_ui_waker(f: Box<dyn Fn() + Send + Sync>) {
    let _ = UI_WAKER.set(f);
}

/// Wake the UI to redraw, if a waker is registered.
fn wake_ui() {
    if let Some(f) = UI_WAKER.get() {
        f();
    }
}

pub type SharedTerm = Arc<Mutex<VtTerm>>;
pub type SharedMaster = Arc<Mutex<Box<dyn MasterPty + Send>>>;

/// portable-pty returns its own error type; map any Display error to io::Error.
fn io_err<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

pub struct Session {
    /// Unique, stable id — used to key this session's per-pane GPU renderer.
    id: u64,
    // All PTY input is funnelled through a dedicated writer thread via this
    // channel: the UI thread's keystrokes AND the reader thread's query replies
    // (cursor-position / device-attribute responses). Sending never blocks, so a
    // slow/blocking PTY write can't wedge the reader (which must keep draining
    // output) or freeze the UI.
    writer_tx: std::sync::mpsc::Sender<Vec<u8>>,
    master: SharedMaster,
    term: SharedTerm,
    cwd: Arc<Mutex<Option<String>>>,
    shell_idle: Arc<Mutex<Option<bool>>>,
    claude_running: Arc<AtomicBool>,
    git: Arc<Mutex<Option<crate::git::GitInfo>>>,
    claude: Arc<crate::claude_status::ClaudeHandle>,
    _watcher: Arc<Mutex<Option<GitWatcher>>>,
    _child: Box<dyn Child + Send + Sync>,
}

impl Session {
    pub fn spawn(cols: u16, rows: u16, mut cmd: CommandBuilder) -> std::io::Result<Self> {
        // Unique pane id, tagged onto the shell env so the statusLine/hook
        // subcommand (claude → our shim) keys its capture to THIS pane — robust
        // when many Claudes launch at once or several share a cwd.
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        cmd.env(crate::claude_shim::PANE_ID_ENV, id.to_string());
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(io_err)?;
        let child = pair.slave.spawn_command(cmd).map_err(io_err)?;
        let shell_pid = child.process_id();
        drop(pair.slave); // child keeps its own handle
        let raw_writer = pair.master.take_writer().map_err(io_err)?;
        let reader = pair.master.try_clone_reader().map_err(io_err)?;
        // Dedicated writer thread: serializes all PTY input (keystrokes + query
        // replies) off the reader/UI threads. A blocking write here can't deadlock
        // the reader, which must keep draining output for the slave to make progress.
        let (writer_tx, writer_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut w = raw_writer;
            while let Ok(bytes) = writer_rx.recv() {
                if w.write_all(&bytes).is_err() {
                    break;
                }
                let _ = w.flush();
            }
        });

        let term: SharedTerm = Arc::new(Mutex::new(VtTerm::new(cols as usize, rows as usize)));
        let cwd = Arc::new(Mutex::new(None));
        let shell_idle = Arc::new(Mutex::new(None));
        let claude_running = Arc::new(AtomicBool::new(false));
        let git = Arc::new(Mutex::new(None));
        let watcher: Arc<Mutex<Option<GitWatcher>>> = Arc::new(Mutex::new(None));
        let cmd_epoch: CmdEpoch = Arc::new((Mutex::new(0), Condvar::new()));

        // Shared Claude status, updated by the capture/hook watcher (registered
        // here so it routes by cwd / session id) + the reader (spinner/menu →
        // activity/attention). Created before the reader so it gets a clone.
        let claude =
            crate::claude_status::ClaudeHandle::new(id, shell_pid, cwd.clone(), claude_running.clone());
        crate::claude_status::register(&claude);

        {
            let term = term.clone();
            let cwd = cwd.clone();
            let shell_idle = shell_idle.clone();
            let claude = claude.clone();
            let git = git.clone();
            let watcher = watcher.clone();
            let cmd_epoch = cmd_epoch.clone();
            let writer_tx = writer_tx.clone();
            std::thread::spawn(move || {
                reader_loop(reader, writer_tx, term, cwd, shell_idle, claude, git, watcher, cmd_epoch)
            });
        }

        // Event-driven Claude monitor: on each busy edge, scan the shell's
        // descendants for a `claude` process (it execs shortly after the edge).
        if let Some(pid) = shell_pid {
            let claude_running = claude_running.clone();
            let shell_idle = shell_idle.clone();
            std::thread::spawn(move || claude_monitor(pid, cmd_epoch, claude_running, shell_idle));
        }

        Ok(Self {
            id,
            writer_tx,
            master: Arc::new(Mutex::new(pair.master)),
            term,
            cwd,
            shell_idle,
            claude_running,
            git,
            claude,
            _watcher: watcher,
            _child: child,
        })
    }

    /// Current Claude status for this pane (stats + derived lifecycle). Cheap;
    /// read it from the view.
    pub fn claude_status(&self) -> crate::claude_status::ClaudeStatus {
        self.claude.snapshot()
    }

    /// True if a `claude` process is running in this pane right now.
    pub fn claude_running(&self) -> bool {
        self.claude_running.load(Ordering::Relaxed)
    }

    /// Ignore Claude spinner detection for `dur_ms` — called for a repaint that doesn't
    /// start work (window/PTY resize, or a newline/mode edit key on Windows ConPTY) so the
    /// rapid redraws don't read as "working".
    pub fn suppress_claude_activity(&self, dur_ms: u64) {
        self.claude.suppress_activity(dur_ms);
    }

    /// Resume Claude spinner detection immediately — called on Enter/submit, where real
    /// working is imminent and must not be delayed by a suppression window.
    pub fn clear_claude_suppression(&self) {
        self.claude.clear_suppression();
    }

    /// The Claude session id to resume on restore IF Claude is running here AND a
    /// real conversation has happened (else `None`, so restore launches a clean
    /// `claude` rather than `--resume`ing a non-existent empty session).
    pub fn claude_session_id(&self) -> Option<String> {
        self.claude_running().then(|| self.claude.resumable_session()).flatten()
    }

    /// Basename of the current working directory, if known.
    pub fn folder(&self) -> Option<String> {
        self.cwd().map(|p| {
            p.trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&p)
                .to_string()
        })
    }

    /// Cached git info for the cwd (branch + status counts), refreshed on cd.
    pub fn git(&self) -> Option<crate::git::GitInfo> {
        self.git.lock().unwrap().clone()
    }

    /// Stable unique id for keying per-session GPU state.
    pub fn id(&self) -> u64 { self.id }

    /// Shared grid handle for the renderer.
    pub fn term(&self) -> SharedTerm { self.term.clone() }
    /// Shared master handle (for resizing the PTY from the render path).
    pub fn master(&self) -> SharedMaster { self.master.clone() }
    /// Latest cwd from OSC-7, if the shell reported one.
    pub fn cwd(&self) -> Option<String> { self.cwd.lock().unwrap().clone() }
    /// Latest OSC-133 idle state (Some(true)=at prompt, Some(false)=running).
    pub fn shell_idle(&self) -> Option<bool> { *self.shell_idle.lock().unwrap() }

    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer_tx.send(bytes.to_vec());
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Ok(m) = self.master.lock() {
            let _ = m.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
        self.term.lock().unwrap().resize(cols as usize, rows as usize);
    }
}

const MAX_UTF8_REMAINDER: usize = 8;

/// True if the chunk contains Claude's working spinner. The animated ✻ bloom
/// frames are the star/asterisk dingbats U+2722–273F (verified by capturing the
/// CLI); tool spinners use Braille U+2800–28FF. Deliberately NOT the whole
/// U+2700–27BF range — that includes the input prompt arrow ❯ (U+276F), which
/// would make typing read as working. Plain typing/output carries no such glyph.
fn chunk_has_spinner(bytes: &[u8]) -> bool {
    let text = unsafe { std::str::from_utf8_unchecked(bytes) };
    text.chars()
        .any(|c| ('\u{2722}'..='\u{273F}').contains(&c) || ('\u{2800}'..='\u{28FF}').contains(&c))
}

fn reader_loop(
    mut reader: Box<dyn Read + Send>,
    writer_tx: std::sync::mpsc::Sender<Vec<u8>>,
    term: SharedTerm,
    cwd: Arc<Mutex<Option<String>>>,
    shell_idle: Arc<Mutex<Option<bool>>>,
    claude: Arc<crate::claude_status::ClaudeHandle>,
    git: Arc<Mutex<Option<crate::git::GitInfo>>>,
    watcher: Arc<Mutex<Option<GitWatcher>>>,
    cmd_epoch: CmdEpoch,
) {
    let claude_running = claude.claude_running.clone();
    let mut buf = [0u8; 8192];
    let mut remainder: Vec<u8> = Vec::new();
    let mut osc = String::new();
    let mut in_osc = false;
    let mut prev_cwd: Option<String> = None;
    let mut prev_idle: Option<bool> = None;
    let mut prev_menu = false;

    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        // Stitch any partial UTF-8 from last read onto this chunk.
        let mut chunk = Vec::with_capacity(remainder.len() + n);
        chunk.extend_from_slice(&remainder);
        chunk.extend_from_slice(&buf[..n]);
        remainder.clear();
        let valid_up_to = match std::str::from_utf8(&chunk) {
            Ok(_) => chunk.len(),
            Err(e) => e.valid_up_to(),
        };
        if valid_up_to < chunk.len() {
            remainder = chunk[valid_up_to..].to_vec();
            if remainder.len() > MAX_UTF8_REMAINDER {
                remainder.clear();
            }
        }
        let valid = &chunk[..valid_up_to];
        if valid.is_empty() {
            continue;
        }

        // Feed the full byte stream to the grid (alacritty parses VT incl. OSC),
        // then write back any replies it produced — responses to queries the
        // running program sent (cursor position, device attributes, status). Apps
        // like vim or .NET/Spectre console UIs wait on these; dropping them made
        // their input handling misbehave.
        let responses = {
            let mut t = term.lock().unwrap();
            t.feed(valid);
            t.take_responses()
        };
        if !responses.is_empty() {
            // Hand off to the writer thread — never write from here, or a blocking
            // PTY write would stop us draining output and could deadlock the slave.
            let _ = writer_tx.send(responses);
        }
        // The grid changed — wake the UI to redraw (event-driven; the UI no longer
        // polls the grid every frame). Coalesced on the UI side, so a burst of output
        // is one redraw.
        wake_ui();

        // Tier-3b: while Claude runs here, reflect the live turn from the *rendered
        // screen* (level-triggered, so attention clears the instant a menu leaves —
        // e.g. the user escapes/answers). A menu/approval prompt → attention (no
        // hook covers AskUserQuestion/plan); Claude's "(esc to interrupt)" status
        // line → working. Plain output (typing, redraws) is neither.
        if claude_running.load(Ordering::Relaxed) {
            // Attention: a menu/approval prompt on the rendered screen (level-based,
            // so amber clears the instant the prompt leaves). Working: the ✻ spinner
            // glyph in the *new* bytes (chunk-based like the web — instant, and a
            // stale star left on screen can't pin it to "working").
            let menu = term.lock().unwrap().visible_menu();
            claude.set_menu(menu);
            if prev_menu && !menu {
                // A menu just LEFT the screen (answered or escaped). AskUserQuestion
                // fires a permission/elicitation hook, but escaping it produces no
                // spinner/Stop to clear that sticky hook attention — so clear it on
                // this on→off edge (markerless prompts never set a menu, so they're
                // unaffected and still clear via activity/Stop).
                claude.clear_hook_attention();
            }
            prev_menu = menu;
            if !menu && chunk_has_spinner(valid) {
                claude.note_activity();
            }
        }

        // Separately scan for OSC-7 (cwd) + OSC-133 (busy/idle), which the grid
        // doesn't surface.
        let text = unsafe { std::str::from_utf8_unchecked(valid) };
        for ch in text.chars() {
            if in_osc {
                if ch == '\x07' || (osc.ends_with('\x1b') && ch == '\\') {
                    let payload = if osc.ends_with('\x1b') { &osc[..osc.len() - 1] } else { &osc };
                    if let Some(rest) = payload.strip_prefix("133;") {
                        let idle = match rest.chars().next() {
                            Some('A') | Some('D') => Some(true),
                            Some('B') | Some('C') => Some(false),
                            _ => None,
                        };
                        if let Some(idle) = idle {
                            *shell_idle.lock().unwrap() = Some(idle);
                            if prev_idle != Some(idle) {
                                prev_idle = Some(idle);
                                if idle {
                                    // Prompt returned → the foreground command
                                    // (incl. Claude) ended.
                                    let was = claude_running.swap(false, Ordering::Relaxed);
                                    if was {
                                        // Claude stopped → persist so a restore doesn't relaunch it.
                                        crate::claude_status::SAVE_DIRTY.store(true, Ordering::Relaxed);
                                    }
                                    // A command just finished — it may have changed
                                    // files, so refresh git for the footer.
                                    recompute_git(cwd.clone(), git.clone());
                                } else {
                                    // A command started → wake the monitor to scan.
                                    let (lock, cvar) = &*cmd_epoch;
                                    *lock.lock().unwrap() += 1;
                                    cvar.notify_all();
                                }
                            }
                        }
                    }
                    if let Some(path) = parse_osc7_uri(payload) {
                        let changed = prev_cwd.as_ref() != Some(&path);
                        *cwd.lock().unwrap() = Some(path.clone());
                        if changed {
                            prev_cwd = Some(path.clone());
                            recompute_git(cwd.clone(), git.clone());
                            // Re-point the FS watcher at the new repo so external
                            // edits (made outside the terminal) refresh git too.
                            repoint_watcher(&watcher, &cwd, &git, path);
                        }
                    }
                    osc.clear();
                    in_osc = false;
                } else {
                    osc.push(ch);
                    if osc.len() > 1024 {
                        osc.clear();
                        in_osc = false;
                    }
                }
            } else if ch == '\x1b' {
                osc.clear();
                osc.push(ch);
            } else if osc == "\x1b" && ch == ']' {
                osc.clear();
                in_osc = true;
            } else {
                osc.clear();
            }
        }
    }
}

/// Recompute git info for the current cwd off-thread. Only applies the result
/// if the cwd hasn't changed since (so a `cd`'s stale pre-`cd` scan can't
/// clobber the new dir's info — the idle edge fires before the OSC-7 cwd update).
fn recompute_git(cwd: Arc<Mutex<Option<String>>>, git: Arc<Mutex<Option<crate::git::GitInfo>>>) {
    let path = match cwd.lock().unwrap().clone() {
        Some(p) => p,
        None => {
            *git.lock().unwrap() = None;
            return;
        }
    };
    std::thread::spawn(move || {
        let info = crate::git::repo_info(&path);
        if cwd.lock().unwrap().as_deref() == Some(path.as_str()) {
            let mut guard = git.lock().unwrap();
            if *guard != info {
                *guard = info;
                drop(guard);
                // Redraw the footer. Without this a watcher-driven refresh (a git
                // command in a SIBLING pane on the same repo) would update the cached
                // info but never repaint until the next unrelated redraw.
                wake_ui();
            }
        }
    });
}

/// Whether a debounced FS change (path relative to the repo root) should refresh
/// the git footer. Watched recursively, so we filter here:
///   - skip gitignored high-churn dirs (`target/`, `node_modules/`, …) git ignores;
///   - inside `.git/`, take only metadata the footer reflects (HEAD, index, refs,
///     packed-refs, MERGE_HEAD, …) — skip the object store + logs/reflog that churn
///     on commits/fetches/gc, and transient `*.lock` files. The `.lock` skip is
///     scoped to `.git/` so a working-tree `Cargo.lock` / `yarn.lock` still counts.
/// Our status reads use `--no-optional-locks`, so observing `.git/` can't self-loop.
fn footer_relevant_change(rel: &std::path::Path) -> bool {
    use std::path::Component;
    let names: Vec<&str> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    if names
        .iter()
        .any(|n| matches!(*n, "target" | "node_modules" | "dist" | ".next" | ".venv" | "__pycache__"))
    {
        return false;
    }
    if names.first() == Some(&".git") {
        return !matches!(names.get(1).copied(), Some("objects") | Some("logs"))
            && !names.last().map_or(false, |f| f.ends_with(".lock"));
    }
    true
}

/// Point the session's FS watcher at the repo containing `cwd_path`, replacing
/// any previous watcher. On any debounced filesystem change under the repo root
/// we recompute git — so edits made *outside* the terminal (a text editor, a
/// branch switch in another tool) refresh the footer without polling. This is
/// what VS Code does (FSEvents / ReadDirectoryChangesW / inotify via `notify`).
/// Runs off the reader thread: resolving the repo root spawns `git`, which we
/// don't want to block terminal output on.
fn repoint_watcher(
    watcher: &Arc<Mutex<Option<GitWatcher>>>,
    cwd: &Arc<Mutex<Option<String>>>,
    git: &Arc<Mutex<Option<crate::git::GitInfo>>>,
    cwd_path: String,
) {
    let watcher = watcher.clone();
    let cwd = cwd.clone();
    let git = git.clone();
    std::thread::spawn(move || {
        let new = crate::git::repo_root(&cwd_path).and_then(|root| {
            let cwd = cwd.clone();
            let git = git.clone();
            let root_path = std::path::PathBuf::from(&root);
            let mut deb = new_debouncer(Duration::from_millis(400), move |res: DebounceEventResult| {
                let Ok(events) = res else { return };
                // Refresh only on changes the footer reflects (see footer_relevant_change):
                // meaningful `.git/` metadata + working-tree files, NOT object/log churn or
                // gitignored build dirs. This lets a git command (staging/commit/branch) in
                // one pane refresh SIBLING panes on the same repo — terminal git commands
                // also still refresh their own pane via the OSC-133 prompt edge.
                let relevant = events.iter().any(|e| {
                    let rel = e.path.strip_prefix(&root_path).unwrap_or(e.path.as_path());
                    footer_relevant_change(rel)
                });
                if relevant {
                    recompute_git(cwd.clone(), git.clone());
                }
            })
            .ok()?;
            deb.watcher()
                .watch(std::path::Path::new(&root), RecursiveMode::Recursive)
                .ok()?;
            Some(deb)
        });
        // Replacing the slot drops the previous watcher, stopping the old watch.
        *watcher.lock().unwrap() = new;
    });
}

/// Per-session Claude monitor: blocks until a busy edge (a command started),
/// then scans the shell's descendants for `claude` — with a short retry since
/// `claude` execs a moment after the edge. Bails early if the shell returns to
/// idle (a quick command that wasn't Claude). The reader clears `claude_running`
/// on the idle edge. (Currently leaks one blocked thread per closed session —
/// cleanup when sessions get a shutdown signal.)
fn claude_monitor(
    shell_pid: u32,
    cmd_epoch: CmdEpoch,
    claude_running: Arc<AtomicBool>,
    shell_idle: Arc<Mutex<Option<bool>>>,
) {
    let (lock, cvar) = &*cmd_epoch;
    let mut last = *lock.lock().unwrap();
    loop {
        // Wait for the next busy edge (epoch advance), with a safety timeout.
        {
            let guard = lock.lock().unwrap();
            let prev = last;
            let (guard, _timeout) = cvar
                .wait_timeout_while(guard, Duration::from_secs(60), |e| *e == prev)
                .unwrap();
            last = *guard;
        }
        // A command started — scan for Claude, retrying through its exec delay.
        crate::claude_shim::debug_log(&format!(
            "claude_monitor: busy edge on shell_pid={shell_pid}, scanning"
        ));
        for i in 0..8 {
            let found = crate::claude::running_under(shell_pid);
            crate::claude_shim::debug_log(&format!(
                "claude_monitor: scan {i} shell_pid={shell_pid} running_under={found}"
            ));
            if found {
                claude_running.store(true, Ordering::Relaxed);
                // Persist that Claude is now running here (even before a session
                // binds), so a restore relaunches it.
                crate::claude_status::SAVE_DIRTY.store(true, Ordering::Relaxed);
                break;
            }
            if *shell_idle.lock().unwrap() == Some(true) {
                break; // finished already → wasn't Claude
            }
            if i + 1 < 8 {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
}

fn parse_osc7_uri(payload: &str) -> Option<String> {
    let uri = payload.strip_prefix("7;")?;
    let path_part = uri.strip_prefix("file://")?;
    let path = if path_part.starts_with('/') {
        path_part.to_string()
    } else {
        let idx = path_part.find('/')?;
        path_part[idx..].to_string()
    };
    let decoded = url_decode(&path);
    #[cfg(target_os = "windows")]
    {
        let trimmed = decoded.strip_prefix('/').unwrap_or(&decoded);
        if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
            return Some(trimmed.replace('/', "\\"));
        }
        Some(trimmed.to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Some(decoded)
    }
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::footer_relevant_change;
    use std::path::Path;

    fn rel(p: &str) -> bool {
        footer_relevant_change(Path::new(p))
    }

    #[test]
    fn git_metadata_that_moves_the_footer_is_relevant() {
        assert!(rel(".git/index")); // staging (git add)
        assert!(rel(".git/HEAD")); // branch switch
        assert!(rel(".git/refs/heads/main")); // commit / branch tip
        assert!(rel(".git/packed-refs"));
        assert!(rel(".git/MERGE_HEAD"));
    }

    #[test]
    fn git_churn_and_locks_are_ignored() {
        // Object store + reflog churn on commits/fetches/gc — footer unaffected.
        assert!(!rel(".git/objects/ab/cdef0123"));
        assert!(!rel(".git/logs/HEAD"));
        // Transient lock files that flap on every git op (would self-fire otherwise).
        assert!(!rel(".git/index.lock"));
        assert!(!rel(".git/refs/heads/main.lock"));
    }

    #[test]
    fn working_tree_changes_are_relevant() {
        assert!(rel("src/main.rs"));
        // A working-tree *.lock is a real tracked file — must NOT be caught by the
        // `.git/`-scoped lock filter.
        assert!(rel("Cargo.lock"));
        assert!(rel("frontend/yarn.lock"));
    }

    #[test]
    fn gitignored_build_dirs_are_ignored() {
        assert!(!rel("target/debug/build/x"));
        assert!(!rel("node_modules/vite/dist/x.js"));
        assert!(!rel("frontend/.next/cache/x"));
    }
}
