//! Core terminal session — the seed of the Tauri-free backend the native app
//! drives directly (no IPC, no events bus). A PTY + headless VT term + a reader
//! thread that feeds the grid and parses OSC-7 (cwd) / OSC-133 (shell busy/idle)
//! / BEL into shared state. Ported from `src-tauri/src/pty.rs`, minus the
//! webview/xterm streaming, flow control and Claude monitoring (those follow as
//! features land). cwd/shell-idle are tracked here and read by the UI; later
//! they'll drive the footer + status, and `core` grows claude/git/shim.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::term::VtTerm;

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
    _child: Box<dyn Child + Send + Sync>,
}

impl Session {
    pub fn spawn(cols: u16, rows: u16, cmd: CommandBuilder) -> std::io::Result<Self> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(io_err)?;
        let child = pair.slave.spawn_command(cmd).map_err(io_err)?;
        drop(pair.slave); // child keeps its own handle
        let writer = pair.master.take_writer().map_err(io_err)?;
        let reader = pair.master.try_clone_reader().map_err(io_err)?;

        let term: SharedTerm = Arc::new(Mutex::new(VtTerm::new(cols as usize, rows as usize)));
        let cwd = Arc::new(Mutex::new(None));
        let shell_idle = Arc::new(Mutex::new(None));
        {
            let term = term.clone();
            let cwd = cwd.clone();
            let shell_idle = shell_idle.clone();
            std::thread::spawn(move || reader_loop(reader, term, cwd, shell_idle));
        }

        Ok(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            writer,
            master: Arc::new(Mutex::new(pair.master)),
            term,
            cwd,
            shell_idle,
            _child: child,
        })
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

fn reader_loop(
    mut reader: Box<dyn Read + Send>,
    term: SharedTerm,
    cwd: Arc<Mutex<Option<String>>>,
    shell_idle: Arc<Mutex<Option<bool>>>,
) {
    let mut buf = [0u8; 8192];
    let mut remainder: Vec<u8> = Vec::new();
    let mut osc = String::new();
    let mut in_osc = false;
    let mut prev_cwd: Option<String> = None;

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
                        }
                    }
                    if let Some(path) = parse_osc7_uri(payload) {
                        if prev_cwd.as_ref() != Some(&path) {
                            prev_cwd = Some(path.clone());
                        }
                        *cwd.lock().unwrap() = Some(path);
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
