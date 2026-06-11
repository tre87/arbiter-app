//! Core terminal session — the seed of the Tauri-free backend the native app
//! drives directly (no IPC, no events bus). A PTY + headless VT term + a reader
//! thread that feeds the grid and parses OSC-7 (cwd) / OSC-133 (shell busy/idle)
//! / BEL into shared state. Ported from `src-tauri/src/pty.rs`, minus the
//! webview/xterm streaming, flow control and Claude monitoring (those follow as
//! features land). cwd/shell-idle are tracked here and read by the UI; later
//! they'll drive the footer + status, and `core` grows claude/git/shim.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
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

pub type SharedTerm = Arc<Mutex<VtTerm>>;
pub type SharedMaster = Arc<Mutex<Box<dyn MasterPty + Send>>>;

/// portable-pty returns its own error type; map any Display error to io::Error.
fn io_err<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

pub struct Session {
    /// Unique, stable id — used to key this session's per-pane GPU renderer.
    id: u64,
    writer: Box<dyn Write + Send>,
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
        let writer = pair.master.take_writer().map_err(io_err)?;
        let reader = pair.master.try_clone_reader().map_err(io_err)?;

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
            std::thread::spawn(move || {
                reader_loop(reader, term, cwd, shell_idle, claude, git, watcher, cmd_epoch)
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
            writer,
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
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
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

        // Feed the full byte stream to the grid (alacritty parses VT incl. OSC).
        term.lock().unwrap().feed(valid);

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
            *git.lock().unwrap() = info;
        }
    });
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
            let mut deb = new_debouncer(Duration::from_millis(400), move |res: DebounceEventResult| {
                if res.is_ok() {
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
