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
}

// Arc so the watcher background thread can share ownership
struct Sessions(Arc<Mutex<HashMap<String, PtySession>>>);

// Tracks active Claude processes: pane_id → (claude_pid, jsonl_path)
struct ClaudeMonitor(Arc<Mutex<HashMap<String, (u32, PathBuf)>>>);

#[tauri::command]
fn create_session(app: AppHandle, sessions: State<Sessions>, monitor: State<ClaudeMonitor>, cols: Option<u16>, rows: Option<u16>, cwd: Option<String>) -> Result<String, String> {
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

    let mut cmd = build_shell_command();
    cmd.env("TERM", "xterm-256color");
    if let Some(ref dir) = cwd {
        let p = std::path::Path::new(dir);
        if p.is_dir() {
            cmd.cwd(p);
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

    // Spawn thread to stream PTY output to the frontend and buffer it for replay
    let app_handle = app.clone();
    let event_name = format!("pty-output-{}", sid);
    std::thread::spawn(move || {
        const MAX_BUF: usize = 102_400; // 100 KB rolling buffer
        let mut buf = [0u8; 4096];
        // Accumulates partial OSC sequences across chunks
        let mut osc_buf = String::new();
        let mut in_osc = false;
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = &buf[..n];

                    // Scan for OSC 7 sequences: \x1b]7;file:///path\x07 or \x1b]7;file:///path\x1b\\
                    let text = String::from_utf8_lossy(chunk);
                    for ch in text.chars() {
                        if in_osc {
                            if ch == '\x07' || (osc_buf.ends_with('\x1b') && ch == '\\') {
                                // End of OSC — parse the URI
                                let payload = if osc_buf.ends_with('\x1b') {
                                    &osc_buf[..osc_buf.len() - 1]
                                } else {
                                    &osc_buf
                                };
                                if let Some(path) = parse_osc7_uri(payload) {
                                    *cwd_writer.lock().unwrap() = Some(path);
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
                        b.extend_from_slice(chunk);
                        if b.len() > MAX_BUF {
                            let excess = b.len() - MAX_BUF;
                            b.drain(..excess);
                        }
                    }
                    let output = String::from_utf8_lossy(chunk).to_string();
                    let _ = app_handle.emit(&event_name, output);
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
        },
    );

    // Permanent per-pane monitor: detect Claude → watch exit → detect again, forever.
    spawn_pane_monitor(app.clone(), sessions.0.clone(), monitor.0.clone(), session_id.clone());

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
fn close_session(session_id: String, sessions: State<Sessions>) {
    sessions.0.lock().unwrap().remove(&session_id);
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

/// Scan all processes to find a Claude→shell→pane mapping for any unmonitored pane.
/// Used by the file watcher when a new JSONL appears.
fn find_unmonitored_claude_pane(
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  &Arc<Mutex<HashMap<String, (u32, PathBuf)>>>,
) -> Option<(String, u32)> {
    let session_map = sessions.lock().unwrap();
    let monitored   = monitor.lock().unwrap();

    for (session_id, session) in session_map.iter() {
        if monitored.contains_key(session_id) { continue; }
        if let Some(shell_pid) = session.shell_pid {
            if let Some((claude_pid, _)) = find_claude_for_shell(shell_pid) {
                return Some((session_id.clone(), claude_pid));
            }
        }
    }
    None
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
    let (_, jsonl_path) = mon.get(&session_id)?;
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

/// Spawn a lightweight thread that blocks until the Claude process exits,
/// then emits `claude-exited-{pane_id}`.
fn spawn_exit_watcher(
    app:      AppHandle,
    monitor:  Arc<Mutex<HashMap<String, (u32, PathBuf)>>>,
    pane_id:  String,
    claude_pid: u32,
) {
    std::thread::spawn(move || {
        let sysinfo_pid = sysinfo::Pid::from_u32(claude_pid);
        let mut sys = System::new();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            // Refresh only this one PID — very cheap (one /proc read or one Win32 call)
            sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[sysinfo_pid]),
                true,
                ProcessRefreshKind::new(),
            );
            if sys.process(sysinfo_pid).is_none() { break; }
        }
        monitor.lock().unwrap().remove(&pane_id);
        app.emit(&format!("claude-exited-{}", pane_id), ()).ok();
    });
}

/// Permanent per-pane monitor: detect Claude → associate JSONL → watch exit → loop.
/// Runs for the lifetime of the PTY session. Complements the file-system watcher
/// as a reliable fallback (the watcher can miss events on Windows).
fn spawn_pane_monitor(
    app:      AppHandle,
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  Arc<Mutex<HashMap<String, (u32, PathBuf)>>>,
    pane_id:  String,
) {
    // Get shell PID for this pane so we only look for Claude under our own shell
    let shell_pid = {
        let guard = sessions.lock().unwrap();
        match guard.get(&pane_id).and_then(|s| s.shell_pid) {
            Some(pid) => pid,
            None => return,
        }
    };

    let projects_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".claude").join("projects"))
        .unwrap_or_default();

    std::thread::spawn(move || {
        loop {
            // ── Phase 1: wait for Claude to appear under our shell ───────────
            let mut start_time: u64 = 0;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if !sessions.lock().unwrap().contains_key(&pane_id) { return; }
                // If file watcher already tracked this pane, skip to exit-wait
                if monitor.lock().unwrap().contains_key(&pane_id) { break; }
                if let Some((pid, st)) = find_claude_for_shell(shell_pid) {
                    start_time = st;
                    // Register immediately so the file watcher knows this pane is taken
                    monitor.lock().unwrap().insert(pane_id.clone(), (pid, PathBuf::new()));
                    // Emit started event with a minimal status (JSONL may not exist yet)
                    let status = ClaudeSessionStatus {
                        session_id: pane_id.clone(),
                        model_id: None, input_tokens: None, output_tokens: None,
                        cache_creation_input_tokens: None, cache_read_input_tokens: None,
                        folder: None, branch: None,
                    };
                    app.emit(&format!("claude-started-{}", pane_id), &status).ok();
                    spawn_exit_watcher(app.clone(), monitor.clone(), pane_id.clone(), pid);
                    break;
                }
            }

            // ── Phase 2: try to find and associate the JSONL file ────────────
            // The JSONL may appear shortly after Claude starts. Keep trying for 30s.
            {
                let has_jsonl = || -> bool {
                    let mon = monitor.lock().unwrap();
                    mon.get(&pane_id).map_or(false, |(_, p)| !p.as_os_str().is_empty())
                };

                if !has_jsonl() && start_time > 0 {
                    for _ in 0..30 {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        if !monitor.lock().unwrap().contains_key(&pane_id) { break; }
                        if has_jsonl() { break; } // file watcher adopted a JSONL
                        if let Some(jsonl) = find_latest_untracked_jsonl(&projects_dir, &monitor, Some(start_time)) {
                            if let Some(status) = parse_jsonl_status(&jsonl) {
                                monitor.lock().unwrap().entry(pane_id.clone())
                                    .and_modify(|(_, p)| *p = jsonl.clone());
                                app.emit(&format!("claude-status-{}", pane_id), &status).ok();
                            }
                            break;
                        }
                    }
                }
            }

            // ── Phase 3: wait for Claude to exit (monitor entry removed) ─────
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if !sessions.lock().unwrap().contains_key(&pane_id) { return; }
                if !monitor.lock().unwrap().contains_key(&pane_id) { break; }
            }
            // Loop back to Phase 1 to detect next Claude launch
        }
    });
}

/// Try to match a new JSONL file to a pane. First checks if any already-monitored
/// pane is missing its JSONL (detected by pane monitor before file appeared), then
/// falls back to scanning for an unmonitored pane. Retries with backoff.
fn handle_new_session(
    app:      &AppHandle,
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  &Arc<Mutex<HashMap<String, (u32, PathBuf)>>>,
    jsonl_path: &std::path::Path,
) -> bool {
    let delays_ms = [300u64, 500, 700, 1500];
    for delay in delays_ms {
        std::thread::sleep(std::time::Duration::from_millis(delay));

        // First: adopt into a pane that the pane-monitor already detected but
        // couldn't associate a JSONL with (empty path in the monitor).
        {
            let mut mon = monitor.lock().unwrap();
            let empty_pane = mon.iter()
                .find(|(_, (_, p))| p.as_os_str().is_empty())
                .map(|(id, (pid, _))| (id.clone(), *pid));
            if let Some((pane_id, claude_pid)) = empty_pane {
                mon.insert(pane_id.clone(), (claude_pid, jsonl_path.to_path_buf()));
                drop(mon);
                // Emit as status update (started was already emitted by pane monitor)
                if let Some(status) = parse_jsonl_status(jsonl_path) {
                    app.emit(&format!("claude-status-{}", pane_id), &status).ok();
                }
                return true;
            }
        }

        // Otherwise: find an unmonitored pane with a Claude process
        if let Some((pane_id, claude_pid)) = find_unmonitored_claude_pane(sessions, monitor) {
            monitor.lock().unwrap().insert(pane_id.clone(), (claude_pid, jsonl_path.to_path_buf()));
            let status = parse_jsonl_status(jsonl_path).unwrap_or_else(|| ClaudeSessionStatus {
                session_id: pane_id.clone(),
                model_id: None, input_tokens: None, output_tokens: None,
                cache_creation_input_tokens: None, cache_read_input_tokens: None,
                folder: None, branch: None,
            });
            app.emit(&format!("claude-started-{}", pane_id), &status).ok();
            spawn_exit_watcher(app.clone(), monitor.clone(), pane_id, claude_pid);
            return true;
        }
    }
    false
}

/// Called on every JSONL modify event — emits a status update to the already-matched pane.
/// Also adopts the JSONL if a monitored pane has an empty path (detected by pane monitor
/// before the file watcher caught the create event).
fn handle_session_update(
    app:     &AppHandle,
    monitor:  &Arc<Mutex<HashMap<String, (u32, PathBuf)>>>,
    jsonl_path: &std::path::Path,
) {
    let mut mon = monitor.lock().unwrap();

    // Check if this JSONL is already tracked → emit status update
    let tracked_pane = mon.iter()
        .find(|(_, (_, p))| p == jsonl_path)
        .map(|(id, _)| id.clone());
    if let Some(pane_id) = tracked_pane {
        drop(mon);
        if let Some(status) = parse_jsonl_status(jsonl_path) {
            app.emit(&format!("claude-status-{}", pane_id), &status).ok();
        }
        return;
    }

    // Check if a monitored pane has an empty JSONL path → adopt this file
    let empty_pane = mon.iter()
        .find(|(_, (_, p))| p.as_os_str().is_empty())
        .map(|(id, (pid, _))| (id.clone(), *pid));
    if let Some((pane_id, claude_pid)) = empty_pane {
        mon.insert(pane_id.clone(), (claude_pid, jsonl_path.to_path_buf()));
        drop(mon);
        if let Some(status) = parse_jsonl_status(jsonl_path) {
            app.emit(&format!("claude-status-{}", pane_id), &status).ok();
        }
        return;
    }
    // Untracked file and no empty panes: belongs to a different Claude session.
}

/// Find the most recently created JSONL file not already in the monitor.
/// When `created_after_secs` is provided (Unix seconds), only consider files whose
/// creation time is within 30 seconds of that timestamp. This prevents matching
/// JSONL files from unrelated Claude sessions (e.g. editor chats).
fn find_latest_untracked_jsonl(
    projects_dir: &std::path::Path,
    monitor: &Arc<Mutex<HashMap<String, (u32, PathBuf)>>>,
    created_after_secs: Option<u64>,
) -> Option<PathBuf> {
    let tracked: Vec<PathBuf> = monitor.lock().unwrap()
        .values().map(|(_, p)| p.clone()).collect();

    let mut best: Option<(u64, PathBuf)> = None;
    let Ok(project_dirs) = std::fs::read_dir(projects_dir) else { return None; };
    for entry in project_dirs.flatten() {
        let dir = entry.path();
        if !dir.is_dir() { continue; }
        let Ok(files) = std::fs::read_dir(&dir) else { continue; };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            if tracked.iter().any(|t| t == &path) { continue; }
            if let Ok(meta) = std::fs::metadata(&path) {
                // Prefer creation time (available on Windows); fall back to modified
                let file_time = meta.created().or_else(|_| meta.modified()).ok();
                if let Some(time) = file_time {
                    let secs = time.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default().as_secs();
                    // If we know when the Claude process started, only consider files
                    // created within 30 seconds of that start time
                    if let Some(after) = created_after_secs {
                        if secs < after.saturating_sub(5) || secs > after + 30 {
                            continue;
                        }
                    }
                    if best.as_ref().map_or(true, |(t, _)| secs > *t) {
                        best = Some((secs, path));
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Start the file-system watcher on `~/.claude/projects/`.
/// Fires OS-level events (inotify / FSEvents / ReadDirectoryChangesW) — no polling.
fn start_claude_watcher(
    app:      AppHandle,
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    monitor:  Arc<Mutex<HashMap<String, (u32, PathBuf)>>>,
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
            match event.kind {
                // Create: new Claude session file appeared. Spawn a thread per path so
                // the retry backoff in handle_new_session never blocks the watcher.
                // Also match Other/Any — Windows ReadDirectoryChangesW sometimes
                // reports new files as non-Create events.
                EventKind::Create(_) | EventKind::Other | EventKind::Any => {
                    for path in paths {
                        let already_tracked = monitor.lock().unwrap()
                            .values()
                            .any(|(_, p)| p == &path);
                        if already_tracked { continue; }

                        let app2      = app.clone();
                        let sessions2 = sessions.clone();
                        let monitor2  = monitor.clone();
                        std::thread::spawn(move || {
                            handle_new_session(&app2, &sessions2, &monitor2, &path);
                        });
                    }
                }
                EventKind::Modify(_) => {
                    for path in paths {
                        let already_tracked = monitor.lock().unwrap()
                            .values()
                            .any(|(_, p)| p == &path);
                        if already_tracked {
                            std::thread::sleep(std::time::Duration::from_millis(80));
                            handle_session_update(&app, &monitor, &path);
                        } else {
                            // On some platforms (Windows), new file creation arrives as Modify
                            let app2      = app.clone();
                            let sessions2 = sessions.clone();
                            let monitor2  = monitor.clone();
                            std::thread::spawn(move || {
                                handle_new_session(&app2, &sessions2, &monitor2, &path);
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

// ── Usage cache ────────────────────────────────────────────────────────────

const AUTH_WINDOW_LABEL: &str = "auth";

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

    cache.0.lock().unwrap().store(UsageResponse {
        five_hour, seven_day, seven_day_opus, seven_day_sonnet, plan,
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
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
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
fn exit_app(app: AppHandle) {
    app.exit(0);
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

fn build_shell_command() -> CommandBuilder {
    #[cfg(target_os = "windows")]
    {
        let mut cmd = CommandBuilder::new("powershell.exe");
        // -NoExit keeps the shell interactive after running the setup command.
        // The prompt override emits OSC 7 with the cwd on every prompt render.
        cmd.args([
            "-NoExit",
            "-Command",
            concat!(
                "$__arbiter_orig_prompt = $function:prompt; ",
                "function prompt { ",
                    "$loc = (Get-Location).Path; ",
                    "$uri = 'file:///' + ($loc -replace '\\\\','/'); ",
                    "$e = [char]27; $bel = [char]7; ",
                    "[Console]::Write(\"${e}]7;${uri}${bel}\"); ",
                    "& $__arbiter_orig_prompt ",
                "}"
            ),
        ]);
        cmd
    }
    #[cfg(not(target_os = "windows"))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        // Set PROMPT_COMMAND for bash; zsh users typically have precmd via their rc files.
        cmd.env(
            "PROMPT_COMMAND",
            r#"printf '\e]7;file://%s%s\a' "$(hostname)" "$(pwd)""#,
        );
        cmd
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage({
            let inner: Arc<Mutex<HashMap<String, PtySession>>> = Arc::new(Mutex::new(HashMap::new()));
            Sessions(inner)
        })
        .manage(ClaudeMonitor(Arc::new(Mutex::new(HashMap::new()))))
        .manage(Cache(Mutex::new(UsageCache::new())))
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

            // Start the event-driven Claude session watcher
            let sessions_arc = app.state::<Sessions>().0.clone();
            let monitor_arc  = app.state::<ClaudeMonitor>().0.clone();
            start_claude_watcher(app.handle().clone(), sessions_arc, monitor_arc);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_session,
            write_to_session,
            resize_session,
            close_session,
            get_session_replay,
            get_usage,
            report_usage,
            report_auth_error,
            open_login_window,
            read_claude_session,
            get_active_claude_status,
            save_config,
            load_config,
            get_session_cwd,
            exit_app,
            get_locale,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
