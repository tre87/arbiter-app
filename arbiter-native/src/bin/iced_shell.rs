//! Arbiter native — Iced shell: workspaces (tabs) of split panes.
//!
//! Each workspace is its own `pane_grid` of split terminals (core `Session`s
//! rendered by `TermGpu` in Iced's `shader` widget). A tab bar switches
//! workspaces; background workspaces keep their shells running. No webview.
//!
//! Run:  cd arbiter-native && cargo run --bin iced_shell --release

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use portable_pty::PtySize;

use iced::widget::shader::{self, wgpu};
use iced::widget::{
    button, column, container, horizontal_space, mouse_area, pane_grid, row, shader as shader_widget, text,
};
use iced::{Element, Length, Rectangle, Subscription, Task};

use arbiter_native::gpu::TermGpu;
use arbiter_native::session::{Session, SharedMaster, SharedTerm};

struct PaneData {
    session: Session,
    name: String,
}

struct Workspace {
    panes: pane_grid::State<PaneData>,
    focus: pane_grid::Pane,
    name: String,
    next_term: usize,
}

impl Workspace {
    fn new(name: String) -> Self {
        let first_pane = PaneData { session: spawn_session(), name: "Terminal 1".to_string() };
        let (panes, first) = pane_grid::State::new(first_pane);
        Workspace { panes, focus: first, name, next_term: 2 }
    }

    /// Next per-workspace terminal name ("Terminal N"); numbering restarts per
    /// workspace, matching the web.
    fn next_name(&mut self) -> String {
        let n = self.next_term;
        self.next_term += 1;
        format!("Terminal {n}")
    }
}

struct State {
    workspaces: Vec<Workspace>,
    active: usize,
    font: Arc<arbiter_native::font::FontSpec>,
    theme: iced::Theme,
}

/// Foundational dark theme matching Arbiter's palette (#121212 bg, azure accent).
/// The detailed chrome polish comes after the status/footer are functional.
fn arbiter_theme() -> iced::Theme {
    iced::Theme::custom(
        "Arbiter".to_string(),
        iced::theme::Palette {
            background: iced::Color::from_rgb8(0x12, 0x12, 0x12),
            text: iced::Color::from_rgb8(0xcc, 0xcc, 0xcc),
            primary: iced::Color::from_rgb8(0x33, 0x99, 0xff),
            success: iced::Color::from_rgb8(0x2d, 0xbd, 0x6e),
            danger: iced::Color::from_rgb8(0xe5, 0x4a, 0x4a),
        },
    )
}

impl State {
    fn active(&self) -> &Workspace { &self.workspaces[self.active] }
    fn active_mut(&mut self) -> &mut Workspace { &mut self.workspaces[self.active] }
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
    NewWorkspace,
    SelectWorkspace(usize),
}

fn spawn_session() -> Session {
    // OSC-7/OSC-133 emitters injected so the Session can track cwd + busy/idle.
    Session::spawn(80, 24, arbiter_native::shell::build_shell_command(None)).expect("spawn session")
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Tick => {}
        Message::Input(bytes) => {
            let ws = state.active_mut();
            if let Some(p) = ws.panes.get_mut(ws.focus) {
                p.session.write(&bytes);
            }
        }
        Message::Focus(pane) => state.active_mut().focus = pane,
        Message::SplitRight => split(state.active_mut(), pane_grid::Axis::Vertical),
        Message::SplitDown => split(state.active_mut(), pane_grid::Axis::Horizontal),
        Message::Close => {
            let ws = state.active_mut();
            if let Some((_, sibling)) = ws.panes.close(ws.focus) {
                ws.focus = sibling;
            }
        }
        Message::Resized(pane_grid::ResizeEvent { split, ratio }) => {
            state.active_mut().panes.resize(split, ratio);
        }
        Message::NewWorkspace => {
            let n = state.workspaces.len() + 1;
            state.workspaces.push(Workspace::new(format!("Workspace {n}")));
            state.active = state.workspaces.len() - 1;
        }
        Message::SelectWorkspace(i) => {
            if i < state.workspaces.len() {
                state.active = i;
            }
        }
    }
    Task::none()
}

fn split(ws: &mut Workspace, axis: pane_grid::Axis) {
    let name = ws.next_name();
    if let Some((new_pane, _)) = ws.panes.split(axis, ws.focus, PaneData { session: spawn_session(), name }) {
        ws.focus = new_pane;
    }
}

fn view(state: &State) -> Element<'_, Message> {
    // Top bar: workspace tabs (left) + split/close actions (right).
    let mut bar = row![].spacing(4);
    for (i, ws) in state.workspaces.iter().enumerate() {
        let mut b = button(text(ws.name.clone()).size(13)).on_press(Message::SelectWorkspace(i)).padding([4, 10]);
        if i != state.active {
            b = b.style(button::secondary);
        }
        bar = bar.push(b);
    }
    bar = bar.push(button(text("+").size(13)).on_press(Message::NewWorkspace).padding([4, 10]).style(button::secondary));
    bar = bar.push(horizontal_space());
    bar = bar.push(button(text("Split →").size(13)).on_press(Message::SplitRight).padding([4, 10]));
    bar = bar.push(button(text("Split ↓").size(13)).on_press(Message::SplitDown).padding([4, 10]));
    bar = bar.push(button(text("Close").size(13)).on_press(Message::Close).style(button::secondary).padding([4, 10]));

    let focus = state.active().focus;
    let font = &state.font;
    let grid = pane_grid::PaneGrid::new(&state.active().panes, |pane, data, _maximized| {
        let term = shader_widget(TermProgram {
            id: data.session.id(),
            term: data.session.term(),
            master: data.session.master(),
            font: font.clone(),
        })
        .width(Length::Fill)
        .height(Length::Fill);

        let focused = pane == focus;
        let content = column![pane_header(&data.name, focused), term, footer_bar(&data.session)]
            .width(Length::Fill)
            .height(Length::Fill);
        // No focus border on the pane body — focus is shown by the header title
        // colour (like the web). The pane paints its own #121212 background so
        // empty terminal cells (the renderer skips them) sit on the right colour;
        // the 2px grid gap shows the divider colour behind the grid.
        let body = mouse_area(content).on_press(Message::Focus(pane));
        let wrapped = container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(iced::Color::from_rgb8(0x12, 0x12, 0x12))),
                ..Default::default()
            });
        pane_grid::Content::new(wrapped)
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .spacing(2)
    .on_resize(8, Message::Resized)
    .style(|_t: &iced::Theme| {
        // Web divider: #2c2c2c, 2px, and no hover highlight (the web tints it
        // azure on hover; we keep it flat per the design request).
        let divider = iced::Color::from_rgb8(0x2c, 0x2c, 0x2c);
        pane_grid::Style {
            hovered_region: pane_grid::Highlight {
                background: iced::Background::Color(iced::Color::TRANSPARENT),
                border: iced::Border { color: divider, width: 0.0, radius: 0.0.into() },
            },
            picked_split: pane_grid::Line { color: divider, width: 2.0 },
            hovered_split: pane_grid::Line { color: divider, width: 2.0 },
        }
    });

    // The grid sits on the divider colour so the 2px inter-pane gaps read as
    // web-style dividers (each pane paints its own #121212 over its area).
    let grid = container(grid)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x2c, 0x2c, 0x2c))),
            ..Default::default()
        });

    column![container(bar).padding(6), grid]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Per-pane footer: folder + git branch + status counts (from the Session's
/// cwd-tracked git info). Claude model/context/tokens land here later.
fn footer_bar(session: &Session) -> Element<'static, Message> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(f) = session.folder() {
        parts.push(f);
    }
    if let Some(g) = session.git() {
        if let Some(b) = &g.branch {
            parts.push(format!("⎇ {b}"));
        }
        let mut counts = String::new();
        if g.staged > 0 {
            counts.push_str(&format!("●{} ", g.staged));
        }
        if g.unstaged > 0 {
            counts.push_str(&format!("✎{} ", g.unstaged));
        }
        if g.untracked > 0 {
            counts.push_str(&format!("+{}", g.untracked));
        }
        let counts = counts.trim();
        if !counts.is_empty() {
            parts.push(counts.to_string());
        }
    }
    container(text(parts.join("   ")).size(11))
        .width(Length::Fill)
        .padding([2, 8])
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x1b, 0x1b, 0x1b))),
            text_color: Some(iced::Color::from_rgb8(0x9c, 0x9c, 0x9c)),
            ..Default::default()
        })
        .into()
}

/// Per-pane header: a full-width bar with the centred terminal title. Focus is
/// shown by the title colour (azure when focused, grey otherwise), like the web.
/// Status (Claude/busy) will live here too in a later phase.
fn pane_header(name: &str, focused: bool) -> Element<'static, Message> {
    let color = if focused {
        iced::Color::from_rgb8(0x4d, 0xa6, 0xff)
    } else {
        iced::Color::from_rgb8(0x6b, 0x6b, 0x6b)
    };
    container(text(name.to_string()).size(12).color(color))
        .center_x(Length::Fill)
        .padding([3, 0])
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x16, 0x16, 0x16))),
            ..Default::default()
        })
        .into()
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
/// type-map shared across all shader widgets, so we key one TermGpu per session
/// (else every pane draws the last-prepared one).
#[derive(Default)]
struct Renderers(HashMap<u64, TermGpu>);

struct TermProgram {
    id: u64,
    term: SharedTerm,
    master: SharedMaster,
    font: Arc<arbiter_native::font::FontSpec>,
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
    font: Arc<arbiter_native::font::FontSpec>,
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
            .or_insert_with(|| {
                TermGpu::new(device, format, &self.font, scale)
            });
        // Rebuild when the window moves to a display with a different scale, so
        // the font px / cell size match the new DPI (else text halves/doubles).
        if (gpu.scale() - scale).abs() > 0.01 {
            *gpu = TermGpu::new(device, format, &self.font, scale);
        }

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
    let font = Arc::new(arbiter_native::font::load());

    iced::application("Arbiter native (Iced shell)", update, view)
        .subscription(subscription)
        .theme(|s: &State| s.theme.clone())
        .run_with(move || {
            let state = State {
                workspaces: vec![Workspace::new("Workspace 1".to_string())],
                active: 0,
                font: font.clone(),
                theme: arbiter_theme(),
            };
            (state, Task::none())
        })
}
