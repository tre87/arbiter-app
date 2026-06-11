//! Arbiter native — Phase 0.1 + 0.2 spike (see ../NATIVE_PLAN.md).
//!
//! A winit window + wgpu renderer drawing a LIVE terminal (real PTY → alacritty
//! parse → glyph-atlas/instanced-quad draw), with NO webview and NO Tauri. This
//! is the smoothness gate: drag the window, stream output (`ls -R /`, run a TUI),
//! scroll — it should be butter-smooth on Windows. Framework-agnostic (raw wgpu);
//! the GPUI/Iced shell comes later (Phase 0.3).
//!
//! Run:  cd arbiter-native && cargo run

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use arbiter_native::gpu::Renderer;
use arbiter_native::term::VtTerm;

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    term: Arc<Mutex<VtTerm>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    font: arbiter_native::font::FontSpec,
    mods: ModifiersState,
    last_grid: (usize, usize),
}

impl App {
    /// Recompute cols/rows from the window size and propagate to the grid + PTY.
    fn apply_size(&mut self) {
        let (Some(win), Some(r)) = (&self.window, &mut self.renderer) else { return };
        let sz = win.inner_size();
        r.resize(sz.width, sz.height);
        let cols = (sz.width / r.cell_w()).max(1) as usize;
        let rows = (sz.height / r.cell_h()).max(1) as usize;
        if (cols, rows) != self.last_grid {
            self.last_grid = (cols, rows);
            self.term.lock().unwrap().resize(cols, rows);
            let _ = self.master.resize(PtySize {
                rows: rows as u16,
                cols: cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("Arbiter native (spike)")
            .with_inner_size(LogicalSize::new(960.0, 640.0));
        let win = Arc::new(el.create_window(attrs).expect("create_window"));
        let scale = win.scale_factor() as f32;
        let renderer = pollster::block_on(Renderer::new(win.clone(), &self.font, scale));
        self.window = Some(win);
        self.renderer = Some(renderer);
        self.apply_size();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.apply_size(),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if let Some(bytes) = key_bytes(&event, self.mods.control_key()) {
                        let _ = self.writer.write_all(&bytes);
                        let _ = self.writer.flush();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(r) = &mut self.renderer {
                    let t = self.term.lock().unwrap();
                    r.render(&t);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        // Continuous redraw (capped to vsync by Fifo present mode) — the spike
        // tests worst-case smoothness while streaming/dragging/scrolling.
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

/// Translate a key press to terminal bytes (minimal set for the spike).
fn key_bytes(ev: &KeyEvent, ctrl: bool) -> Option<Vec<u8>> {
    match &ev.logical_key {
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Space) => Some(b" ".to_vec()),
        Key::Character(s) => {
            if ctrl {
                if let Some(c) = s.chars().next() {
                    let lc = c.to_ascii_lowercase();
                    if lc.is_ascii_alphabetic() {
                        return Some(vec![(lc as u8) - b'a' + 1]); // Ctrl-A = 0x01
                    }
                }
            }
            Some(s.as_bytes().to_vec())
        }
        _ => ev.text.as_ref().map(|t| t.as_bytes().to_vec()),
    }
}

fn shell_command() -> CommandBuilder {
    if cfg!(windows) {
        CommandBuilder::new("powershell.exe")
    } else {
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut c = CommandBuilder::new(sh);
        c.arg("-l");
        c.env("TERM", "xterm-256color");
        c
    }
}

fn main() {
    let font = arbiter_native::font::load();

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .expect("openpty");
    let child = pair.slave.spawn_command(shell_command()).expect("spawn shell");
    drop(pair.slave); // child keeps its own handle; we read/write the master
    let writer = pair.master.take_writer().expect("pty writer");
    let mut reader = pair.master.try_clone_reader().expect("pty reader");

    let term = Arc::new(Mutex::new(VtTerm::new(80, 24)));
    {
        let term = term.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => term.lock().unwrap().feed(&buf[..n]),
                }
            }
        });
    }

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        window: None,
        renderer: None,
        term,
        writer,
        master: pair.master,
        _child: child,
        font,
        mods: ModifiersState::empty(),
        last_grid: (0, 0),
    };
    event_loop.run_app(&mut app).expect("run app");
}
