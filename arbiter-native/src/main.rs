//! Arbiter native — Phase 0.0 spike (see ../NATIVE_PLAN.md).
//!
//! Goal of THIS step: prove the perf-critical backend core — PTY + the
//! `alacritty_terminal` VT parser/cell-grid — runs natively, standalone, and
//! cross-platform, with NO webview and NO Tauri. It spawns a one-shot shell
//! command through a real PTY, feeds the output into a headless terminal, and
//! prints the rendered grid. If this runs on Windows, the foundation the native
//! app stands on is sound; Phase 0.1 adds the wgpu window.
//!
//! Run:  cd arbiter-native && cargo run

use std::io::Read;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// alacritty's `Term` requires an event listener; headless parsing needs none.
#[derive(Clone, Copy, Default)]
struct NoopListener;
impl EventListener for NoopListener {}

/// Fixed grid size for the spike.
#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize { self.rows }
    fn screen_lines(&self) -> usize { self.rows }
    fn columns(&self) -> usize { self.cols }
}

fn main() {
    let (cols, rows) = (80usize, 24usize);

    // 1) Open a real PTY (ConPTY on Windows, openpty elsewhere) — same crate the
    //    shipping app uses.
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: rows as u16, cols: cols as u16, pixel_width: 0, pixel_height: 0 })
        .expect("openpty");

    // 2) Spawn a one-shot shell command that prints a marker + some system info,
    //    then exits (so the reader hits EOF and we can dump the final screen).
    let cmd = if cfg!(windows) {
        let mut c = CommandBuilder::new("cmd.exe");
        c.args(["/c", "echo Arbiter native: PTY + alacritty_terminal OK && ver"]);
        c
    } else {
        let mut c = CommandBuilder::new("/bin/sh");
        c.args(["-c", "echo 'Arbiter native: PTY + alacritty_terminal OK'; uname -srm"]);
        c
    };
    let mut child = pair.slave.spawn_command(cmd).expect("spawn shell command");
    // Drop the slave so the master read hits EOF once the child exits.
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone reader");

    // 3) Headless terminal: feed raw PTY bytes into the VT parser + cell grid.
    let size = Size { cols, rows };
    let mut config = Config::default();
    config.scrolling_history = 5000;
    let mut term: Term<NoopListener> = Term::new(config, &size, NoopListener);
    let mut parser: Processor = Processor::new();

    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,            // EOF
            Ok(n) => parser.advance(&mut term, &buf[..n]),
            Err(_) => break,
        }
    }
    let _ = child.wait();

    // 4) Dump the rendered grid — proof the parse produced a real screen.
    println!("--- rendered grid ({cols}x{rows}) ---");
    let grid = term.grid();
    for line in 0..rows as i32 {
        let row = &grid[Line(line)];
        let mut s = String::with_capacity(cols);
        for c in 0..cols {
            let ch = row[Column(c)].c;
            s.push(if ch == '\0' { ' ' } else { ch });
        }
        let trimmed = s.trim_end();
        if !trimmed.is_empty() {
            println!("{line:>3} | {trimmed}");
        }
    }
    println!("--- native PTY + parse pipeline works (no webview, no Tauri) ---");
}
