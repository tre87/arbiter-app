use portable_pty::{NativePtySystem, PtySize, PtySystem};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::claude::{spawn_pane_monitor, ClaudeMonitor, ExpectedClaudeSessions};
use crate::git::get_git_info;
use crate::shell::build_shell_command;

pub struct PtySession {
    writer: Box<dyn Write + Send>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    pub(crate) shell_pid: Option<u32>,
    output_buffer: Arc<Mutex<Vec<u8>>>,
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

    let mut cmd = build_shell_command(shell.as_deref());
    cmd.env("TERM", "xterm-256color");
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
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    let output_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let buf_writer = output_buffer.clone();
    let session_cwd: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(cwd.clone()));
    let cwd_writer = session_cwd.clone();
    let session_shell_idle: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let shell_idle_writer = session_shell_idle.clone();
    let claude_tracked: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let claude_tracked_reader = claude_tracked.clone();

    // Spawn thread to stream PTY output to the frontend and buffer it for replay
    let app_handle = app.clone();
    let event_name = format!("pty-output-{}", sid);
    let cwd_event_name = format!("cwd-changed-{}", sid);
    let activity_event_name = format!("shell-activity-{}", sid);
    let bell_event_name = format!("claude-bell-{}", sid);
    let claude_activity_event_name = format!("claude-activity-{}", sid);
    let context_event_name = format!("claude-context-{}", sid);
    let error_event_name = format!("pty-error-{}", sid);
    let reader_sid = sid.clone();
    std::thread::spawn(move || {
        let _sid = reader_sid;
        const MAX_BUF: usize = 102_400; // 100 KB rolling buffer
        const MAX_UTF8_REMAINDER: usize = 8;
        let mut buf = [0u8; 4096];
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
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // clean EOF — shell closed
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
                                            "git": { "is_repo": git.is_repo, "branch": git.branch }
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

                        // Context window % detection from Claude's status bar.
                        // Pattern: "context left until auto-compact: NN%"
                        if let Some(idx) = text.find("context left until auto-compact") {
                            let after = &text[idx..];
                            // Find the digits before '%'
                            if let Some(pct_end) = after.find('%') {
                                let before_pct = &after[..pct_end];
                                // Extract trailing digits
                                let digits: String = before_pct.chars().rev()
                                    .take_while(|c| c.is_ascii_digit())
                                    .collect::<String>().chars().rev().collect();
                                if let Ok(pct) = digits.parse::<u32>() {
                                    // pct is "% left", convert to "% used"
                                    let used = 100u32.saturating_sub(pct);
                                    app_handle.emit(&context_event_name, used).ok();
                                }
                            }
                        }
                    }

                    {
                        let mut b = buf_writer.lock().unwrap();
                        b.extend_from_slice(valid_chunk);
                        if b.len() > MAX_BUF {
                            let excess = b.len() - MAX_BUF;
                            b.drain(..excess);
                        }
                    }
                    let _ = app_handle.emit(&event_name, text.to_string());
                }
            }
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
    let mut map = sessions.0.lock().unwrap();
    let session = map.get_mut(&session_id)
        .ok_or_else(|| format!("session not found: {}", session_id))?;
    session.writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
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
    sessions.0.lock().unwrap().remove(&session_id);
    expected.0.lock().unwrap().remove(&session_id);
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
        let buf = session.output_buffer.lock().unwrap();
        String::from_utf8_lossy(&buf).to_string()
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
