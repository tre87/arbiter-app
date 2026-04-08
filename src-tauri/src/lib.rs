use notify::{Config as NotifyConfig, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde::Serialize;
use sysinfo::{ProcessRefreshKind, System, UpdateKind};
use tauri::WebviewWindowBuilder;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

struct PtySession {
    writer: Box<dyn Write + Send>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    shell_pid: Option<u32>,
    output_buffer: Arc<Mutex<Vec<u8>>>,
    cwd: Arc<Mutex<Option<String>>>,
    title: Arc<Mutex<Option<String>>>,
    // Latest OSC 133 idle state — None before any prompt-marker seen.
    // Queried by the frontend on (re)mount so a subscription that races
    // past the first idle transition can still show the shell-switch button.
    shell_idle: Arc<Mutex<Option<bool>>>,
}

// Arc so the watcher background thread can share ownership
struct Sessions(Arc<Mutex<HashMap<String, PtySession>>>);

/// State for an active Claude process detected under a pane's shell.
#[derive(Clone)]
struct ClaudeEntry {
    jsonl: PathBuf,
}

// Tracks active Claude processes: pane_id → ClaudeEntry
struct ClaudeMonitor(Arc<Mutex<HashMap<String, ClaudeEntry>>>);

// Frontend-registered expected Claude session id per pane (for `claude --resume`).
// Lets the JSONL adoption logic pick the *correct* pane when several panes are
// waiting for adoption simultaneously, instead of racing on whichever empty
// pane the HashMap iterator yields first.
struct ExpectedClaudeSessions(Arc<Mutex<HashMap<String, String>>>);

#[tauri::command]
fn create_session(app: AppHandle, sessions: State<Sessions>, cols: Option<u16>, rows: Option<u16>, cwd: Option<String>, shell: Option<String>) -> Result<String, String> {
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
    let session_title: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let title_writer = session_title.clone();
    let session_shell_idle: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let shell_idle_writer = session_shell_idle.clone();

    // Spawn thread to stream PTY output to the frontend and buffer it for replay
    let app_handle = app.clone();
    let event_name = format!("pty-output-{}", sid);
    let cwd_event_name = format!("cwd-changed-{}", sid);
    let activity_event_name = format!("shell-activity-{}", sid);
    let title_event_name = format!("title-changed-{}", sid);
    std::thread::spawn(move || {
        const MAX_BUF: usize = 102_400; // 100 KB rolling buffer
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
        let mut prev_title: Option<String> = None;
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Prepend any leftover bytes from the previous read
                    let chunk: Vec<u8> = if utf8_remainder.is_empty() {
                        buf[..n].to_vec()
                    } else {
                        let mut combined = std::mem::take(&mut utf8_remainder);
                        combined.extend_from_slice(&buf[..n]);
                        combined
                    };

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
                                // OSC 0 / OSC 2: set title (payload = "0;title" or "2;title")
                                if payload.starts_with("0;") || payload.starts_with("2;") {
                                    let title = payload[2..].to_string();
                                    *title_writer.lock().unwrap() = Some(title.clone());
                                    // Emit title changes so listeners (e.g. the
                                    // worktree sidebar) can track Claude's
                                    // working state without needing a mounted
                                    // xterm to run the OSC parser.
                                    if prev_title.as_ref() != Some(&title) {
                                        prev_title = Some(title.clone());
                                        app_handle.emit(&title_event_name, &title).ok();
                                    }
                                }
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
                        } else {
                            osc_buf.clear();
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

    sessions.0.lock().unwrap().insert(
        session_id.clone(),
        PtySession {
            writer,
            _master: pair.master,
            shell_pid,
            output_buffer,
            cwd: session_cwd,
            title: session_title,
            shell_idle: session_shell_idle,
        },
    );

    // Claude detection, adoption, and exit tracking are driven entirely by the
    // global filesystem watcher on ~/.claude/projects/ (see start_claude_watcher).
    // No per-pane polling thread.

    Ok(session_id)
}

#[tauri::command]
fn write_to_session(
    session_id: String,
    data: String,
    sessions: State<Sessions>,
) -> Result<(), String> {
    let mut map = sessions.0.lock().unwrap();
    if let Some(session) = map.get_mut(&session_id) {
        session
            .writer
            .write_all(data.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn resize_session(
    session_id: String,
    cols: u16,
    rows: u16,
    sessions: State<Sessions>,
) -> Result<(), String> {
    let map = sessions.0.lock().unwrap();
    if let Some(session) = map.get(&session_id) {
        session
            ._master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn close_session(
    session_id: String,
    sessions: State<Sessions>,
    expected: State<ExpectedClaudeSessions>,
) {
    sessions.0.lock().unwrap().remove(&session_id);
    expected.0.lock().unwrap().remove(&session_id);
}

/// Frontend tells us the Claude session id it's about to resume in a pane,
/// so the JSONL watcher can adopt the matching file into the *correct* pane
/// (rather than the first empty one it finds).
#[tauri::command]
fn set_expected_claude_session(
    session_id: String,
    claude_session_id: String,
    expected: State<ExpectedClaudeSessions>,
) {
    expected.0.lock().unwrap().insert(session_id, claude_session_id);
}

/// Return the buffered PTY output for a session as a UTF-8 string (lossy).
/// Called on TerminalPane remount after a split to replay terminal history.
#[tauri::command]
fn get_session_replay(session_id: String, sessions: State<Sessions>) -> String {
    let map = sessions.0.lock().unwrap();
    if let Some(session) = map.get(&session_id) {
        let buf = session.output_buffer.lock().unwrap();
        String::from_utf8_lossy(&buf).to_string()
    } else {
        String::new()
    }
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

/// Find the most recently started Claude process that is a descendant of the given shell.
/// Returns (claude_pid, start_time) or None.
fn find_claude_for_shell(shell_pid: u32) -> Option<(u32, u64)> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::new().with_cmd(UpdateKind::Always),
    );

    let mut best: Option<(u32, u64)> = None;

    for (pid, proc) in sys.processes() {
        let name = proc.name().to_string_lossy().to_lowercase();
        let cmd  = proc.cmd().iter()
            .map(|s| s.to_string_lossy().to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");

        let is_claude = name.starts_with("claude")
            || (name.contains("node") && cmd.contains("claude"));
        if !is_claude { continue; }

        if is_descendant(&sys, pid.as_u32(), shell_pid) {
            let start_time = proc.start_time();
            if best.as_ref().map_or(true, |(_, t)| start_time > *t) {
                best = Some((pid.as_u32(), start_time));
            }
        }
    }

    best
}

/// If Claude is currently running in `session_id`'s pane, return its latest status.
/// Called once on TerminalPane remount (after a split) to restore footer state.
/// Returns a minimal status even if the JSONL hasn't been written yet — the presence
/// of the pane in the monitor means Claude is running and the footer should show.
#[tauri::command]
fn get_active_claude_status(
    session_id: String,
    monitor: State<ClaudeMonitor>,
) -> Option<ClaudeSessionStatus> {
    let mon = monitor.0.lock().unwrap();
    let entry = mon.get(&session_id)?;
    let jsonl_path = &entry.jsonl;
    // Try to parse full status from JSONL; fall back to a minimal status
    let jsonl_status = if !jsonl_path.as_os_str().is_empty() {
        parse_jsonl_status(jsonl_path)
    } else {
        None
    };
    Some(jsonl_status.unwrap_or_else(|| ClaudeSessionStatus {
        session_id: session_id.clone(),
        model_id: None, input_tokens: None, output_tokens: None,
        cache_creation_input_tokens: None, cache_read_input_tokens: None,
        folder: None, branch: None,
    }))
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
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else { continue };

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
    monitor:  Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    pane_id:  String,
    claude_pid: u32,
) {
    std::thread::spawn(move || {
        wait_for_pid_exit(claude_pid);
        monitor.lock().unwrap().remove(&pane_id);
        app.emit(&format!("claude-exited-{}", pane_id), ()).ok();
    });
}

/// Find the most recently started Claude process under `shell_pid`, looking up
/// to `attempts` times with a 150ms delay between tries. Returns the PID only;
/// the start-time disambiguation used by the old polling monitor is gone.
///
/// Called exactly once at JSONL-adoption time to obtain a PID for the blocking
/// exit watcher. Bounded retry (not continuous polling) handles the narrow race
/// where the file watcher fires before sysinfo has observed the new process.
fn find_claude_pid_for_shell_bounded(shell_pid: u32, attempts: u32) -> Option<u32> {
    for i in 0..attempts {
        if let Some((pid, _)) = find_claude_for_shell(shell_pid) {
            return Some(pid);
        }
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
/// Returns true if the JSONL was adopted into some pane (or already tracked).
fn try_adopt_jsonl(
    app:      &AppHandle,
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  &Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    expected: &Arc<Mutex<HashMap<String, String>>>,
    jsonl_path: &std::path::Path,
) -> bool {
    // Already tracked? Just emit a status update.
    let already = monitor.lock().unwrap().iter()
        .find(|(_, e)| e.jsonl == jsonl_path)
        .map(|(id, _)| id.clone());
    if let Some(pane_id) = already {
        if let Some(status) = parse_jsonl_status(jsonl_path) {
            app.emit(&format!("claude-status-{}", pane_id), &status).ok();
        }
        return true;
    }

    let Some(pane_id) = match_jsonl_to_pane(jsonl_path, sessions, monitor, expected) else {
        return false;
    };

    // Adopt: write the JSONL into the pane's monitor entry, clear any expected entry.
    {
        let mut mon = monitor.lock().unwrap();
        mon.insert(pane_id.clone(), ClaudeEntry { jsonl: jsonl_path.to_path_buf() });
    }
    expected.lock().unwrap().remove(&pane_id);

    // Emit "started" so the frontend footer appears, then a status update with
    // whatever the JSONL has so far (may be partial — cwd line isn't written
    // until the first user turn).
    let started_status = ClaudeSessionStatus {
        session_id: jsonl_path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| pane_id.clone()),
        model_id: None, input_tokens: None, output_tokens: None,
        cache_creation_input_tokens: None, cache_read_input_tokens: None,
        folder: None, branch: None,
    };
    app.emit(&format!("claude-started-{}", pane_id), &started_status).ok();
    if let Some(status) = parse_jsonl_status(jsonl_path) {
        app.emit(&format!("claude-status-{}", pane_id), &status).ok();
    }

    // Find the Claude PID under this pane's shell and spawn a blocking exit
    // watcher so we get a `claude-exited` event the instant the process dies.
    // Runs on a background thread so the file-watcher loop isn't blocked.
    if let Some(shell_pid) = pane_shell_pid(sessions, &pane_id) {
        let app2      = app.clone();
        let monitor2  = monitor.clone();
        let pane_id2  = pane_id.clone();
        std::thread::spawn(move || {
            if let Some(claude_pid) = find_claude_pid_for_shell_bounded(shell_pid, 6) {
                spawn_exit_watcher(app2, monitor2, pane_id2, claude_pid);
            }
            // If no PID is ever found (e.g. Claude exited before we could scan),
            // the JSONL entry stays registered; the frontend gets no exit event,
            // but manual interaction (a new JSONL or a fresh start) will recover.
        });
    }

    true
}

/// Single source of truth for "which pane should this JSONL belong to?".
/// Called by the global file-system watcher when an untracked JSONL appears.
/// Returns Some(pane_id) only when there is a single, unambiguous match.
///
/// Match priority:
/// 1. **Expected**: a pane registered this JSONL's filename stem via
///    `set_expected_claude_session` (resume case). Cwd is sanity-checked.
/// 2. **Fresh detection**: among panes whose cwd's encoded form matches the
///    JSONL's parent directory AND which aren't already tracked, find those
///    whose shell has a Claude descendant *right now*. If exactly one matches,
///    adopt it. If multiple (or none) match, refuse to guess.
fn match_jsonl_to_pane(
    jsonl_path: &std::path::Path,
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  &Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    expected: &Arc<Mutex<HashMap<String, String>>>,
) -> Option<String> {
    let stem = jsonl_path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string())?;

    // 1) Expected match (resumed sessions).
    {
        let exp = expected.lock().unwrap();
        for (pane_id, exp_id) in exp.iter() {
            if exp_id != &stem { continue; }
            if let Some(p_cwd) = pane_cwd(sessions, pane_id) {
                if !jsonl_parent_matches_cwd(jsonl_path, &p_cwd) { continue; }
            }
            return Some(pane_id.clone());
        }
    }

    // 2) Fresh-session match by parent-dir encoding.
    //    The parent directory of every Claude JSONL is the encoded form of
    //    its cwd, so parent-dir alone filters down to panes in that exact
    //    directory — no need to read the JSONL contents.
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

    if candidates.is_empty() { return None; }
    if candidates.len() == 1 {
        return Some(candidates.into_iter().next().unwrap().0);
    }

    // Multiple panes share this cwd — disambiguate by which one actually has
    // Claude running right now. One sysinfo scan, shared across all candidates.
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::new().with_cmd(UpdateKind::Always),
    );
    let mut matches: Vec<String> = Vec::new();
    for (pane_id, shell_pid) in &candidates {
        let Some(shell_pid) = shell_pid else { continue };
        if has_claude_descendant(&sys, *shell_pid) {
            matches.push(pane_id.clone());
        }
    }
    if matches.len() == 1 {
        Some(matches.into_iter().next().unwrap())
    } else {
        // Zero matches (Claude hasn't appeared in sysinfo yet) or multiple
        // (ambiguous) — refuse to adopt; a subsequent Modify event will retry.
        None
    }
}

/// True if `shell_pid` has any Claude descendant in the given sysinfo snapshot.
fn has_claude_descendant(sys: &System, shell_pid: u32) -> bool {
    for (pid, proc) in sys.processes() {
        let name = proc.name().to_string_lossy().to_lowercase();
        let cmd  = proc.cmd().iter()
            .map(|s| s.to_string_lossy().to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        let is_claude = name.starts_with("claude")
            || (name.contains("node") && cmd.contains("claude"));
        if !is_claude { continue; }
        if is_descendant(sys, pid.as_u32(), shell_pid) {
            return true;
        }
    }
    false
}

/// Start the file-system watcher on `~/.claude/projects/`.
/// Fires OS-level events (inotify / FSEvents / ReadDirectoryChangesW) — no polling.
fn start_claude_watcher(
    app:      AppHandle,
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  Arc<Mutex<HashMap<String, ClaudeEntry>>>,
    expected: Arc<Mutex<HashMap<String, String>>>,
) {
    let home = match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        Ok(h) => h,
        Err(_) => return,
    };
    let projects_dir = PathBuf::from(home).join(".claude").join("projects");

    std::thread::spawn(move || {
        let _ = std::fs::create_dir_all(&projects_dir);

        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match RecommendedWatcher::new(tx, NotifyConfig::default()) {
            Ok(w) => w,
            Err(_) => return,
        };
        if watcher.watch(&projects_dir, RecursiveMode::Recursive).is_err() { return; }
        let _watcher = watcher; // keep alive

        for result in &rx {
            let event = match result {
                Ok(e) => e,
                Err(_) => continue,
            };

            // Only care about .jsonl files
            let paths: Vec<PathBuf> = event.paths.into_iter()
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
                .collect();
            if paths.is_empty() { continue; }
            // Both Create and Modify drive the same path: try_adopt_jsonl is
            // idempotent for already-tracked files (it just re-emits the
            // status) and will repeatedly re-attempt matching on untracked
            // ones as Claude writes the file's initial contents. No retry
            // backoff needed — the next Modify event is the retry.
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_)
                | EventKind::Other | EventKind::Any => {
                    for path in paths {
                        try_adopt_jsonl(&app, &sessions, &monitor, &expected, &path);
                    }
                }
                _ => {}
            }
        }
    });
}

// ── Usage cache ────────────────────────────────────────────────────────────

const AUTH_WINDOW_LABEL: &str = "auth";
const OVERVIEW_WINDOW_LABEL: &str = "overview";

/// Check if a point is visible on any monitor
fn is_position_on_screen(app: &AppHandle, x: i32, y: i32, w: u32, h: u32) -> bool {
    if let Ok(monitors) = app.available_monitors() {
        for mon in monitors {
            let mp = mon.position();
            let ms = mon.size();
            // Window is on-screen if at least 50px of it overlaps a monitor
            let overlap_x = (x + w as i32).min(mp.x + ms.width as i32) - x.max(mp.x);
            let overlap_y = (y + h as i32).min(mp.y + ms.height as i32) - y.max(mp.y);
            if overlap_x >= 50 && overlap_y >= 30 {
                return true;
            }
        }
    }
    false
}

const OVERVIEW_DEFAULT_WIDTH: f64 = 240.0;
const OVERVIEW_DEFAULT_HEIGHT: f64 = 320.0;

fn center_overview_on_main(app: &AppHandle, w: &tauri::WebviewWindow) {
    if let Some(main) = app.get_webview_window("main") {
        if let (Ok(mp), Ok(ms)) = (main.outer_position(), main.outer_size()) {
            let x = mp.x + (ms.width as i32 - OVERVIEW_DEFAULT_WIDTH as i32) / 2;
            let y = mp.y + (ms.height as i32 - OVERVIEW_DEFAULT_HEIGHT as i32) / 2;
            w.set_position(tauri::PhysicalPosition::new(x, y)).ok();
        }
    }
    w.set_size(tauri::PhysicalSize::new(OVERVIEW_DEFAULT_WIDTH as u32, OVERVIEW_DEFAULT_HEIGHT as u32)).ok();
}

#[tauri::command]
fn show_overview_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        // Validate position is on-screen (may have moved off after sleep/monitor change)
        if let (Ok(pos), Ok(size)) = (w.outer_position(), w.inner_size()) {
            if !is_position_on_screen(&app, pos.x, pos.y, size.width, size.height) {
                center_overview_on_main(&app, &w);
            }
        }
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn hide_overview_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        w.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn get_overview_state(app: AppHandle) -> Result<Option<serde_json::Value>, String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        let visible = w.is_visible().unwrap_or(false);
        if visible {
            let pos = w.outer_position().map_err(|e| e.to_string())?;
            let size = w.inner_size().map_err(|e| e.to_string())?;
            return Ok(Some(serde_json::json!({
                "x": pos.x,
                "y": pos.y,
                "width": size.width,
                "height": size.height,
            })));
        }
    }
    Ok(None)
}

#[tauri::command]
fn restore_overview_window(app: AppHandle, x: i32, y: i32, width: u32, height: u32) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        if is_position_on_screen(&app, x, y, width, height) {
            w.set_position(tauri::PhysicalPosition::new(x, y)).map_err(|e| e.to_string())?;
            w.set_size(tauri::PhysicalSize::new(width, height)).map_err(|e| e.to_string())?;
        } else {
            center_overview_on_main(&app, &w);
        }
        w.show().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn reset_overview_window(app: AppHandle, to_default: bool) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        if to_default {
            center_overview_on_main(&app, &w);
        } else {
            // Restore to saved config position
            let path = config_path(&app)?;
            if path.exists() {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Ok(config) = serde_json::from_str::<serde_json::Value>(&data) {
                        if let Some(ov) = config.get("overview") {
                            let x = ov.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                            let y = ov.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                            let width = ov.get("width").and_then(|v| v.as_u64()).unwrap_or(OVERVIEW_DEFAULT_WIDTH as u64) as u32;
                            let height = ov.get("height").and_then(|v| v.as_u64()).unwrap_or(OVERVIEW_DEFAULT_HEIGHT as u64) as u32;
                            if is_position_on_screen(&app, x, y, width, height) {
                                w.set_position(tauri::PhysicalPosition::new(x, y)).ok();
                                w.set_size(tauri::PhysicalSize::new(width, height)).ok();
                                w.show().map_err(|e| e.to_string())?;
                                w.set_focus().map_err(|e| e.to_string())?;
                                return Ok(());
                            }
                        }
                    }
                }
            }
            // Fallback to default if no saved config or position is off-screen
            center_overview_on_main(&app, &w);
        }
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

// Injected into the auth WebView. Uses the browser's own session cookies.
const USAGE_INIT_SCRIPT: &str = r#"
(function() {
    async function fetchUsage() {
        try {
            const orgsResp = await fetch('/api/organizations');
            if (orgsResp.status === 401 || orgsResp.status === 403) {
                window.__TAURI_INTERNALS__.invoke('report_auth_error', {});
                return;
            }
            if (!orgsResp.ok) return;
            const orgs = await orgsResp.json();
            const org = Array.isArray(orgs) ? orgs[0] : orgs;
            const orgId = org && (org.uuid || org.id);
            if (!orgId) return;
            const usageResp = await fetch('/api/organizations/' + orgId + '/usage');
            if (!usageResp.ok) return;
            const usage = await usageResp.json();
            // Attach account info for display in settings
            try {
                const accResp = await fetch('/api/account');
                if (accResp.ok) {
                    const acc = await accResp.json();
                    usage.__account_email = acc.email_address || acc.email || null;
                    usage.__account_name = acc.full_name || acc.name || null;
                }
            } catch(_) {}
            window.__TAURI_INTERNALS__.invoke('report_usage', { data: JSON.stringify(usage) });
        } catch (_) {}
    }

    // Initial fetch after page settles
    setTimeout(fetchUsage, 800);

    // Periodic refresh every 2 minutes
    setInterval(fetchUsage, 120000);

    // Watch for login completion (SPA navigation away from /login)
    var loginWatcher = setInterval(function() {
        if (!window.location.pathname.startsWith('/login')) {
            clearInterval(loginWatcher);
            setTimeout(fetchUsage, 800);
        }
    }, 1000);
    setTimeout(function() { clearInterval(loginWatcher); }, 600000);
})();
"#;

// Three-state auth: waiting for first WebView response, login required, or ok
#[derive(PartialEq)]
enum AuthState { Pending, NeedsLogin, Ok }

struct UsageCache {
    data: Option<UsageResponse>,
    auth: AuthState,
}

impl UsageCache {
    fn new() -> Self { Self { data: None, auth: AuthState::Pending } }
    fn store(&mut self, data: UsageResponse) {
        self.auth = AuthState::Ok;
        self.data = Some(data);
    }
    fn set_needs_login(&mut self) { self.auth = AuthState::NeedsLogin; }
}

struct Cache(Mutex<UsageCache>);

// ── Usage stats ────────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
struct UsagePeriod {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Serialize, Clone)]
struct UsageResponse {
    five_hour: Option<UsagePeriod>,
    seven_day: Option<UsagePeriod>,
    seven_day_opus: Option<UsagePeriod>,
    seven_day_sonnet: Option<UsagePeriod>,
    plan: String,
    account_email: Option<String>,
    account_name: Option<String>,
}

fn parse_usage_period(obj: &serde_json::Value) -> Option<UsagePeriod> {
    let utilization = obj.get("utilization")?.as_f64()?;
    let resets_at = obj.get("resets_at").and_then(|v| v.as_str()).map(|s| s.to_string());
    Some(UsagePeriod {
        utilization: utilization.clamp(0.0, 100.0),
        resets_at,
    })
}

// Called by the auth WebView's injected script after fetching usage data
#[tauri::command]
async fn report_usage(data: String, cache: State<'_, Cache>, app: AppHandle) -> Result<(), String> {
    let json: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| format!("Parse error: {e}"))?;

    let five_hour = json.get("five_hour").and_then(parse_usage_period);
    let seven_day = json.get("seven_day").and_then(parse_usage_period);
    let seven_day_opus = json.get("seven_day_opus").and_then(parse_usage_period);
    let seven_day_sonnet = json.get("seven_day_sonnet").and_then(parse_usage_period);

    let plan = if seven_day_opus.is_some() || seven_day_sonnet.is_some() {
        "Max"
    } else if seven_day.is_some() {
        "Pro"
    } else {
        "Free"
    }.to_string();

    let account_email = json.get("__account_email").and_then(|v| v.as_str()).map(|s| s.to_string());
    let account_name = json.get("__account_name").and_then(|v| v.as_str()).map(|s| s.to_string());

    cache.0.lock().unwrap().store(UsageResponse {
        five_hour, seven_day, seven_day_opus, seven_day_sonnet, plan,
        account_email, account_name,
    });

    // Hide the login window once we have data
    if let Some(w) = app.get_webview_window(AUTH_WINDOW_LABEL) {
        w.hide().ok();
    }

    // Notify the main window to refresh its display
    app.emit("usage-updated", ()).ok();

    Ok(())
}

// Called by the auth WebView when it receives a 401/403
#[tauri::command]
fn report_auth_error(cache: State<'_, Cache>, app: AppHandle) {
    cache.0.lock().unwrap().set_needs_login();
    app.emit("usage-updated", ()).ok();
}

// Logs out by clearing WebView2 cookies and resetting usage cache
#[tauri::command]
async fn logout_usage(cache: State<'_, Cache>, app: AppHandle) -> Result<(), String> {
    // Clear the auth WebView cookies by navigating to a logout-triggering script
    if let Some(w) = app.get_webview_window(AUTH_WINDOW_LABEL) {
        w.eval(r#"
            document.cookie.split(';').forEach(function(c) {
                document.cookie = c.replace(/^ +/, '').replace(/=.*/, '=;expires=' + new Date().toUTCString() + ';path=/');
            });
            if (window.cookieStore) {
                cookieStore.getAll().then(function(cookies) {
                    cookies.forEach(function(c) { cookieStore.delete(c.name); });
                });
            }
            fetch('/api/logout', { method: 'POST' }).catch(function(){});
        "#).map_err(|e| e.to_string())?;
        w.hide().ok();
    }

    // Reset cache to needs-login state
    {
        let mut guard = cache.0.lock().unwrap();
        guard.data = None;
        guard.set_needs_login();
    }
    app.emit("usage-updated", ()).ok();

    Ok(())
}

// Returns cached usage; errors distinguish "still loading" from "must log in"
#[tauri::command]
fn get_usage(cache: State<'_, Cache>) -> Result<UsageResponse, String> {
    let guard = cache.0.lock().unwrap();
    match (&guard.auth, &guard.data) {
        (_, Some(d)) => Ok(d.clone()),
        (AuthState::NeedsLogin, _) => Err("needs_login".to_string()),
        _ => Err("pending".to_string()),
    }
}

// Shows the auth WebView window so the user can log in
#[tauri::command]
fn open_login_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(AUTH_WINDOW_LABEL) {
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let url: url::Url = "https://claude.ai".parse().map_err(|e: url::ParseError| e.to_string())?;
    WebviewWindowBuilder::new(&app, AUTH_WINDOW_LABEL, tauri::WebviewUrl::External(url))
        .title("Sign in to Claude")
        .inner_size(960.0, 720.0)
        .initialization_script(USAGE_INIT_SCRIPT)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ── Claude session status (reads ~/.claude/projects/ JSONL files) ───────────

#[derive(Serialize, Clone)]
struct ClaudeSessionStatus {
    session_id: String,
    model_id: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    folder: Option<String>,
    branch: Option<String>,
}

/// Find the JSONL session file most recently modified at or after `since_ms`,
/// then parse the last assistant message for model/usage + session metadata for cwd/branch.
#[tauri::command]
fn read_claude_session(since_ms: Option<u64>) -> Option<ClaudeSessionStatus> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let projects_dir = std::path::Path::new(&home).join(".claude").join("projects");
    let threshold = since_ms.unwrap_or(0);

    // Collect all .jsonl files modified at or after threshold
    let mut candidates: Vec<(u64, std::path::PathBuf)> = Vec::new();
    if let Ok(projects) = std::fs::read_dir(&projects_dir) {
        for project in projects.flatten() {
            let project_path = project.path();
            if !project_path.is_dir() { continue; }
            if let Ok(files) = std::fs::read_dir(&project_path) {
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
                    if let Ok(meta) = std::fs::metadata(&path) {
                        if let Ok(modified) = meta.modified() {
                            let ms = modified
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            if ms >= threshold {
                                candidates.push((ms, path));
                            }
                        }
                    }
                }
            }
        }
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let (_, jsonl_path) = candidates.into_iter().next()?;

    // Session ID is the filename stem
    let session_id = jsonl_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let content = std::fs::read_to_string(&jsonl_path).ok()?;

    let mut cwd: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut model_id: Option<String> = None;
    let mut input_tokens: Option<u64> = None;
    let mut output_tokens: Option<u64> = None;
    let mut cache_creation: Option<u64> = None;
    let mut cache_read: Option<u64> = None;

    for line in content.lines() {
        if line.is_empty() { continue; }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else { continue };

        // Top-level metadata entries contain cwd and gitBranch
        if let Some(c) = entry.get("cwd").and_then(|v| v.as_str()) {
            cwd = Some(c.to_string());
        }
        if let Some(b) = entry.get("gitBranch").and_then(|v| v.as_str()) {
            branch = Some(b.to_string());
        }

        // Assistant messages contain model + usage — keep overwriting to get the latest turn
        if entry.pointer("/message/role").and_then(|v| v.as_str()) == Some("assistant") {
            if let Some(m) = entry.pointer("/message/model").and_then(|v| v.as_str()) {
                model_id = Some(m.to_string());
            }
            if let Some(usage) = entry.pointer("/message/usage") {
                input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).or(input_tokens);
                output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64()).or(output_tokens);
                cache_creation = usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).or(cache_creation);
                cache_read = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).or(cache_read);
            }
        }
    }

    let folder = cwd.as_deref().map(|p| {
        p.replace('\\', "/")
            .split('/')
            .filter(|s| !s.is_empty())
            .last()
            .unwrap_or(p)
            .to_string()
    });

    Some(ClaudeSessionStatus {
        session_id,
        model_id,
        input_tokens,
        output_tokens,
        cache_creation_input_tokens: cache_creation,
        cache_read_input_tokens: cache_read,
        folder,
        branch,
    })
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
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().and_then(|c| (c as char).to_digit(16));
            let lo = chars.next().and_then(|c| (c as char).to_digit(16));
            if let (Some(h), Some(l)) = (hi, lo) {
                result.push((h * 16 + l) as u8 as char);
            }
        } else {
            result.push(b as char);
        }
    }
    result
}

// ── Config persistence ──────────────────────────────────────────────────────

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("config.json"))
}

#[tauri::command]
fn save_config(app: AppHandle, config: serde_json::Value) -> Result<(), String> {
    let path = config_path(&app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    // Atomic write: write to temp file, then rename for crash safety
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn load_config(app: AppHandle) -> Result<Option<serde_json::Value>, String> {
    let path = config_path(&app)?;
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let config: serde_json::Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    Ok(Some(config))
}

/// Returns the last known working directory for a session, tracked via OSC 7.
#[tauri::command]
fn get_session_cwd(session_id: String, sessions: State<Sessions>) -> Option<String> {
    let map = sessions.0.lock().unwrap();
    let session = map.get(&session_id)?;
    let cwd = session.cwd.lock().unwrap().clone();
    cwd
}

#[tauri::command]
fn get_session_title(session_id: String, sessions: State<Sessions>) -> Option<String> {
    let map = sessions.0.lock().unwrap();
    let session = map.get(&session_id)?;
    let title = session.title.lock().unwrap().clone();
    title
}

/// Returns the last known OSC 133 idle state, or None before any prompt
/// marker has been seen. Lets a newly mounted TerminalPane recover idle
/// state without waiting for the next idle↔busy transition.
#[tauri::command]
fn get_session_shell_idle(session_id: String, sessions: State<Sessions>) -> Option<bool> {
    let map = sessions.0.lock().unwrap();
    let session = map.get(&session_id)?;
    let idle = *session.shell_idle.lock().unwrap();
    idle
}

#[derive(Serialize, Clone)]
struct GitInfo {
    is_repo: bool,
    branch: Option<String>,
}

/// Standalone git info lookup (usable from both Tauri commands and background threads)
fn get_git_info(cwd: &str) -> GitInfo {
    let path = std::path::Path::new(cwd);
    if !path.is_dir() {
        return GitInfo { is_repo: false, branch: None };
    }
    let is_repo = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !is_repo {
        return GitInfo { is_repo: false, branch: None };
    }
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty());
    GitInfo { is_repo, branch }
}

#[tauri::command]
fn get_session_git_info(cwd: String) -> GitInfo {
    get_git_info(&cwd)
}

#[tauri::command]
fn exit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn focus_webview(webview_window: tauri::WebviewWindow) {
    #[cfg(windows)]
    {
        let _ = webview_window.with_webview(|webview| {
            unsafe {
                use webview2_com::Microsoft::Web::WebView2::Win32::*;
                let controller = webview.controller();
                let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            }
        });
    }
}

#[tauri::command]
fn get_locale() -> String {
    // On Windows, sys-locale returns the display language (e.g. en-US) not the
    // regional format (e.g. da-DK). Read the regional format from the registry.
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-Command", "(Get-ItemProperty 'HKCU:\\Control Panel\\International').LocaleName"])
            .output()
        {
            let locale = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !locale.is_empty() {
                return locale;
            }
        }
    }
    sys_locale::get_locale().unwrap_or_else(|| "en-US".to_string())
}

// ── Shell ───────────────────────────────────────────────────────────────────

#[tauri::command]
fn check_git_bash() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ];
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                return Some(path.to_string());
            }
        }
        // Fallback: check PATH via `where bash.exe`, filtering out WSL/System32
        if let Ok(output) = std::process::Command::new("where").arg("bash.exe").output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    let lower = line.to_lowercase();
                    if lower.contains("git") && !lower.contains("system32") {
                        return Some(line.trim().to_string());
                    }
                }
            }
        }
        None
    }
    #[cfg(not(target_os = "windows"))]
    { None }
}

// ── Git Worktree Commands ───────────────────────────────────────────────────

#[derive(Serialize, Clone)]
struct WorktreeInfo {
    path: String,
    branch: Option<String>,
    head: Option<String>,
    is_main: bool,
}

#[tauri::command]
fn git_worktree_list(repo_root: String) -> Result<Vec<WorktreeInfo>, String> {
    let output = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("Failed to run git worktree list: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut worktrees = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_head: Option<String> = None;
    let mut current_branch: Option<String> = None;
    let mut is_bare = false;

    for line in stdout.lines() {
        if line.starts_with("worktree ") {
            // Flush previous entry
            if let Some(path) = current_path.take() {
                if !is_bare {
                    worktrees.push(WorktreeInfo {
                        path: path.clone(),
                        branch: current_branch.take(),
                        head: current_head.take(),
                        is_main: false, // set below
                    });
                }
            }
            current_path = Some(line[9..].to_string());
            current_head = None;
            current_branch = None;
            is_bare = false;
        } else if line.starts_with("HEAD ") {
            current_head = Some(line[5..].to_string());
        } else if line.starts_with("branch ") {
            // "branch refs/heads/main" → "main"
            let branch = line[7..].to_string();
            current_branch = Some(branch.strip_prefix("refs/heads/").unwrap_or(&branch).to_string());
        } else if line == "bare" {
            is_bare = true;
        }
    }
    // Flush last entry
    if let Some(path) = current_path {
        if !is_bare {
            worktrees.push(WorktreeInfo {
                path,
                branch: current_branch,
                head: current_head,
                is_main: false,
            });
        }
    }

    // The first worktree (at repo root) is the main one
    if let Some(first) = worktrees.first_mut() {
        first.is_main = true;
    }

    Ok(worktrees)
}

#[tauri::command]
fn git_worktree_add(repo_root: String, branch_name: String, base_branch: Option<String>) -> Result<WorktreeInfo, String> {
    // Place worktree as sibling directory: ../reponame-branchname
    let repo_path = std::path::Path::new(&repo_root);
    let repo_name = repo_path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());

    let parent = repo_path.parent()
        .ok_or_else(|| "Cannot determine parent directory of repo".to_string())?;
    let worktree_dir = parent.join(format!("{}-{}", repo_name, branch_name));
    let worktree_path = worktree_dir.to_string_lossy().to_string();

    let mut args = vec![
        "worktree".to_string(),
        "add".to_string(),
        "-b".to_string(),
        branch_name.clone(),
        worktree_path.clone(),
    ];
    if let Some(base) = &base_branch {
        args.push(base.clone());
    }

    let output = std::process::Command::new("git")
        .args(&args)
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("Failed to run git worktree add: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    // Get HEAD of the new worktree
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&worktree_path)
        .output()
        .ok()
        .and_then(|o| if o.status.success() {
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else { None });

    Ok(WorktreeInfo {
        path: worktree_path,
        branch: Some(branch_name),
        head,
        is_main: false,
    })
}

#[tauri::command]
fn git_worktree_remove(repo_root: String, worktree_path: String, force: bool) -> Result<(), String> {
    let mut args: Vec<&str> = vec!["-C", &repo_root, "worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&worktree_path);

    let output = std::process::Command::new("git")
        .args(&args)
        .output()
        .map_err(|e| format!("Failed to run git worktree remove: {}", e))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Fallback: if git doesn't recognise the path as a worktree any more
    // (e.g. the .git gitlink is broken from a previous half-completed
    // removal, or the administrative entry in .git/worktrees was lost),
    // the directory still exists on disk. For force removals (discard),
    // delete the directory ourselves and run `git worktree prune` to
    // clean up any stale administrative state.
    let not_a_worktree = stderr.contains("is not a working tree")
        || stderr.contains("not a working tree");

    if force && not_a_worktree {
        let wt_path = std::path::Path::new(&worktree_path);
        if wt_path.exists() {
            std::fs::remove_dir_all(wt_path)
                .map_err(|e| format!("Filesystem removal failed: {}", e))?;
        }
        // Clean up stale administrative entries in <repo>/.git/worktrees.
        let _ = std::process::Command::new("git")
            .args(["-C", &repo_root, "worktree", "prune"])
            .output();
        return Ok(());
    }

    Err(stderr.trim().to_string())
}

#[tauri::command]
fn git_merge_branch(repo_root: String, source_branch: String, target_branch: String) -> Result<String, String> {
    // Find the worktree that has the target branch checked out
    let worktrees = git_worktree_list(repo_root.clone())?;
    let target_wt = worktrees.iter().find(|wt| wt.branch.as_deref() == Some(&target_branch));
    let merge_dir = target_wt.map(|wt| wt.path.clone()).unwrap_or(repo_root);

    let output = std::process::Command::new("git")
        .args(["merge", &source_branch])
        .current_dir(&merge_dir)
        .output()
        .map_err(|e| format!("Failed to run git merge: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(format!("{}\n{}", stdout, stderr).trim().to_string());
    }
    Ok(stdout.trim().to_string())
}

#[tauri::command]
fn git_push_and_create_pr(worktree_path: String) -> Result<String, String> {
    // Push branch
    let push_output = std::process::Command::new("git")
        .args(["push", "-u", "origin", "HEAD"])
        .current_dir(&worktree_path)
        .output()
        .map_err(|e| format!("Failed to push: {}", e))?;

    if !push_output.status.success() {
        return Err(String::from_utf8_lossy(&push_output.stderr).trim().to_string());
    }

    // Create PR using gh CLI
    let pr_output = std::process::Command::new("gh")
        .args(["pr", "create", "--fill"])
        .current_dir(&worktree_path)
        .output()
        .map_err(|e| format!("Failed to create PR (is gh CLI installed?): {}", e))?;

    if !pr_output.status.success() {
        return Err(String::from_utf8_lossy(&pr_output.stderr).trim().to_string());
    }

    Ok(String::from_utf8_lossy(&pr_output.stdout).trim().to_string())
}

#[tauri::command]
fn git_list_branches(repo_path: String) -> Result<Vec<String>, String> {
    // List local branches and remote branches that don't have a local counterpart.
    // Local branches are returned by their short name; remote-only branches keep the
    // remote prefix (e.g. "origin/foo") so the value is directly usable as a git ref.
    let output = std::process::Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"])
        .current_dir(&repo_path)
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut local: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut remote: Vec<String> = Vec::new();

    for line in stdout.lines() {
        let full = line.trim();
        if let Some(rest) = full.strip_prefix("refs/heads/") {
            if !rest.is_empty() { local.insert(rest.to_string()); }
        } else if let Some(rest) = full.strip_prefix("refs/remotes/") {
            if rest.ends_with("/HEAD") { continue; }
            remote.push(rest.to_string());
        }
    }

    let mut result: Vec<String> = local.iter().cloned().collect();
    for r in remote {
        // Strip the remote name to compare against local branches
        let short = match r.find('/') {
            Some(idx) => &r[idx + 1..],
            None => continue,
        };
        if !local.contains(short) {
            result.push(r);
        }
    }
    result.sort();
    Ok(result)
}

#[tauri::command]
fn git_repo_root(path: String) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&path)
        .output()
        .ok()?;

    if output.status.success() {
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !root.is_empty() { Some(root) } else { None }
    } else {
        None
    }
}

// ── File Explorer Commands ──────────────────────────────────────────────────

#[derive(Serialize, Clone)]
struct DirEntry {
    name: String,
    path: String,
    is_dir: bool,
    is_symlink: bool,
}

#[tauri::command]
fn get_project_model(project_path: String) -> Option<String> {
    let settings_path = std::path::Path::new(&project_path).join(".claude").join("settings.json");
    let content = std::fs::read_to_string(&settings_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("model").and_then(|v| v.as_str()).map(|s| s.to_string())
}

#[tauri::command]
fn read_directory(path: String, show_hidden: Option<bool>) -> Result<Vec<DirEntry>, String> {
    let show_hidden = show_hidden.unwrap_or(false);
    let dir = std::fs::read_dir(&path).map_err(|e| format!("Failed to read directory: {}", e))?;

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in dir {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden files unless requested
        if !show_hidden && name.starts_with('.') {
            continue;
        }

        // Skip .git directory
        if name == ".git" {
            continue;
        }

        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        let is_symlink = entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false);
        let is_dir = metadata.is_dir();

        let item = DirEntry {
            name: name.clone(),
            path: entry.path().to_string_lossy().to_string(),
            is_dir,
            is_symlink,
        };

        if is_dir {
            dirs.push(item);
        } else {
            files.push(item);
        }
    }

    // Sort: directories first (alphabetical), then files (alphabetical)
    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    dirs.extend(files);

    Ok(dirs)
}

#[tauri::command]
fn git_file_status(repo_root: String, worktree_path: Option<String>) -> Result<HashMap<String, String>, String> {
    let cwd = worktree_path.as_deref().unwrap_or(&repo_root);
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain=v1", "-uall"])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("Failed to run git status: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut statuses = HashMap::new();

    for line in stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let xy = &line[0..2];
        let file_path = &line[3..];

        // Determine status from XY codes
        let status = match xy.trim() {
            "M" | "MM" | "AM" => "modified",
            "A" => "added",
            "D" => "deleted",
            "R" => "renamed",
            "??" => "untracked",
            "UU" | "AA" | "DD" => "conflicted",
            _ if xy.contains('M') => "modified",
            _ if xy.contains('A') => "added",
            _ if xy.contains('D') => "deleted",
            _ => "modified",
        };

        // Handle renamed files: "R  old -> new"
        let actual_path = if file_path.contains(" -> ") {
            file_path.split(" -> ").last().unwrap_or(file_path)
        } else {
            file_path
        };

        statuses.insert(actual_path.to_string(), status.to_string());
    }

    Ok(statuses)
}

// ── File Watcher ────────────────────────────────────────────────────────────

struct FileWatchers(Arc<Mutex<HashMap<String, RecommendedWatcher>>>);

#[tauri::command]
fn watch_directory(app: AppHandle, watchers: State<FileWatchers>, path: String, recursive: Option<bool>) -> Result<String, String> {
    let watcher_id = Uuid::new_v4().to_string();
    let app_handle = app.clone();
    let watcher_id_clone = watcher_id.clone();

    let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
        if let Ok(event) = res {
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                    let _ = app_handle.emit(&format!("fs-changed-{}", watcher_id_clone), ());
                }
                _ => {}
            }
        }
    }).map_err(|e| format!("Failed to create watcher: {}", e))?;

    let mode = if recursive.unwrap_or(false) {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    watcher.watch(std::path::Path::new(&path), mode)
        .map_err(|e| format!("Failed to watch directory: {}", e))?;

    watchers.0.lock().unwrap().insert(watcher_id.clone(), watcher);
    Ok(watcher_id)
}

#[tauri::command]
fn git_is_branch_merged(repo_root: String, branch: String, into_branch: String) -> Result<bool, String> {
    // A branch is "merged" into its parent when:
    //   1. branch's tip is reachable from into_branch (the ancestor check), AND
    //   2. the two tips are NOT the same commit.
    //
    // Without (2), a freshly-created branch (which shares a commit with its
    // parent) would be marked merged immediately, because every commit is its
    // own ancestor. Erring toward "not merged" when tips are equal also means
    // we won't falsely mark a just-fast-forwarded branch as merged, which is
    // acceptable — that case self-corrects as soon as the parent advances.

    let rev = |refname: &str| -> Result<String, String> {
        let out = std::process::Command::new("git")
            .args(["rev-parse", refname])
            .current_dir(&repo_root)
            .output()
            .map_err(|e| format!("Failed to run git rev-parse: {}", e))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };

    let branch_sha = rev(&branch)?;
    let parent_sha = rev(&into_branch)?;
    if branch_sha == parent_sha {
        return Ok(false);
    }

    // `git merge-base --is-ancestor <branch> <into_branch>`
    // exit 0 → branch's tip is reachable from into_branch (fully merged)
    // exit 1 → not merged
    // any other → real error
    let output = std::process::Command::new("git")
        .args(["merge-base", "--is-ancestor", &branch, &into_branch])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("Failed to run git merge-base: {}", e))?;

    if let Some(code) = output.status.code() {
        match code {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        }
    } else {
        Err("git merge-base terminated by signal".to_string())
    }
}

#[tauri::command]
fn unwatch_directory(watcher_id: String, watchers: State<FileWatchers>) {
    watchers.0.lock().unwrap().remove(&watcher_id);
}

fn build_shell_command(shell: Option<&str>) -> CommandBuilder {
    // OSC 133 (FinalTerm prompt markers) lets the PTY parser emit
    // `shell-activity-{sid}` events without polling sysinfo:
    //   133;A → prompt start (idle)
    //   133;C → pre-execution (busy)
    //   133;D → command finished (idle)
    // We embed these in PROMPT_COMMAND / PS0 / the PS prompt function so users
    // don't need any shell-init changes.

    #[cfg(target_os = "windows")]
    {
        if let Some(bash_path) = shell {
            // Git Bash on Windows — use PROMPT_COMMAND with pwd -W for Windows paths
            let mut cmd = CommandBuilder::new(bash_path);
            cmd.args(["--login", "-i"]);
            cmd.env(
                "PROMPT_COMMAND",
                concat!(
                    // D (prev command finished), 7 (cwd), A (new prompt starts)
                    r#"printf '\e]133;D\a\e]7;file:///%s\a\e]133;A\a' "$(pwd -W | sed 's/ /%20/g' | sed 's/\\/\//g')""#,
                ),
            );
            // PS0 is emitted by bash just before executing the command — literal
            // ESC/BEL bytes so bash doesn't need to parse `\e`/`\a` escapes.
            cmd.env("PS0", "\x1b]133;C\x07");
            cmd
        } else {
            let mut cmd = CommandBuilder::new("powershell.exe");
            // -NoExit keeps the shell interactive after running the setup command.
            // The prompt override emits OSC 7 with cwd plus OSC 133 A (prompt
            // start → idle). OSC 133 C (command start → busy) is emitted via a
            // PSReadLine Enter handler so busy transitions are detected too.
            cmd.args([
                "-NoExit",
                "-Command",
                concat!(
                    "$__arbiter_orig_prompt = $function:prompt; ",
                    "function prompt { ",
                        "$loc = (Get-Location).Path; ",
                        "$uri = 'file:///' + ($loc -replace '\\\\','/'); ",
                        "$e = [char]27; $bel = [char]7; ",
                        "[Console]::Write(\"${e}]7;${uri}${bel}${e}]133;A${bel}\"); ",
                        "& $__arbiter_orig_prompt ",
                    "}; ",
                    "if (Get-Module PSReadLine -ErrorAction SilentlyContinue) { ",
                        "Set-PSReadLineKeyHandler -Key Enter -ScriptBlock { ",
                            "param($key, $arg) ",
                            "[Console]::Write([char]27 + ']133;C' + [char]7); ",
                            "[Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine() ",
                        "} ",
                    "}"
                ),
            ]);
            cmd
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = shell; // unused on non-Windows
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(&sh);
        cmd.arg("-l");
        // Bash PROMPT_COMMAND: emit OSC 133 D (command finished) + OSC 7 (cwd)
        // + OSC 133 A (prompt start). Works for bash; zsh users typically have
        // precmd hooks from their rc files instead.
        cmd.env(
            "PROMPT_COMMAND",
            r#"printf '\e]133;D\a\e]7;file://%s%s\a\e]133;A\a' "$(hostname)" "$(pwd)""#,
        );
        // PS0 fires just before executing a command → OSC 133 C (busy). Literal
        // bytes so bash doesn't re-interpret the escapes.
        cmd.env("PS0", "\x1b]133;C\x07");
        cmd
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED
                        | tauri_plugin_window_state::StateFlags::FULLSCREEN,
                )
                .with_denylist(&[AUTH_WINDOW_LABEL, OVERVIEW_WINDOW_LABEL])
                .build(),
        )
        .manage({
            let inner: Arc<Mutex<HashMap<String, PtySession>>> = Arc::new(Mutex::new(HashMap::new()));
            Sessions(inner)
        })
        .manage(ClaudeMonitor(Arc::new(Mutex::new(HashMap::new()))))
        .manage(ExpectedClaudeSessions(Arc::new(Mutex::new(HashMap::new()))))
        .manage(Cache(Mutex::new(UsageCache::new())))
        .manage(FileWatchers(Arc::new(Mutex::new(HashMap::new()))))
        .setup(|app| {
            // Create a hidden auth WebView at startup. If the user has a valid
            // session (persisted WebView2 cookies), the injected script will
            // silently fetch usage and populate the cache without any user action.
            let url: url::Url = "https://claude.ai".parse().unwrap();
            WebviewWindowBuilder::new(app, AUTH_WINDOW_LABEL, tauri::WebviewUrl::External(url))
                .title("Sign in to Claude")
                .inner_size(960.0, 720.0)
                .visible(false)
                .initialization_script(USAGE_INIT_SCRIPT)
                .build()?;

            // Create a hidden overview window at startup so WebView2
            // initialises during the event-loop setup phase (avoids the
            // deadlock that occurs when building a window from a command).
            WebviewWindowBuilder::new(app, OVERVIEW_WINDOW_LABEL, tauri::WebviewUrl::default())
                .title("Arbiter – Overview")
                .inner_size(240.0, 320.0)
                .min_inner_size(180.0, 120.0)
                .always_on_top(true)
                .decorations(false)
                .resizable(true)
                .visible(false)
                .build()?;

            // Start the event-driven Claude session watcher
            let sessions_arc = app.state::<Sessions>().0.clone();
            let monitor_arc  = app.state::<ClaudeMonitor>().0.clone();
            let expected_arc = app.state::<ExpectedClaudeSessions>().0.clone();
            start_claude_watcher(app.handle().clone(), sessions_arc, monitor_arc, expected_arc);

            // Show the main window after the window-state plugin has restored
            // its position/size so there's no visible jump.
            if let Some(w) = app.get_webview_window("main") {
                w.show().unwrap_or_default();
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Intercept close on the overview window — hide instead of destroy
            if window.label() == OVERVIEW_WINDOW_LABEL {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    window.hide().unwrap_or_default();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            create_session,
            set_expected_claude_session,
            write_to_session,
            resize_session,
            close_session,
            get_session_replay,
            get_usage,
            report_usage,
            report_auth_error,
            open_login_window,
            logout_usage,
            read_claude_session,
            get_active_claude_status,
            save_config,
            load_config,
            get_session_cwd,
            get_session_title,
            get_session_shell_idle,
            get_session_git_info,
            exit_app,
            get_locale,
            focus_webview,
            check_git_bash,
            show_overview_window,
            hide_overview_window,
            get_overview_state,
            restore_overview_window,
            reset_overview_window,
            git_worktree_list,
            git_worktree_add,
            git_is_branch_merged,
            git_worktree_remove,
            git_merge_branch,
            git_push_and_create_pr,
            git_repo_root,
            git_list_branches,
            read_directory,
            git_file_status,
            watch_directory,
            unwatch_directory,
            get_project_model,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
