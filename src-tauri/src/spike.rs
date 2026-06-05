// SPIKE — Rust-side VT parsing + binary cell-diff transport.
//
// The decisive test for the "native-class terminal perf without a native UI
// rewrite" direction (see memory: terminal-renderer-architecture):
//   1. Parse PTY bytes in Rust with `alacritty_terminal` (off the webview's
//      single JS main thread — the real ceiling the earlier single-canvas
//      spike exposed), one parser per pane on its own reader thread so parsing
//      parallelises across cores.
//   2. Each frame (~120 Hz) collect per-pane DAMAGE (only changed lines),
//      pack a compact binary blob for ALL dirty panes, and ship it over a
//      Tauri `Channel` as raw bytes (`InvokeResponseBody::Raw` — no JSON, no
//      base64; events are the documented anti-pattern at this rate).
//   3. The webview decodes the diff into per-pane cell grids and draws them
//      all into ONE WebGL2 canvas (one compositing layer).
//
// Throwaway/measurement code. Glyph + fg/bg + cursor + inverse/hidden only.
//
// ── Wire format (little-endian), one blob per frame ──────────────────────────
//   u8   version (=1)
//   u16  paneCount
//   repeat paneCount:
//     u16  paneIdx
//     u16  cols
//     u16  rows
//     u16  cursorRow
//     u16  cursorCol
//     u8   cursorVisible (0/1)
//     u16  dirtyLineCount
//     repeat dirtyLineCount:
//       u16  row
//       u16  left           (inclusive)
//       u16  right          (inclusive)
//       repeat (right-left+1) cells:
//         u32  codepoint
//         u8   fgR, fgG, fgB
//         u8   bgR, bgG, bgB
//         u8   flags  (bit0 INVERSE, bit1 BOLD, bit2 ITALIC, bit3 UNDERLINE,
//                      bit4 HIDDEN, bit5 WIDE, bit6 WIDE_SPACER)

use portable_pty::{NativePtySystem, PtySize, PtySystem};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, State};

use crate::shell::build_shell_command;
use crate::termgrid::HeadlessTerm;

// ── Runtime ──────────────────────────────────────────────────────────────────

struct SpikePane {
    term: Arc<Mutex<HeadlessTerm>>,
    dirty: Arc<AtomicBool>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn portable_pty::MasterPty + Send>,
}

struct SpikeRuntime {
    panes: Vec<SpikePane>,
    stop: Arc<AtomicBool>,
}

pub struct SpikeState(Arc<Mutex<Option<SpikeRuntime>>>);
impl SpikeState {
    pub fn new() -> Self {
        SpikeState(Arc::new(Mutex::new(None)))
    }
}

fn teardown(rt: SpikeRuntime) {
    // Stop the frame thread, then drop masters so reader threads hit EOF.
    rt.stop.store(true, Ordering::Relaxed);
    drop(rt.panes); // drops masters → readers EOF and exit
}

#[tauri::command]
pub fn spike_start(
    app: AppHandle,
    state: State<SpikeState>,
    channel: Channel<InvokeResponseBody>,
    count: u16,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
) -> Result<(), String> {
    // Replace any existing run.
    if let Some(old) = state.0.lock().unwrap().take() {
        teardown(old);
    }

    let pty_system = NativePtySystem::default();
    let stop = Arc::new(AtomicBool::new(false));
    let mut panes: Vec<SpikePane> = Vec::with_capacity(count as usize);
    // (term, dirty) clones the frame thread reads from.
    let mut frame_panes: Vec<(Arc<Mutex<HeadlessTerm>>, Arc<AtomicBool>)> = Vec::with_capacity(count as usize);

    for _ in 0..count {
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;

        let mut cmd = build_shell_command(&app, None);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(ref dir) = cwd {
            let p = std::path::Path::new(dir);
            if p.is_dir() {
                cmd.cwd(p);
            }
        }
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        drop(child);

        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer().map_err(|e| e.to_string())?));

        let term = Arc::new(Mutex::new(HeadlessTerm::new(cols as usize, rows as usize)));
        let dirty = Arc::new(AtomicBool::new(true)); // force first frame

        // Reader thread: parse PTY bytes straight into this pane's Term.
        let term_reader = term.clone();
        let dirty_reader = dirty.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 65_536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut t) = term_reader.lock() {
                            t.feed(&buf[..n]);
                        }
                        dirty_reader.store(true, Ordering::Relaxed);
                    }
                }
            }
        });

        frame_panes.push((term.clone(), dirty.clone()));
        panes.push(SpikePane { term, dirty, writer, master: pair.master });
    }

    // Frame thread: ~120 Hz, pack all dirty panes' diffs into one blob and send.
    let stop_frame = stop.clone();
    std::thread::spawn(move || {
        let mut first = true;
        loop {
            if stop_frame.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(8));
            let mut body: Vec<u8> = Vec::with_capacity(4096);
            let mut pane_count: u16 = 0;
            for (idx, (term_arc, dirty)) in frame_panes.iter().enumerate() {
                let was_dirty = dirty.swap(false, Ordering::Relaxed);
                if !was_dirty && !first {
                    continue;
                }
                if let Ok(mut t) = term_arc.lock() {
                    if t.pack_into(idx as u16, &mut body, first) {
                        pane_count += 1;
                    }
                }
            }
            first = false;
            if pane_count == 0 {
                continue;
            }
            let mut frame: Vec<u8> = Vec::with_capacity(body.len() + 3);
            frame.push(1u8); // version
            frame.extend_from_slice(&pane_count.to_le_bytes());
            frame.extend_from_slice(&body);
            if channel.send(InvokeResponseBody::Raw(frame)).is_err() {
                break; // frontend went away
            }
        }
    });

    *state.0.lock().unwrap() = Some(SpikeRuntime { panes, stop });
    Ok(())
}

#[tauri::command]
pub fn spike_stop(state: State<SpikeState>) {
    if let Some(rt) = state.0.lock().unwrap().take() {
        teardown(rt);
    }
}

#[tauri::command]
pub fn spike_write(state: State<SpikeState>, idx: usize, data: String) {
    let guard = state.0.lock().unwrap();
    if let Some(rt) = guard.as_ref() {
        if let Some(pane) = rt.panes.get(idx) {
            if let Ok(mut w) = pane.writer.lock() {
                let _ = w.write_all(data.as_bytes());
            }
        }
    }
}

#[tauri::command]
pub fn spike_resize(state: State<SpikeState>, cols: u16, rows: u16) {
    let guard = state.0.lock().unwrap();
    if let Some(rt) = guard.as_ref() {
        for pane in &rt.panes {
            let _ = pane.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
            if let Ok(mut t) = pane.term.lock() {
                t.resize(cols as usize, rows as usize);
            }
            pane.dirty.store(true, Ordering::Relaxed);
        }
    }
}

/// Flood every pane with continuous colored output to stress parse+transport+draw.
#[tauri::command]
pub fn spike_stress(state: State<SpikeState>) {
    // zsh/bash loop; works on the macOS dev target. Emits a BURST of colored
    // lines then `sleep`s — a bounded high output rate that yields CPU between
    // bursts, instead of an infinite `while :;` spin that pegs every core and
    // starves the parse/transport threads (the real bottleneck is then the
    // shells, not the renderer). ~20 bursts/s × 25 lines = ~500 lines/s/pane.
    const CMD: &str = "while :; do for i in $(seq 1 25); do printf '\\033[38;5;%dm%s\\033[0m\\n' $((RANDOM%256)) \"arbiter spike $RANDOM lorem ipsum dolor sit amet consectetur adipiscing\"; done; sleep 0.05; done\r";
    let guard = state.0.lock().unwrap();
    if let Some(rt) = guard.as_ref() {
        for pane in &rt.panes {
            if let Ok(mut w) = pane.writer.lock() {
                let _ = w.write_all(CMD.as_bytes());
            }
        }
    }
}

/// Ctrl-C every pane to stop the stress loop.
#[tauri::command]
pub fn spike_stress_stop(state: State<SpikeState>) {
    let guard = state.0.lock().unwrap();
    if let Some(rt) = guard.as_ref() {
        for pane in &rt.panes {
            if let Ok(mut w) = pane.writer.lock() {
                let _ = w.write_all(b"\x03");
            }
        }
    }
}

