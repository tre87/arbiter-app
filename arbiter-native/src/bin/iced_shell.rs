//! Arbiter native — Phase 0.3: the Iced shell hosting the wgpu terminal.
//!
//! An Iced app with a tab bar + one live terminal per tab, each rendered by the
//! `TermGpu` renderer inside Iced's custom `shader` widget. No webview. Proves
//! the chrome framework (Iced) and the GPU terminal compose cleanly.
//!
//! Run:  cd arbiter-native && cargo run --bin iced_shell --release

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use iced::widget::shader::{self, wgpu};
use iced::widget::{button, column, container, row, shader as shader_widget, text, Space};
use iced::{Element, Length, Rectangle, Subscription, Task};

use arbiter_native::gpu::TermGpu;
use arbiter_native::term::VtTerm;

type SharedTerm = Arc<Mutex<VtTerm>>;
type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;
type SharedMaster = Arc<Mutex<Box<dyn MasterPty + Send>>>;

struct Tab {
    term: SharedTerm,
    writer: SharedWriter,
    master: SharedMaster,
    title: String,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

struct State {
    tabs: Vec<Tab>,
    active: usize,
    font: Arc<(Vec<u8>, u32)>,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    Input(Vec<u8>),
    NewTab,
    SelectTab(usize),
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

fn spawn_tab(font: &Arc<(Vec<u8>, u32)>, n: usize) -> Tab {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: 30, cols: 100, pixel_width: 0, pixel_height: 0 })
        .expect("openpty");
    let child = pair.slave.spawn_command(shell_command()).expect("spawn shell");
    drop(pair.slave);
    let writer = pair.master.take_writer().expect("writer");
    let mut reader = pair.master.try_clone_reader().expect("reader");

    let term: SharedTerm = Arc::new(Mutex::new(VtTerm::new(100, 30)));
    {
        let term = term.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => term.lock().unwrap().feed(&buf[..k]),
                }
            }
        });
    }
    let _ = font;
    Tab {
        term,
        writer: Arc::new(Mutex::new(writer)),
        master: Arc::new(Mutex::new(pair.master)),
        title: format!("Terminal {n}"),
        _child: child,
    }
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Tick => {}
        Message::Input(bytes) => {
            if let Some(tab) = state.tabs.get(state.active) {
                if let Ok(mut w) = tab.writer.lock() {
                    let _ = w.write_all(&bytes);
                    let _ = w.flush();
                }
            }
        }
        Message::NewTab => {
            let n = state.tabs.len() + 1;
            let tab = spawn_tab(&state.font, n);
            state.tabs.push(tab);
            state.active = state.tabs.len() - 1;
        }
        Message::SelectTab(i) => {
            if i < state.tabs.len() {
                state.active = i;
            }
        }
    }
    Task::none()
}

fn view(state: &State) -> Element<'_, Message> {
    // Tab bar.
    let mut tabs = row![].spacing(4);
    for (i, t) in state.tabs.iter().enumerate() {
        let label = text(t.title.clone()).size(13);
        let mut b = button(label).on_press(Message::SelectTab(i)).padding([4, 10]);
        if i != state.active {
            b = b.style(button::secondary);
        }
        tabs = tabs.push(b);
    }
    tabs = tabs.push(button(text("+").size(13)).on_press(Message::NewTab).padding([4, 10]));

    // Active terminal via the custom shader widget.
    let active = &state.tabs[state.active];
    let term_widget = shader_widget(TermProgram {
        term: active.term.clone(),
        master: active.master.clone(),
        font: state.font.clone(),
    })
    .width(Length::Fill)
    .height(Length::Fill);

    let chrome = container(tabs).padding(6).width(Length::Fill);
    column![chrome, term_widget, Space::new(Length::Fill, Length::Fixed(0.0))]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn subscription(_state: &State) -> Subscription<Message> {
    let tick = iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick);
    let keys = iced::event::listen_with(|event, _status, _id| handle_key(event));
    Subscription::batch([tick, keys])
}

/// Map a keyboard event to PTY bytes. Special keys are hand-mapped; for
/// printable input we use the event's `text` field, which already reflects
/// Shift / symbols / keyboard layout (the base `key` is NOT modifier-applied,
/// which is why holding Shift didn't capitalise).
fn handle_key(event: iced::Event) -> Option<Message> {
    use iced::keyboard::{key::Named, Event::KeyPressed, Key};
    let iced::Event::Keyboard(KeyPressed { key, text, modifiers, .. }) = event else {
        return None;
    };
    match &key {
        Key::Named(Named::Enter) => return Some(Message::Input(b"\r".to_vec())),
        Key::Named(Named::Backspace) => return Some(Message::Input(vec![0x7f])),
        Key::Named(Named::Tab) => return Some(Message::Input(b"\t".to_vec())),
        Key::Named(Named::Escape) => return Some(Message::Input(vec![0x1b])),
        Key::Named(Named::ArrowUp) => return Some(Message::Input(b"\x1b[A".to_vec())),
        Key::Named(Named::ArrowDown) => return Some(Message::Input(b"\x1b[B".to_vec())),
        Key::Named(Named::ArrowRight) => return Some(Message::Input(b"\x1b[C".to_vec())),
        Key::Named(Named::ArrowLeft) => return Some(Message::Input(b"\x1b[D".to_vec())),
        // Ctrl+letter → control byte (use the base, un-shifted character).
        Key::Character(s) if modifiers.control() => {
            if let Some(c) = s.chars().next() {
                let lc = c.to_ascii_lowercase();
                if lc.is_ascii_alphabetic() {
                    return Some(Message::Input(vec![(lc as u8) - b'a' + 1]));
                }
            }
        }
        _ => {}
    }
    // Printable text — Shift/symbols/layout already applied. Skip when a
    // meaning-changing modifier (Ctrl/Alt/Cmd) is held; Shift is fine.
    if !modifiers.control() && !modifiers.alt() && !modifiers.logo() {
        if let Some(t) = text {
            if !t.is_empty() {
                return Some(Message::Input(t.as_bytes().to_vec()));
            }
        }
    }
    None
}

// ── Custom shader widget: the wgpu terminal hosted inside Iced ────────────────

struct TermProgram {
    term: SharedTerm,
    master: SharedMaster,
    font: Arc<(Vec<u8>, u32)>,
}

impl std::fmt::Debug for TermProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TermProgram")
    }
}

impl shader::Program<Message> for TermProgram {
    type State = ();
    type Primitive = TermPrimitive;

    fn draw(&self, _state: &Self::State, _cursor: iced::mouse::Cursor, _bounds: Rectangle) -> Self::Primitive {
        TermPrimitive {
            term: self.term.clone(),
            master: self.master.clone(),
            font: self.font.clone(),
        }
    }
}

struct TermPrimitive {
    term: SharedTerm,
    master: SharedMaster,
    font: Arc<(Vec<u8>, u32)>,
}

impl std::fmt::Debug for TermPrimitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TermPrimitive")
    }
}

impl shader::Primitive for TermPrimitive {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        storage: &mut shader::Storage,
        bounds: &Rectangle,
        viewport: &shader::Viewport,
    ) {
        let scale = viewport.scale_factor() as f32;
        if !storage.has::<TermGpu>() {
            storage.store(TermGpu::new(device, format, self.font.0.clone(), self.font.1, scale));
        }
        let gpu = storage.get_mut::<TermGpu>().unwrap();

        // Physical draw area for this widget.
        let pw = (bounds.width * scale).max(1.0) as u32;
        let ph = (bounds.height * scale).max(1.0) as u32;

        // Resize the grid + PTY to fit the widget (self-contained — the widget
        // knows its real size, so the app doesn't need scale/resize plumbing).
        let cols = (pw / gpu.cell_w).max(1) as usize;
        let rows = (ph / gpu.cell_h).max(1) as usize;
        {
            let mut t = self.term.lock().unwrap();
            if t.size() != (cols, rows) {
                t.resize(cols, rows);
                if let Ok(m) = self.master.lock() {
                    let _ = m.resize(PtySize {
                        rows: rows as u16,
                        cols: cols as u16,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
            }
            gpu.prepare(device, queue, &t, pw, ph);
        }
    }

    fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        storage: &shader::Storage,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        let Some(gpu) = storage.get::<TermGpu>() else { return };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("term-widget"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    // Load: composite over what Iced already drew (the chrome).
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_viewport(
            clip_bounds.x as f32,
            clip_bounds.y as f32,
            clip_bounds.width as f32,
            clip_bounds.height as f32,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(clip_bounds.x, clip_bounds.y, clip_bounds.width, clip_bounds.height);
        gpu.draw(&mut pass);
    }
}

fn main() -> iced::Result {
    let font = {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let q = fontdb::Query { families: &[fontdb::Family::Monospace], ..Default::default() };
        let id = db.query(&q).expect("no monospace font");
        let (bytes, index) = db.with_face_data(id, |d, i| (d.to_vec(), i)).expect("face data");
        Arc::new((bytes, index))
    };

    iced::application("Arbiter native (Iced shell)", update, view)
        .subscription(subscription)
        .run_with(move || {
            let mut state = State { tabs: Vec::new(), active: 0, font: font.clone() };
            state.tabs.push(spawn_tab(&state.font, 1));
            (state, Task::none())
        })
}
