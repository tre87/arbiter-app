//! Arbiter native — Iced shell: workspaces (tabs) of split panes.
//!
//! Each workspace is its own `pane_grid` of split terminals (core `Session`s
//! rendered by `TermGpu` in Iced's `shader` widget). A tab bar switches
//! workspaces; background workspaces keep their shells running. No webview.
//!
//! Run:  cd arbiter-native && cargo run --bin iced_shell --release

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use portable_pty::PtySize;

use iced::widget::shader::{self, wgpu};
use iced::widget::{
    button, column, container, horizontal_space, mouse_area, pane_grid, row, scrollable,
    shader as shader_widget, svg, text, Space,
};
use iced::{Element, Length, Rectangle, Subscription, Task};

use arbiter_native::claude_status::Lifecycle;
use arbiter_native::gpu::TermGpu;
use arbiter_native::session::{Session, SharedMaster, SharedTerm};
use arbiter_native::persist;
use arbiter_native::term::SelectKind;

/// Which shell a terminal is running. Windows can switch PowerShell ↔ Git Bash;
/// other platforms only ever use the default (so the switch button never shows).
#[derive(Clone, Copy, PartialEq)]
enum ShellKind {
    PowerShell,
    GitBash,
}

struct PaneData {
    session: Session,
    name: String,
    shell: ShellKind,
}

struct Workspace {
    panes: pane_grid::State<PaneData>,
    focus: pane_grid::Pane,
    name: String,
    next_term: usize,
}

impl Workspace {
    fn new(name: String) -> Self {
        let first_pane = PaneData {
            session: spawn_session(None, None),
            name: "Terminal 1".to_string(),
            shell: ShellKind::PowerShell,
        };
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
    git_bash: Option<String>,
    theme: iced::Theme,
    /// The main terminal window; the app exits when it closes.
    main_window: iced::window::Id,
    /// The popout overview window, while open.
    overview_window: Option<iced::window::Id>,
    /// Live geometry of each window (tracked from move/resize events) so it can be
    /// persisted and restored. Positions are `None` until the WM reports one.
    main_size: iced::Size,
    main_pos: Option<iced::Point>,
    overview_size: iced::Size,
    overview_pos: Option<iced::Point>,
}

/// The main window id, for routing keyboard input (so typing in the overview
/// window doesn't reach the terminal). Set once at startup.
static MAIN_WINDOW: std::sync::OnceLock<iced::window::Id> = std::sync::OnceLock::new();

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
    SwitchShell(pane_grid::Pane),
    ShiftEnter,
    /// Copy selection to clipboard; bool = fall back to interrupt (^C) if there's
    /// no selection (plain Ctrl+C).
    Copy(bool),
    Paste,
    Pasted(Option<String>),
    /// Toggle the popout overview window.
    ToggleOverview,
    /// Jump to a pane from the overview (select its workspace + focus it).
    JumpTo(usize, pane_grid::Pane),
    /// A window was closed (main → exit; overview → forget it).
    WindowClosed(iced::window::Id),
    /// A window was moved/resized — track its geometry for persistence.
    WindowMoved(iced::window::Id, iced::Point),
    WindowResized(iced::window::Id, iced::Size),
    /// No-op (used to discard a window-open Task's result).
    Noop,
}

/// Spawn a session running `shell` (None = the platform default / PowerShell;
/// Some(path) = Git Bash) starting in `cwd` if given. OSC-7/OSC-133 emitters are
/// injected so the Session tracks cwd + busy/idle.
fn spawn_session(shell: Option<&str>, cwd: Option<&str>) -> Session {
    let mut cmd = arbiter_native::shell::build_shell_command(shell);
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    }
    Session::spawn(80, 24, cmd).expect("spawn session")
}

// ── Session persistence ──────────────────────────────────────────────────────
// Autosave/restore the workspace layout (web autosave parity). The live PTYs
// can't be restored, so each saved leaf is respawned in its saved cwd/shell.

/// Spawn a session for a restored leaf, honouring its saved shell + cwd. Falls
/// back to the default shell/dir if Git Bash is unavailable or the cwd is gone,
/// so a stale save can never panic the launch.
fn spawn_restored(
    shell: persist::SavedShell,
    cwd: Option<&str>,
    git_bash: Option<&str>,
    claude_running: bool,
    claude_session: Option<&str>,
) -> (Session, ShellKind) {
    let cwd = cwd.filter(|d| std::path::Path::new(d).is_dir());
    let (mut session, kind) = match shell {
        persist::SavedShell::GitBash => match git_bash {
            Some(gb) => (spawn_session(Some(gb), cwd), ShellKind::GitBash),
            None => (spawn_session(None, cwd), ShellKind::PowerShell),
        },
        persist::SavedShell::PowerShell => (spawn_session(None, cwd), ShellKind::PowerShell),
    };
    if claude_running {
        // Relaunch Claude here — resuming the previous conversation if one was bound,
        // else a fresh session. The command queues in the PTY and runs at the shell's
        // first prompt (after rc sets the shim PATH), so it goes through our launcher
        // and rebinds statusline/hooks.
        let cmd = match claude_session {
            Some(sid) => format!("claude --resume {sid}\r"),
            None => "claude\r".to_string(),
        };
        session.write(cmd.as_bytes());
    }
    (session, kind)
}

/// Build a workspace's `pane_grid` from its saved split tree, respawning each leaf.
fn saved_to_config(
    node: persist::SavedNode,
    git_bash: Option<&str>,
) -> pane_grid::Configuration<PaneData> {
    match node {
        persist::SavedNode::Split { vertical, ratio, a, b } => pane_grid::Configuration::Split {
            axis: if vertical { pane_grid::Axis::Vertical } else { pane_grid::Axis::Horizontal },
            ratio,
            a: Box::new(saved_to_config(*a, git_bash)),
            b: Box::new(saved_to_config(*b, git_bash)),
        },
        persist::SavedNode::Leaf { name, shell, cwd, claude_running, claude_session } => {
            let (session, kind) = spawn_restored(
                shell,
                cwd.as_deref(),
                git_bash,
                claude_running,
                claude_session.as_deref(),
            );
            pane_grid::Configuration::Pane(PaneData { session, name, shell: kind })
        }
    }
}

/// Rebuild workspaces from a saved session. `None` if nothing usable (→ fresh start).
fn restore_workspaces(
    saved: persist::SavedState,
    git_bash: Option<&str>,
) -> Option<(Vec<Workspace>, usize)> {
    let mut workspaces = Vec::new();
    for sw in saved.workspaces {
        let panes = pane_grid::State::with_configuration(saved_to_config(sw.layout, git_bash));
        let Some(focus) = panes.iter().next().map(|(p, _)| *p) else { continue };
        workspaces.push(Workspace { panes, focus, name: sw.name, next_term: sw.next_term });
    }
    if workspaces.is_empty() {
        return None;
    }
    let active = saved.active.min(workspaces.len() - 1);
    Some((workspaces, active))
}

/// Snapshot one workspace's live split tree into the serialisable form.
fn node_to_saved(ws: &Workspace, node: &pane_grid::Node) -> persist::SavedNode {
    match node {
        pane_grid::Node::Split { axis, ratio, a, b, .. } => persist::SavedNode::Split {
            vertical: matches!(axis, pane_grid::Axis::Vertical),
            ratio: *ratio,
            a: Box::new(node_to_saved(ws, a)),
            b: Box::new(node_to_saved(ws, b)),
        },
        pane_grid::Node::Pane(pane) => {
            let data = ws.panes.get(*pane);
            persist::SavedNode::Leaf {
                name: data.map(|d| d.name.clone()).unwrap_or_default(),
                shell: match data.map(|d| d.shell) {
                    Some(ShellKind::GitBash) => persist::SavedShell::GitBash,
                    _ => persist::SavedShell::PowerShell,
                },
                cwd: data.and_then(|d| d.session.cwd()),
                claude_running: data.map(|d| d.session.claude_running()).unwrap_or(false),
                claude_session: data.and_then(|d| d.session.claude_session_id()),
            }
        }
    }
}

/// Window settings for the overview popout at a (saved) size + optional position.
fn overview_settings(size: iced::Size, pos: Option<iced::Point>) -> iced::window::Settings {
    let mut settings = iced::window::Settings { size, ..Default::default() };
    if let Some(p) = pos {
        settings.position = iced::window::Position::Specific(p);
    }
    settings
}

/// Open the overview popout at its saved geometry. Opens at the saved size +
/// position, then issues an explicit `move_to`: the at-creation position is
/// ignored by winit/macOS for off-primary (e.g. negative/second-display) coords,
/// but a post-open `set_outer_position` places it there reliably.
fn open_overview(size: iced::Size, pos: Option<iced::Point>) -> (iced::window::Id, Task<Message>) {
    let (id, open) = iced::window::open(overview_settings(size, pos));
    let mut task = open.map(|_| Message::Noop);
    if let Some(p) = pos {
        task = Task::batch([task, iced::window::move_to(id, p)]);
    }
    (id, task)
}

/// Build a `SavedWindow` from a tracked size + optional position.
fn saved_window(size: iced::Size, pos: Option<iced::Point>) -> persist::SavedWindow {
    persist::SavedWindow {
        width: size.width,
        height: size.height,
        x: pos.map(|p| p.x),
        y: pos.map(|p| p.y),
    }
}

/// Persist the current layout + window geometry (after layout-changing actions +
/// on exit).
fn save_session(state: &State) {
    persist::save(&persist::SavedState {
        active: state.active,
        main_window: Some(saved_window(state.main_size, state.main_pos)),
        overview_window: Some(saved_window(state.overview_size, state.overview_pos)),
        overview_visible: state.overview_window.is_some(),
        workspaces: state
            .workspaces
            .iter()
            .map(|ws| persist::SavedWorkspace {
                name: ws.name.clone(),
                next_term: ws.next_term,
                layout: node_to_saved(ws, ws.panes.layout()),
            })
            .collect(),
    });
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Tick => {
            // Persist when a Claude session newly bound in a pane (the watcher sets
            // this on an FS event; the tick just reacts to the flag — a cheap atomic
            // read, and the save only runs on the rare bind, not every frame).
            if arbiter_native::claude_status::SAVE_DIRTY
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                save_session(state);
            }
        }
        Message::Input(bytes) => {
            let ws = state.active_mut();
            if let Some(p) = ws.panes.get_mut(ws.focus) {
                // Typing returns the view to the live bottom + clears selection.
                if let Ok(mut t) = p.session.term().lock() {
                    t.scroll_to_bottom();
                    t.clear_selection();
                }
                p.session.write(&bytes);
            }
        }
        Message::ShiftEnter => {
            // Claude (Ink) wants the kitty Shift+Enter sequence to insert a
            // newline; a plain shell would echo those bytes as garbage, so send
            // a normal CR there instead.
            let ws = state.active_mut();
            if let Some(p) = ws.panes.get_mut(ws.focus) {
                if let Ok(mut t) = p.session.term().lock() {
                    t.scroll_to_bottom();
                }
                let bytes: &[u8] = if p.session.claude_running() { b"\x1b[13;2u" } else { b"\r" };
                p.session.write(bytes);
            }
        }
        Message::Focus(pane) => state.active_mut().focus = pane,
        Message::SplitRight => {
            split(state.active_mut(), pane_grid::Axis::Vertical);
            save_session(state);
        }
        Message::SplitDown => {
            split(state.active_mut(), pane_grid::Axis::Horizontal);
            save_session(state);
        }
        Message::Close => {
            let ws = state.active_mut();
            if let Some((_, sibling)) = ws.panes.close(ws.focus) {
                ws.focus = sibling;
            }
            save_session(state);
        }
        Message::Resized(pane_grid::ResizeEvent { split, ratio }) => {
            state.active_mut().panes.resize(split, ratio);
        }
        Message::NewWorkspace => {
            let n = state.workspaces.len() + 1;
            state.workspaces.push(Workspace::new(format!("Workspace {n}")));
            state.active = state.workspaces.len() - 1;
            save_session(state);
        }
        Message::SelectWorkspace(i) => {
            if i < state.workspaces.len() {
                state.active = i;
                save_session(state);
            }
        }
        Message::Noop => {}
        Message::ToggleOverview => {
            if let Some(id) = state.overview_window.take() {
                save_session(state); // persist "overview closed"
                return iced::window::close(id);
            }
            let (id, task) = open_overview(state.overview_size, state.overview_pos);
            state.overview_window = Some(id);
            save_session(state); // persist "overview open"
            return task;
        }
        Message::WindowClosed(id) => {
            if id == state.main_window {
                // Capture the final layout (incl. each terminal's current cwd) on exit.
                save_session(state);
                return iced::exit();
            }
            if state.overview_window == Some(id) {
                state.overview_window = None;
                save_session(state); // persist "overview closed" (e.g. via its own close button)
            }
        }
        Message::WindowMoved(id, p) => {
            let known = id == state.main_window || state.overview_window == Some(id);
            if id == state.main_window {
                state.main_pos = Some(p);
            } else if state.overview_window == Some(id) {
                state.overview_pos = Some(p);
            }
            // Persist geometry as it changes, so it survives any exit path.
            if known {
                save_session(state);
            }
        }
        Message::WindowResized(id, s) => {
            let known = id == state.main_window || state.overview_window == Some(id);
            if id == state.main_window {
                state.main_size = s;
            } else if state.overview_window == Some(id) {
                state.overview_size = s;
            }
            if known {
                save_session(state);
            }
        }
        Message::JumpTo(ws, pane) => {
            if ws < state.workspaces.len() {
                state.active = ws;
                if state.workspaces[ws].panes.get(pane).is_some() {
                    state.workspaces[ws].focus = pane;
                }
                return iced::window::gain_focus(state.main_window);
            }
        }
        Message::Copy(allow_interrupt) => {
            let ws = state.active_mut();
            if let Some(p) = ws.panes.get_mut(ws.focus) {
                let text = if let Ok(mut t) = p.session.term().lock() {
                    let s = t.selection_text();
                    if s.is_some() {
                        t.clear_selection();
                    }
                    s
                } else {
                    None
                };
                match text {
                    Some(text) => return iced::clipboard::write(text),
                    None if allow_interrupt => p.session.write(b"\x03"),
                    None => {}
                }
            }
        }
        Message::Paste => return iced::clipboard::read().map(Message::Pasted),
        Message::Pasted(text) => {
            if let Some(text) = text.filter(|t| !t.is_empty()) {
                let ws = state.active_mut();
                if let Some(p) = ws.panes.get_mut(ws.focus) {
                    let bracketed =
                        p.session.term().lock().map(|t| t.bracketed_paste()).unwrap_or(false);
                    if bracketed {
                        p.session.write(b"\x1b[200~");
                        p.session.write(text.as_bytes());
                        p.session.write(b"\x1b[201~");
                    } else {
                        p.session.write(text.as_bytes());
                    }
                }
            }
        }
        Message::SwitchShell(pane) => {
            // Respawn the pane's terminal with the other shell, preserving cwd.
            // (You can't change a running process's shell, so the scrollback
            // resets — same as the web.)
            let git_bash = state.git_bash.clone();
            let ws = state.active_mut();
            if let Some(data) = ws.panes.get_mut(pane) {
                let target = match data.shell {
                    ShellKind::PowerShell => git_bash.map(|p| (ShellKind::GitBash, Some(p))),
                    ShellKind::GitBash => Some((ShellKind::PowerShell, None)),
                };
                if let Some((kind, shell_arg)) = target {
                    let cwd = data.session.cwd();
                    data.session = spawn_session(shell_arg.as_deref(), cwd.as_deref());
                    data.shell = kind;
                }
            }
            save_session(state);
        }
    }
    Task::none()
}

fn split(ws: &mut Workspace, axis: pane_grid::Axis) {
    let name = ws.next_name();
    let pane = PaneData { session: spawn_session(None, None), name, shell: ShellKind::PowerShell };
    if let Some((new_pane, _)) = ws.panes.split(axis, ws.focus, pane) {
        ws.focus = new_pane;
    }
}

/// Per-window view: the terminal UI for the main window, the session overview
/// for the popout window.
fn view(state: &State, window: iced::window::Id) -> Element<'_, Message> {
    if Some(window) == state.overview_window {
        overview_view(state)
    } else {
        main_view(state)
    }
}

fn main_view(state: &State) -> Element<'_, Message> {
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
    bar = bar.push(button(text("⊞ Overview").size(13)).on_press(Message::ToggleOverview).padding([4, 10]).style(button::secondary));
    bar = bar.push(button(text("Split →").size(13)).on_press(Message::SplitRight).padding([4, 10]));
    bar = bar.push(button(text("Split ↓").size(13)).on_press(Message::SplitDown).padding([4, 10]));
    bar = bar.push(button(text("Close").size(13)).on_press(Message::Close).style(button::secondary).padding([4, 10]));

    let focus = state.active().focus;
    let font = &state.font;
    let has_git_bash = state.git_bash.is_some();
    let grid = pane_grid::PaneGrid::new(&state.active().panes, |pane, data, _maximized| {
        let term = shader_widget(TermProgram {
            id: data.session.id(),
            pane,
            term: data.session.term(),
            master: data.session.master(),
            font: font.clone(),
        })
        .width(Length::Fill)
        .height(Length::Fill);

        let focused = pane == focus;
        // Claude status indicator in the header while Claude runs in this pane.
        let status = data
            .session
            .claude_running()
            .then(|| pane_dot(true, data.session.claude_status().lifecycle, false));
        let header = pane_header(&data.name, focused, data.shell, has_git_bash, pane, status);
        let content = column![header, term, footer_bar(&data.session)]
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

/// Hover-highlight style for a clickable overview row.
fn overview_row_style(_t: &iced::Theme, status: button::Status) -> button::Style {
    let mut s = button::Style {
        background: None,
        text_color: iced::Color::from_rgb8(0xe8, 0xea, 0xed),
        border: iced::Border::default(),
        shadow: Default::default(),
    };
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        s.background = Some(iced::Background::Color(iced::Color::from_rgb8(0x2c, 0x2c, 0x2c)));
    }
    s
}

/// Git stat counts (●staged ✎unstaged +untracked), matching the footer's colours.
fn overview_git(session: &Session) -> Element<'static, Message> {
    let mut r = row![].spacing(6).align_y(iced::Center);
    if let Some(g) = session.git() {
        if g.staged > 0 {
            r = r.push(text(format!("●{}", g.staged)).size(11).color(iced::Color::from_rgb8(0x6a, 0x99, 0x55)));
        }
        if g.unstaged > 0 {
            r = r.push(text(format!("○{}", g.unstaged)).size(11).color(iced::Color::from_rgb8(0xe5, 0xa0, 0x3c)));
        }
        if g.untracked > 0 {
            r = r.push(text(format!("+{}", g.untracked)).size(11).color(iced::Color::from_rgb8(0x56, 0x9c, 0xd6)));
        }
    }
    r.into()
}

/// The popout overview window: workspaces as titles (with terminal counts), each
/// terminal under it with a Claude icon (when active), git stats, and a live
/// status indicator (idle/running/ready dot · animated ✻ working · amber
/// attention). Clicking a row jumps to that pane. Reads the same shared status
/// the footer does — redrawn each frame, no polling.
fn overview_view(state: &State) -> Element<'_, Message> {
    let muted = iced::Color::from_rgb8(0x6b, 0x7a, 0x8d);
    let mut col = column![]
        .spacing(2)
        .push(container(text("Arbiter").size(13).font(title_font()).color(muted)).padding([8, 12]));

    for (wi, ws) in state.workspaces.iter().enumerate() {
        let count = ws.panes.iter().count();
        // Workspace title + terminal count.
        let header = row![
            text(ws.name.to_uppercase()).size(10).color(muted),
            horizontal_space(),
            text(count.to_string()).size(9).color(muted),
        ]
        .padding([3, 12])
        .align_y(iced::Center);
        col = col.push(header);

        for (pane, data) in ws.panes.iter() {
            let running = data.session.claude_running();
            let lc = data.session.claude_status().lifecycle;
            let busy = data.session.shell_idle() == Some(false);
            let dot = pane_dot(running, lc, busy);

            // Left: Claude icon (when active) + terminal name.
            let mut left = row![].spacing(6).align_y(iced::Center);
            if running {
                left = left.push(claude_icon(13.0));
            }
            left = left.push(text(data.name.clone()).size(12));

            let r = row![
                left,
                horizontal_space(),
                overview_git(&data.session),
                container(indicator(dot, 12)).width(Length::Fixed(22.0)).center_x(Length::Fixed(22.0)),
            ]
            .spacing(8)
            .align_y(iced::Center);

            col = col.push(
                button(r)
                    .on_press(Message::JumpTo(wi, *pane))
                    .padding([5, 12])
                    .width(Length::Fill)
                    .style(overview_row_style),
            );
        }
    }

    container(scrollable(col).width(Length::Fill).height(Length::Fill))
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x25, 0x25, 0x25))),
            ..Default::default()
        })
        .into()
}

/// Per-pane footer: folder + git branch + status counts (from the Session's
/// cwd-tracked git info). Claude model/context/tokens land here later.
fn footer_style(_t: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgb8(0x1b, 0x1b, 0x1b))),
        text_color: Some(iced::Color::from_rgb8(0x9c, 0x9c, 0x9c)),
        ..Default::default()
    }
}

/// Compact token count: 4200 → "4.2k".
fn fmt_k(n: u64) -> String {
    if n >= 1000 {
        // Truncate to one decimal (match the web's fmtK), e.g. 24450 → "24.4K".
        format!("{:.1}K", (n / 100) as f64 / 10.0)
    } else {
        n.to_string()
    }
}

/// Context-window size: 1_000_000 → "1M", 200_000 → "200k".
fn fmt_ctx_size(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else {
        format!("{}k", n / 1000)
    }
}

fn footer_bar(session: &Session) -> Element<'static, Message> {
    // Claude stats (from the capture/hook watcher) take over the footer while
    // Claude runs here.
    let c = session.claude_status();
    if session.claude_running() && c.has_stats {
        let mut r = row![].spacing(12).align_y(iced::Center);
        if let Some(m) = &c.model {
            r = r.push(text(m.clone()).size(11).color(iced::Color::from_rgb8(0x4e, 0xc9, 0xb0)));
        }
        if let Some(p) = c.used_percent {
            let size = c.context_size.map(fmt_ctx_size).unwrap_or_default();
            r = r.push(
                text(format!("ctx {p:.0}%/{size}")).size(11).color(iced::Color::from_rgb8(0x56, 0x9c, 0xd6)),
            );
        }
        r = r.push(
            text(format!(
                // ↓ input, ↑ output, + cache-write, ↻ cache-read — matching the web's order/arrows.
                "↓{} ↑{} +{} ↻{}",
                fmt_k(c.input_tokens),
                fmt_k(c.output_tokens),
                fmt_k(c.cache_write),
                fmt_k(c.cache_read),
            ))
            .size(11),
        );
        if c.cost_usd > 0.0 {
            r = r.push(
                text(format!("${:.2}", c.cost_usd)).size(11).color(iced::Color::from_rgb8(0xd7, 0xba, 0x7d)),
            );
        }
        if let Some(f) = session.folder() {
            r = r.push(text(format!("· {f}")).size(11));
        }
        return container(r).width(Length::Fill).padding([2, 8]).style(footer_style).into();
    }

    // Otherwise the git footer (folder · branch · counts).
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
            counts.push_str(&format!("○{} ", g.unstaged));
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
        .style(footer_style)
        .into()
}

// MDI icons (mdiPowershell / mdiBash) for the shell-switch button. The button
// shows the icon of the shell you'd switch *to* (matching the web).
const ICON_POWERSHELL: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#b0b0b0" d="M21.83,4C22.32,4 22.63,4.4 22.5,4.89L19.34,19.11C19.23,19.6 18.75,20 18.26,20H2.17C1.68,20 1.37,19.6 1.5,19.11L4.66,4.89C4.77,4.4 5.25,4 5.74,4H21.83M15.83,16H11.83C11.37,16 11,16.38 11,16.84C11,17.31 11.37,17.69 11.83,17.69H15.83C16.3,17.69 16.68,17.31 16.68,16.84C16.68,16.38 16.3,16 15.83,16M5.78,16.28C5.38,16.56 5.29,17.11 5.57,17.5C5.85,17.92 6.41,18 6.81,17.73C14.16,12.56 14.21,12.5 14.26,12.47C14.44,12.31 14.53,12.09 14.54,11.87C14.55,11.67 14.5,11.5 14.38,11.31L9.46,6.03C9.13,5.67 8.57,5.65 8.21,6C7.85,6.32 7.83,6.88 8.16,7.24L12.31,11.68L5.78,16.28Z"/></svg>"##;
const ICON_BASH: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#b0b0b0" d="M5 9H7.31L7.63 6H9.63L9.31 9H11.31L11.63 6H13.63L13.31 9H15V11H13.1L12.9 13H15V15H12.69L12.37 18H10.37L10.69 15H8.69L8.37 18H6.37L6.69 15H5V13H6.9L7.1 11H5V9M9.1 11L8.9 13H10.9L11.1 11M19 6H17V14H19M19 16H17V18H19Z"/></svg>"##;

/// The Claude starburst logo (matches the web's ClaudeIcon, fill #D97757).
const CLAUDE_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 248 248"><path fill="#D97757" d="M52.4285 162.873L98.7844 136.879L99.5485 134.602L98.7844 133.334H96.4921L88.7237 132.862L62.2346 132.153L39.3113 131.207L17.0249 130.026L11.4214 128.844L6.2 121.873L6.7094 118.447L11.4214 115.257L18.171 115.847L33.0711 116.911L55.485 118.447L71.6586 119.392L95.728 121.873H99.5485L100.058 120.337L98.7844 119.392L97.7656 118.447L74.5877 102.732L49.4995 86.1905L36.3823 76.62L29.3779 71.7757L25.8121 67.2858L24.2839 57.3608L30.6515 50.2716L39.3113 50.8623L41.4763 51.4531L50.2636 58.1879L68.9842 72.7209L93.4357 90.6804L97.0015 93.6343L98.4374 92.6652L98.6571 91.9801L97.0015 89.2625L83.757 65.2772L69.621 40.8192L63.2534 30.6579L61.5978 24.632C60.9565 22.1032 60.579 20.0111 60.579 17.4246L67.8381 7.49965L71.9133 6.19995L81.7193 7.49965L85.7946 11.0443L91.9074 24.9865L101.714 46.8451L116.996 76.62L121.453 85.4816L123.873 93.6343L124.764 96.1155H126.292V94.6976L127.566 77.9197L129.858 57.3608L132.15 30.8942L132.915 23.4505L136.608 14.4708L143.994 9.62643L149.725 12.344L154.437 19.0788L153.8 23.4505L150.998 41.6463L145.522 70.1215L141.957 89.2625H143.994L146.414 86.7813L156.093 74.0206L172.266 53.698L179.398 45.6635L187.803 36.802L193.152 32.5484H203.34L210.726 43.6549L207.415 55.1159L196.972 68.3492L188.312 79.5739L175.896 96.2095L168.191 109.585L168.882 110.689L170.738 110.53L198.755 104.504L213.91 101.787L231.994 98.7149L240.144 102.496L241.036 106.395L237.852 114.311L218.495 119.037L195.826 123.645L162.07 131.592L161.696 131.893L162.137 132.547L177.36 133.925L183.855 134.279H199.774L229.447 136.524L237.215 141.605L241.8 147.867L241.036 152.711L229.065 158.737L213.019 154.956L175.45 145.977L162.587 142.787H160.805V143.85L171.502 154.366L191.242 172.089L215.82 195.011L217.094 200.682L213.91 205.172L210.599 204.699L188.949 188.394L180.544 181.069L161.696 165.118H160.422V166.772L164.752 173.152L187.803 207.771L188.949 218.405L187.294 221.832L181.308 223.959L174.813 222.777L161.187 203.754L147.305 182.486L136.098 163.345L134.745 164.2L128.075 235.42L125.019 239.082L117.887 241.8L111.902 237.31L108.718 229.984L111.902 215.452L115.722 196.547L118.779 181.541L121.58 162.873L123.291 156.636L123.14 156.219L121.773 156.449L107.699 175.752L86.304 204.699L69.3663 222.777L65.291 224.431L58.2867 220.768L58.9235 214.27L62.8713 208.48L86.304 178.705L100.44 160.155L109.551 149.507L109.462 147.967L108.959 147.924L46.6977 188.512L35.6182 189.93L30.7788 185.44L31.4156 178.115L33.7079 175.752L52.4285 162.873Z"/></svg>"##;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The animated Claude "working" glyph (asterisk bloom) — matches the web's
/// ClaudeWorkingIcon: 12-frame ping-pong at 110ms/frame, per-frame colour.
fn working_frame() -> (&'static str, iced::Color) {
    const F: [(&str, (u8, u8, u8)); 12] = [
        ("·", (0x9c, 0x56, 0x38)), ("·", (0x9c, 0x56, 0x38)), ("✢", (0xb8, 0x6a, 0x45)),
        ("✳", (0xc9, 0x7a, 0x52)), ("✶", (0xd9, 0x88, 0x5f)), ("✻", (0xe8, 0x98, 0x70)),
        ("✽", (0xf4, 0xad, 0x88)), ("✽", (0xf4, 0xad, 0x88)), ("✻", (0xe8, 0x98, 0x70)),
        ("✶", (0xd9, 0x88, 0x5f)), ("✳", (0xc9, 0x7a, 0x52)), ("✢", (0xb8, 0x6a, 0x45)),
    ];
    let (g, (r, gn, b)) = F[(now_ms() / 110 % 12) as usize];
    (g, iced::Color::from_rgb8(r, gn, b))
}

/// The font carrying the working-animation dingbats (`·✢✳✶✻✽`, U+2722–273F +
/// U+00B7). Bundled (a 3KB subset of Noto Sans Symbols 2, renamed) so the ✻ looks
/// IDENTICAL on macOS and Windows — the default UI font lacks these glyphs and the
/// previous per-OS system fonts (Menlo / Segoe UI Symbol) rendered them at
/// different sizes.
fn symbols_font() -> iced::Font {
    iced::Font::with_name("ArbiterSymbols")
}

/// Pulse alpha in [0.5, 1.0] over `period_ms` (0.5 at the ends, 1.0 mid-cycle).
fn pulse_alpha(period_ms: u64) -> f32 {
    let t = (now_ms() % period_ms) as f32 / period_ms as f32;
    0.5 + 0.5 * (std::f32::consts::PI * t).sin()
}

/// The status a terminal shows (header dot + overview).
#[derive(Clone, Copy)]
enum Dot {
    Idle,
    Running,
    Ready,
    Working,
    Attention,
}

/// Resolve a pane's status: Claude lifecycle if it's running, else the shell's
/// busy/idle state.
fn pane_dot(claude_running: bool, lc: Lifecycle, shell_busy: bool) -> Dot {
    if claude_running {
        match lc {
            Lifecycle::Working => Dot::Working,
            Lifecycle::Attention => Dot::Attention,
            _ => Dot::Ready,
        }
    } else if shell_busy {
        Dot::Running
    } else {
        Dot::Idle
    }
}

/// The status indicator widget: the animated ✻ for working, else a dot (pulsing
/// for running/attention). `size` is the dot text size; the glyph is a bit larger.
fn indicator(dot: Dot, size: u16) -> Element<'static, Message> {
    let rgba = iced::Color::from_rgba8;
    match dot {
        Dot::Working => {
            let (g, c) = working_frame();
            // The ✻ bloom glyphs live in a symbols font — Iced's default UI font
            // lacks them (renders tofu) and won't fall back.
            text(g).size(size + 5).color(c).font(symbols_font()).into()
        }
        Dot::Attention => text("●").size(size).color(rgba(0xe5, 0xa0, 0x3c, pulse_alpha(1200))).into(),
        Dot::Running => text("●").size(size).color(rgba(0x22, 0xc5, 0x5e, pulse_alpha(1500))).into(),
        // Claude running but idle (between turns): solid grey — present, not busy.
        Dot::Ready => text("●").size(size).color(rgba(0x6b, 0x7a, 0x8d, 0.85)).into(),
        Dot::Idle => text("●").size(size).color(rgba(0x6b, 0x7a, 0x8d, 0.5)).into(),
    }
}

/// Static status dot for the pane header. The animated ✻ cycles glyph widths/
/// sizes and jumps the header, so the header shows a plain coloured dot (no glyph
/// animation, no pulse) — the animation lives in the overview instead.
fn header_dot(dot: Dot) -> Element<'static, Message> {
    let rgba = iced::Color::from_rgba8;
    let c = match dot {
        Dot::Working => rgba(0x4d, 0xa6, 0xff, 1.0),   // azure — Claude working
        Dot::Attention => rgba(0xe5, 0xa0, 0x3c, 1.0), // amber — needs input
        Dot::Running => rgba(0x22, 0xc5, 0x5e, 1.0),   // green — shell busy
        Dot::Ready => rgba(0x6b, 0x7a, 0x8d, 0.9),     // grey — Claude idle
        Dot::Idle => rgba(0x6b, 0x7a, 0x8d, 0.5),      // dim grey — no Claude
    };
    text("●").size(11).color(c).into()
}

/// The Claude starburst icon at `size` px.
fn claude_icon(size: f32) -> Element<'static, Message> {
    svg(svg::Handle::from_memory(CLAUDE_ICON.as_bytes())).width(size).height(size).into()
}

/// Per-pane header: a centred terminal title (focus shown by colour) with a
/// shell-switch button on the right (Windows) and a Claude status indicator on
/// the left while Claude runs. A matching left/right slot keeps the title centred.
fn pane_header(
    name: &str,
    focused: bool,
    shell: ShellKind,
    has_git_bash: bool,
    pane: pane_grid::Pane,
    status: Option<Dot>,
) -> Element<'static, Message> {
    const SLOT: f32 = 26.0;
    let color = if focused {
        iced::Color::from_rgb8(0x4d, 0xa6, 0xff)
    } else {
        iced::Color::from_rgb8(0x6b, 0x6b, 0x6b)
    };
    let left: Element<'static, Message> = match status {
        // Static dot in the header (no jumpy ✻ animation — that's in the overview).
        Some(d) => header_dot(d),
        None => Space::with_width(Length::Fixed(0.0)).into(),
    };
    let title = container(text(name.to_string()).size(12).color(color)).center_x(Length::Fill);
    let right: Element<'static, Message> = if has_git_bash {
        let icon = match shell {
            ShellKind::PowerShell => ICON_BASH, // click → switch to Git Bash
            ShellKind::GitBash => ICON_POWERSHELL, // click → switch to PowerShell
        };
        button(svg(svg::Handle::from_memory(icon.as_bytes())).width(15).height(15))
            .on_press(Message::SwitchShell(pane))
            .padding(2)
            .style(button::text)
            .into()
    } else {
        Space::with_width(Length::Fixed(0.0)).into()
    };
    let header = row![
        container(left).width(Length::Fixed(SLOT)).center_x(Length::Fixed(SLOT)),
        title,
        container(right).width(Length::Fixed(SLOT)).center_x(Length::Fixed(SLOT)),
    ]
    .align_y(iced::Center)
    .padding([2, 4]);
    container(header)
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x16, 0x16, 0x16))),
            ..Default::default()
        })
        .into()
}

fn subscription(_state: &State) -> Subscription<Message> {
    let tick = iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick);
    // Only the main window's keys drive the terminal (not the overview window).
    let keys = iced::event::listen_with(|event, _status, id| {
        (MAIN_WINDOW.get().copied() == Some(id)).then(|| handle_key(event)).flatten()
    });
    let closes = iced::window::close_events().map(Message::WindowClosed);
    // Track each window's geometry (no move_events(), so filter the event stream).
    let geom = iced::window::events().map(|(id, ev)| match ev {
        iced::window::Event::Moved(p) => Message::WindowMoved(id, p),
        iced::window::Event::Resized(s) => Message::WindowResized(id, s),
        iced::window::Event::Opened { position, size } => match position {
            Some(p) => Message::WindowMoved(id, p),
            None => Message::WindowResized(id, size),
        },
        _ => Message::Noop,
    });
    Subscription::batch([tick, keys, closes, geom])
}

/// xterm modifier code: 1 + shift + 2·alt + 4·ctrl (matches Alacritty's
/// `SequenceModifiers::encode_esc_sequence`).
fn mod_code(m: iced::keyboard::Modifiers) -> u8 {
    1 + m.shift() as u8 + ((m.alt() as u8) << 1) + ((m.control() as u8) << 2)
}

/// Letter-terminated cursor/edit key (arrows, Home, End, F1–F4): `CSI <final>`
/// unmodified, else `CSI 1;<code><final>`.
fn csi_mod(m: iced::keyboard::Modifiers, final_byte: char) -> Vec<u8> {
    let code = mod_code(m);
    if code == 1 {
        format!("\x1b[{final_byte}").into_bytes()
    } else {
        format!("\x1b[1;{code}{final_byte}").into_bytes()
    }
}

/// Tilde-terminated key (Ins/Del/PgUp/PgDn/F5+): `CSI <n>~` unmodified, else
/// `CSI <n>;<code>~`.
fn csi_tilde(m: iced::keyboard::Modifiers, n: u32) -> Vec<u8> {
    let code = mod_code(m);
    if code == 1 {
        format!("\x1b[{n}~").into_bytes()
    } else {
        format!("\x1b[{n};{code}~").into_bytes()
    }
}

/// F1–F4: the SS3 form unmodified (xterm-256color terminfo), CSI when modified
/// (SS3 can't carry a modifier).
fn fkey(m: iced::keyboard::Modifiers, ss3: &str, final_byte: char) -> Vec<u8> {
    if mod_code(m) == 1 {
        ss3.as_bytes().to_vec()
    } else {
        csi_mod(m, final_byte)
    }
}

/// Map a keyboard event to PTY bytes. Special keys are hand-mapped; printable
/// input uses the event's `text` (Shift/symbols/layout already applied).
fn handle_key(event: iced::Event) -> Option<Message> {
    use iced::keyboard::{key::Named, Event::KeyPressed, Key};
    let iced::Event::Keyboard(KeyPressed { key, text, modifiers, .. }) = event else {
        return None;
    };
    match &key {
        Key::Named(Named::Enter) => {
            // Shift+Enter → resolved in update() (kitty CSI 13;2u for Claude,
            // else CR). Ctrl+Enter → LF (Claude multi-line). Plain → CR.
            if modifiers.shift() {
                return Some(Message::ShiftEnter);
            }
            return Some(Message::Input(if modifiers.control() { b"\n".to_vec() } else { b"\r".to_vec() }));
        }
        Key::Named(Named::Backspace) => return Some(Message::Input(vec![0x7f])),
        Key::Named(Named::Tab) => {
            // Shift+Tab → CSI Z (back-tab); Claude cycles its mode with it.
            let bytes = if modifiers.shift() { b"\x1b[Z".to_vec() } else { b"\t".to_vec() };
            return Some(Message::Input(bytes));
        }
        Key::Named(Named::Escape) => return Some(Message::Input(vec![0x1b])),
        // Cursor + editing keys. Arrows/Home/End carry modifiers (Ctrl+→ etc.)
        // as the xterm `CSI 1;<mod><final>` form.
        Key::Named(Named::ArrowUp) => return Some(Message::Input(csi_mod(modifiers, 'A'))),
        Key::Named(Named::ArrowDown) => return Some(Message::Input(csi_mod(modifiers, 'B'))),
        Key::Named(Named::ArrowRight) => return Some(Message::Input(csi_mod(modifiers, 'C'))),
        Key::Named(Named::ArrowLeft) => return Some(Message::Input(csi_mod(modifiers, 'D'))),
        Key::Named(Named::Home) => return Some(Message::Input(csi_mod(modifiers, 'H'))),
        Key::Named(Named::End) => return Some(Message::Input(csi_mod(modifiers, 'F'))),
        Key::Named(Named::Insert) => return Some(Message::Input(csi_tilde(modifiers, 2))),
        Key::Named(Named::Delete) => return Some(Message::Input(csi_tilde(modifiers, 3))),
        Key::Named(Named::PageUp) => return Some(Message::Input(csi_tilde(modifiers, 5))),
        Key::Named(Named::PageDown) => return Some(Message::Input(csi_tilde(modifiers, 6))),
        // F1–F4 use SS3 unmodified (matches xterm-256color terminfo kf1=\EOP);
        // modified, they fall back to the CSI form like the other keys.
        Key::Named(Named::F1) => return Some(Message::Input(fkey(modifiers, "\x1bOP", 'P'))),
        Key::Named(Named::F2) => return Some(Message::Input(fkey(modifiers, "\x1bOQ", 'Q'))),
        Key::Named(Named::F3) => return Some(Message::Input(fkey(modifiers, "\x1bOR", 'R'))),
        Key::Named(Named::F4) => return Some(Message::Input(fkey(modifiers, "\x1bOS", 'S'))),
        Key::Named(Named::F5) => return Some(Message::Input(csi_tilde(modifiers, 15))),
        Key::Named(Named::F6) => return Some(Message::Input(csi_tilde(modifiers, 17))),
        Key::Named(Named::F7) => return Some(Message::Input(csi_tilde(modifiers, 18))),
        Key::Named(Named::F8) => return Some(Message::Input(csi_tilde(modifiers, 19))),
        Key::Named(Named::F9) => return Some(Message::Input(csi_tilde(modifiers, 20))),
        Key::Named(Named::F10) => return Some(Message::Input(csi_tilde(modifiers, 21))),
        Key::Named(Named::F11) => return Some(Message::Input(csi_tilde(modifiers, 23))),
        Key::Named(Named::F12) => return Some(Message::Input(csi_tilde(modifiers, 24))),
        Key::Named(Named::Space) if modifiers.control() => return Some(Message::Input(vec![0])),
        // Copy/paste: Cmd+C/V (macOS), Ctrl+Shift+C/V, and Ctrl+C/V. Plain Ctrl+C
        // copies only if there's a selection, else sends interrupt (^C).
        Key::Character(s) if modifiers.control() || modifiers.logo() => {
            let lc = s.chars().next().map(|c| c.to_ascii_lowercase());
            match lc {
                Some('c') => {
                    let interrupt = modifiers.control() && !modifiers.shift() && !modifiers.logo();
                    return Some(Message::Copy(interrupt));
                }
                Some('v') => return Some(Message::Paste),
                Some(lc) if modifiers.control() && !modifiers.logo() && lc.is_ascii_alphabetic() => {
                    return Some(Message::Input(vec![(lc as u8) - b'a' + 1]));
                }
                _ => {}
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

/// Map a cursor position (logical px, relative to the widget) to a visible
/// (row, col) cell plus whether the cursor is in the cell's right half. Cell
/// size is derived from the widget bounds and the grid dimensions.
fn cell_at(pos: iced::Point, bounds: Rectangle, term: &SharedTerm) -> (usize, usize, bool) {
    let (cols, rows) = term.lock().unwrap().size();
    let cw = (bounds.width / cols.max(1) as f32).max(1.0);
    let ch = (bounds.height / rows.max(1) as f32).max(1.0);
    let fx = pos.x / cw;
    let col = (fx.max(0.0).floor() as usize).min(cols.saturating_sub(1));
    let row = ((pos.y / ch).max(0.0).floor() as usize).min(rows.saturating_sub(1));
    (row, col, fx.fract() >= 0.5)
}

// ── Custom shader widget: the wgpu terminal hosted inside Iced ────────────────

/// Per-pane GPU renderers, keyed by session id. Iced's `Storage` is a global
/// type-map shared across all shader widgets, so we key one TermGpu per session
/// (else every pane draws the last-prepared one).
#[derive(Default)]
struct Renderers(HashMap<u64, TermGpu>);

struct TermProgram {
    id: u64,
    pane: pane_grid::Pane,
    term: SharedTerm,
    master: SharedMaster,
    font: Arc<arbiter_native::font::FontSpec>,
}

/// Per-widget interaction state for selection + scrolling.
#[derive(Default)]
struct TermState {
    dragging: bool,
    /// Lines/frame to auto-scroll while a drag is past the top/bottom edge
    /// (signed: + = up into history). Applied each RedrawRequested.
    autoscroll: i32,
    /// Clamped (row, col, right-half) the selection extends to during auto-scroll.
    drag_cell: (usize, usize, bool),
    /// Multi-click tracking (double = word, triple = line).
    last_click: Option<std::time::Instant>,
    last_cell: (usize, usize),
    clicks: u8,
}

/// Max gap between clicks to count as a double/triple click.
const CLICK_THRESHOLD: Duration = Duration::from_millis(300);

impl std::fmt::Debug for TermProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TermProgram")
    }
}

impl shader::Program<Message> for TermProgram {
    type State = TermState;
    type Primitive = TermPrimitive;

    fn update(
        &self,
        state: &mut Self::State,
        event: shader::Event,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
        _shell: &mut iced::advanced::Shell<'_, Message>,
    ) -> (iced::event::Status, Option<Message>) {
        use iced::event::Status::{Captured, Ignored};
        use iced::mouse::{Button, Event::*, ScrollDelta};
        let captured = (Captured, None);
        match event {
            // Wheel over this pane scrolls its scrollback. ×3 lines per notch
            // (matches Alacritty); pixel deltas (trackpads) convert via cell
            // height. Typing jumps back to the bottom (handled in update()).
            shader::Event::Mouse(WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let mut t = self.term.lock().unwrap();
                let rows = t.size().1.max(1) as f32;
                let lines = match delta {
                    ScrollDelta::Lines { y, .. } => (y * 3.0).round() as i32,
                    ScrollDelta::Pixels { y, .. } => (y / (bounds.height / rows).max(1.0)).round() as i32,
                };
                if lines != 0 {
                    t.scroll(lines);
                }
                captured
            }
            // Left press: focus the pane + begin a selection. Single/double/
            // triple click selects char/word/line (alacritty Simple/Semantic/
            // Lines), tracked by click timing + same-cell.
            shader::Event::Mouse(ButtonPressed(Button::Left)) => {
                if let Some(pos) = cursor.position_in(bounds) {
                    let (row, col, right) = cell_at(pos, bounds, &self.term);
                    let now = std::time::Instant::now();
                    let multi = state.last_cell == (row, col)
                        && state.last_click.is_some_and(|t| now.duration_since(t) < CLICK_THRESHOLD);
                    state.clicks = if multi { (state.clicks % 3) + 1 } else { 1 };
                    state.last_click = Some(now);
                    state.last_cell = (row, col);
                    let kind = match state.clicks {
                        2 => SelectKind::Word,
                        3 => SelectKind::Line,
                        _ => SelectKind::Simple,
                    };
                    self.term.lock().unwrap().start_selection(row, col, right, kind);
                    state.dragging = true;
                    state.autoscroll = 0;
                    (Captured, Some(Message::Focus(self.pane)))
                } else {
                    (Ignored, None)
                }
            }
            // Drag: extend the selection. Past the top/bottom edge, arm
            // auto-scroll (applied per frame in RedrawRequested) and clamp the
            // extension to the edge row.
            shader::Event::Mouse(CursorMoved { .. }) if state.dragging => {
                if let Some(abs) = cursor.position() {
                    let (rx, ry) = (abs.x - bounds.x, abs.y - bounds.y);
                    // Scroll speed grows with distance past the edge (1–8 lines/frame).
                    let speed = |over: f32| ((over / 12.0).ceil() as i32).clamp(1, 8);
                    state.autoscroll = if ry < 0.0 {
                        speed(-ry) // above top → scroll up (positive = into history)
                    } else if ry > bounds.height {
                        -speed(ry - bounds.height) // below bottom → scroll down
                    } else {
                        0
                    };
                    let cx = rx.clamp(0.0, bounds.width - 0.5);
                    let cy = ry.clamp(0.0, bounds.height - 0.5);
                    let (row, col, right) = cell_at(iced::Point::new(cx, cy), bounds, &self.term);
                    state.drag_cell = (row, col, right);
                    self.term.lock().unwrap().update_selection(row, col, right);
                }
                captured
            }
            // Continuous auto-scroll while a drag is held past an edge.
            shader::Event::RedrawRequested(_) if state.dragging && state.autoscroll != 0 => {
                let mut t = self.term.lock().unwrap();
                t.scroll(state.autoscroll);
                let (r, c, right) = state.drag_cell;
                t.update_selection(r, c, right);
                (Ignored, None)
            }
            shader::Event::Mouse(ButtonReleased(Button::Left)) if state.dragging => {
                state.dragging = false;
                state.autoscroll = 0;
                captured
            }
            _ => (Ignored, None),
        }
    }

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

/// Web-parity UI fonts — the same the web app used outside terminals: Inter for
/// body text, DM Sans for the title/logo. Both are variable fonts (weight axis);
/// cosmic-text selects the weight. Bundled under assets/ (SIL OFL).
const INTER_FONT: &[u8] = include_bytes!("../../assets/Inter-VariableFont.ttf");
const DMSANS_FONT: &[u8] = include_bytes!("../../assets/DMSans-VariableFont.ttf");
/// 3KB subset of Noto Sans Symbols 2 (the `·✢✳✶✻✽` working-animation dingbats),
/// renamed "ArbiterSymbols" — bundled so the ✻ is identical on macOS + Windows.
const ARBITER_SYMBOLS_FONT: &[u8] = include_bytes!("../../assets/ArbiterSymbols.ttf");

/// The base UI font (Inter), matching the web's `font-family: 'Inter', …`.
fn ui_font() -> iced::Font {
    iced::Font::with_name("Inter")
}

/// The title/logo font (DM Sans Bold), matching the web's "Arbiter" wordmark.
fn title_font() -> iced::Font {
    iced::Font { weight: iced::font::Weight::Bold, ..iced::Font::with_name("DM Sans") }
}

fn main() -> iced::Result {
    // Headless subcommands Claude invokes via our injected --settings: capture
    // its statusLine JSON / hook signals, then exit without starting the GUI.
    match std::env::args().nth(1).as_deref() {
        Some("claude-statusline") => {
            arbiter_native::claude_shim::run_statusline_capture();
            return Ok(());
        }
        Some("claude-hook") => {
            arbiter_native::claude_shim::run_hook_signal();
            return Ok(());
        }
        _ => {}
    }

    let font = Arc::new(arbiter_native::font::load());
    let git_bash = arbiter_native::shell::detect_git_bash();
    // Event-driven Claude status: a single notify watcher over the capture + hook
    // dirs updates each Session's shared status (no polling). Lives for the app.
    std::mem::forget(arbiter_native::claude_status::start_watcher());

    let title = |state: &State, id: iced::window::Id| {
        if state.overview_window == Some(id) {
            "Arbiter — Overview".to_string()
        } else {
            "Arbiter native".to_string()
        }
    };

    iced::daemon(title, update, view)
        .subscription(subscription)
        .theme(|s: &State, _id| s.theme.clone())
        .font(INTER_FONT)
        .font(DMSANS_FONT)
        .font(ARBITER_SYMBOLS_FONT)
        .default_font(ui_font())
        .run_with(move || {
            // daemon starts with no windows — open the main one here.
            let saved = arbiter_native::persist::load();
            let main_geom = saved.as_ref().and_then(|s| s.main_window);
            let overview_geom = saved.as_ref().and_then(|s| s.overview_window);
            let overview_was_open = saved.as_ref().map(|s| s.overview_visible).unwrap_or(false);

            // Open the main window at its saved size/position (or the default).
            let mut settings = iced::window::Settings::default();
            if let Some(g) = main_geom {
                settings.size = iced::Size::new(g.width, g.height);
                if let (Some(x), Some(y)) = (g.x, g.y) {
                    settings.position = iced::window::Position::Specific(iced::Point::new(x, y));
                }
            }
            let main_size = settings.size;
            let (main_id, open) = iced::window::open(settings);
            let _ = MAIN_WINDOW.set(main_id);

            // Restore the saved layout (respawning each terminal in its cwd/shell,
            // resuming Claude where it ran); fall back to one fresh workspace.
            let (workspaces, active) = saved
                .and_then(|saved| restore_workspaces(saved, git_bash.as_deref()))
                .unwrap_or_else(|| (vec![Workspace::new("Workspace 1".to_string())], 0));

            let point = |g: persist::SavedWindow| g.x.zip(g.y).map(|(x, y)| iced::Point::new(x, y));
            let overview_size = overview_geom
                .map(|g| iced::Size::new(g.width, g.height))
                .unwrap_or(iced::Size::new(720.0, 520.0));
            let overview_pos = overview_geom.and_then(point);

            // Reopen the overview popout if it was open at quit (matches the web).
            let mut tasks = vec![open.map(|_| Message::Noop)];
            let overview_window = if overview_was_open {
                let (ov_id, ov_task) = open_overview(overview_size, overview_pos);
                tasks.push(ov_task);
                Some(ov_id)
            } else {
                None
            };

            let state = State {
                workspaces,
                active,
                font: font.clone(),
                git_bash: git_bash.clone(),
                theme: arbiter_theme(),
                main_window: main_id,
                overview_window,
                main_size,
                main_pos: main_geom.and_then(point),
                overview_size,
                overview_pos,
            };
            (state, iced::Task::batch(tasks))
        })
}
