use portable_pty::{NativePtySystem, PtySize, PtySystem};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::claude::{spawn_pane_monitor, ClaudeMonitor, ExpectedClaudeSessions};
use crate::git::get_git_info;
use crate::shell::build_shell_command;
use crate::termgrid::HeadlessTerm;

pub struct PtySession {
    // Per-session writer lock so `write_to_session` doesn't serialize through
    // the global `Sessions` mutex — a slow write on one pane (e.g. a paused
    // shell absorbing a paste) no longer blocks keystrokes to other panes.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    pub(crate) shell_pid: Option<u32>,
    // VecDeque so trimming from the front when the rolling buffer is full is
    // O(excess) head-advance instead of an O(remaining) memmove. With a fully
    // saturated 256KB buffer and 4KB reads, this drops per-read overhead from
    // ~256KB shifts to a handful of pointer updates.
    output_buffer: Arc<Mutex<VecDeque<u8>>>,
    pub(crate) cwd: Arc<Mutex<Option<String>>>,
    // Latest OSC 133 idle state — None before any prompt-marker seen.
    // Queried by the frontend on (re)mount so a subscription that races
    // past the first idle transition can still show the shell-switch button.
    shell_idle: Arc<Mutex<Option<bool>>>,
    // Last resize dimensions — resize_session skips the PTY resize (and thus
    // SIGWINCH) when the requested size matches, avoiding unnecessary TUI
    // redraws that cause ghost cursor artefacts in Claude's Ink renderer.
    last_size: Mutex<(u16, u16)>,
    // Mirrors ClaudeMonitor membership so the PTY reader can cheaply check
    // "is Claude tracked for this pane?" without locking the global monitor
    // mutex on every 4KB read chunk. Updated at adoption and exit sites.
    pub(crate) claude_tracked: Arc<AtomicBool>,
    // Flow control: the frontend pauses us when its xterm falls behind during a
    // firehose of output. The reader then parks (stops draining the PTY) so the
    // OS pipe back-pressures the shell — the consumer never builds an unbounded
    // backlog and the UI stays responsive. Resumed under a low watermark.
    paused: Arc<(Mutex<bool>, std::sync::Condvar)>,
    // GPU renderer (termgrid): when the frontend attaches this session, the PTY
    // reader feeds bytes into this alacritty-backed grid in addition to the
    // xterm byte stream, and a frame thread ships cell diffs. None/inactive by
    // default, so the existing xterm path is unaffected when the renderer is off.
    grid: Arc<Mutex<Option<HeadlessTerm>>>,
    grid_active: Arc<AtomicBool>,
    pub(crate) grid_dirty: Arc<AtomicBool>,
}

impl PtySession {
    /// Start GPU rendering: build the grid, replay buffered history so the
    /// current screen is reconstructed, mark it active+dirty. Returns the Arcs
    /// the frame thread registry needs.
    pub(crate) fn attach_grid(
        &self,
        cols: u16,
        rows: u16,
        fg: &[u8],
        bg: &[u8],
        ansi: &[u8],
    ) -> (Arc<Mutex<Option<HeadlessTerm>>>, Arc<AtomicBool>, Arc<AtomicBool>) {
        let mut term = HeadlessTerm::new(cols as usize, rows as usize);
        term.set_theme(fg, bg, ansi);
        if let Ok(buf) = self.output_buffer.lock() {
            let (a, b) = buf.as_slices();
            term.feed(a);
            term.feed(b);
        }
        *self.grid.lock().unwrap() = Some(term);
        self.grid_active.store(true, Ordering::Relaxed);
        self.grid_dirty.store(true, Ordering::Relaxed);
        (self.grid.clone(), self.grid_active.clone(), self.grid_dirty.clone())
    }

    pub(crate) fn detach_grid(&self) {
        self.grid_active.store(false, Ordering::Relaxed);
        *self.grid.lock().unwrap() = None;
    }

    fn resize_grid(&self, cols: u16, rows: u16) {
        if self.grid_active.load(Ordering::Relaxed) {
            if let Ok(mut g) = self.grid.lock() {
                if let Some(t) = g.as_mut() {
                    t.resize(cols as usize, rows as usize);
                }
            }
            self.grid_dirty.store(true, Ordering::Relaxed);
        }
    }
}

// Arc so the watcher background thread can share ownership
pub struct Sessions(pub Arc<Mutex<HashMap<String, PtySession>>>);

#[tauri::command]
pub fn create_session(app: AppHandle, sessions: State<Sessions>, monitor: State<ClaudeMonitor>, cols: Option<u16>, rows: Option<u16>, cwd: Option<String>, shell: Option<String>) -> Result<String, String> {
    let session_id = Uuid::new_v4().to_string();
    let sid = session_id.clone();

    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.unwrap_or(24),
            cols: cols.unwrap_or(80),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    let mut cmd = build_shell_command(&app, shell.as_deref());
    cmd.env("TERM", "xterm-256color");
    // GUI launches (Finder/Dock) start with a minimal environment that lacks
    // COLORTERM, so Claude Code and other TUIs downgrade to 256-color and render
    // accent colors (e.g. Claude's #D97757) differently than in `tauri dev`,
    // which inherits COLORTERM=truecolor from the launching shell. Set it
    // explicitly so 24-bit color is consistent regardless of launch method.
    cmd.env("COLORTERM", "truecolor");
    if let Some(ref dir) = cwd {
        let p = std::path::Path::new(dir);
        if p.is_dir() {
            cmd.cwd(p);
        } else if let Ok(canon) = std::fs::canonicalize(p) {
            // Accept forward-slash or otherwise non-native paths on Windows
            // (`git` outputs Unix-style paths that still pass `is_dir()`,
            // but this is a belt-and-braces fallback).
            cmd.cwd(canon);
        } else {
            eprintln!("create_session: cwd '{}' is not a directory; shell will inherit process cwd", dir);
        }
    }

    let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    let shell_pid = child.process_id();
    // slave must be dropped after spawning so the master gets EOF when child exits
    drop(pair.slave);
    drop(child);

    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer_inner: Box<dyn Write + Send> = pair.master.take_writer().map_err(|e| e.to_string())?;
    let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer_inner));

    let output_buffer: Arc<Mutex<VecDeque<u8>>> = Arc::new(Mutex::new(VecDeque::with_capacity(4096)));
    let buf_writer = output_buffer.clone();
    let session_cwd: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(cwd.clone()));
    let cwd_writer = session_cwd.clone();
    let session_shell_idle: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let shell_idle_writer = session_shell_idle.clone();
    let claude_tracked: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let claude_tracked_reader = claude_tracked.clone();
    let paused: Arc<(Mutex<bool>, std::sync::Condvar)> = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let paused_reader = paused.clone();
    let grid: Arc<Mutex<Option<HeadlessTerm>>> = Arc::new(Mutex::new(None));
    let grid_active: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let grid_dirty: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let grid_reader = grid.clone();
    let grid_active_reader = grid_active.clone();
    let grid_dirty_reader = grid_dirty.clone();

    // Spawn thread to stream PTY output to the frontend and buffer it for replay
    let app_handle = app.clone();
    let event_name = format!("pty-output-{}", sid);
    let cwd_event_name = format!("cwd-changed-{}", sid);
    let activity_event_name = format!("shell-activity-{}", sid);
    let bell_event_name = format!("claude-bell-{}", sid);
    let claude_activity_event_name = format!("claude-activity-{}", sid);
    let attention_event_name = format!("claude-attention-{}", sid);
    let error_event_name = format!("pty-error-{}", sid);
    let reader_sid = sid.clone();
    // Cloned for the panic-recovery emit outside the inner closure that owns
    // app_handle / error_event_name during the read loop.
    let panic_app = app.clone();
    let panic_event = error_event_name.clone();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _sid = reader_sid;
        // 256 KB ≈ ~16 screens @ 200×80 — comfortably covers a typical Claude
        // turn's worth of streamed output for replay after a split remount.
        // Memory cost scales per-pane; at 20 concurrent panes this is 5 MB total.
        const MAX_BUF: usize = 262_144;
        const MAX_UTF8_REMAINDER: usize = 8;
        // Large read buffer: one IPC emit per 4 KB read floods the webview during
        // heavy Claude output. Reading up to 64 KB per syscall (and coalescing
        // below) cuts the number of emits — and the per-chunk scan passes — for
        // bursty output, while small interactive reads still return immediately.
        let mut buf = vec![0u8; 65_536];
        // Holds trailing bytes from an incomplete UTF-8 sequence at chunk boundary
        let mut utf8_remainder: Vec<u8> = Vec::new();
        // Accumulates partial OSC sequences across chunks
        let mut osc_buf = String::new();
        let mut in_osc = false;
        let mut prev_cwd: Option<String> = None;
        // OSC 133 prompt-marker state: true = shell idle at prompt, false = busy.
        // None until the shell first reports; we only emit on transitions.
        let mut prev_idle: Option<bool> = None;
        // Debounced activity classification: "thinking" (spinner) or "generating" (text).
        // Only emitted when Claude is tracked for this session (monitor entry exists).
        let mut last_activity_emit = std::time::Instant::now() - std::time::Duration::from_secs(10);
        let mut last_activity_kind: Option<&str> = None;
        // Debounce for the "needs attention" signal (interactive prompt on screen).
        let mut last_attention_emit = std::time::Instant::now() - std::time::Duration::from_secs(10);
        // Coalesce display output into ~60fps frames. Emitting one IPC event per
        // read floods the webview's main thread (deserialize + term.write) during
        // heavy Claude output, which janks the whole UI — even toolbar
        // animations. Accumulate and flush on size / a ~16ms frame / a small read
        // (interactive typing or a burst tail) so the renderer gets fewer, larger
        // writes without adding latency to interactive use.
        let mut pending = String::new();
        let mut last_flush = std::time::Instant::now();
        loop {
            // Flow control: park while the frontend has paused us (its xterm is
            // behind). Not reading lets the PTY's OS buffer back-pressure the
            // shell, capping the consumer's backlog. close_session resumes us so
            // a parked reader wakes, hits EOF, and exits rather than leaking.
            {
                let (lock, cvar) = &*paused_reader;
                let mut p = lock.lock().unwrap();
                while *p {
                    p = cvar.wait(p).unwrap();
                }
            }
            match reader.read(&mut buf) {
                Ok(0) => {
                    if !pending.is_empty() {
                        let _ = app_handle.emit(&event_name, std::mem::take(&mut pending));
                    }
                    break; // clean EOF — shell closed
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    // Real read error — surface to the frontend so the pane
                    // doesn't silently freeze with the user assuming the shell
                    // is still alive.
                    app_handle.emit(&error_event_name, e.to_string()).ok();
                    break;
                }
                Ok(n) => {
                    // Prepend any leftover bytes from the previous read
                    let chunk: Vec<u8> = if utf8_remainder.is_empty() {
                        buf[..n].to_vec()
                    } else {
                        let mut combined = std::mem::take(&mut utf8_remainder);
                        combined.extend_from_slice(&buf[..n]);
                        combined
                    };
                    // Safety: a valid UTF-8 codepoint is at most 4 bytes, so a
                    // remainder longer than 8 bytes means the stream is garbage
                    // and retaining it would accumulate indefinitely.
                    if utf8_remainder.len() > MAX_UTF8_REMAINDER {
                        utf8_remainder.clear();
                    }

                    // Find the last valid UTF-8 boundary so we don't corrupt
                    // multi-byte characters (like ━) split across reads.
                    let valid_up_to = match std::str::from_utf8(&chunk) {
                        Ok(_) => chunk.len(),
                        Err(e) => e.valid_up_to(),
                    };
                    // Keep trailing incomplete bytes for next read
                    if valid_up_to < chunk.len() {
                        utf8_remainder = chunk[valid_up_to..].to_vec();
                    }
                    let valid_chunk = &chunk[..valid_up_to];
                    if valid_chunk.is_empty() {
                        continue;
                    }

                    // Safe: we just validated this is valid UTF-8
                    let text = unsafe { std::str::from_utf8_unchecked(valid_chunk) };

                    // Scan for OSC 7 sequences: \x1b]7;file:///path\x07 or \x1b]7;file:///path\x1b\\
                    for ch in text.chars() {
                        if in_osc {
                            if ch == '\x07' || (osc_buf.ends_with('\x1b') && ch == '\\') {
                                // End of OSC — parse the URI
                                let payload = if osc_buf.ends_with('\x1b') {
                                    &osc_buf[..osc_buf.len() - 1]
                                } else {
                                    &osc_buf
                                };
                                // OSC 133: FinalTerm prompt markers.
                                //   133;A → prompt start   (idle)
                                //   133;B → command start  (busy)
                                //   133;C → pre-execution  (busy)
                                //   133;D → command finish (idle)
                                if let Some(rest) = payload.strip_prefix("133;") {
                                    let marker = rest.chars().next();
                                    let idle = match marker {
                                        Some('A') | Some('D') => Some(true),
                                        Some('B') | Some('C') => Some(false),
                                        _ => None,
                                    };
                                    if let Some(idle) = idle {
                                        *shell_idle_writer.lock().unwrap() = Some(idle);
                                        if prev_idle != Some(idle) {
                                            prev_idle = Some(idle);
                                            app_handle.emit(&activity_event_name, idle).ok();
                                        }
                                    }
                                }
                                // OSC 7: CWD change
                                if let Some(path) = parse_osc7_uri(payload) {
                                    let changed = prev_cwd.as_ref() != Some(&path);
                                    *cwd_writer.lock().unwrap() = Some(path.clone());
                                    if changed {
                                        prev_cwd = Some(path.clone());
                                        let git = get_git_info(&path);
                                        let folder = std::path::Path::new(&path)
                                            .file_name()
                                            .map(|n| n.to_string_lossy().to_string());
                                        app_handle.emit(&cwd_event_name, serde_json::json!({
                                            "cwd": path,
                                            "folder": folder,
                                            "git": git
                                        })).ok();
                                    }
                                }
                                osc_buf.clear();
                                in_osc = false;
                            } else {
                                osc_buf.push(ch);
                                // Safety: bail if OSC is absurdly long (not a real OSC 7)
                                if osc_buf.len() > 1024 {
                                    osc_buf.clear();
                                    in_osc = false;
                                }
                            }
                        } else if ch == '\x1b' {
                            // Might be start of OSC — peek handled on next char
                            osc_buf.clear();
                            osc_buf.push(ch);
                        } else if osc_buf == "\x1b" && ch == ']' {
                            osc_buf.clear();
                            in_osc = true;
                        } else if ch == '\x07' {
                            // BEL outside of an OSC sequence — Claude Code uses
                            // this to signal "waiting for user input" (permission
                            // prompts, option menus).
                            app_handle.emit(&bell_event_name, ()).ok();
                            osc_buf.clear();
                        } else {
                            osc_buf.clear();
                        }
                    }

                    // Unified activity classification: thinking vs generating.
                    // Spinners: Braille patterns U+2800-U+28FF (⠋⠙⠹⠸ etc.) and
                    // dingbats U+2700-U+27BF — Claude Code uses these for its
                    // animated thinking indicator.
                    // Substantial printable text without spinners → "generating"
                    // Pure ANSI escapes / short chunks → ignored (resize redraws)
                    if claude_tracked_reader.load(Ordering::Relaxed) {
                        // "Needs attention" for the prompts Claude does NOT
                        // expose via a hook: AskUserQuestion menus and plan-mode
                        // (ExitPlanMode) approval. Permission prompts come from
                        // the PermissionRequest/Notification hooks instead, so
                        // they're intentionally not matched here. Detected from
                        // the rendered menu text and handled INSTEAD of activity
                        // classification — a menu redraw is substantial text that
                        // would otherwise read as "generating" and flicker
                        // attention↔working. Cleared when Claude resumes.
                        let needs_attention = text.contains("to navigate")
                            || text.contains("Esc to cancel")
                            || text.contains("Would you like to proceed");

                        if needs_attention {
                            let now = std::time::Instant::now();
                            if now.duration_since(last_attention_emit).as_millis() >= 400 {
                                app_handle.emit(&attention_event_name, ()).ok();
                                last_attention_emit = now;
                            }
                        } else {
                        // Activity classification: thinking (spinner) vs generating.
                        let is_spinner_char = |c: char| {
                            ('\u{2800}'..='\u{28FF}').contains(&c) ||
                            ('\u{2700}'..='\u{27BF}').contains(&c)
                        };
                        let has_spinner = text.chars().any(|c| is_spinner_char(c));
                        // Count printable non-escape, non-control, non-spinner characters
                        let printable_count = text.chars()
                            .filter(|c| !c.is_control() && *c != '\x1b' && !is_spinner_char(*c))
                            .count();

                        let kind: Option<&str> = if has_spinner {
                            Some("thinking")
                        } else if printable_count > 10 {
                            // Substantial text output without spinners = generating
                            Some("generating")
                        } else {
                            None // Pure escape sequences or tiny output — ignore (resize, cursor moves)
                        };

                        if let Some(k) = kind {
                            let now = std::time::Instant::now();
                            // Emit if: different kind, or same kind but 200ms since last
                            let should_emit = last_activity_kind != Some(k)
                                || now.duration_since(last_activity_emit).as_millis() >= 200;
                            if should_emit {
                                app_handle.emit(&claude_activity_event_name, k).ok();
                                last_activity_emit = now;
                                last_activity_kind = Some(k);
                            }
                        }
                        } // end else (activity classification)
                    }

                    {
                        let mut b = buf_writer.lock().unwrap();
                        b.extend(valid_chunk.iter().copied());
                        let excess = b.len().saturating_sub(MAX_BUF);
                        if excess > 0 { b.drain(..excess); }
                    }
                    // Feed the GPU-renderer grid (termgrid) when this session is
                    // attached. Cheap atomic check skips it entirely otherwise.
                    if grid_active_reader.load(Ordering::Relaxed) {
                        if let Ok(mut g) = grid_reader.lock() {
                            if let Some(t) = g.as_mut() {
                                t.feed(valid_chunk);
                            }
                        }
                        grid_dirty_reader.store(true, Ordering::Relaxed);
                    }
                    // Frame-coalesced emit (see `pending` above). Flush on a 32 KB
                    // batch, a 16 ms frame, or a small read (n < 4 KB ⇒ interactive
                    // input or the tail of a burst) so nothing is held while idle.
                    // Skipped when the GPU renderer is attached: the frontend has
                    // no pty-output listener then (it renders from grid diffs), so
                    // serialising the string to nobody is pure waste. The OSC/cwd/
                    // activity scanning above and the replay buffer still run.
                    if !grid_active_reader.load(Ordering::Relaxed) {
                        pending.push_str(text);
                        if pending.len() >= 32_768
                            || n < 4096
                            || last_flush.elapsed() >= std::time::Duration::from_millis(16)
                        {
                            let _ = app_handle.emit(&event_name, std::mem::take(&mut pending));
                            last_flush = std::time::Instant::now();
                        }
                    }
                }
            }
        }
        }));
        if let Err(panic) = result {
            let msg = panic.downcast_ref::<String>().cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "PTY reader thread panicked".to_string());
            eprintln!("pty reader panicked: {msg}");
            let _ = panic_app.emit(&panic_event, format!("reader panic: {msg}"));
        }
    });

    let initial_cols = cols.unwrap_or(80);
    let initial_rows = rows.unwrap_or(24);
    sessions.0.lock().unwrap().insert(
        session_id.clone(),
        PtySession {
            writer,
            _master: pair.master,
            shell_pid,
            output_buffer,
            cwd: session_cwd,
            shell_idle: session_shell_idle,
            last_size: Mutex::new((initial_cols, initial_rows)),
            claude_tracked,
            paused,
            grid,
            grid_active,
            grid_dirty,
        },
    );

    // Per-PTY process monitor: polls every 1s for Claude descendants.
    //
    // Polling is intentional here (CLAUDE.md forbids polling without a comment):
    // the `notify`-based watcher in start_claude_watcher sees JSONL files but
    // cannot observe process lifecycle, and no cross-platform event-driven API
    // exists for "a descendant of PID N was spawned." The alternatives
    // (`ptrace`/`PROC_EVENT` on Linux only, ETW on Windows only) aren't portable.
    // The scan uses a shared sysinfo System cached across all panes and
    // refreshed at most once per 250ms globally, so 8 panes = 1 scan/250ms
    // total, not 8.
    {
        let app3 = app.clone();
        let sessions3 = sessions.0.clone();
        let monitor3 = monitor.0.clone();
        let expected3: Arc<Mutex<HashMap<String, String>>> = app.state::<ExpectedClaudeSessions>().0.clone();
        let sid3 = session_id.clone();
        spawn_pane_monitor(app3, sessions3, monitor3, expected3, sid3);
    }

    Ok(session_id)
}

#[tauri::command]
pub fn write_to_session(
    session_id: String,
    data: String,
    sessions: State<Sessions>,
) -> Result<(), String> {
    // Clone the Arc out under the global lock, then drop the global lock
    // before doing the actual PTY write. Otherwise a blocking write on one
    // pane (e.g. paste into a paused shell) would stall keystrokes to every
    // other pane until the OS flushed.
    let writer = {
        let map = sessions.0.lock().unwrap();
        let session = map.get(&session_id)
            .ok_or_else(|| format!("session not found: {}", session_id))?;
        session.writer.clone()
    };
    let mut w = writer.lock().unwrap();
    w.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn resize_session(
    session_id: String,
    cols: u16,
    rows: u16,
    sessions: State<Sessions>,
) -> Result<bool, String> {
    let map = sessions.0.lock().unwrap();
    if let Some(session) = map.get(&session_id) {
        // Skip if dimensions unchanged — avoids SIGWINCH which triggers TUI
        // redraws in Claude's Ink renderer, causing ghost cursor artefacts.
        let mut last = session.last_size.lock().unwrap();
        if last.0 == cols && last.1 == rows {
            return Ok(false);
        }
        *last = (cols, rows);
        session
            ._master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        session.resize_grid(cols, rows);
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
pub fn close_session(
    session_id: String,
    sessions: State<Sessions>,
    expected: State<ExpectedClaudeSessions>,
) {
    // Wake a parked (flow-control-paused) reader first, so dropping the master
    // below gives it EOF and it exits instead of leaking a blocked thread.
    {
        let map = sessions.0.lock().unwrap();
        if let Some(s) = map.get(&session_id) {
            *s.paused.0.lock().unwrap() = false;
            s.paused.1.notify_all();
        }
    }
    sessions.0.lock().unwrap().remove(&session_id);
    expected.0.lock().unwrap().remove(&session_id);
}

/// Flow control: the frontend pauses a session's PTY reader when its xterm
/// write-buffer crosses the high watermark, and resumes it under the low one.
/// Pausing stops draining the PTY so the OS pipe back-pressures the shell.
#[tauri::command]
pub fn pause_session(session_id: String, sessions: State<Sessions>) {
    if let Some(s) = sessions.0.lock().unwrap().get(&session_id) {
        *s.paused.0.lock().unwrap() = true;
    }
}

#[tauri::command]
pub fn resume_session(session_id: String, sessions: State<Sessions>) {
    if let Some(s) = sessions.0.lock().unwrap().get(&session_id) {
        *s.paused.0.lock().unwrap() = false;
        s.paused.1.notify_all();
    }
}

/// Return the current terminal dimensions (cols, rows) for a session.
/// Used by bootstrapBackgroundSessions to create background PTY sessions at
/// the correct size instead of the default 80×24.
#[tauri::command]
pub fn get_session_size(session_id: String, sessions: State<Sessions>) -> Option<(u16, u16)> {
    let map = sessions.0.lock().unwrap();
    map.get(&session_id).map(|s| *s.last_size.lock().unwrap())
}

/// Return the buffered PTY output for a session as a UTF-8 string (lossy).
/// Called on TerminalPane remount after a split to replay terminal history.
#[tauri::command]
pub fn get_session_replay(session_id: String, sessions: State<Sessions>) -> String {
    let map = sessions.0.lock().unwrap();
    if let Some(session) = map.get(&session_id) {
        let mut buf = session.output_buffer.lock().unwrap();
        String::from_utf8_lossy(buf.make_contiguous()).to_string()
    } else {
        String::new()
    }
}

/// Returns the last known working directory for a session, tracked via OSC 7.
#[tauri::command]
pub fn get_session_cwd(session_id: String, sessions: State<Sessions>) -> Option<String> {
    let map = sessions.0.lock().unwrap();
    let session = map.get(&session_id)?;
    let cwd = session.cwd.lock().unwrap().clone();
    cwd
}

/// Returns the last known OSC 133 idle state, or None before any prompt
/// marker has been seen. Lets a newly mounted TerminalPane recover idle
/// state without waiting for the next idle↔busy transition.
#[tauri::command]
pub fn get_session_shell_idle(session_id: String, sessions: State<Sessions>) -> Option<bool> {
    let map = sessions.0.lock().unwrap();
    let session = map.get(&session_id)?;
    let idle = *session.shell_idle.lock().unwrap();
    idle
}

/// Parse an OSC 7 payload like "7;file:///C:/Users/foo/bar" → "C:/Users/foo/bar"
/// Also handles "7;file:///home/user/dir" on Unix.
fn parse_osc7_uri(payload: &str) -> Option<String> {
    // OSC 7 format: "7;file://hostname/path" or "7;file:///path"
    let uri = payload.strip_prefix("7;")?;
    let path_part = uri.strip_prefix("file://")?;
    // Skip hostname (everything up to the next /)
    let path = if path_part.starts_with('/') {
        // "file:///path" — no hostname
        path_part.to_string()
    } else {
        // "file://hostname/path"
        let idx = path_part.find('/')?;
        path_part[idx..].to_string()
    };
    // URL-decode percent-encoded characters
    let decoded = url_decode(&path);
    // On Windows, file URIs produce "/C:/path" — strip the leading slash
    #[cfg(target_os = "windows")]
    {
        let trimmed = decoded.strip_prefix('/').unwrap_or(&decoded);
        if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
            return Some(trimmed.replace('/', "\\"));
        }
        return Some(trimmed.to_string());
    }
    #[cfg(not(target_os = "windows"))]
    {
        Some(decoded)
    }
}

fn url_decode(s: &str) -> String {
    // Decode into a byte buffer so multi-byte UTF-8 (e.g. %C3%A9 → é) is
    // reassembled correctly. Casting bytes straight to `char` would split a
    // code point across two scalars.
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = bytes.next().and_then(|c| (c as char).to_digit(16));
            let lo = bytes.next().and_then(|c| (c as char).to_digit(16));
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
            }
        } else {
            out.push(b);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
