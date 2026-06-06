//! Arbiter native — Phase 0.3 / multiplexing: the Iced shell with split panes.
//!
//! Uses Iced's `pane_grid` for resizable H/V splits, each pane a live terminal
//! (a core `Session`) rendered by `TermGpu` inside Iced's `shader` widget. No
//! webview.  Toolbar: Split →, Split ↓, Close. Click a pane to focus it;
//! keystrokes go to the focused pane.
//!
//! Run:  cd arbiter-native && cargo run --bin iced_shell --release

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize};

use iced::widget::shader::{self, wgpu};
use iced::widget::{button, column, container, mouse_area, pane_grid, row, shader as shader_widget, text};
use iced::{Element, Length, Rectangle, Subscription, Task};

use arbiter_native::gpu::TermGpu;
use arbiter_native::session::{Session, SharedMaster, SharedTerm};

struct PaneData {
    session: Session,
}

struct State {
    panes: pane_grid::State<PaneData>,
    focus: pane_grid::Pane,
    font: Arc<(Vec<u8>, u32)>,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    Input(Vec<u8>),
    Focus(pane_grid::Pane),
    SplitRight,
    SplitDown,
    Close,
    Resized(pane_grid::ResizeEvent),
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

fn spawn_session() -> Session {
    Session::spawn(80, 24, shell_command()).expect("spawn session")
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Tick => {}
        Message::Input(bytes) => {
            if let Some(p) = state.panes.get_mut(state.focus) {
                p.session.write(&bytes);
            }
        }
        Message::Focus(pane) => state.focus = pane,
        Message::SplitRight => split(state, pane_grid::Axis::Vertical),
        Message::SplitDown => split(state, pane_grid::Axis::Horizontal),
        Message::Close => {
            if let Some((_, sibling)) = state.panes.close(state.focus) {
                state.focus = sibling;
            }
        }
        Message::Resized(pane_grid::ResizeEvent { split, ratio }) => {
            state.panes.resize(split, ratio);
        }
    }
    Task::none()
}

fn split(state: &mut State, axis: pane_grid::Axis) {
    if let Some((new_pane, _)) = state.panes.split(axis, state.focus, PaneData { session: spawn_session() }) {
        state.focus = new_pane;
    }
}

fn view(state: &State) -> Element<'_, Message> {
    let toolbar = row![
        button(text("Split →").size(13)).on_press(Message::SplitRight).padding([4, 10]),
        button(text("Split ↓").size(13)).on_press(Message::SplitDown).padding([4, 10]),
        button(text("Close").size(13)).on_press(Message::Close).style(button::secondary).padding([4, 10]),
    ]
    .spacing(6);

    let focus = state.focus;
    let font = &state.font;
    let grid = pane_grid::PaneGrid::new(&state.panes, |pane, data, _maximized| {
        let term = shader_widget(TermProgram {
            id: data.session.id(),
            term: data.session.term(),
            master: data.session.master(),
            font: font.clone(),
        })
        .width(Length::Fill)
        .height(Length::Fill);

        // Click anywhere in a pane to focus it.
        let body = mouse_area(term).on_press(Message::Focus(pane));
        let focused = pane == focus;
        let wrapped = container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |theme: &iced::Theme| pane_style(theme, focused));
        pane_grid::Content::new(wrapped)
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .spacing(2)
    .on_resize(8, Message::Resized);

    column![container(toolbar).padding(6), grid]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn pane_style(theme: &iced::Theme, focused: bool) -> container::Style {
    let mut s = container::Style::default();
    if focused {
        s.border = iced::Border {
            color: theme.palette().primary,
            width: 1.5,
            radius: 0.0.into(),
        };
    }
    s
}

fn subscription(_state: &State) -> Subscription<Message> {
    let tick = iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick);
    let keys = iced::event::listen_with(|event, _status, _id| handle_key(event));
    Subscription::batch([tick, keys])
}

/// Map a keyboard event to PTY bytes. Special keys are hand-mapped; printable
/// input uses the event's `text` (Shift/symbols/layout already applied).
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

/// Per-pane GPU renderers, keyed by session id. Iced's `Storage` is a global
/// type-map shared across all shader widgets, so a single TermGpu would be
/// shared by every pane (all drawing the last-prepared session). Key one
/// TermGpu per session instead.
#[derive(Default)]
struct Renderers(HashMap<u64, TermGpu>);

struct TermProgram {
    id: u64,
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
            id: self.id,
            term: self.term.clone(),
            master: self.master.clone(),
            font: self.font.clone(),
        }
    }
}

struct TermPrimitive {
    id: u64,
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
        if !storage.has::<Renderers>() {
            storage.store(Renderers::default());
        }
        let renderers = storage.get_mut::<Renderers>().unwrap();
        let gpu = renderers
            .0
            .entry(self.id)
            .or_insert_with(|| TermGpu::new(device, format, self.font.0.clone(), self.font.1, scale));

        let pw = (bounds.width * scale).max(1.0) as u32;
        let ph = (bounds.height * scale).max(1.0) as u32;
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
        let Some(renderers) = storage.get::<Renderers>() else { return };
        let Some(gpu) = renderers.0.get(&self.id) else { return };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("term-widget"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
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
            let (panes, first) = pane_grid::State::new(PaneData { session: spawn_session() });
            (State { panes, focus: first, font: font.clone() }, Task::none())
        })
}
