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
    button, column, container, horizontal_space, mouse_area, pane_grid, pick_list, row, scrollable,
    shader as shader_widget, svg, text, text_input, toggler, Space,
};
use iced::{Element, Length, Rectangle, Subscription, Task};

use arbiter_native::claude_status::Lifecycle;
use arbiter_native::gpu::TermGpu;
use arbiter_native::session::{Session, SharedMaster, SharedTerm};
use arbiter_native::persist;
use arbiter_native::term::SelectKind;

/// File-explorer file-type icons + colours (generated from @mdi/js).
mod file_icons;

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
    /// Some → this tab is a project workspace (git repo with worktrees + sidebars).
    /// `panes`/`focus` above always hold the ACTIVE worktree's grid; the other
    /// worktrees stash theirs in `Worktree::stash` (swapped on switch), so every
    /// existing grid handler keeps operating on the visible worktree unchanged.
    project: Option<Project>,
}

/// A project workspace: a git repo, its worktrees, and the file-explorer state.
struct Project {
    root: String,
    active: usize,
    worktrees: Vec<Worktree>,
    explorer: Explorer,
}

/// One worktree of a project. The ACTIVE worktree's pane grid lives in
/// `Workspace.panes` (so `stash` is None); inactive worktrees keep theirs here.
struct Worktree {
    branch: String,
    path: String,
    stash: Option<(pane_grid::State<PaneData>, pane_grid::Pane)>,
    /// Whether this worktree's branch has been merged into its parent (strikethrough).
    merged: bool,
    /// Bumped by "New robot" to reroll the avatar; mixed into the avatar seed so a
    /// new (still deterministic) face is drawn. Persisted so it survives restart.
    avatar_salt: u32,
}

impl Workspace {
    fn new(name: String) -> Self {
        let first_pane = PaneData {
            session: spawn_session(None, None),
            name: "Terminal 1".to_string(),
            shell: ShellKind::PowerShell,
        };
        let (panes, first) = pane_grid::State::new(first_pane);
        Workspace { panes, focus: first, name, next_term: 2, project: None }
    }

    /// Next per-workspace terminal name ("Terminal N"); numbering restarts per
    /// workspace, matching the web.
    fn next_name(&mut self) -> String {
        let n = self.next_term;
        self.next_term += 1;
        format!("Terminal {n}")
    }
}

/// File-explorer state for a project workspace (lazy tree; phase 4 fills cache).
#[derive(Default)]
struct Explorer {
    /// Directory paths the user has expanded.
    expanded: std::collections::HashSet<String>,
    /// Cached children per directory path (lazy-loaded; dirs first, then files).
    entries: std::collections::HashMap<String, Vec<DirEntry>>,
    /// git status per path (relative to the worktree) → modified/added/… colour key.
    git_status: std::collections::HashMap<String, String>,
    /// The worktree path the cache currently reflects (cleared on worktree switch).
    cached_for: String,
}

/// One file-explorer row.
#[derive(Clone)]
struct DirEntry {
    name: String,
    path: String,
    is_dir: bool,
}

/// Build a worktree's terminal grid: an 80/20 horizontal split — Claude on top
/// (80%), a shell on the bottom (20%) — both in the worktree's dir. Matches the
/// web. Further-splittable like any pane.
fn build_worktree_grid(path: &str) -> (pane_grid::State<PaneData>, pane_grid::Pane) {
    use pane_grid::Configuration;
    // The "Claude" pane auto-launches Claude (queued in the PTY; runs at the shell's
    // first prompt once rc sets the shim PATH) so the worktree's status, model, and
    // "ask Claude to merge" have a live Claude — like the web's Claude pane.
    let mut claude_session = spawn_session(None, Some(path));
    claude_session.write(b"claude\r");
    let claude = PaneData {
        session: claude_session,
        name: "Claude".to_string(),
        shell: ShellKind::PowerShell,
    };
    let term = PaneData {
        session: spawn_session(None, Some(path)),
        name: "Terminal".to_string(),
        shell: ShellKind::PowerShell,
    };
    let config = Configuration::Split {
        axis: pane_grid::Axis::Horizontal,
        ratio: 0.8,
        a: Box::new(Configuration::Pane(claude)),
        b: Box::new(Configuration::Pane(term)),
    };
    let state = pane_grid::State::with_configuration(config);
    let focus = *state.iter().next().map(|(p, _)| p).expect("grid has a pane");
    (state, focus)
}

/// Build a project workspace from a repo root + its worktree list. The main
/// worktree is active (its grid in `Workspace.panes`); the rest are stashed.
fn new_project(root: String, infos: Vec<arbiter_native::git::WorktreeInfo>) -> Workspace {
    // Order: main first, then existing linked worktrees that have a branch.
    let mut ordered: Vec<&arbiter_native::git::WorktreeInfo> = Vec::new();
    if let Some(m) = infos.iter().find(|w| w.is_main) {
        ordered.push(m);
    }
    for w in &infos {
        if !w.is_main && w.exists && w.branch.is_some() {
            ordered.push(w);
        }
    }
    if ordered.is_empty() {
        ordered.extend(infos.first());
    }

    let mut worktrees = Vec::new();
    let mut active: Option<(pane_grid::State<PaneData>, pane_grid::Pane)> = None;
    for (i, info) in ordered.iter().enumerate() {
        let (grid, focus) = build_worktree_grid(&info.path);
        let branch = info.branch.clone().unwrap_or_else(|| "detached".to_string());
        let stash = if i == 0 {
            active = Some((grid, focus));
            None
        } else {
            Some((grid, focus))
        };
        worktrees.push(Worktree { branch, path: info.path.clone(), stash, merged: false, avatar_salt: 0 });
    }
    let (panes, focus) = active.expect("project has a main worktree");
    let name = std::path::Path::new(&root)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();
    Workspace {
        panes,
        focus,
        name,
        next_term: 1,
        project: Some(Project { root, active: 0, worktrees, explorer: Explorer::default() }),
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
    /// Whether the main window is focused — drives the Windows caption-button
    /// glyph colour (white when active, dimmed when not), like native controls.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    main_focused: bool,
    /// Whether the main window is maximized — swaps the Windows caption button
    /// between the maximize square and the restore (double-square) glyph.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    main_maximized: bool,
    /// One-shot guard: apply platform window-chrome tweaks once the window is
    /// actually up (first frame) — Win11 rounded corners on Windows, traffic-light
    /// repositioning on macOS. The startup Opened event can fire before our
    /// subscription is listening, so the first Tick is the reliable hook.
    chrome_init: bool,
    /// Display scale the logo was rasterized for, and the resulting handle. The
    /// logo is rendered at the exact physical pixel size (1:1) so it stays crisp;
    /// re-rendered when the scale changes (see [`render_logo`]).
    logo_scale: f32,
    logo: iced::widget::image::Handle,
    /// The "new worktree" modal, while open (branch name + base-branch dropdown).
    worktree_dialog: Option<WorktreeDialog>,
    /// The index of the worktree whose right-click context menu is open, if any.
    worktree_menu: Option<usize>,
    /// Whether the "+" new-workspace dropdown (Terminal / Project) is open.
    new_ws_menu: bool,
    /// Last known cursor position (window coords) — used to anchor the "+" dropdown
    /// under the click, since iced can't report a widget's screen position.
    cursor: iced::Point,
    /// The x at which the "+" dropdown was opened (snapshot of `cursor.x`).
    new_ws_menu_x: f32,
    /// Latest Claude usage from the sidecar helper (drives the titlebar meters).
    usage: UsageData,
    /// When `usage` was last updated (epoch ms) — for the refresh countdown.
    usage_updated_ms: u64,
    /// The chosen claude.ai org uuid for usage (persisted; auto-sent to the helper).
    usage_org: Option<String>,
    /// Whether the org-selection modal is open.
    usage_org_menu: bool,
    /// User preferences (the Settings dialog) — persisted with the session.
    settings: persist::Settings,
    /// Whether the Settings modal is open, and which tab it's showing.
    settings_open: bool,
    settings_tab: SettingsTab,
}

/// The Settings dialog's sidebar tabs (web `SettingsDialog.vue` tabs). Only the
/// tabs whose settings have native backing are present; the rest of the web's
/// tabs (Display/Files) cover features the native build doesn't have yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SettingsTab {
    General,
    Display,
    ClaudeUsage,
}

/// State of the "new worktree" modal: the branch name being typed, the chosen
/// base branch, and the repo's branches (for the dropdown).
#[derive(Default)]
struct WorktreeDialog {
    name: String,
    base: Option<String>,
    branches: Vec<String>,
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
    /// Toggle the "+" dropdown (choose Terminal vs Project workspace).
    ToggleNewWsMenu,
    /// Dismiss the "+" dropdown.
    CloseNewWsMenu,
    /// Cursor moved (window coords) — tracked to anchor the "+" dropdown.
    CursorMoved(iced::Point),
    /// New Claude usage data from the sidecar helper.
    UsageUpdated(UsageData),
    /// Raise the helper's claude.ai sign-in window (titlebar Sign-in button).
    ShowUsageLogin,
    /// Open / dismiss the org-selection modal.
    ShowUsageOrgMenu,
    CloseUsageOrgMenu,
    /// Pick a claude.ai org for usage (persist + tell the helper).
    SelectUsageOrg(String),
    /// Sign out of the usage webview only (clears its claude.ai session).
    UsageSignOut,
    /// Refresh usage stats now (refresh button): refetch + restart the countdown.
    RefreshUsage,
    /// Open / dismiss the Settings dialog, and switch its active tab.
    OpenSettings,
    CloseSettings,
    SettingsSelectTab(SettingsTab),
    /// Settings toggles (persisted).
    ToggleHideUsageBar(bool),
    ToggleHideSonnetUsage(bool),
    ToggleOverviewClaudeOnly(bool),
    ToggleHideShellButton(bool),
    /// Settings → scrollback lines (text input; parsed + clamped).
    SetScrollback(String),
    /// Settings → "Clear saved data": delete the on-disk session layout.
    ClearSavedData,
    SelectWorkspace(usize),
    /// Close workspace tab `i` (never the last one).
    CloseWorkspace(usize),
    /// New project workspace: pick a folder, then validate it's a git repo.
    NewProjectWorkspace,
    ProjectFolderPicked(Option<String>),
    /// Switch the active project workspace to worktree `i` (swaps its pane grid in).
    SwitchWorktree(usize),
    /// Expand/collapse a directory in the file explorer.
    ExplorerToggle(String),
    /// Open the "new worktree" dialog (branch name + base-branch dropdown).
    NewWorktree,
    /// Live edits in the new-worktree dialog.
    WtDialogName(String),
    WtDialogPickBase(String),
    /// Dismiss the new-worktree dialog without creating.
    WtDialogCancel,
    /// Create the worktree from the dialog's current name + base.
    WtDialogCreate,
    /// Open the right-click context menu for worktree `i`.
    WorktreeMenu(usize),
    /// Dismiss the worktree context menu.
    WorktreeMenuClose,
    /// Merge worktree `i`'s branch into the main worktree's branch (manual git merge).
    WorktreeMerge(usize),
    /// Merge worktree `i` into main, then remove the worktree (web "merge & delete").
    WorktreeMergeDelete(usize),
    /// Ask Claude (the main worktree's first idle session) to merge worktree `i`.
    WorktreeAskClaudeMerge(usize),
    /// Discard all uncommitted changes in worktree `i` (reset --hard + clean).
    WorktreeDiscard(usize),
    /// Reroll worktree `i`'s avatar (bump its salt → a new deterministic robot).
    RegenerateAvatar(usize),
    /// Remove worktree `i` from the active project (git worktree remove --force).
    RemoveWorktree(usize),
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
    /// A window finished opening — seed its geometry and (Windows) round its
    /// corners now that the HWND exists.
    WindowOpened(iced::window::Id, Option<iced::Point>, iced::Size),
    /// A window gained/lost focus — drives the Windows caption-button dimming.
    WindowFocusChanged(iced::window::Id, bool),
    /// The main window's display scale (re-renders the logo at the exact pixel size).
    ScaleChanged(f32),
    /// Result of querying the main window's maximized state (Windows caption glyph).
    #[cfg(target_os = "windows")]
    SetMaximized(bool),
    /// Custom titlebar (Windows, decorations off): drag the window + window controls.
    #[cfg(target_os = "windows")]
    DragWindow,
    #[cfg(target_os = "windows")]
    WinMinimize,
    #[cfg(target_os = "windows")]
    WinMaximizeToggle,
    #[cfg(target_os = "windows")]
    WinClose,
    /// Begin an interactive edge/corner resize (carries a Win32 HT* hit-test code).
    #[cfg(target_os = "windows")]
    WinResize(usize),
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
    fallback_cwd: Option<&str>,
    git_bash: Option<&str>,
    claude_running: bool,
    claude_session: Option<&str>,
) -> (Session, ShellKind) {
    // Prefer the saved cwd; if it's missing or gone, fall back (a project worktree
    // passes its path, so its terminals reopen in the worktree even when the saved
    // cwd was never captured — e.g. before the shell first emitted OSC-7).
    let cwd = cwd
        .filter(|d| std::path::Path::new(d).is_dir())
        .or_else(|| fallback_cwd.filter(|d| std::path::Path::new(d).is_dir()));
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
    fallback_cwd: Option<&str>,
) -> pane_grid::Configuration<PaneData> {
    match node {
        persist::SavedNode::Split { vertical, ratio, a, b } => pane_grid::Configuration::Split {
            axis: if vertical { pane_grid::Axis::Vertical } else { pane_grid::Axis::Horizontal },
            ratio,
            a: Box::new(saved_to_config(*a, git_bash, fallback_cwd)),
            b: Box::new(saved_to_config(*b, git_bash, fallback_cwd)),
        },
        persist::SavedNode::Leaf { name, shell, cwd, claude_running, claude_session } => {
            let (session, kind) = spawn_restored(
                shell,
                cwd.as_deref(),
                fallback_cwd,
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
        match sw.project {
            // Project workspace: rebuild each worktree's grid; the active one lives
            // in Workspace.panes, the rest are stashed (mirrors `new_project`).
            Some(sp) => {
                let active_idx = sp.active.min(sp.worktrees.len().saturating_sub(1));
                let mut worktrees = Vec::new();
                let mut active: Option<(pane_grid::State<PaneData>, pane_grid::Pane)> = None;
                for (i, swt) in sp.worktrees.into_iter().enumerate() {
                    // Worktree terminals fall back to the worktree path if their saved
                    // cwd is gone/empty, so they reopen in the right folder.
                    let grid = pane_grid::State::with_configuration(saved_to_config(
                        swt.layout,
                        git_bash,
                        Some(swt.path.as_str()),
                    ));
                    let Some(focus) = grid.iter().next().map(|(p, _)| *p) else { continue };
                    let stash = if i == active_idx {
                        active = Some((grid, focus));
                        None
                    } else {
                        Some((grid, focus))
                    };
                    worktrees.push(Worktree {
                        branch: swt.branch,
                        path: swt.path,
                        stash,
                        merged: false,
                        avatar_salt: swt.avatar_salt,
                    });
                }
                let Some((panes, focus)) = active else { continue };
                let mut ws = Workspace {
                    panes,
                    focus,
                    name: sw.name,
                    next_term: sw.next_term,
                    project: Some(Project {
                        root: sp.root,
                        active: active_idx,
                        worktrees,
                        explorer: Explorer {
                            expanded: sp.expanded.into_iter().collect(),
                            ..Default::default()
                        },
                    }),
                };
                if let Some(p) = ws.project.as_mut() {
                    load_explorer(p);
                }
                workspaces.push(ws);
            }
            None => {
                let panes =
                    pane_grid::State::with_configuration(saved_to_config(sw.layout, git_bash, None));
                let Some(focus) = panes.iter().next().map(|(p, _)| *p) else { continue };
                workspaces.push(Workspace { panes, focus, name: sw.name, next_term: sw.next_term, project: None });
            }
        }
    }
    if workspaces.is_empty() {
        return None;
    }
    let active = saved.active.min(workspaces.len() - 1);
    Some((workspaces, active))
}

/// Snapshot one workspace's live split tree into the serialisable form.
fn node_to_saved(grid: &pane_grid::State<PaneData>, node: &pane_grid::Node) -> persist::SavedNode {
    match node {
        pane_grid::Node::Split { axis, ratio, a, b, .. } => persist::SavedNode::Split {
            vertical: matches!(axis, pane_grid::Axis::Vertical),
            ratio: *ratio,
            a: Box::new(node_to_saved(grid, a)),
            b: Box::new(node_to_saved(grid, b)),
        },
        pane_grid::Node::Pane(pane) => {
            let data = grid.get(*pane);
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

/// The leaf pane at one corner of the grid: at each split, descend into the half
/// that owns this corner (`right` picks the right of a vertical split, `bottom`
/// the bottom of a horizontal one). Used to round ONLY the grid's outer corners
/// (a pane can own up to two of them, e.g. a full-height edge pane).
fn corner_pane(node: &pane_grid::Node, right: bool, bottom: bool) -> pane_grid::Pane {
    match node {
        pane_grid::Node::Split { axis, a, b, .. } => {
            let pick_b = match axis {
                pane_grid::Axis::Vertical => right,   // vertical divider: a=left, b=right
                pane_grid::Axis::Horizontal => bottom, // horizontal divider: a=top, b=bottom
            };
            corner_pane(if pick_b { b } else { a }, right, bottom)
        }
        pane_grid::Node::Pane(p) => *p,
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

/// Whether a window position looks like a real on-screen spot. Windows parks a
/// minimized window's top-left at the documented sentinel `(-32000, -32000)`
/// (and the OS only reveals the real spot via `GetWindowPlacement`, which iced
/// doesn't surface). Persisting/restoring that sentinel left the window opening
/// off-screen — visible only as a taskbar icon. Guard against it (and any other
/// absurd coordinate) so we keep the last real position instead.
fn on_screen_ish(p: iced::Point) -> bool {
    p.x > -30000.0 && p.y > -30000.0 && p.x < 30000.0 && p.y < 30000.0
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
        usage_org: state.usage_org.clone(),
        settings: state.settings.clone(),
        workspaces: state
            .workspaces
            .iter()
            .map(|ws| {
                // Project workspaces save each worktree's tree (active one from
                // ws.panes, the rest from their stash) + which is active + explorer.
                let project = ws.project.as_ref().map(|p| persist::SavedProject {
                    root: p.root.clone(),
                    active: p.active,
                    expanded: p.explorer.expanded.iter().cloned().collect(),
                    worktrees: p
                        .worktrees
                        .iter()
                        .enumerate()
                        .map(|(i, w)| {
                            let grid = if i == p.active {
                                &ws.panes
                            } else {
                                w.stash.as_ref().map(|(g, _)| g).unwrap_or(&ws.panes)
                            };
                            persist::SavedWorktree {
                                branch: w.branch.clone(),
                                path: w.path.clone(),
                                layout: node_to_saved(grid, grid.layout()),
                                avatar_salt: w.avatar_salt,
                            }
                        })
                        .collect(),
                });
                persist::SavedWorkspace {
                    name: ws.name.clone(),
                    next_term: ws.next_term,
                    layout: node_to_saved(&ws.panes, ws.panes.layout()),
                    project,
                }
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
            // Apply platform window-chrome once, on the first frame — by now the
            // native window exists (its startup Opened event may have fired before
            // our subscription was listening). Win11 corner attribute is persistent;
            // macOS traffic lights also get re-applied on resize/focus below.
            if !state.chrome_init {
                state.chrome_init = true;
                #[cfg(target_os = "windows")]
                winround::round_our_windows();
                #[cfg(target_os = "macos")]
                trafficlights::position();
            }
            // Drive the usage refresh from the app: once the 120s countdown hits 0,
            // ask the helper to refetch and restart the countdown. (The helper's own
            // setInterval is unreliable while its window is hidden — webviews
            // throttle background timers — so the app owns the cadence.)
            if state.usage.state == UsageState::Ok
                && state.usage_updated_ms != 0
                && now_ms().saturating_sub(state.usage_updated_ms) >= USAGE_REFRESH_MS
            {
                usage_helper_cmd("fetch");
                state.usage_updated_ms = now_ms(); // restart the countdown immediately
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
            state.new_ws_menu = false;
            let n = state.workspaces.len() + 1;
            state.workspaces.push(Workspace::new(format!("Workspace {n}")));
            state.active = state.workspaces.len() - 1;
            save_session(state);
        }
        Message::ToggleNewWsMenu => {
            state.new_ws_menu = !state.new_ws_menu;
            if state.new_ws_menu {
                state.new_ws_menu_x = state.cursor.x; // anchor the dropdown under the +
            }
        }
        Message::CloseNewWsMenu => {
            state.new_ws_menu = false;
        }
        Message::CursorMoved(p) => {
            state.cursor = p;
        }
        Message::UsageUpdated(mut data) => {
            // Only stamp the refresh countdown when real data arrives (not on a
            // needs-login ping), so the countdown reflects the last successful poll.
            if data.state == UsageState::Ok {
                state.usage_updated_ms = now_ms();
            }
            // Multi-org: if we have a saved choice that's still valid, auto-apply it
            // (handles page reloads) instead of re-showing the picker; if it's stale,
            // drop it and show the picker.
            if data.state == UsageState::NeedsOrg {
                match &state.usage_org {
                    Some(saved) if data.orgs.iter().any(|o| &o.uuid == saved) => {
                        usage_helper_cmd(&format!("org:{saved}"));
                        data.state = UsageState::Pending; // wait for the real data
                    }
                    _ => {
                        state.usage_org = None;
                    }
                }
            }
            state.usage = data;
        }
        Message::ShowUsageLogin => usage_show_login(),
        Message::ShowUsageOrgMenu => state.usage_org_menu = true,
        Message::CloseUsageOrgMenu => state.usage_org_menu = false,
        Message::SelectUsageOrg(uuid) => {
            state.usage_org = Some(uuid.clone());
            state.usage_org_menu = false;
            state.usage.state = UsageState::Pending; // loading until the helper replies
            usage_helper_cmd(&format!("org:{uuid}"));
            save_session(state);
        }
        Message::UsageSignOut => {
            usage_helper_cmd("signout");
            state.usage_org = None;
            state.usage = UsageData::default(); // back to Pending (→ NeedsLogin)
            save_session(state);
        }
        Message::RefreshUsage => {
            // Force a refetch and restart the 2-minute countdown immediately.
            usage_helper_cmd("fetch");
            state.usage_updated_ms = now_ms();
        }
        Message::OpenSettings => state.settings_open = true,
        Message::CloseSettings => state.settings_open = false,
        Message::SettingsSelectTab(tab) => state.settings_tab = tab,
        Message::ToggleHideUsageBar(v) => {
            state.settings.hide_usage_bar = v;
            save_session(state);
        }
        Message::ToggleHideSonnetUsage(v) => {
            state.settings.hide_sonnet_usage = v;
            save_session(state);
        }
        Message::ToggleOverviewClaudeOnly(v) => {
            state.settings.overview_claude_only = v;
            save_session(state);
        }
        Message::ToggleHideShellButton(v) => {
            state.settings.hide_shell_button = v;
            save_session(state);
        }
        Message::SetScrollback(s) => {
            // Keep only digits, clamp to the supported range; new terminals pick it
            // up via the global (existing grids keep their current scrollback).
            let digits: String = s.chars().filter(|c| c.is_ascii_digit()).take(6).collect();
            let n = digits
                .parse::<usize>()
                .unwrap_or(persist::SCROLLBACK_MIN)
                .clamp(persist::SCROLLBACK_MIN, persist::SCROLLBACK_MAX);
            state.settings.scrollback = n;
            arbiter_native::term::SCROLLBACK.store(n, std::sync::atomic::Ordering::Relaxed);
            save_session(state);
        }
        Message::ClearSavedData => {
            // Forget the on-disk layout only — live workspaces are untouched.
            persist::clear();
        }
        Message::NewProjectWorkspace => {
            state.new_ws_menu = false;
            // Pick a folder off-thread (native dialog), then validate as a repo.
            return iced::Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Open a Git repository as a project workspace")
                        .pick_folder()
                        .await
                        .map(|h| h.path().to_string_lossy().into_owned())
                },
                Message::ProjectFolderPicked,
            );
        }
        Message::ProjectFolderPicked(Some(path)) => {
            match arbiter_native::git::repo_root(&path) {
                Some(root) => {
                    let infos = arbiter_native::git::worktree_list(&root);
                    state.workspaces.push(new_project(root, infos));
                    state.active = state.workspaces.len() - 1;
                    if let Some(p) = state.active_mut().project.as_mut() {
                        load_explorer(p);
                    }
                    save_session(state);
                }
                None => {
                    // Not a git repo — explain (project workspaces manage worktrees).
                    let _ = rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Warning)
                        .set_title("Not a Git repository")
                        .set_description(format!(
                            "\"{path}\" isn't inside a Git repository. Project workspaces \
                             manage git worktrees, so they need a repo. Use a Terminal \
                             workspace for a plain folder, or run \"git init\" first."
                        ))
                        .show();
                }
            }
        }
        Message::ProjectFolderPicked(None) => {} // dialog cancelled
        Message::SwitchWorktree(i) => {
            activate_worktree(state.active_mut(), i);
        }
        Message::ExplorerToggle(path) => {
            if let Some(p) = state.active_mut().project.as_mut() {
                let ex = &mut p.explorer;
                if ex.expanded.remove(&path) {
                    // collapsed
                } else {
                    ex.expanded.insert(path.clone());
                    if std::path::Path::new(&path).is_dir() {
                        let children = read_dir_entries(&path);
                        p.explorer.entries.insert(path, children);
                    }
                }
            }
        }
        Message::NewWorktree => {
            // Open the dialog, pre-filled with a random name + the current branch as
            // the base, and the repo's branch list for the dropdown.
            if let Some(p) = state.active().project.as_ref() {
                let base = p.worktrees.get(p.active).map(|w| w.branch.clone());
                let branches = arbiter_native::git::list_branches(&p.root);
                state.worktree_dialog =
                    Some(WorktreeDialog { name: random_worktree_name(), base, branches });
                return text_input::focus(text_input::Id::new(WT_NAME_INPUT));
            }
        }
        Message::WtDialogName(s) => {
            if let Some(d) = state.worktree_dialog.as_mut() {
                d.name = s;
            }
        }
        Message::WtDialogPickBase(b) => {
            if let Some(d) = state.worktree_dialog.as_mut() {
                d.base = Some(b);
            }
        }
        Message::WtDialogCancel => {
            state.worktree_dialog = None;
        }
        Message::WtDialogCreate => {
            // The branch name must be non-empty; otherwise keep the dialog open.
            let name = state
                .worktree_dialog
                .as_ref()
                .map(|d| d.name.trim().to_string())
                .unwrap_or_default();
            if name.is_empty() {
                return iced::Task::none();
            }
            let base = state.worktree_dialog.as_ref().and_then(|d| d.base.clone());
            let ws = state.active_mut();
            let Some(root) = ws.project.as_ref().map(|p| p.root.clone()) else {
                return iced::Task::none();
            };
            match arbiter_native::git::worktree_add(&root, &name, base.as_deref()) {
                Ok(info) => {
                    // Build + activate the new worktree (stash the current active one).
                    let (ng, nf) = build_worktree_grid(&info.path);
                    let og = std::mem::replace(&mut ws.panes, ng);
                    let of = std::mem::replace(&mut ws.focus, nf);
                    if let Some(p) = ws.project.as_mut() {
                        let old = p.active;
                        p.worktrees[old].stash = Some((og, of));
                        p.worktrees.push(Worktree {
                            branch: info.branch.unwrap_or(name),
                            path: info.path,
                            stash: None,
                            merged: false,
                            avatar_salt: 0,
                        });
                        p.active = p.worktrees.len() - 1;
                        p.explorer = Explorer::default();
                        load_explorer(p);
                    }
                }
                Err(e) => {
                    // Keep the dialog open so the name/base can be corrected.
                    let _ = rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Error)
                        .set_title("Couldn't create worktree")
                        .set_description(e)
                        .show();
                    return iced::Task::none();
                }
            }
            // Success: close the dialog + persist (the `ws` borrow has ended).
            state.worktree_dialog = None;
            save_session(state);
        }
        Message::WorktreeMenu(i) => {
            state.worktree_menu = Some(i);
        }
        Message::WorktreeMenuClose => {
            state.worktree_menu = None;
        }
        Message::WorktreeMerge(i) => {
            state.worktree_menu = None;
            let Some(p) = state.active().project.as_ref() else { return iced::Task::none() };
            let (Some(feature), Some(main)) = (p.worktrees.get(i), p.worktrees.first()) else {
                return iced::Task::none();
            };
            let feature_branch = feature.branch.clone();
            let main_path = main.path.clone();
            let main_branch = main.branch.clone();
            if !confirm(
                "Merge worktree?",
                &format!("Merge '{feature_branch}' into '{main_branch}'? The worktree is kept (marked merged)."),
            ) {
                return iced::Task::none();
            }
            match arbiter_native::git::merge_branch(&main_path, &feature_branch) {
                Ok(_) => {
                    // Web parity: a plain merge keeps the worktree but marks it
                    // "merged" (greyed). Use "Merge & delete" to remove it.
                    if let Some(p) = state.active_mut().project.as_mut() {
                        if let Some(w) = p.worktrees.get_mut(i) {
                            w.merged = true;
                        }
                        load_explorer(p);
                    }
                    save_session(state);
                    let _ = rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Info)
                        .set_title("Merge complete")
                        .set_description(format!(
                            "Merged '{feature_branch}' into '{main_branch}'. The worktree is kept \
                             and marked merged — use \"Merge & delete\" to remove it."
                        ))
                        .show();
                }
                Err(e) => {
                    let _ = rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Error)
                        .set_title("Merge failed")
                        .set_description(e)
                        .show();
                }
            }
        }
        Message::WorktreeMergeDelete(i) => {
            state.worktree_menu = None;
            if i == 0 {
                return iced::Task::none(); // never the main worktree
            }
            let Some(p) = state.active().project.as_ref() else { return iced::Task::none() };
            let (Some(feature), Some(main)) = (p.worktrees.get(i), p.worktrees.first()) else {
                return iced::Task::none();
            };
            let feature_branch = feature.branch.clone();
            let main_path = main.path.clone();
            let feature_path = feature.path.clone();
            let root = p.root.clone();
            if !confirm(
                "Merge & delete worktree?",
                &format!(
                    "Merge '{feature_branch}' into '{main_branch}', then delete the worktree? Any \
                     uncommitted changes in it will be lost.",
                    main_branch = main.branch
                ),
            ) {
                return iced::Task::none();
            }
            // 1. Merge the feature branch into the main worktree's branch.
            if let Err(e) = arbiter_native::git::merge_branch(&main_path, &feature_branch) {
                let _ = rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("Merge failed")
                    .set_description(format!("{e}\n\nThe worktree was NOT deleted."))
                    .show();
                return iced::Task::none();
            }
            // 2. Switch off it if it's active (can't remove the live worktree), then
            //    remove it (force: the branch is merged, the working copy is expendable).
            if state.active().project.as_ref().map(|p| p.active) == Some(i) {
                activate_worktree(state.active_mut(), 0);
            }
            match arbiter_native::git::worktree_remove(&root, &feature_path, true) {
                Ok(()) => {
                    if let Some(p) = state.active_mut().project.as_mut() {
                        if i < p.worktrees.len() {
                            p.worktrees.remove(i);
                            if p.active > i {
                                p.active -= 1;
                            }
                        }
                        load_explorer(p);
                    }
                    save_session(state);
                }
                Err(e) => {
                    let _ = rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Error)
                        .set_title("Merged, but couldn't remove worktree")
                        .set_description(e)
                        .show();
                }
            }
        }
        Message::WorktreeAskClaudeMerge(i) => {
            state.worktree_menu = None;
            let ws = state.active_mut();
            let (feature, main_branch, main_active) = {
                let Some(p) = ws.project.as_ref() else { return iced::Task::none() };
                (
                    p.worktrees.get(i).map(|w| w.branch.clone()).unwrap_or_default(),
                    p.worktrees.first().map(|w| w.branch.clone()).unwrap_or_default(),
                    p.active == 0,
                )
            };
            let cmd = format!(
                "Please merge the '{feature}' branch into '{main_branch}', resolving any conflicts.\r"
            );
            // The main worktree's grid is `ws.panes` when it's active, else its stash.
            let sent = if main_active {
                send_to_idle_claude(&mut ws.panes, cmd.as_bytes())
            } else if let Some((grid, _)) =
                ws.project.as_mut().and_then(|p| p.worktrees.first_mut()).and_then(|w| w.stash.as_mut())
            {
                send_to_idle_claude(grid, cmd.as_bytes())
            } else {
                false
            };
            if !sent {
                let _ = rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Warning)
                    .set_title("No idle Claude available")
                    .set_description(
                        "Couldn't send the merge request: the main worktree has no idle Claude \
                         session. Open Claude in the main worktree (and wait for it to finish its \
                         current task) before asking it to merge.",
                    )
                    .show();
            }
        }
        Message::WorktreeDiscard(i) => {
            state.worktree_menu = None;
            let info = state
                .active()
                .project
                .as_ref()
                .and_then(|p| p.worktrees.get(i))
                .map(|w| (w.path.clone(), w.branch.clone()));
            if let Some((path, branch)) = info {
                if !confirm(
                    "Discard changes?",
                    &format!("Discard ALL uncommitted changes in '{branch}'? This cannot be undone."),
                ) {
                    return iced::Task::none();
                }
                match arbiter_native::git::discard_changes(&path) {
                    Ok(()) => {
                        if let Some(p) = state.active_mut().project.as_mut() {
                            load_explorer(p);
                        }
                    }
                    Err(e) => {
                        let _ = rfd::MessageDialog::new()
                            .set_level(rfd::MessageLevel::Error)
                            .set_title("Couldn't discard changes")
                            .set_description(e)
                            .show();
                    }
                }
            }
        }
        Message::RegenerateAvatar(i) => {
            state.worktree_menu = None;
            if let Some(p) = state.active_mut().project.as_mut() {
                if let Some(w) = p.worktrees.get_mut(i) {
                    w.avatar_salt = w.avatar_salt.wrapping_add(1);
                }
            }
            save_session(state);
        }
        Message::RemoveWorktree(i) => {
            state.worktree_menu = None;
            if i == 0 {
                return iced::Task::none(); // never the main worktree
            }
            // Confirm (it discards any uncommitted changes; the branch is kept).
            let branch = state
                .active()
                .project
                .as_ref()
                .and_then(|p| p.worktrees.get(i))
                .map(|w| w.branch.clone());
            let Some(branch) = branch else { return iced::Task::none() };
            if !confirm(
                "Delete worktree?",
                &format!(
                    "Delete the worktree for '{branch}'? Any uncommitted changes are lost; the \
                     branch itself is kept (no merge)."
                ),
            ) {
                return iced::Task::none();
            }
            // Can't remove the live worktree — switch to main first if it's active.
            if state.active().project.as_ref().map(|p| p.active) == Some(i) {
                activate_worktree(state.active_mut(), 0);
            }
            let ws = state.active_mut();
            let Some(p) = ws.project.as_mut() else { return iced::Task::none() };
            if i >= p.worktrees.len() {
                return iced::Task::none();
            }
            let root = p.root.clone();
            let path = p.worktrees[i].path.clone();
            match arbiter_native::git::worktree_remove(&root, &path, true) {
                Ok(()) => {
                    p.worktrees.remove(i); // drops its stashed grid → sessions close
                    if p.active > i {
                        p.active -= 1;
                    }
                }
                Err(e) => {
                    let _ = rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Error)
                        .set_title("Couldn't remove worktree")
                        .set_description(e)
                        .show();
                    return iced::Task::none();
                }
            }
            save_session(state); // the `ws`/`p` borrow has ended
        }
        Message::CloseWorkspace(i) => {
            if state.workspaces.len() > 1 && i < state.workspaces.len() {
                state.workspaces.remove(i); // drops it → its sessions close
                if state.active >= i {
                    state.active = state.active.saturating_sub(1);
                }
                state.active = state.active.min(state.workspaces.len() - 1);
                save_session(state);
            }
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
            // Ignore the minimized/off-screen sentinel so we don't persist (and
            // later restore to) an invisible position. Keep the last real one.
            if on_screen_ish(p) {
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
        }
        Message::WindowResized(id, s) => {
            // Skip the degenerate size a minimized window reports, so the saved
            // (restored) size isn't clobbered.
            if s.width >= 100.0 && s.height >= 100.0 {
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
            // macOS resets the traffic-light buttons on resize — re-inset them.
            #[cfg(target_os = "macos")]
            if id == state.main_window {
                trafficlights::position();
            }
            if id == state.main_window {
                // Re-check the display scale (the window may have moved to a
                // different-DPI monitor) so the logo re-rasterizes 1:1.
                let scale = iced::window::get_scale_factor(state.main_window).map(Message::ScaleChanged);
                // Maximize/restore always produces a resize — refresh the caption
                // glyph (maximize ↔ restore). Catches button/double-click/Win+Up/snap.
                #[cfg(target_os = "windows")]
                return iced::Task::batch([
                    scale,
                    iced::window::get_maximized(state.main_window).map(Message::SetMaximized),
                ]);
                #[cfg(not(target_os = "windows"))]
                return scale;
            }
        }
        Message::WindowOpened(id, pos, size) => {
            let known = id == state.main_window || state.overview_window == Some(id);
            if id == state.main_window {
                if let Some(p) = pos.filter(|p| on_screen_ish(*p)) {
                    state.main_pos = Some(p);
                }
                state.main_size = size;
            } else if state.overview_window == Some(id) {
                if let Some(p) = pos.filter(|p| on_screen_ish(*p)) {
                    state.overview_pos = Some(p);
                }
                state.overview_size = size;
            }
            if known {
                save_session(state);
            }
        }
        Message::WindowFocusChanged(id, focused) => {
            if id == state.main_window {
                state.main_focused = focused;
                // macOS can reset the traffic-light buttons on focus changes.
                #[cfg(target_os = "macos")]
                if focused {
                    trafficlights::position();
                }
            }
        }
        Message::ScaleChanged(s) => {
            // Re-rasterize the logo at the new exact physical size so it stays 1:1
            // crisp (e.g. when the window moves to a display with a different DPI).
            if s > 0.0 && (s - state.logo_scale).abs() > 0.01 {
                state.logo_scale = s;
                state.logo = render_logo((LOGO_LOGICAL * s).round() as u32);
                set_ui_scale(s); // crisp titlebar icons rasterise at this scale
            }
        }
        #[cfg(target_os = "windows")]
        Message::DragWindow => return iced::window::drag(state.main_window),
        #[cfg(target_os = "windows")]
        Message::WinMinimize => return iced::window::minimize(state.main_window, true),
        #[cfg(target_os = "windows")]
        Message::WinMaximizeToggle => {
            // Flip optimistically so the glyph swaps instantly; the resize-driven
            // get_maximized query reconciles it.
            state.main_maximized = !state.main_maximized;
            return iced::window::toggle_maximize(state.main_window);
        }
        #[cfg(target_os = "windows")]
        Message::SetMaximized(m) => {
            state.main_maximized = m;
        }
        #[cfg(target_os = "windows")]
        Message::WinClose => return iced::window::close(state.main_window),
        #[cfg(target_os = "windows")]
        Message::WinResize(ht) => winresize::begin(ht),
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

/// Make worktree `target` the active one: swap its stashed grid into the live
/// `panes`/`focus`, stashing the outgoing grid. No-op if already active / invalid.
fn activate_worktree(ws: &mut Workspace, target: usize) {
    let taken = ws.project.as_mut().and_then(|p| {
        if target != p.active && target < p.worktrees.len() {
            p.worktrees.get_mut(target).and_then(|w| w.stash.take()).map(|g| (p.active, g))
        } else {
            None
        }
    });
    if let Some((old, (ng, nf))) = taken {
        let og = std::mem::replace(&mut ws.panes, ng);
        let of = std::mem::replace(&mut ws.focus, nf);
        if let Some(p) = ws.project.as_mut() {
            p.worktrees[old].stash = Some((og, of));
            p.active = target;
            p.explorer = Explorer::default(); // tree reflects the new worktree
            load_explorer(p);
        }
    }
}

fn split(ws: &mut Workspace, axis: pane_grid::Axis) {
    let name = ws.next_name();
    // In a project workspace, new terminals open in the active worktree's folder
    // (the web sets the split's cwd to the worktree path); plain workspaces default.
    let cwd =
        ws.project.as_ref().and_then(|p| p.worktrees.get(p.active)).map(|w| w.path.clone());
    let pane =
        PaneData { session: spawn_session(None, cwd.as_deref()), name, shell: ShellKind::PowerShell };
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

/// Left inset of the titlebar content so it clears the macOS traffic lights
/// (the content extends behind them via fullsize_content_view). Matches the web's
/// `--titlebar-pad-left` (88px on macOS, 6px elsewhere).
#[cfg(target_os = "macos")]
const TITLEBAR_LEFT_PAD: f32 = 88.0;
#[cfg(not(target_os = "macos"))]
const TITLEBAR_LEFT_PAD: f32 = 8.0;

/// Right inset of the titlebar content. Zero on Windows so the custom caption
/// buttons sit flush in the top-right corner like native controls; a small gap
/// elsewhere (macOS controls live on the left).
#[cfg(target_os = "windows")]
const TITLEBAR_RIGHT_PAD: f32 = 0.0;
#[cfg(not(target_os = "windows"))]
const TITLEBAR_RIGHT_PAD: f32 = 8.0;

/// The web's top-left azure glow (`.app::before` radial gradient over the WHOLE
/// app chrome). iced has no radial gradient, so approximate with a diagonal
/// linear one (135° = top-left→bottom-right) fading azure-tinted chrome (#294b6e)
/// → plain chrome (#222222). Used as the app-wide background so the glow flows
/// continuously through the (transparent) titlebar AND the content spacing.
fn app_glow_gradient() -> iced::Gradient {
    iced::gradient::Linear::new(iced::Degrees(135.0))
        .add_stop(0.0, iced::Color::from_rgb8(0x29, 0x4b, 0x6e))
        .add_stop(0.10, iced::Color::from_rgb8(0x25, 0x39, 0x4a))
        .add_stop(0.20, iced::Color::from_rgb8(0x23, 0x2a, 0x31))
        .add_stop(0.32, iced::Color::from_rgb8(0x22, 0x22, 0x22))
        .add_stop(1.0, iced::Color::from_rgb8(0x22, 0x22, 0x22))
        .into()
}

/// Text colour for a file/dir by its git status (web FileExplorerNode), or the
/// default explorer text colour when clean/untracked-by-status.
fn git_status_color(status: Option<&str>) -> iced::Color {
    match status {
        Some("modified") => iced::Color::from_rgb8(0xe2, 0xc0, 0x8d),
        Some("added") | Some("untracked") | Some("renamed") => iced::Color::from_rgb8(0x73, 0xc9, 0x91),
        Some("deleted") => iced::Color::from_rgb8(0xc7, 0x4e, 0x39),
        Some("conflicted") => iced::Color::from_rgb8(0xe5, 0xc0, 0x7b),
        _ => iced::Color::from_rgb8(0xc8, 0xcc, 0xd4),
    }
}

/// Read a directory for the explorer: dirs first then files, alpha
/// (case-insensitive), skipping `.git` and dotfiles. Matches the web `read_directory`.
fn read_dir_entries(dir: &str) -> Vec<DirEntry> {
    let mut out: Vec<DirEntry> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue; // .git + dotfiles
        }
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(DirEntry { name, path: e.path().to_string_lossy().into_owned(), is_dir });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
    out
}

/// (Re)load the active worktree's explorer cache: root + expanded dirs + git
/// status (keyed by absolute path). Called when the cache is stale or files change.
fn load_explorer(project: &mut Project) {
    let Some(wt) = project.worktrees.get(project.active).map(|w| w.path.clone()) else {
        return;
    };
    let ex = &mut project.explorer;
    ex.cached_for = wt.clone();
    ex.entries.clear();
    ex.entries.insert(wt.clone(), read_dir_entries(&wt));
    let expanded: Vec<String> = ex.expanded.iter().cloned().collect();
    for d in expanded {
        if std::path::Path::new(&d).is_dir() {
            ex.entries.insert(d.clone(), read_dir_entries(&d));
        }
    }
    ex.git_status.clear();
    let wtp = std::path::Path::new(&wt);
    for (rel, status) in arbiter_native::git::file_status(&wt) {
        let abs = wtp.join(&rel).to_string_lossy().into_owned();
        ex.git_status.insert(abs, status);
    }
}

/// Flatten the visible tree (root children, recursing into expanded dirs) into
/// (entry, depth) rows for rendering.
fn flatten_tree(ex: &Explorer, dir: &str, depth: usize, out: &mut Vec<(DirEntry, usize)>) {
    if depth > 40 {
        return;
    }
    if let Some(children) = ex.entries.get(dir) {
        for e in children {
            out.push((e.clone(), depth));
            if e.is_dir && ex.expanded.contains(&e.path) {
                flatten_tree(ex, &e.path, depth + 1, out);
            }
        }
    }
}

/// One file-explorer row: indent + chevron (dirs) + name, git-status coloured.
/// Dir rows toggle expand; file rows are inert (open/reveal come with the menu).
fn explorer_row(ex: &Explorer, entry: &DirEntry, depth: usize) -> Element<'static, Message> {
    let color = git_status_color(ex.git_status.get(&entry.path).map(String::as_str));
    let indent = depth as f32 * 16.0;
    // The icon slot: a chevron for directories (the ▸/▾ glyphs aren't in the UI
    // font → tofu), a file-type icon (coloured by type, like the web) for files.
    let icon: Element<Message> = if entry.is_dir {
        let path = if ex.expanded.contains(&entry.path) {
            mdi_path::CHEVRON_DOWN
        } else {
            mdi_path::CHEVRON_RIGHT
        };
        mdi(path, 16.0, iced::Color::from_rgb8(0x9c, 0x9c, 0x9c))
    } else {
        let (path, (r, g, b)) = file_icons::file_icon(&entry.name);
        mdi(path, 16.0, iced::Color::from_rgb8(r, g, b))
    };
    let content = row![
        Space::with_width(Length::Fixed(indent)),
        icon,
        text(entry.name.clone()).size(13).color(color),
    ]
    .spacing(4)
    .align_y(iced::Center);
    if entry.is_dir {
        button(content)
            .width(Length::Fill)
            .padding([2, 8])
            .on_press(Message::ExplorerToggle(entry.path.clone()))
            .style(|_t: &iced::Theme, status| button::Style {
                background: matches!(status, button::Status::Hovered)
                    .then(|| iced::Background::Color(iced::Color::from_rgb8(0x25, 0x25, 0x25))),
                border: iced::Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            })
            .into()
    } else {
        container(content).width(Length::Fill).padding([2, 8]).into()
    }
}

/// Project-workspace sidebar container chrome: same #121212 as the terminals,
/// radius 8.
fn sidebar_style(_t: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgb8(0x12, 0x12, 0x12))),
        border: iced::Border { radius: 8.0.into(), ..Default::default() },
        ..Default::default()
    }
}

/// Uppercase sidebar section header (File explorer branch / "WORKTREES") with an
/// optional trailing widget (e.g. the "+" button). Shared size/weight/colour AND a
/// fixed height so both panels' titles line up identically.
fn sidebar_header<'a>(label: String, trailing: Option<Element<'a, Message>>) -> Element<'a, Message> {
    let mut r = row![
        text(label).size(12).font(ui_semibold()).color(iced::Color::from_rgb8(0xa0, 0xaa, 0xb8)),
        horizontal_space(),
    ]
    .align_y(iced::Center)
    .height(Length::Fixed(34.0))
    .padding([0, 10]);
    if let Some(t) = trailing {
        r = r.push(t);
    }
    r.into()
}

/// Left sidebar: file explorer for the active worktree. Phase 3 = header only
/// (branch name); phase 4 fills in the git-coloured file tree.
fn explorer_sidebar(project: &Project) -> Element<'static, Message> {
    let branch = project.worktrees.get(project.active).map(|w| w.branch.clone()).unwrap_or_default();
    let header = sidebar_header(branch.to_uppercase(), None);
    let mut rows: Vec<(DirEntry, usize)> = Vec::new();
    if let Some(wt) = project.worktrees.get(project.active) {
        flatten_tree(&project.explorer, &wt.path, 0, &mut rows);
    }
    let mut tree = column![].spacing(0);
    for (entry, depth) in rows {
        tree = tree.push(explorer_row(&project.explorer, &entry, depth));
    }
    container(
        column![header, scrollable(tree).width(Length::Fill).height(Length::Fill)]
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fixed(220.0))
    .height(Length::Fill)
    .style(sidebar_style)
    .into()
}

/// Aggregate Claude state across ALL panes of a worktree grid (a worktree can
/// run several Claude instances): counts per lifecycle + the stats of the first
/// one with a capture (for the model + context display).
struct WorktreeClaude {
    working: usize,
    attention: usize,
    idle: usize,
    model: Option<String>,
    percent: Option<f64>,
}

fn worktree_claude(grid: &pane_grid::State<PaneData>) -> WorktreeClaude {
    use arbiter_native::claude_status::Lifecycle;
    let mut wc = WorktreeClaude { working: 0, attention: 0, idle: 0, model: None, percent: None };
    for (_, d) in grid.iter() {
        if !d.session.claude_running() {
            continue;
        }
        let cs = d.session.claude_status();
        match cs.lifecycle {
            Lifecycle::Working => wc.working += 1,
            Lifecycle::Attention => wc.attention += 1,
            _ => wc.idle += 1,
        }
        if wc.model.is_none() && cs.has_stats {
            wc.model = cs.model.clone();
            wc.percent = cs.used_percent;
        }
    }
    wc
}

/// A blocking native Yes/No confirmation. Returns true only on "Yes".
fn confirm(title: &str, body: &str) -> bool {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(title)
        .set_description(body)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

/// Write `bytes` to the first pane in `grid` running a Claude that's accepting
/// input (Ready or Attention — i.e. not mid-task), matching the web's gate.
/// Returns false if there's no such pane (caller warns the user).
fn send_to_idle_claude(grid: &mut pane_grid::State<PaneData>, bytes: &[u8]) -> bool {
    let target = grid
        .iter()
        .find(|(_, d)| {
            d.session.claude_running()
                && matches!(
                    d.session.claude_status().lifecycle,
                    Lifecycle::Ready | Lifecycle::Attention
                )
        })
        .map(|(p, _)| *p);
    match target.and_then(|p| grid.get_mut(p)) {
        Some(d) => {
            d.session.write(bytes);
            true
        }
        None => false,
    }
}

/// A small filled status dot (drawn, not a glyph) of the given colour.
fn status_dot(color: iced::Color) -> Element<'static, Message> {
    container(Space::new(Length::Fixed(8.0), Length::Fixed(8.0)))
        .style(move |_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(color)),
            border: iced::Border { radius: 4.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

/// A random `adjective-noun` worktree branch name (web WorktreeNewDialog), seeded
/// off the clock (no rand dep).
fn random_worktree_name() -> String {
    const ADJ: &[&str] = &[
        "swift", "brave", "clever", "witty", "lucky", "mighty", "silent", "bold", "eager", "jolly",
        "nimble", "quirky", "sunny", "wild", "cosmic", "frosty", "golden", "lunar", "misty", "zesty",
    ];
    const NOUN: &[&str] = &[
        "otter", "falcon", "panda", "tiger", "wolf", "fox", "lynx", "hawk", "badger", "cobra",
        "dragon", "eagle", "gecko", "koala", "narwhal", "octopus", "penguin", "raven", "shark", "whale",
    ];
    let t = now_ms() as usize;
    format!("{}-{}", ADJ[t % ADJ.len()], NOUN[(t / 7) % NOUN.len()])
}

/// HSL → RGB (h in degrees, s/l in 0..1).
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| (((v + m) * 255.0).round()).clamp(0.0, 255.0) as u8;
    (to(r1), to(g1), to(b1))
}

/// The avatar cache key for a worktree: its branch, plus the reroll salt when set.
fn avatar_seed(branch: &str, salt: u32) -> String {
    if salt == 0 {
        branch.to_string()
    } else {
        format!("{branch}#{salt}")
    }
}

/// A rounded-rect path (x,y,w,h with corner radius r).
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> tiny_skia::Path {
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish().unwrap()
}

/// Number of pre-rendered frames in the working-Claude avatar animation.
const ANIM_FRAMES: u32 = 8;

/// Linear interpolate between two RGB colours (t in 0..1).
fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    (l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}

/// A closed polygon path through `pts`.
fn poly(pts: &[(f32, f32)]) -> tiny_skia::Path {
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(pts[0].0, pts[0].1);
    for p in &pts[1..] {
        pb.line_to(p.0, p.1);
    }
    pb.close();
    pb.finish().unwrap()
}

/// A deterministic 64×64 robot avatar drawn from `seed`, at animation `frame`
/// (0 = the neutral/static pose; higher frames bob + pulse the "thinking" LED).
/// Every part — head shape, eye style/count, antenna, mouth, side bolts, and the
/// background pattern — is selected from a distinct slice of the seed's hash, so
/// branches differ in shape, not just colour. Rounded corners; the rest is
/// transparent. tiny-skia outputs premultiplied RGBA, so it's un-premultiplied to
/// match iced's straight-alpha expectation (same as `render_logo`).
fn worktree_avatar(seed: &str, frame: u32) -> iced::widget::image::Handle {
    use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
    // FNV-1a hash, then carve feature selectors from different bit ranges.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let pick = |shift: u32, n: u64| ((h >> shift) % n) as usize;
    let hue = (h % 360) as f32;
    let head_shape = pick(8, 4); // 0 square · 1 rounded · 2 pill · 3 hexagon
    let eye_style = pick(16, 3); // 0 round · 1 square · 2 visor bar
    let eye_count = [2usize, 2, 1, 3, 2][pick(24, 5)]; // mostly 2, sometimes 1 or 3
    let antenna = pick(33, 4); // 0 none · 1 single · 2 twin · 3 dish
    let mouth = pick(41, 4); // 0 line · 1 teeth · 2 dots · 3 smile
    let bolts = pick(49, 3); // 0 none · 1 bolts · 2 ears
    let bg_pat = pick(53, 4); // 0 solid · 1 rings · 2 frame · 3 corner dots

    let bg = hsl_to_rgb(hue, 0.50, 0.26);
    let bg_accent = hsl_to_rgb(hue, 0.45, 0.36);
    let head = hsl_to_rgb((hue + 25.0) % 360.0, 0.52, 0.62);
    let head_dark = hsl_to_rgb((hue + 25.0) % 360.0, 0.45, 0.46);
    let eye_dim = hsl_to_rgb((hue + 185.0) % 360.0, 0.45, 0.42);
    let eye_glow = hsl_to_rgb((hue + 185.0) % 360.0, 0.80, 0.70);

    // Animation: vertical bob + "thinking" LED/eye pulse (both neutral at frame 0).
    let tau = std::f32::consts::TAU;
    let t = frame as f32 / ANIM_FRAMES as f32;
    let oy = (t * tau).sin() * 2.0;
    let glow = 0.55 + 0.45 * (t * tau).cos();
    let eye = lerp_rgb(eye_dim, eye_glow, glow);

    const N: u32 = 64;
    let mut pm = Pixmap::new(N, N).unwrap();
    let mut paint = Paint::default();
    paint.anti_alias = true;
    let id = Transform::identity();
    let set = |paint: &mut Paint, c: (u8, u8, u8)| paint.set_color_rgba8(c.0, c.1, c.2, 255);
    let fill = |pm: &mut Pixmap, paint: &Paint, path: &tiny_skia::Path| {
        pm.fill_path(path, paint, FillRule::Winding, Transform::identity(), None);
    };
    let rect = |pm: &mut Pixmap, paint: &Paint, x, y, w, hh| {
        if let Some(r) = Rect::from_xywh(x, y, w, hh) {
            pm.fill_path(&PathBuilder::from_rect(r), paint, FillRule::Winding, Transform::identity(), None);
        }
    };
    let circle = |pm: &mut Pixmap, paint: &Paint, cx, cy, rad| {
        if let Some(p) = PathBuilder::from_circle(cx, cy, rad) {
            pm.fill_path(&p, paint, FillRule::Winding, Transform::identity(), None);
        }
    };

    // 1. Rounded-rect background (corners stay transparent).
    set(&mut paint, bg);
    fill(&mut pm, &paint, &rounded_rect(0.0, 0.0, 64.0, 64.0, 12.0));

    // 2. Background pattern (accent colour, mostly visible as a frame around the head).
    set(&mut paint, bg_accent);
    match bg_pat {
        1 => {
            if let Some(p) = PathBuilder::from_circle(32.0, 32.0, 27.0) {
                pm.stroke_path(&p, &paint, &Stroke { width: 2.0, ..Default::default() }, id, None);
            }
        }
        2 => {
            rect(&mut pm, &paint, 6.0, 6.0, 52.0, 1.5);
            rect(&mut pm, &paint, 6.0, 56.5, 52.0, 1.5);
            rect(&mut pm, &paint, 6.0, 6.0, 1.5, 52.0);
            rect(&mut pm, &paint, 56.5, 6.0, 1.5, 52.0);
        }
        3 => {
            for (cx, cy) in [(11.0, 11.0), (53.0, 11.0), (11.0, 53.0), (53.0, 53.0)] {
                circle(&mut pm, &paint, cx, cy, 2.5);
            }
        }
        _ => {}
    }

    // 3. Antenna (light; tip is the pulsing eye colour).
    set(&mut paint, head);
    match antenna {
        1 => {
            rect(&mut pm, &paint, 30.5, 6.0 + oy, 3.0, 10.0);
            set(&mut paint, eye);
            circle(&mut pm, &paint, 32.0, 6.0 + oy, 3.5);
            set(&mut paint, head);
        }
        2 => {
            rect(&mut pm, &paint, 22.0, 7.0 + oy, 2.5, 9.0);
            rect(&mut pm, &paint, 39.5, 7.0 + oy, 2.5, 9.0);
            set(&mut paint, eye);
            circle(&mut pm, &paint, 23.0, 7.5 + oy, 2.5);
            circle(&mut pm, &paint, 41.0, 7.5 + oy, 2.5);
            set(&mut paint, head);
        }
        3 => {
            rect(&mut pm, &paint, 30.5, 9.0 + oy, 3.0, 7.0);
            rect(&mut pm, &paint, 25.0, 6.0 + oy, 14.0, 3.5);
        }
        _ => {}
    }

    // 4. Side bolts / ears.
    match bolts {
        1 => {
            set(&mut paint, eye);
            circle(&mut pm, &paint, 12.0, 33.0 + oy, 3.0);
            circle(&mut pm, &paint, 52.0, 33.0 + oy, 3.0);
        }
        2 => {
            set(&mut paint, head_dark);
            rect(&mut pm, &paint, 9.0, 28.0 + oy, 4.0, 12.0);
            rect(&mut pm, &paint, 51.0, 28.0 + oy, 4.0, 12.0);
        }
        _ => {}
    }

    // 5. Head.
    set(&mut paint, head);
    let (hx, hy, hw, hh) = (14.0, 16.0 + oy, 36.0, 34.0);
    match head_shape {
        0 => rect(&mut pm, &paint, hx, hy, hw, hh),
        1 => fill(&mut pm, &paint, &rounded_rect(hx, hy, hw, hh, 8.0)),
        2 => fill(&mut pm, &paint, &rounded_rect(hx, hy, hw, hh, 16.0)),
        _ => {
            let midy = hy + hh / 2.0;
            fill(
                &mut pm,
                &paint,
                &poly(&[
                    (hx, midy),
                    (hx + 9.0, hy),
                    (hx + hw - 9.0, hy),
                    (hx + hw, midy),
                    (hx + hw - 9.0, hy + hh),
                    (hx + 9.0, hy + hh),
                ]),
            );
        }
    }

    // 6. Eyes.
    set(&mut paint, eye);
    let ey = 31.0 + oy;
    if eye_style == 2 {
        fill(&mut pm, &paint, &rounded_rect(20.0, ey - 4.0, 24.0, 8.0, 3.5));
    } else {
        let xs: &[f32] = match eye_count {
            1 => &[32.0],
            3 => &[22.0, 32.0, 42.0],
            _ => &[25.0, 39.0],
        };
        let r = if eye_count == 3 { 3.4 } else { 4.6 };
        for &ex in xs {
            if eye_style == 1 {
                rect(&mut pm, &paint, ex - r, ey - r, r * 2.0, r * 2.0);
            } else {
                circle(&mut pm, &paint, ex, ey, r);
            }
        }
    }

    // 7. Mouth.
    let my = 41.0 + oy;
    match mouth {
        0 => rect(&mut pm, &paint, 24.0, my, 16.0, 3.0),
        1 => {
            rect(&mut pm, &paint, 24.0, my - 1.0, 16.0, 5.0);
            set(&mut paint, bg);
            rect(&mut pm, &paint, 28.0, my - 1.0, 1.5, 5.0);
            rect(&mut pm, &paint, 31.5, my - 1.0, 1.5, 5.0);
            rect(&mut pm, &paint, 35.0, my - 1.0, 1.5, 5.0);
        }
        2 => {
            for ex in [27.0, 32.0, 37.0] {
                circle(&mut pm, &paint, ex, my + 1.5, 1.6);
            }
        }
        _ => {
            let mut pb = PathBuilder::new();
            pb.move_to(25.0, my);
            pb.quad_to(32.0, my + 5.0, 39.0, my);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &paint, &Stroke { width: 2.5, ..Default::default() }, id, None);
            }
        }
    }

    // Un-premultiply (tiny-skia premultiplied → iced straight alpha).
    let mut data = pm.data().to_vec();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            px[0] = ((px[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            px[1] = ((px[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            px[2] = ((px[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
    iced::widget::image::Handle::from_rgba(N, N, data)
}

/// Cached [`worktree_avatar`], keyed by (seed, frame) — each frame is drawn once
/// and the GPU texture is reused as the working animation cycles through frames.
fn avatar_for(seed: &str, frame: u32) -> iced::widget::image::Handle {
    static CACHE: std::sync::Mutex<
        Option<std::collections::HashMap<(String, u32), iced::widget::image::Handle>>,
    > = std::sync::Mutex::new(None);
    let mut guard = CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    let key = (seed.to_string(), frame);
    if let Some(h) = map.get(&key) {
        return h.clone();
    }
    let handle = worktree_avatar(seed, frame);
    map.insert(key, handle.clone());
    handle
}

/// Right sidebar: worktree cards with Claude stats (status / model / context),
/// a "+" to add a worktree, and "×" to remove a non-main one. Click → switch.
fn worktree_sidebar(ws: &Workspace) -> Element<'static, Message> {
    let project = ws.project.as_ref().expect("worktree_sidebar called on a project workspace");
    let muted = iced::Color::from_rgb8(0x6b, 0x7a, 0x8d);
    let azure = iced::Color::from_rgb8(0x33, 0x99, 0xff);
    let orange = iced::Color::from_rgb8(0xe5, 0xa0, 0x3c);
    let purple = iced::Color::from_rgb8(0xa3, 0x71, 0xf7);

    let plus = button(text("+").size(14).color(muted))
        .padding([0, 6])
        .on_press(Message::NewWorktree)
        .style(button::text);
    let header = sidebar_header("WORKTREES".to_string(), Some(plus.into()));

    let mut col = column![header].spacing(2).padding([0, 6]);
    for (i, w) in project.worktrees.iter().enumerate() {
        let active = i == project.active;
        let empty = WorktreeClaude { working: 0, attention: 0, idle: 0, model: None, percent: None };
        let wc = if active {
            worktree_claude(&ws.panes)
        } else {
            w.stash.as_ref().map(|(g, _)| worktree_claude(g)).unwrap_or(empty)
        };
        let total = wc.working + wc.attention + wc.idle;

        let branch_color = if active { azure } else { iced::Color::from_rgb8(0x9c, 0x9c, 0x9c) };
        // Top row: branch (left) · model (top-right) · "⋯" menu. The model is only
        // shown once a Claude here has captured stats.
        let mut top = row![text(w.branch.clone())
            .size(13)
            .font(ui_semibold())
            .color(branch_color)
            .width(Length::Fill)]
        .spacing(6)
        .align_y(iced::Center);
        if let Some(m) = &wc.model {
            let c = clean_model(m);
            top = top.push(text(c.clone()).size(11).color(model_color(&c)));
        }
        top = top.push(
            button(mdi(mdi_path::DOTS_VERTICAL, 16.0, muted))
                .padding([0, 2])
                .on_press(Message::WorktreeMenu(i))
                .style(button::text),
        );
        let mut info = column![top].spacing(3);

        // Status line: one dot+count group per lifecycle (working glyph is the
        // shared animated ✻; attention amber; idle muted). Merged/no-Claude special.
        if w.merged {
            info = info.push(text("Merged").size(11).color(purple));
        } else if total == 0 {
            info = info
                .push(text("Terminal").size(11).color(iced::Color::from_rgba8(0x6b, 0x7a, 0x8d, 0.7)));
        } else {
            let mut status = row![].spacing(8).align_y(iced::Center);
            if wc.working > 0 {
                let (g, c) = working_frame();
                status = status.push(
                    row![
                        text(g).font(symbols_font()).size(13).color(c),
                        text(wc.working.to_string()).size(11).color(azure),
                    ]
                    .spacing(3)
                    .align_y(iced::Center),
                );
            }
            if wc.attention > 0 {
                status = status.push(
                    row![status_dot(orange), text(wc.attention.to_string()).size(11).color(orange)]
                        .spacing(4)
                        .align_y(iced::Center),
                );
            }
            if wc.idle > 0 {
                status = status.push(
                    row![status_dot(muted), text(wc.idle.to_string()).size(11).color(muted)]
                        .spacing(4)
                        .align_y(iced::Center),
                );
            }
            info = info.push(status);
        }

        // Context bar of the first Claude instance with captured stats.
        if let Some(pct) = wc.percent {
            let p = (pct.round() as u16).min(100);
            let fill = if pct > 80.0 {
                iced::Color::from_rgb8(0xef, 0x44, 0x44)
            } else if pct > 60.0 {
                iced::Color::from_rgb8(0xf5, 0x9e, 0x0b)
            } else {
                iced::Color::from_rgb8(0x22, 0xc5, 0x5e)
            };
            // Just the percentage — the context size ("1M"/"200k") is too wide.
            info = info
                .push(text(format!("{p}%")).size(10).color(iced::Color::from_rgb8(0x56, 0x9c, 0xd6)));
            let bar = row![
                container(Space::new(Length::Fill, Length::Fixed(3.0)))
                    .width(Length::FillPortion(p.max(1)))
                    .style(move |_t: &iced::Theme| container::Style {
                        background: Some(iced::Background::Color(fill)),
                        ..Default::default()
                    }),
                container(Space::new(Length::Fill, Length::Fixed(3.0)))
                    .width(Length::FillPortion((100 - p).max(1)))
                    .style(|_t: &iced::Theme| container::Style {
                        background: Some(iced::Background::Color(iced::Color::from_rgb8(0x12, 0x12, 0x12))),
                        ..Default::default()
                    }),
            ]
            .height(Length::Fixed(3.0));
            info = info.push(bar);
        }

        // Card body: a deterministic avatar (left, vertically centred) + the info
        // column. It animates while a Claude here is working, and dims for merged
        // worktrees to match their greyed treatment.
        let frame = if wc.working > 0 {
            ((now_ms() / 110) % ANIM_FRAMES as u64) as u32
        } else {
            0
        };
        let avatar = iced::widget::image(avatar_for(&avatar_seed(&w.branch, w.avatar_salt), frame))
            .width(Length::Fixed(32.0))
            .height(Length::Fixed(32.0))
            .opacity(if w.merged { 0.45 } else { 1.0 })
            .filter_method(iced::widget::image::FilterMethod::Linear);
        let body = row![avatar, info.width(Length::Fill)].spacing(8).align_y(iced::Center);
        let card = mouse_area(
            container(body).width(Length::Fill).padding([8, 10]).style(move |_t: &iced::Theme| {
                container::Style {
                    background: active
                        .then(|| iced::Background::Color(iced::Color::from_rgba8(0x56, 0x9c, 0xd6, 0.12))),
                    border: iced::Border { radius: 6.0.into(), ..Default::default() },
                    ..Default::default()
                }
            }),
        )
        .on_press(Message::SwitchWorktree(i))
        .on_right_press(Message::WorktreeMenu(i));
        col = col.push(card);
    }
    container(scrollable(col).width(Length::Fill).height(Length::Fill))
        .width(Length::Fixed(260.0))
        .height(Length::Fill)
        .style(sidebar_style)
        .into()
}

/// The text_input id of the new-worktree dialog's branch-name field (for autofocus).
const WT_NAME_INPUT: &str = "wt-name-input";

/// The modal layer over the whole window, if a worktree dialog or context menu is
/// open: the new-worktree form, or the right-click actions for a worktree.
fn modal_overlay(state: &State) -> Option<Element<'_, Message>> {
    if state.new_ws_menu {
        return Some(new_ws_menu_view(state.new_ws_menu_x));
    }
    // The org picker layers above Settings (it's reached from the Settings "Switch
    // organization" button), so check it first; dismissing it returns to Settings.
    if state.usage_org_menu {
        return Some(usage_org_menu_view(&state.usage.orgs));
    }
    if state.settings_open {
        return Some(settings_dialog_view(state));
    }
    if let Some(dlg) = &state.worktree_dialog {
        return Some(worktree_dialog_view(dlg));
    }
    state.worktree_menu.map(|i| worktree_menu_view(state, i))
}

/// Centred modal listing the user's claude.ai orgs to pick usage for.
fn usage_org_menu_view(orgs: &[OrgInfo]) -> Element<'static, Message> {
    let mut items = column![text("Choose organization")
        .size(12)
        .font(ui_semibold())
        .color(iced::Color::from_rgb8(0x6b, 0x7a, 0x8d))]
    .spacing(2)
    .padding(iced::Padding { top: 4.0, right: 4.0, bottom: 6.0, left: 12.0 });
    for o in orgs {
        let uuid = o.uuid.clone();
        items = items.push(
            button(text(o.name.clone()).size(13).color(iced::Color::from_rgb8(0xcc, 0xcc, 0xcc)))
                .width(Length::Fill)
                .padding([7, 12])
                .on_press(Message::SelectUsageOrg(uuid))
                .style(|_t: &iced::Theme, s| button::Style {
                    background: matches!(s, button::Status::Hovered)
                        .then(|| iced::Background::Color(AZURE)),
                    text_color: if matches!(s, button::Status::Hovered) {
                        iced::Color::WHITE
                    } else {
                        iced::Color::from_rgb8(0xcc, 0xcc, 0xcc)
                    },
                    ..Default::default()
                }),
        );
    }
    let panel = container(items).padding(8).width(Length::Fixed(280.0));
    modal_scrim(modal_panel(panel.into()), Message::CloseUsageOrgMenu)
}

/// A full-window dimming scrim centring `panel`; a click on the scrim (outside the
/// panel) sends `dismiss`.
fn modal_scrim<'a>(panel: Element<'a, Message>, dismiss: Message) -> Element<'a, Message> {
    mouse_area(
        container(panel).center(Length::Fill).style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba8(0x00, 0x00, 0x00, 0.5))),
            ..Default::default()
        }),
    )
    .on_press(dismiss)
    .into()
}

/// Wrap modal `content` in the panel card (bg #1c1c1c, hairline border, radius 10).
/// A `Noop`-pressing mouse_area swallows clicks so they don't dismiss the scrim.
fn modal_panel<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    mouse_area(container(content).style(|_t: &iced::Theme| container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgb8(0x1c, 0x1c, 0x1c))),
        border: iced::Border {
            color: iced::Color::from_rgba8(0xff, 0xff, 0xff, 0.08),
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    }))
    .on_press(Message::Noop)
    .into()
}

fn worktree_dialog_view(dlg: &WorktreeDialog) -> Element<'_, Message> {
    let label = |s: &str| {
        text(s.to_string()).size(12).color(iced::Color::from_rgb8(0xa0, 0xaa, 0xb8))
    };
    let name_input = text_input("branch-name", &dlg.name)
        .id(text_input::Id::new(WT_NAME_INPUT))
        .on_input(Message::WtDialogName)
        .on_submit(Message::WtDialogCreate)
        .padding([7, 9])
        .size(13);
    let base = pick_list(dlg.branches.as_slice(), dlg.base.clone(), Message::WtDialogPickBase)
        .placeholder("HEAD (current)")
        .padding([7, 9])
        .text_size(13)
        .width(Length::Fill);
    let actions = row![
        horizontal_space(),
        button(text("Cancel").size(13))
            .on_press(Message::WtDialogCancel)
            .style(button::secondary)
            .padding([6, 14]),
        button(text("Create").size(13))
            .on_press(Message::WtDialogCreate)
            .style(button::primary)
            .padding([6, 14]),
    ]
    .spacing(8)
    .align_y(iced::Center);
    let panel = column![
        text("New worktree").size(15).font(ui_semibold()),
        column![label("Branch name"), name_input].spacing(5),
        column![label("Base branch"), base].spacing(5),
        actions,
    ]
    .spacing(14)
    .padding(18)
    .width(Length::Fixed(360.0));
    modal_scrim(modal_panel(panel.into()), Message::WtDialogCancel)
}

fn worktree_menu_view(state: &State, i: usize) -> Element<'_, Message> {
    let Some(p) = state.active().project.as_ref() else {
        return modal_scrim(Space::new(0.0, 0.0).into(), Message::WorktreeMenuClose);
    };
    let Some(wt) = p.worktrees.get(i) else {
        return modal_scrim(Space::new(0.0, 0.0).into(), Message::WorktreeMenuClose);
    };
    let branch = wt.branch.clone();
    let main_branch = p.worktrees.first().map(|w| w.branch.clone()).unwrap_or_default();
    let is_main = i == 0;

    let item = |lbl: String, msg: Message, danger: bool| -> Element<'static, Message> {
        let color = if danger {
            iced::Color::from_rgb8(0xe5, 0x4a, 0x4a)
        } else {
            iced::Color::from_rgb8(0xcc, 0xcc, 0xcc)
        };
        button(text(lbl).size(13).color(color))
            .width(Length::Fill)
            .padding([7, 12])
            .on_press(msg)
            .style(button::text)
            .into()
    };

    let mut items = column![
        text(branch).size(12).font(ui_semibold()).color(iced::Color::from_rgb8(0x6b, 0x7a, 0x8d)),
    ]
    .spacing(2)
    .padding(iced::Padding { top: 4.0, right: 4.0, bottom: 6.0, left: 12.0 });

    if !is_main {
        items = items.push(item(
            format!("Ask Claude to merge into {main_branch}"),
            Message::WorktreeAskClaudeMerge(i),
            false,
        ));
        items = items.push(item(format!("Merge into {main_branch}"), Message::WorktreeMerge(i), false));
        items = items.push(item(
            format!("Merge into {main_branch} & delete"),
            Message::WorktreeMergeDelete(i),
            false,
        ));
    }
    items = items.push(item("New robot".into(), Message::RegenerateAvatar(i), false));
    items = items.push(item("Discard changes".into(), Message::WorktreeDiscard(i), false));
    if !is_main {
        items = items.push(item("Delete worktree".into(), Message::RemoveWorktree(i), true));
    }

    let panel = container(items).padding(8).width(Length::Fixed(280.0));
    modal_scrim(modal_panel(panel.into()), Message::WorktreeMenuClose)
}

// ── Settings dialog (web SettingsDialog.vue: sidebar + tabbed content) ─────────

/// Variants of the web `.btn` family used in Settings.
#[derive(Clone, Copy)]
enum BtnKind {
    Primary,
    Secondary,
    Danger,
}

/// A 1px full-width hairline (web `border-bottom`/`border-top` on sections/footer).
fn settings_hdivider() -> Element<'static, Message> {
    container(Space::new(Length::Fill, Length::Fixed(1.0)))
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x2c, 0x2c, 0x2c))),
            ..Default::default()
        })
        .into()
}

/// A web `.btn` (Primary / Secondary / Danger), exact colours from `color.css`.
fn settings_btn(label: &str, msg: Message, kind: BtnKind) -> Element<'static, Message> {
    button(text(label.to_string()).size(13))
        .padding([8, 16])
        .on_press(msg)
        .style(move |_t: &iced::Theme, s| {
            let hovered = matches!(s, button::Status::Hovered);
            let danger = iced::Color::from_rgb8(0xef, 0x44, 0x44);
            let (bg, tc, bc): (Option<iced::Color>, iced::Color, iced::Color) = match kind {
                BtnKind::Primary => {
                    let c = if hovered { iced::Color::from_rgb8(0x02, 0x7d, 0xff) } else { AZURE };
                    (Some(c), iced::Color::WHITE, c)
                }
                BtnKind::Secondary => (
                    Some(if hovered {
                        iced::Color::from_rgb8(0x12, 0x12, 0x12)
                    } else {
                        iced::Color::from_rgb8(0x1c, 0x1c, 0x1c)
                    }),
                    if hovered { TXT_PRIMARY } else { TXT_SECONDARY },
                    if hovered {
                        iced::Color::from_rgb8(0x3a, 0x3a, 0x3a)
                    } else {
                        iced::Color::from_rgb8(0x2c, 0x2c, 0x2c)
                    },
                ),
                BtnKind::Danger => (
                    hovered.then(|| iced::Color::from_rgba8(0xef, 0x44, 0x44, 0.15)),
                    if hovered { danger } else { TXT_SECONDARY },
                    if hovered {
                        iced::Color::from_rgba8(0xef, 0x44, 0x44, 0.4)
                    } else {
                        iced::Color::from_rgb8(0x2c, 0x2c, 0x2c)
                    },
                ),
            };
            button::Style {
                background: bg.map(iced::Background::Color),
                text_color: tc,
                border: iced::Border { color: bc, width: 1.0, radius: 6.0.into() },
                ..Default::default()
            }
        })
        .into()
}

/// Uppercase section header with a hairline underline (web `.panel-title`).
fn settings_section(label: &str) -> Element<'static, Message> {
    column![
        text(label.to_uppercase()).size(11).font(ui_semibold()).color(TXT_MUTED),
        settings_hdivider(),
    ]
    .spacing(6)
    .into()
}

/// Small muted explanatory text under a section (web `.panel-hint`).
fn settings_hint(s: &str) -> Element<'static, Message> {
    text(s.to_string()).size(12).color(TXT_MUTED).into()
}

/// A toggle row: label (+ optional sub-label) on the left, switch on the right
/// (web `.toggle-row` + `.switch`).
fn settings_toggle(
    label: &str,
    sub: Option<&str>,
    value: bool,
    on_toggle: fn(bool) -> Message,
) -> Element<'static, Message> {
    let mut labels = column![text(label.to_string()).size(13).color(TXT_SECONDARY)].spacing(2);
    if let Some(s) = sub {
        labels = labels.push(text(s.to_string()).size(11).color(TXT_MUTED));
    }
    let tog = toggler(value).size(20.0).on_toggle(on_toggle).style(
        |_t: &iced::Theme, s| {
            let on = matches!(
                s,
                toggler::Status::Active { is_toggled: true } | toggler::Status::Hovered { is_toggled: true }
            );
            toggler::Style {
                background: if on { AZURE } else { iced::Color::from_rgb8(0x2c, 0x2c, 0x2c) },
                background_border_width: 0.0,
                background_border_color: iced::Color::TRANSPARENT,
                foreground: iced::Color::WHITE,
                foreground_border_width: 0.0,
                foreground_border_color: iced::Color::TRANSPARENT,
            }
        },
    );
    container(row![labels, horizontal_space(), tog].spacing(12).align_y(iced::Center))
        .padding([10, 4])
        .into()
}

/// A numeric setting row: label (+ sub) on the left, a small text input on the
/// right (web `.toggle-row` + `.num-input`). Parsing/clamping happens in `update`.
fn settings_number_row(
    label: &str,
    sub: &str,
    value: usize,
    on_input: fn(String) -> Message,
) -> Element<'static, Message> {
    let labels = column![
        text(label.to_string()).size(13).color(TXT_SECONDARY),
        text(sub.to_string()).size(11).color(TXT_MUTED),
    ]
    .spacing(2);
    let input = text_input("", &value.to_string())
        .on_input(on_input)
        .width(Length::Fixed(90.0))
        .padding([6, 8])
        .size(13)
        .style(|_t: &iced::Theme, status| {
            let focused = matches!(status, text_input::Status::Focused);
            text_input::Style {
                background: iced::Background::Color(iced::Color::from_rgb8(0x12, 0x12, 0x12)),
                border: iced::Border {
                    color: if focused { AZURE } else { iced::Color::from_rgb8(0x2c, 0x2c, 0x2c) },
                    width: 1.0,
                    radius: 6.0.into(),
                },
                icon: TXT_MUTED,
                placeholder: TXT_MUTED,
                value: TXT_PRIMARY,
                selection: iced::Color::from_rgba8(0x33, 0x99, 0xff, 0.35),
            }
        });
    container(row![labels, horizontal_space(), input].spacing(12).align_y(iced::Center))
        .padding([10, 4])
        .into()
}

/// One `label  value` line in the account block (web `.account-meta-row`).
fn settings_account_line(label: &str, value: &str) -> Element<'static, Message> {
    row![
        text(label.to_string()).size(12).color(TXT_MUTED).width(Length::Fixed(104.0)),
        text(value.to_string()).size(12).color(TXT_SECONDARY),
    ]
    .spacing(8)
    .into()
}

/// The account block of the Claude Usage tab — mirrors web's account section: who
/// you're signed in as (plan + org), plus Switch-organization / Sign-out (or a
/// Sign-in button when not authenticated).
fn settings_account(u: &UsageData) -> Element<'static, Message> {
    let head = |s: &str| text(s.to_string()).size(13).font(ui_semibold()).color(TXT_PRIMARY);
    match u.state {
        UsageState::Pending => head("Loading…").into(),
        UsageState::NeedsLogin => column![
            head("Not signed in"),
            settings_hint("Sign in to claude.ai to show your usage limits in the titlebar."),
            row![settings_btn("Sign in", Message::ShowUsageLogin, BtnKind::Primary)],
        ]
        .spacing(10)
        .into(),
        UsageState::NeedsOrg => column![
            head("Signed in"),
            settings_hint("Your account has multiple organizations — pick the one to track usage for."),
            row![
                settings_btn("Choose organization", Message::ShowUsageOrgMenu, BtnKind::Primary),
                settings_btn("Sign out", Message::UsageSignOut, BtnKind::Danger),
            ]
            .spacing(8),
        ]
        .spacing(10)
        .into(),
        UsageState::Error => column![
            head("Signed in"),
            settings_account_line("Usage", "Unavailable — try reconnecting"),
            row![
                settings_btn("Reconnect", Message::ShowUsageLogin, BtnKind::Secondary),
                settings_btn("Sign out", Message::UsageSignOut, BtnKind::Danger),
            ]
            .spacing(8),
        ]
        .spacing(10)
        .into(),
        UsageState::Ok => {
            let mut col = column![head("Signed in")].spacing(6);
            if let Some(plan) = &u.plan {
                col = col.push(settings_account_line("Plan", plan));
            }
            if let Some(org) = &u.org_name {
                col = col.push(settings_account_line("Organization", org));
            }
            let mut btns = row![].spacing(8);
            if u.orgs.len() > 1 {
                btns = btns
                    .push(settings_btn("Switch organization", Message::ShowUsageOrgMenu, BtnKind::Secondary));
            }
            btns = btns.push(settings_btn("Sign out", Message::UsageSignOut, BtnKind::Danger));
            col = col.push(Space::with_height(Length::Fixed(4.0))).push(btns);
            col.into()
        }
    }
}

/// One sidebar tab entry (web `.tab`): active tab gets the tinted-white wash.
fn settings_tab_item(label: &str, tab: SettingsTab, active: SettingsTab) -> Element<'static, Message> {
    let is = tab == active;
    button(text(label.to_string()).size(13))
        .width(Length::Fill)
        .padding([7, 10])
        .on_press(Message::SettingsSelectTab(tab))
        .style(move |_t: &iced::Theme, s| {
            let hovered = matches!(s, button::Status::Hovered);
            let (bg, tc) = if is {
                (Some(white_a(0.08)), TXT_PRIMARY)
            } else if hovered {
                (Some(white_a(0.04)), TXT_PRIMARY)
            } else {
                (None, TXT_SECONDARY)
            };
            button::Style {
                background: bg.map(iced::Background::Color),
                text_color: tc,
                border: iced::Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}

/// Wrap a tab's `body` in the scrollable content pane + a footer with Close
/// (web `.content` → `.tab-panel` scroll area + `.dialog-actions`).
fn settings_content(body: Element<'static, Message>) -> Element<'static, Message> {
    column![
        scrollable(container(body).padding(24).width(Length::Fill)).height(Length::Fill),
        settings_hdivider(),
        container(row![horizontal_space(), settings_btn("Close", Message::CloseSettings, BtnKind::Secondary)])
            .padding([12, 16]),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The Settings modal: an 820×600 card (web `.dialog`) — a left sidebar of tabs +
/// a scrollable content pane — over a dimming scrim. Capped to the viewport so it
/// never overflows a small window.
fn settings_dialog_view(state: &State) -> Element<'static, Message> {
    let sidebar = container(
        column![
            text("Settings").size(15).font(ui_semibold()).color(TXT_PRIMARY),
            column![
                settings_tab_item("General", SettingsTab::General, state.settings_tab),
                settings_tab_item("Display", SettingsTab::Display, state.settings_tab),
                settings_tab_item("Claude Usage", SettingsTab::ClaudeUsage, state.settings_tab),
            ]
            .spacing(2),
            Space::with_height(Length::Fill),
            text(format!("Arbiter {} · native", env!("CARGO_PKG_VERSION")))
                .size(11)
                .color(TXT_MUTED),
        ]
        .spacing(14),
    )
    .width(Length::Fixed(184.0))
    .height(Length::Fill)
    .padding(16);

    let body = match state.settings_tab {
        SettingsTab::General => column![
            settings_section("Saved Data"),
            settings_hint(
                "Forget the workspace layout and window geometry Arbiter remembers between launches. \
                 This clears data on disk only — your open terminals aren't affected."
            ),
            row![settings_btn("Clear all saved data", Message::ClearSavedData, BtnKind::Danger)],
        ]
        .spacing(12),
        SettingsTab::Display => {
            let mut col = column![
                settings_section("Overview"),
                settings_toggle(
                    "Only show terminals running Claude",
                    Some("Filter the overview popout to terminals with Claude running."),
                    state.settings.overview_claude_only,
                    Message::ToggleOverviewClaudeOnly,
                ),
                Space::with_height(Length::Fixed(8.0)),
                settings_section("Terminal"),
                settings_number_row(
                    "Scrollback lines",
                    &format!(
                        "Lines kept per terminal ({}–{}). Lower uses less memory. Applies to new terminals.",
                        persist::SCROLLBACK_MIN, persist::SCROLLBACK_MAX
                    ),
                    state.settings.scrollback,
                    Message::SetScrollback,
                ),
            ]
            .spacing(12);
            // The header shell-switch button only exists when Git Bash is found, so
            // only surface its toggle then (mirrors web's Windows-only gate).
            if state.git_bash.is_some() {
                col = col.push(settings_toggle(
                    "Hide Git Bash button in terminal header",
                    None,
                    state.settings.hide_shell_button,
                    Message::ToggleHideShellButton,
                ));
            }
            col
        }
        SettingsTab::ClaudeUsage => column![
            settings_section("Account"),
            settings_account(&state.usage),
            Space::with_height(Length::Fixed(8.0)),
            settings_section("Display"),
            settings_toggle("Hide usage bar", None, state.settings.hide_usage_bar, Message::ToggleHideUsageBar),
            settings_toggle(
                "Hide Sonnet usage",
                Some("Hide the per-model Sonnet meter — Sonnet is rarely the binding limit."),
                state.settings.hide_sonnet_usage,
                Message::ToggleHideSonnetUsage,
            ),
        ]
        .spacing(12),
    };

    let inner = row![sidebar, settings_vdivider(), settings_content(body.into())].height(Length::Fill);
    let card = container(inner)
        .width(Length::Fill)
        .max_width(820.0)
        .height(Length::Fill)
        .max_height(600.0)
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x25, 0x25, 0x25))),
            border: iced::Border {
                color: iced::Color::from_rgb8(0x2c, 0x2c, 0x2c),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });
    // Inner clicks are swallowed (Noop); clicks on the dim margin dismiss.
    let card = mouse_area(card).on_press(Message::Noop);
    mouse_area(
        container(card).center(Length::Fill).padding(24).style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba8(0x00, 0x00, 0x00, 0.5))),
            ..Default::default()
        }),
    )
    .on_press(Message::CloseSettings)
    .into()
}

/// A 1px full-height divider between the sidebar and content (web `border-right`).
fn settings_vdivider() -> Element<'static, Message> {
    container(Space::new(Length::Fixed(1.0), Length::Fill))
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x2c, 0x2c, 0x2c))),
            ..Default::default()
        })
        .into()
}

// ── Titlebar pieces (web parity: WorkspaceTabs.vue + StatsBar.vue + btn-icon) ──

/// Translucent white at alpha `a` — the tab / "+" chrome uses white borders/fills.
fn white_a(a: f32) -> iced::Color {
    iced::Color::from_rgba8(0xff, 0xff, 0xff, a)
}

const TXT_SECONDARY: iced::Color = iced::Color { r: 0xa0 as f32 / 255.0, g: 0xaa as f32 / 255.0, b: 0xb8 as f32 / 255.0, a: 1.0 };
const TXT_PRIMARY: iced::Color = iced::Color { r: 0xe8 as f32 / 255.0, g: 0xea as f32 / 255.0, b: 0xed as f32 / 255.0, a: 1.0 };
const TXT_MUTED: iced::Color = iced::Color { r: 0x6b as f32 / 255.0, g: 0x7a as f32 / 255.0, b: 0x8d as f32 / 255.0, a: 1.0 };
const AZURE: iced::Color = iced::Color { r: 0x33 as f32 / 255.0, g: 0x99 as f32 / 255.0, b: 0xff as f32 / 255.0, a: 1.0 };

/// Truncate `name` to `max_chars`, appending "…" when cut (keeps 3–4 chars min).
fn truncate_name(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        return name.to_string();
    }
    let keep = max_chars.saturating_sub(1).max(1);
    let mut s: String = name.chars().take(keep).collect();
    s.push('…');
    s
}

/// One workspace tab pill (web `.tab`): type icon + name + (×) close, 26px tall,
/// translucent-white border, tinted bg when active. The close is a nested button
/// (it captures its own clicks, so the tab's select fires only elsewhere). The
/// name truncates to `max_chars` so tabs shrink on a narrow window; icon + × stay.
fn tab_pill(i: usize, ws: &Workspace, active: bool, show_close: bool, max_chars: usize) -> Element<'static, Message> {
    let icon = if ws.project.is_some() { mdi_path::FOLDER } else { mdi_path::CONSOLE };
    // Type icon + close go near-white on the active tab (visible), muted otherwise.
    let fg = if active { TXT_PRIMARY } else { TXT_MUTED };
    let mut content = row![cmdi(icon, 12.0, fg), text(truncate_name(&ws.name, max_chars)).size(12)]
        .spacing(4)
        .align_y(iced::Center)
        .height(Length::Fixed(26.0));
    if show_close {
        content = content.push(
            button(cmdi(mdi_path::CLOSE, 13.0, fg))
                .padding(2)
                .on_press(Message::CloseWorkspace(i))
                .style(|_t: &iced::Theme, s| button::Style {
                    background: matches!(s, button::Status::Hovered)
                        .then(|| iced::Background::Color(white_a(0.10))),
                    border: iced::Border { radius: 4.0.into(), ..Default::default() },
                    ..Default::default()
                }),
        );
    }
    button(content)
        .padding([0, 6])
        .on_press(Message::SelectWorkspace(i))
        .style(move |_t: &iced::Theme, s| {
            let hovered = matches!(s, button::Status::Hovered);
            let (bg, bc, tc) = if active {
                (Some(white_a(0.08)), white_a(0.14), TXT_PRIMARY)
            } else if hovered {
                (Some(white_a(0.04)), white_a(0.10), TXT_PRIMARY)
            } else {
                (None, white_a(0.05), TXT_SECONDARY)
            };
            button::Style {
                background: bg.map(iced::Background::Color),
                text_color: tc,
                border: iced::Border { color: bc, width: 1.0, radius: 6.0.into() },
                ..Default::default()
            }
        })
        .into()
}

/// The square "+" new-workspace button (web `.tab-add`).
fn tab_add_button() -> Element<'static, Message> {
    button(
        container(text("+").size(16))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(26.0))
    .height(Length::Fixed(26.0))
    .padding(0)
    .on_press(Message::ToggleNewWsMenu)
    .style(|_t: &iced::Theme, s| {
        let hovered = matches!(s, button::Status::Hovered);
        let (bg, bc, tc) =
            if hovered { (Some(white_a(0.04)), white_a(0.10), TXT_PRIMARY) } else { (None, white_a(0.05), TXT_SECONDARY) };
        button::Style {
            background: bg.map(iced::Background::Color),
            text_color: tc,
            border: iced::Border { color: bc, width: 1.0, radius: 6.0.into() },
            ..Default::default()
        }
    })
    .into()
}

/// A 1px × 22px vertical divider in a dark grey (web `.stat` border-right).
fn vsep() -> Element<'static, Message> {
    container(Space::new(Length::Fixed(1.0), Length::Fixed(22.0)))
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x3a, 0x3a, 0x3a))),
            ..Default::default()
        })
        .into()
}

/// One usage meter (web `.stat`): label + 72×18 bar (track #121212, coloured fill,
/// centred % text) + reset-time text.
// ── Claude usage (fed by the arbiter-usage-helper sidecar over stdout) ─────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum UsageState {
    /// Initial — first fetch in flight (show a loading indicator).
    #[default]
    Pending,
    /// Not signed in to claude.ai (show a Sign-in button).
    NeedsLogin,
    /// Signed in, multiple orgs, none chosen yet (show the org selector).
    NeedsOrg,
    /// Signed in but the usage call failed (show a warning).
    Error,
    /// Have usage data (show the bars).
    Ok,
}

/// A claude.ai organization the user can pick usage for.
#[derive(Clone, Debug)]
struct OrgInfo {
    uuid: String,
    name: String,
}

#[derive(Clone, Copy, Debug)]
struct UsagePeriod {
    utilization: f64,
    resets_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Default)]
struct UsageData {
    state: UsageState,
    five_hour: Option<UsagePeriod>,
    seven_day: Option<UsagePeriod>,
    seven_day_opus: Option<UsagePeriod>,
    seven_day_sonnet: Option<UsagePeriod>,
    /// Plan name from the usage API ("Pro" / "Max" / "Free"), shown in Settings.
    plan: Option<String>,
    /// Display name of the org usage is being read from (shown in Settings).
    org_name: Option<String>,
    /// Org list (for the selector); populated on `NeedsOrg` and on `Ok` so the
    /// "Switch organization" button in Settings always has the list.
    orgs: Vec<OrgInfo>,
}

#[derive(serde::Deserialize)]
struct HelperPeriod {
    utilization: f64,
    resets_at_ms: Option<i64>,
}
#[derive(serde::Deserialize)]
struct HelperOrg {
    uuid: String,
    name: String,
}
#[derive(serde::Deserialize)]
struct HelperLine {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    org_name: Option<String>,
    #[serde(default)]
    orgs: Vec<HelperOrg>,
    #[serde(default)]
    five_hour: Option<HelperPeriod>,
    #[serde(default)]
    seven_day: Option<HelperPeriod>,
    #[serde(default)]
    seven_day_opus: Option<HelperPeriod>,
    #[serde(default)]
    seven_day_sonnet: Option<HelperPeriod>,
}

/// Parse one stdout line from the usage helper into a [`UsageData`].
fn parse_usage_line(line: &str) -> Option<UsageData> {
    let l: HelperLine = serde_json::from_str(line.trim()).ok()?;
    if !l.ok {
        let state = match l.error.as_deref() {
            Some("needs_login") => UsageState::NeedsLogin,
            Some("needs_org") => UsageState::NeedsOrg,
            _ => UsageState::Error,
        };
        let orgs = l.orgs.into_iter().map(|o| OrgInfo { uuid: o.uuid, name: o.name }).collect();
        return Some(UsageData { state, orgs, ..Default::default() });
    }
    let cv = |p: Option<HelperPeriod>| {
        p.map(|x| UsagePeriod { utilization: x.utilization, resets_at_ms: x.resets_at_ms })
    };
    Some(UsageData {
        state: UsageState::Ok,
        five_hour: cv(l.five_hour),
        seven_day: cv(l.seven_day),
        seven_day_opus: cv(l.seven_day_opus),
        seven_day_sonnet: cv(l.seven_day_sonnet),
        plan: l.plan,
        org_name: l.org_name,
        orgs: l.orgs.into_iter().map(|o| OrgInfo { uuid: o.uuid, name: o.name }).collect(),
    })
}

/// Path to the sidecar, next to the main executable.
fn usage_helper_path() -> std::path::PathBuf {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_default();
    let name = if cfg!(windows) { "arbiter-usage-helper.exe" } else { "arbiter-usage-helper" };
    dir.join(name)
}

/// The helper's stdin, so the main app can ask it to raise the sign-in window
/// ("show\n") when the user clicks the titlebar Sign-in button.
static HELPER_STDIN: std::sync::Mutex<Option<std::process::ChildStdin>> = std::sync::Mutex::new(None);

/// Send a line to the usage helper's stdin.
fn usage_helper_cmd(line: &str) {
    if let Some(s) = HELPER_STDIN.lock().unwrap().as_mut() {
        use std::io::Write;
        let _ = s.write_all(line.as_bytes());
        let _ = s.write_all(b"\n");
        let _ = s.flush();
    }
}

/// Ask the usage helper to show its claude.ai sign-in window.
fn usage_show_login() {
    usage_helper_cmd("show");
}

/// Subscription: spawn the usage helper and turn each stdout line into a
/// `UsageUpdated` message. The helper holds the webview; we just read JSON.
fn usage_subscription() -> Subscription<Message> {
    Subscription::run(usage_worker)
}

fn usage_worker() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(8, |mut output| async move {
        use iced::futures::{SinkExt, StreamExt};
        let mut cmd = std::process::Command::new(usage_helper_path());
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let Ok(mut child) = cmd.spawn() else {
            // Helper missing/unspawnable → no usage bars; idle forever.
            std::future::pending::<()>().await;
            unreachable!()
        };
        // Keep the helper's stdin so the Sign-in button can raise its window.
        *HELPER_STDIN.lock().unwrap() = child.stdin.take();
        // Bridge the helper's (blocking) stdout lines into this async task.
        let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<String>();
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.unbounded_send(line).is_err() {
                        break;
                    }
                }
            });
        }
        // Holding `child` keeps its piped stdin open; the helper exits on EOF when
        // this process dies, so no orphan webview.
        while let Some(line) = rx.next().await {
            if let Some(data) = parse_usage_line(&line) {
                let _ = output.send(Message::UsageUpdated(data)).await;
            }
        }
        let _ = child.kill();
        std::future::pending::<()>().await;
    })
}

/// A reset timestamp (epoch ms) → "1d 22h" / "1h 41m" / "15m" (web `formatReset`).
fn fmt_reset(resets_at_ms: Option<i64>) -> String {
    let Some(t) = resets_at_ms else { return "—".to_string() };
    let ms = t - now_ms() as i64;
    if ms <= 0 {
        return "now".to_string();
    }
    let (d, h, m) = (ms / 86_400_000, (ms % 86_400_000) / 3_600_000, (ms % 3_600_000) / 60_000);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// The titlebar usage section + an estimated width (for the responsive budget):
/// the live bars (Ok), a loading indicator (Pending), a Sign-in button
/// (NeedsLogin), or a warning (Error). The bars are a meter per present period
/// (5h blue / 7d green / Opus green / Sonnet blue) + the refresh button.
fn usage_section(
    u: &UsageData,
    updated_ms: u64,
    hide_sonnet: bool,
) -> Option<(Element<'static, Message>, f32)> {
    match u.state {
        UsageState::Pending => {
            Some((row![usage_loading(), vsep()].spacing(8).align_y(iced::Center).into(), 60.0))
        }
        UsageState::NeedsLogin => Some((
            // Trailing divider so the button reads as separate from the actions.
            row![sign_in_button(), vsep()].spacing(8).align_y(iced::Center).into(),
            178.0,
        )),
        UsageState::NeedsOrg => Some((
            row![tinted_pill_button("Choose Claude org", Message::ShowUsageOrgMenu), vsep()]
                .spacing(8)
                .align_y(iced::Center)
                .into(),
            170.0,
        )),
        UsageState::Error => Some((row![usage_warning(), vsep()].spacing(8).align_y(iced::Center).into(), 168.0)),
        UsageState::Ok => {
            let green = iced::Color::from_rgb8(0x22, 0xc5, 0x5e);
            // Sonnet is hidden by default (Settings → "Hide Sonnet usage").
            let sonnet = if hide_sonnet { None } else { u.seven_day_sonnet };
            let entries: [(&str, iced::Color, Option<UsagePeriod>); 4] = [
                ("5h", AZURE, u.five_hour),
                ("7d", green, u.seven_day),
                ("Opus", green, u.seven_day_opus),
                ("Sonnet", AZURE, sonnet),
            ];
            let mut row = row![].spacing(8).align_y(iced::Center);
            let mut n = 0u32;
            for (label, color, period) in entries {
                if let Some(p) = period {
                    row = row.push(usage_stat(label, p.utilization.round() as u16, color, &fmt_reset(p.resets_at_ms)));
                    row = row.push(vsep());
                    n += 1;
                }
            }
            if n == 0 {
                return None;
            }
            row = row.push(refresh_btn(updated_ms));
            Some((row.into(), 60.0 + n as f32 * 150.0))
        }
    }
}

/// Loading indicator while the first usage fetch is in flight: three azure dots
/// pulsing in a wave (re-drawn each tick).
fn usage_loading() -> Element<'static, Message> {
    let dot = |offset_ms: u64| -> Element<'static, Message> {
        // Smooth 0.25→1.0→0.25 pulse over ~1s, each dot phase-shifted.
        let p = (now_ms().wrapping_add(offset_ms) % 1000) as f32 / 1000.0;
        let a = 0.25 + 0.75 * (0.5 - 0.5 * (2.0 * std::f32::consts::PI * p).cos());
        container(Space::new(Length::Fixed(6.0), Length::Fixed(6.0)))
            .style(move |_t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(iced::Color::from_rgba8(0x33, 0x99, 0xff, a))),
                border: iced::Border { radius: 3.0.into(), ..Default::default() },
                ..Default::default()
            })
            .into()
    };
    row![dot(0), dot(333), dot(666)].spacing(4).align_y(iced::Center).into()
}

/// A workspace-tab-shaped pill, azure-tinted (web `.btn-icon.is-active`).
fn tinted_pill_button(label: &str, msg: Message) -> Element<'static, Message> {
    let content = row![text(label.to_string()).size(12)].height(Length::Fixed(26.0)).align_y(iced::Center);
    button(content)
        .padding([0, 10])
        .on_press(msg)
        .style(|_t: &iced::Theme, s| {
            let hovered = matches!(s, button::Status::Hovered);
            button::Style {
                background: Some(iced::Background::Color(iced::Color::from_rgba8(
                    0x33, 0x99, 0xff,
                    if hovered { 0.22 } else { 0.15 },
                ))),
                text_color: iced::Color::from_rgb8(0x8f, 0xc4, 0xff),
                border: iced::Border {
                    color: iced::Color::from_rgba8(0x33, 0x99, 0xff, if hovered { 0.50 } else { 0.35 }),
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

/// Sign-in button shown when not authenticated → raises the helper's webview.
fn sign_in_button() -> Element<'static, Message> {
    tinted_pill_button("Claude Usage Sign In", Message::ShowUsageLogin)
}

/// Warning shown when signed in but the usage fetch failed (amber icon + text);
/// clicking re-opens the sign-in webview to recover.
fn usage_warning() -> Element<'static, Message> {
    let amber = iced::Color::from_rgb8(0xe5, 0xa0, 0x3c);
    button(
        row![cmdi(mdi_path::ALERT_CIRCLE, 14.0, amber), text("Usage unavailable").size(11).color(amber)]
            .spacing(5)
            .align_y(iced::Center),
    )
    .padding([3, 6])
    .on_press(Message::ShowUsageLogin)
    .style(|_t: &iced::Theme, s| button::Style {
        background: matches!(s, button::Status::Hovered)
            .then(|| iced::Background::Color(iced::Color::from_rgb8(0x25, 0x25, 0x25))),
        border: iced::Border { radius: 6.0.into(), ..Default::default() },
        ..Default::default()
    })
    .into()
}

fn usage_stat(label: &str, pct: u16, fill: iced::Color, reset: &str) -> Element<'static, Message> {
    // An 18px absolute line height (= bar height) so the label / % / reset glyphs
    // sit on the same line as the bar. Regular weight on the % avoids the ~1px rise
    // the semibold face has in iced.
    let lh = iced::widget::text::LineHeight::Absolute(iced::Pixels(18.0));
    let p = pct.min(100);
    let bar_row: Element<Message> = row![
        container(Space::new(Length::Fill, Length::Fill))
            .width(Length::FillPortion(p.max(1)))
            .height(Length::Fill)
            .style(move |_t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(fill)),
                border: iced::Border { radius: 3.0.into(), ..Default::default() },
                ..Default::default()
            }),
        Space::with_width(Length::FillPortion((100 - p).max(1))),
    ]
    .height(Length::Fill)
    .into();
    let pct_text: Element<Message> =
        container(text(format!("{p}%")).size(10).color(iced::Color::WHITE).line_height(lh))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    let track = container(iced::widget::stack([bar_row, pct_text]))
        .width(Length::Fixed(72.0))
        .height(Length::Fixed(18.0))
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x12, 0x12, 0x12))),
            border: iced::Border { radius: 4.0.into(), ..Default::default() },
            ..Default::default()
        });
    row![
        text(label.to_string()).size(11).color(TXT_SECONDARY).line_height(lh),
        track,
        text(reset.to_string()).size(11).color(TXT_SECONDARY).line_height(lh),
    ]
    .spacing(5)
    .align_y(iced::Center)
    .into()
}

/// How often usage auto-refreshes (the countdown length). The app drives this on
/// the Tick (the helper's own background timer throttles while hidden).
const USAGE_REFRESH_MS: u64 = 120_000;

/// The usage refresh button (web `.refresh-btn`): shows the countdown to the next
/// auto-poll and, when clicked, refetches now + restarts the countdown.
fn refresh_btn(updated_ms: u64) -> Element<'static, Message> {
    let secs = USAGE_REFRESH_MS / 1000;
    let cd = if updated_ms == 0 {
        secs
    } else {
        secs.saturating_sub(now_ms().saturating_sub(updated_ms) / 1000).min(secs)
    };
    let label = format!("{}:{:02}", cd / 60, cd % 60);
    button(
        row![
            cmdi(mdi_path::REFRESH, 13.0, TXT_MUTED),
            // Fixed-width slot so varying digit widths don't resize the button
            // (iced has no tabular-nums); right-aligned like a timer.
            container(
                text(label)
                    .size(11)
                    .color(TXT_SECONDARY)
                    .line_height(iced::widget::text::LineHeight::Absolute(iced::Pixels(13.0))),
            )
            .width(Length::Fixed(26.0))
            .align_x(iced::alignment::Horizontal::Right),
        ]
        .spacing(4)
        .align_y(iced::Center),
    )
    .padding([5, 7])
    .on_press(Message::RefreshUsage)
    .style(|_t: &iced::Theme, s| button::Style {
        background: None,
        border: iced::Border {
            // Match the #3a3a3a separators (the #2c2c2c card border was too faint
            // against the titlebar glow).
            color: if matches!(s, button::Status::Hovered) {
                AZURE
            } else {
                iced::Color::from_rgb8(0x3a, 0x3a, 0x3a)
            },
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// A right-side action icon button (web `.btn-icon`): 16px icon, transparent until
/// hover; `active` shows the azure-dim selected wash (used for the overview toggle).
fn action_icon_btn(path: &'static str, msg: Message, active: bool) -> Element<'static, Message> {
    let color = if active { AZURE } else { TXT_SECONDARY };
    button(cmdi(path, 16.0, color))
        .padding([4, 6])
        .on_press(msg)
        .style(move |_t: &iced::Theme, s| {
            let (bg, bc) = if active {
                (Some(iced::Color::from_rgba8(0x33, 0x99, 0xff, 0.15)), iced::Color::from_rgba8(0x33, 0x99, 0xff, 0.35))
            } else if matches!(s, button::Status::Hovered) {
                (Some(iced::Color::from_rgb8(0x25, 0x25, 0x25)), iced::Color::from_rgb8(0x2c, 0x2c, 0x2c))
            } else {
                (None, iced::Color::TRANSPARENT)
            };
            button::Style {
                background: bg.map(iced::Background::Color),
                border: iced::Border { color: bc, width: 1.0, radius: 6.0.into() },
                ..Default::default()
            }
        })
        .into()
}

/// One item in the "+" dropdown (web `.new-menu-item`): icon + label, azure hover.
fn new_ws_menu_item(icon: &'static str, label: &str, msg: Message) -> Element<'static, Message> {
    button(
        row![cmdi(icon, 14.0, TXT_SECONDARY), text(label.to_string()).size(12)]
            .spacing(8)
            .align_y(iced::Center),
    )
    .width(Length::Fill)
    .padding([6, 12])
    .on_press(msg)
    .style(|_t: &iced::Theme, s| {
        let hovered = matches!(s, button::Status::Hovered);
        button::Style {
            background: hovered.then(|| iced::Background::Color(AZURE)),
            text_color: if hovered { iced::Color::WHITE } else { TXT_SECONDARY },
            ..Default::default()
        }
    })
    .into()
}

/// The "+" dropdown overlay (web `.new-menu`): pick Terminal or Project workspace.
/// Anchored below the titlebar near the tab area (iced can't read the +'s screen
/// position, so the left inset is a fixed approximation).
fn new_ws_menu_view(anchor_x: f32) -> Element<'static, Message> {
    let menu = container(
        column![
            new_ws_menu_item(mdi_path::CONSOLE, "Terminal Workspace", Message::NewWorkspace),
            new_ws_menu_item(mdi_path::FOLDER, "Project Workspace", Message::NewProjectWorkspace),
        ]
        .spacing(0),
    )
    .width(Length::Fixed(180.0))
    .padding([4, 0])
    .style(|_t: &iced::Theme| container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgb8(0x25, 0x25, 0x25))),
        border: iced::Border {
            color: iced::Color::from_rgb8(0x2c, 0x2c, 0x2c),
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    });
    // Anchor the menu's left edge just under the click on the "+" (web `.new-menu`
    // opens at rect.left, bottom+2). 40px titlebar → top 42.
    let left = (anchor_x - 4.0).max(4.0);
    let anchored = container(mouse_area(menu).on_press(Message::Noop))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding { top: 42.0, right: 0.0, bottom: 0.0, left });
    mouse_area(anchored).on_press(Message::CloseNewWsMenu).into()
}

/// The whole titlebar row, laid out for the available width `avail_w` (from
/// `responsive`): the right-side action buttons always show and take priority;
/// the usage bars + refresh drop out first when space is tight; the tabs shrink
/// (names truncate to a char budget, with icon + × always visible) inside a
/// clipped band so they never push the actions off-screen; "+" always shows.
fn titlebar_row(state: &State, avail_w: f32) -> Element<'_, Message> {
    let brand = row![
        iced::widget::image(state.logo.clone())
            .width(Length::Fixed(LOGO_LOGICAL))
            .height(Length::Fixed(LOGO_LOGICAL))
            .filter_method(iced::widget::image::FilterMethod::Linear),
        arbiter_wordmark(),
    ]
    .spacing(8)
    .align_y(iced::Center);
    #[cfg(target_os = "windows")]
    let brand = mouse_area(brand).on_press(Message::DragWindow);

    // Approximate widths of the fixed regions (logical px) to budget the tab band.
    const BRAND_W: f32 = 106.0;
    const PLUS_W: f32 = 30.0;
    const TAB_MIN: f32 = 60.0;
    #[cfg(target_os = "windows")]
    let caption_w = 140.0;
    #[cfg(not(target_os = "windows"))]
    let caption_w = 0.0;
    let actions_w = 204.0 + caption_w; // 6 btn-icons (+ Windows caption strip)
    let n = state.workspaces.len().max(1) as f32;
    let avail = (avail_w - BRAND_W - PLUS_W - actions_w - 30.0).max(0.0);
    // Usage section (bars / loading / sign-in / warning), built once with its own
    // width estimate so the tab budget and the actual push agree. Shown when its
    // width still leaves a minimum tab band — unless hidden in Settings.
    let usage_el = if state.settings.hide_usage_bar {
        None
    } else {
        usage_section(&state.usage, state.usage_updated_ms, state.settings.hide_sonnet_usage)
    };
    let usage_w = usage_el.as_ref().map(|(_, w)| *w).unwrap_or(0.0);
    let show_usage = usage_el.is_some() && (avail - usage_w) >= (n * TAB_MIN);
    let tab_area = (if show_usage { avail - usage_w } else { avail }).max(TAB_MIN);
    let per = tab_area / n;
    let multi = state.workspaces.len() > 1;
    let fixed = if multi { 52.0 } else { 30.0 }; // icon + padding + (× when multi)
    let max_chars = (((per - fixed) / 6.5).floor() as i32).clamp(3, 40) as usize;

    // Every tab truncates to the same `max_chars`, so each is ≤ tab_area/n and the
    // row never exceeds the band — no clip needed (clipping cut the last tab's ×).
    let mut tabs = row![].spacing(3).align_y(iced::Center);
    for (i, ws) in state.workspaces.iter().enumerate() {
        tabs = tabs.push(tab_pill(i, ws, i == state.active, multi, max_chars));
    }

    let mut bar = row![brand, Space::with_width(Length::Fixed(12.0)), tabs, tab_add_button()]
        .spacing(6)
        .align_y(iced::Center)
        .height(Length::Fill);

    // Flexible middle (drag region on Windows; spacer on macOS).
    #[cfg(target_os = "windows")]
    {
        bar = bar.push(mouse_area(Space::new(Length::Fill, Length::Fill)).on_press(Message::DragWindow));
    }
    #[cfg(not(target_os = "windows"))]
    {
        bar = bar.push(horizontal_space());
    }

    if let (true, Some((usage, _))) = (show_usage, usage_el) {
        bar = bar.push(usage);
    }
    bar = bar.push(
        row![
            action_icon_btn(mdi_path::VIEW_DASHBOARD, Message::ToggleOverview, state.overview_window.is_some()),
            action_icon_btn(mdi_path::ARROW_ALL, Message::Noop, false),
            action_icon_btn(mdi_path::COG, Message::OpenSettings, state.settings_open),
            action_icon_btn(mdi_path::ARROW_RIGHT, Message::SplitRight, false),
            action_icon_btn(mdi_path::ARROW_DOWN, Message::SplitDown, false),
            action_icon_btn(mdi_path::CLOSE, Message::Close, false),
        ]
        .spacing(4)
        .align_y(iced::Center),
    );
    #[cfg(target_os = "windows")]
    {
        let f = state.main_focused;
        let mid = if state.main_maximized { caption_glyph::RESTORE } else { caption_glyph::MAXIMIZE };
        bar = bar.push(
            row![
                caption_button(caption_glyph::MINIMIZE, Message::WinMinimize, false, f),
                caption_button(mid, Message::WinMaximizeToggle, false, f),
                caption_button(caption_glyph::CLOSE, Message::WinClose, true, f),
            ]
            .spacing(0),
        );
    }
    bar.into()
}

fn main_view(state: &State) -> Element<'_, Message> {
    // Unified titlebar: Arbiter logo + animated wordmark, then workspace tabs
    // (left) + actions (right). On macOS this IS the window titlebar (content
    // extends behind it; traffic lights overlay the left pad).
    let focus = state.active().focus;
    let font = &state.font;
    // The header's shell-switch button shows only when Git Bash is available AND
    // it isn't hidden in Settings (web `devStore.hideShellButton`).
    let has_git_bash = state.git_bash.is_some() && !state.settings.hide_shell_button;
    // The terminal area's four OUTER corners are rounded (web
    // `.terminal-workspace-card` border-radius: 8px). Find the leaf pane owning
    // each corner so only those round — never interior corners where panes meet.
    let layout = state.active().panes.layout();
    let (c_tl, c_tr) = (corner_pane(layout, false, false), corner_pane(layout, true, false));
    let (c_bl, c_br) = (corner_pane(layout, false, true), corner_pane(layout, true, true));
    let grid = pane_grid::PaneGrid::new(&state.active().panes, move |pane, data, _maximized| {
        // 2px of left breathing room so glyphs don't touch the pane's left edge
        // (the pane's own #121212 shows through the gap; the renderer derives its
        // cols from the shrunken width). Transparent container → no colour seam.
        let term = container(
            shader_widget(TermProgram {
                id: data.session.id(),
                pane,
                term: data.session.term(),
                master: data.session.master(),
                font: font.clone(),
            })
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 2.0 });

        let focused = pane == focus;
        // Which of the grid's outer corners this pane owns → round those. The
        // header covers the top corners, the footer the bottom, and the pane's own
        // #121212 background must round to match so the glow shows through.
        let pick = |yes: bool| if yes { 8.0_f32 } else { 0.0 };
        let (rtl, rtr) = (pane == c_tl, pane == c_tr);
        let (rbl, rbr) = (pane == c_bl, pane == c_br);
        let header_round = iced::border::Radius {
            top_left: pick(rtl),
            top_right: pick(rtr),
            bottom_right: 0.0,
            bottom_left: 0.0,
        };
        let footer_round = iced::border::Radius {
            top_left: 0.0,
            top_right: 0.0,
            bottom_right: pick(rbr),
            bottom_left: pick(rbl),
        };
        let pane_round = iced::border::Radius {
            top_left: pick(rtl),
            top_right: pick(rtr),
            bottom_right: pick(rbr),
            bottom_left: pick(rbl),
        };
        // Claude status indicator in the header while Claude runs in this pane.
        let status = data
            .session
            .claude_running()
            .then(|| pane_dot(true, data.session.claude_status().lifecycle, false));
        let header = pane_header(&data.name, focused, data.shell, has_git_bash, pane, status, header_round);
        // 1px #2c2c2c dividers under the header and above the footer (web card look).
        let content = column![header, hline(), term, hline(), footer_bar(&data.session, footer_round)]
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
            .style(move |_t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(iced::Color::from_rgb8(0x12, 0x12, 0x12))),
                border: iced::Border { radius: pane_round, ..Default::default() },
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
            // Round the whole grid's outer rect (radius 8) so the corner panes'
            // rounded corners reveal the glow behind, not this divider colour.
            border: iced::Border { radius: 8.0.into(), ..Default::default() },
            ..Default::default()
        });

    // Titlebar: web height (40px), left-padded for the traffic lights. Transparent
    // so the app-wide azure glow shows through it. The bar's available width comes
    // from the tracked window size (not a `responsive`/lazy widget — that ran an
    // extra layout pass on every resize event and made the macOS traffic lights
    // flicker), so tabs/usage shrink/hide via `titlebar_row`.
    let bar_w = (state.main_size.width - TITLEBAR_LEFT_PAD - TITLEBAR_RIGHT_PAD).max(0.0);
    let titlebar = container(titlebar_row(state, bar_w))
        .width(Length::Fill)
        .height(Length::Fixed(40.0))
        .padding(iced::Padding { top: 0.0, right: TITLEBAR_RIGHT_PAD, bottom: 0.0, left: TITLEBAR_LEFT_PAD });

    // Workspace body, inset from the window edges (web padding `0 6px 6px` — flush
    // under the titlebar, 6px on the other three sides). A terminal workspace is
    // just the grid; a project workspace is explorer | grid | worktrees (6px gaps,
    // matching the web `.project-workspace`).
    let inner: Element<Message> = match state.active().project.as_ref() {
        Some(project) => row![explorer_sidebar(project), grid, worktree_sidebar(state.active())]
            .spacing(6)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        None => grid.into(),
    };
    let framed = container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding { top: 0.0, right: 6.0, bottom: 6.0, left: 6.0 });

    // App-wide chrome background carrying the top-left azure glow, so it's
    // continuous across the titlebar and the content spacing (no hard #222222 edge).
    let chrome = container(column![titlebar, framed].width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Gradient(app_glow_gradient())),
            ..Default::default()
        });
    // Windows: overlay thin resize hit-zones at the window edges via a stack so
    // the content layout/spacing stays byte-identical to macOS (no extra inset).
    // The stack delivers a press to the top layer first and stops if it captures,
    // so an edge press resizes without also triggering the titlebar drag beneath.
    #[cfg(target_os = "windows")]
    let base: Element<Message> = iced::widget::stack([chrome.into(), resize_overlay()]).into();
    #[cfg(not(target_os = "windows"))]
    let base: Element<Message> = chrome.into();

    // A worktree dialog / context menu, if open, layers over everything else.
    match modal_overlay(state) {
        Some(modal) => iced::widget::stack([base, modal]).into(),
        None => base,
    }
}

/// A full-window overlay of thin resize hit-zones for the borderless Windows
/// window (a decorations-off winit window has no OS resize hit-zones and iced
/// 0.13 exposes no drag-resize). Edge/corner `mouse_area`s sit at the window
/// edges (within the existing 6px content border, so no layout shift); the centre
/// is a non-interactive `Space` that lets presses fall through to the content
/// beneath in the stack. On press each zone hands the OS a synthetic
/// `WM_NCLBUTTONDOWN` + `HT*` code (see `winresize`) — the same trick as drag.
#[cfg(target_os = "windows")]
fn resize_overlay<'a>() -> Element<'a, Message> {
    use iced::mouse::Interaction;
    const T: f32 = 4.0; // edge grab thickness (inside the 6px content border)
    const C: f32 = 16.0; // corner grab length
    // Win32 HT* hit-test codes.
    const HTLEFT: usize = 10;
    const HTRIGHT: usize = 11;
    const HTTOP: usize = 12;
    const HTTOPLEFT: usize = 13;
    const HTTOPRIGHT: usize = 14;
    const HTBOTTOM: usize = 15;
    const HTBOTTOMLEFT: usize = 16;
    const HTBOTTOMRIGHT: usize = 17;
    let zone = |w: Length, h: Length, ht: usize, cur: Interaction| -> Element<'a, Message> {
        mouse_area(Space::new(w, h)).on_press(Message::WinResize(ht)).interaction(cur).into()
    };
    column![
        row![
            zone(Length::Fixed(C), Length::Fixed(T), HTTOPLEFT, Interaction::ResizingDiagonallyDown),
            zone(Length::Fill, Length::Fixed(T), HTTOP, Interaction::ResizingVertically),
            zone(Length::Fixed(C), Length::Fixed(T), HTTOPRIGHT, Interaction::ResizingDiagonallyUp),
        ]
        .height(Length::Fixed(T)),
        row![
            zone(Length::Fixed(T), Length::Fill, HTLEFT, Interaction::ResizingHorizontally),
            Space::new(Length::Fill, Length::Fill),
            zone(Length::Fixed(T), Length::Fill, HTRIGHT, Interaction::ResizingHorizontally),
        ]
        .height(Length::Fill),
        row![
            zone(Length::Fixed(C), Length::Fixed(T), HTBOTTOMLEFT, Interaction::ResizingDiagonallyUp),
            zone(Length::Fill, Length::Fixed(T), HTBOTTOM, Interaction::ResizingVertically),
            zone(Length::Fixed(C), Length::Fixed(T), HTBOTTOMRIGHT, Interaction::ResizingDiagonallyDown),
        ]
        .height(Length::Fixed(T)),
    ]
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
        .push(container(text("ARBITER").size(11).font(ui_semibold()).color(muted)).padding([8, 12]));

    let only_claude = state.settings.overview_claude_only;
    for (wi, ws) in state.workspaces.iter().enumerate() {
        // Optionally list only terminals running Claude (Settings → Display). A
        // workspace with nothing to show is skipped entirely.
        let panes: Vec<_> = ws
            .panes
            .iter()
            .filter(|(_, data)| !only_claude || data.session.claude_running())
            .collect();
        if panes.is_empty() {
            continue;
        }
        // Workspace title + terminal count.
        let header = row![
            text(ws.name.to_uppercase()).size(10).color(muted),
            horizontal_space(),
            text(panes.len().to_string()).size(9).color(muted),
        ]
        .padding([3, 12])
        .align_y(iced::Center);
        col = col.push(header);

        for (pane, data) in panes {
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
        // Same as the terminal (web `--color-bg` #121212); the 1px #2c2c2c top
        // border is the `hline()` added above the footer in the pane's content column.
        background: Some(iced::Background::Color(iced::Color::from_rgb8(0x12, 0x12, 0x12))),
        text_color: Some(iced::Color::from_rgb8(0x9c, 0x9c, 0x9c)),
        ..Default::default()
    }
}

/// A 1px full-width divider line in the card-border colour (#2c2c2c) — the web's
/// `.pane-toolbar` bottom border / `.terminal-footer` top border, which iced's
/// single-width `Border` can't do per-side.
fn hline() -> Element<'static, Message> {
    container(Space::new(Length::Fill, Length::Fixed(1.0)))
        .width(Length::Fill)
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x2c, 0x2c, 0x2c))),
            ..Default::default()
        })
        .into()
}

/// Compact token count: 4200 → "4.2K". TRUNCATES to one decimal (NOT round) to
/// match Claude's status line, which formats via `bc scale=1` (e.g. 20450 →
/// "20.4K", not "20.5K") — same as the web's `fmtK`.
fn fmt_k(n: u64) -> String {
    if n >= 1000 {
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

// ── Footer (matches the web TerminalFooter.vue: same colours + MDI icons) ──────

/// An MDI 24×24 path rendered at `size` px, filled with `color`.
fn mdi(path: &str, size: f32, color: iced::Color) -> Element<'static, Message> {
    let b = |v: f32| (v * 255.0).round() as u8;
    let src = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#{:02x}{:02x}{:02x}" d="{path}"/></svg>"##,
        b(color.r), b(color.g), b(color.b),
    );
    svg(svg::Handle::from_memory(src.into_bytes())).width(size).height(size).into()
}

/// Current display scale (physical px per logical px), tracked from the main
/// window so crisp icons can rasterise at the exact pixel size without threading
/// it through every helper. Defaults to 2.0 (the common Mac case) until known.
static UI_SCALE_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
fn set_ui_scale(s: f32) {
    UI_SCALE_BITS.store(s.to_bits(), std::sync::atomic::Ordering::Relaxed);
}
fn ui_scale() -> f32 {
    let b = UI_SCALE_BITS.load(std::sync::atomic::Ordering::Relaxed);
    if b == 0 { 2.0 } else { f32::from_bits(b) }
}

/// Rasterise an SVG to an RGBA `image::Handle` at exactly `px`×`px` physical
/// pixels (crisp at the display scale, unlike the `svg` widget). Same path as
/// `render_logo`; un-premultiplies (tiny-skia premult → iced straight alpha).
fn raster_svg(svg_bytes: &[u8], px: u32) -> iced::widget::image::Handle {
    use resvg::{tiny_skia, usvg};
    let px = px.max(1);
    let mut pm = tiny_skia::Pixmap::new(px, px).unwrap();
    if let Ok(tree) = usvg::Tree::from_data(svg_bytes, &usvg::Options::default()) {
        let s = tree.size();
        let scale = (px as f32 / s.width()).min(px as f32 / s.height());
        resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pm.as_mut());
    }
    let mut rgba = pm.data().to_vec();
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 {
            p[0] = (p[0] as u32 * 255 / a) as u8;
            p[1] = (p[1] as u32 * 255 / a) as u8;
            p[2] = (p[2] as u32 * 255 / a) as u8;
        }
    }
    iced::widget::image::Handle::from_rgba(px, px, rgba)
}

/// Crisp MDI icon: a 24×24 `path` filled `color`, rasterised at `size`×scale and
/// shown 1:1. Cached by (path, colour, px). Use in the titlebar where the soft
/// `svg`-widget icons (refresh/keyboard/etc.) read as pixelated.
fn cmdi(path: &'static str, size: f32, color: iced::Color) -> Element<'static, Message> {
    static CACHE: std::sync::Mutex<
        Option<std::collections::HashMap<(usize, u32, u32), iced::widget::image::Handle>>,
    > = std::sync::Mutex::new(None);
    let px = (size * ui_scale()).round().max(1.0) as u32;
    let b = |v: f32| (v * 255.0).round() as u32;
    let rgb = (b(color.r) << 16) | (b(color.g) << 8) | b(color.b);
    let key = (path.as_ptr() as usize, rgb, px);
    let handle = {
        let mut guard = CACHE.lock().unwrap();
        let map = guard.get_or_insert_with(std::collections::HashMap::new);
        if let Some(h) = map.get(&key) {
            h.clone()
        } else {
            let bb = |v: f32| (v * 255.0).round() as u8;
            let src = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path fill="#{:02x}{:02x}{:02x}" d="{path}"/></svg>"##,
                bb(color.r), bb(color.g), bb(color.b),
            );
            let h = raster_svg(src.as_bytes(), px);
            map.insert(key, h.clone());
            h
        }
    };
    iced::widget::image(handle)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        // Nearest at the exact physical size = pixel-crisp (Linear softens icons
        // whose on-screen position lands on a fractional pixel).
        .filter_method(iced::widget::image::FilterMethod::Nearest)
        .into()
}

/// Per-family model colour (web: `.model-opus`/`.model-sonnet`/…).
fn model_color(model: &str) -> iced::Color {
    let l = model.to_ascii_lowercase();
    if l.contains("opus") {
        iced::Color::from_rgb8(0x4e, 0xc9, 0xb0)
    } else if l.contains("sonnet") {
        iced::Color::from_rgb8(0x9c, 0xdc, 0xfe)
    } else if l.contains("haiku") {
        iced::Color::from_rgb8(0xb5, 0xce, 0xa8)
    } else {
        iced::Color::from_rgb8(0xe8, 0xea, 0xed)
    }
}

/// The model display name without any context-window suffix Claude's statusLine
/// may append — "Opus 4.8 (1M context)" / "Opus 4.8 · 1M context" → "Opus 4.8".
/// Too wide for the titlebar otherwise. A model name with no such suffix is kept
/// verbatim (the trailing size-token drop only runs once a "context" word is seen).
fn clean_model(m: &str) -> String {
    let mut s = m.trim();
    let mut had_context = false;
    // Parenthetical form: "Opus 4.8 (1M context)".
    if let Some(i) = s.find('(') {
        if s[i..].to_ascii_lowercase().contains("context") {
            s = s[..i].trim_end();
            had_context = true;
        }
    }
    // Inline form: "Opus 4.8 · 1M context".
    if !had_context {
        let lower = s.to_ascii_lowercase();
        if let Some(i) = lower.find("context") {
            s = s[..i].trim_end();
            had_context = true;
        }
    }
    if !had_context {
        return s.to_string();
    }
    // Drop a trailing size token left behind (e.g. "1M"/"200k").
    let mut toks: Vec<&str> =
        s.split(|c: char| matches!(c, ' ' | '·' | '•' | '|')).filter(|t| !t.is_empty()).collect();
    if let Some(last) = toks.last() {
        let l = last.to_ascii_lowercase();
        let is_size =
            last.chars().any(|c| c.is_ascii_digit()) && (l.ends_with('m') || l.ends_with('k') || l.ends_with('g'));
        if is_size {
            toks.pop();
        }
    }
    toks.join(" ")
}

mod mdi_path {
    pub const FOLDER: &str = "M20,18H4V8H20M20,6H12L10,4H4C2.89,4 2,4.89 2,6V18A2,2 0 0,0 4,20H20A2,2 0 0,0 22,18V8C22,6.89 21.1,6 20,6Z";
    pub const DOTS_VERTICAL: &str = "M12,16A2,2 0 0,1 14,18A2,2 0 0,1 12,20A2,2 0 0,1 10,18A2,2 0 0,1 12,16M12,10A2,2 0 0,1 14,12A2,2 0 0,1 12,14A2,2 0 0,1 10,12A2,2 0 0,1 12,10M12,4A2,2 0 0,1 14,6A2,2 0 0,1 12,8A2,2 0 0,1 10,6A2,2 0 0,1 12,4Z";
    pub const BRANCH: &str = "M13,14C9.64,14 8.54,15.35 8.18,16.24C9.25,16.7 10,17.76 10,19A3,3 0 0,1 7,22A3,3 0 0,1 4,19C4,17.69 4.83,16.58 6,16.17V7.83C4.83,7.42 4,6.31 4,5A3,3 0 0,1 7,2A3,3 0 0,1 10,5C10,6.31 9.17,7.42 8,7.83V13.12C8.88,12.47 10.16,12 12,12C14.67,12 15.56,10.66 15.85,9.77C14.77,9.32 14,8.25 14,7A3,3 0 0,1 17,4A3,3 0 0,1 20,7C20,8.34 19.12,9.5 17.91,9.86C17.65,11.29 16.68,14 13,14M7,18A1,1 0 0,0 6,19A1,1 0 0,0 7,20A1,1 0 0,0 8,19A1,1 0 0,0 7,18M7,4A1,1 0 0,0 6,5A1,1 0 0,0 7,6A1,1 0 0,0 8,5A1,1 0 0,0 7,4M17,6A1,1 0 0,0 16,7A1,1 0 0,0 17,8A1,1 0 0,0 18,7A1,1 0 0,0 17,6Z";
    pub const ROBOT: &str = "M17.5 15.5C17.5 16.61 16.61 17.5 15.5 17.5S13.5 16.61 13.5 15.5 14.4 13.5 15.5 13.5 17.5 14.4 17.5 15.5M8.5 13.5C7.4 13.5 6.5 14.4 6.5 15.5S7.4 17.5 8.5 17.5 10.5 16.61 10.5 15.5 9.61 13.5 8.5 13.5M23 15V18C23 18.55 22.55 19 22 19H21V20C21 21.11 20.11 22 19 22H5C3.9 22 3 21.11 3 20V19H2C1.45 19 1 18.55 1 18V15C1 14.45 1.45 14 2 14H3C3 10.13 6.13 7 10 7H11V5.73C10.4 5.39 10 4.74 10 4C10 2.9 10.9 2 12 2S14 2.9 14 4C14 4.74 13.6 5.39 13 5.73V7H14C17.87 7 21 10.13 21 14H22C22.55 14 23 14.45 23 15M21 16H19V14C19 11.24 16.76 9 14 9H10C7.24 9 5 11.24 5 14V16H3V17H5V20H19V17H21V16Z";
    pub const DATABASE: &str = "M12,3C7.58,3 4,4.79 4,7C4,9.21 7.58,11 12,11C16.42,11 20,9.21 20,7C20,4.79 16.42,3 12,3M4,9V12C4,14.21 7.58,16 12,16C16.42,16 20,14.21 20,12V9C20,11.21 16.42,13 12,13C7.58,13 4,11.21 4,9M4,14V17C4,19.21 7.58,21 12,21C16.42,21 20,19.21 20,17V14C20,16.21 16.42,18 12,18C7.58,18 4,16.21 4,14Z";
    pub const ARROW_DOWN: &str = "M11,4H13V16L18.5,10.5L19.92,11.92L12,19.84L4.08,11.92L5.5,10.5L11,16V4Z";
    pub const ARROW_UP: &str = "M13,20H11V8L5.5,13.5L4.08,12.08L12,4.16L19.92,12.08L18.5,13.5L13,8V20Z";
    pub const CACHED: &str = "M19,8L15,12H18A6,6 0 0,1 12,18C11,18 10.03,17.75 9.2,17.3L7.74,18.76C8.97,19.54 10.43,20 12,20A8,8 0 0,0 20,12H23M6,12A6,6 0 0,1 12,6C13,6 13.97,6.25 14.8,6.7L16.26,5.24C15.03,4.46 13.57,4 12,4A8,8 0 0,0 4,12H1L5,16L9,12";
    pub const BOOK: &str = "M19 2L14 6.5V17.5L19 13V2M6.5 5C4.55 5 2.45 5.4 1 6.5V21.16C1 21.41 1.25 21.66 1.5 21.66C1.6 21.66 1.65 21.59 1.75 21.59C3.1 20.94 5.05 20.5 6.5 20.5C8.45 20.5 10.55 20.9 12 22C13.35 21.15 15.8 20.5 17.5 20.5C19.15 20.5 20.85 20.81 22.25 21.56C22.35 21.61 22.4 21.59 22.5 21.59C22.75 21.59 23 21.34 23 21.09V6.5C22.4 6.05 21.75 5.75 21 5.5V19C19.9 18.65 18.7 18.5 17.5 18.5C15.8 18.5 13.35 19.15 12 20V6.5C10.55 5.4 8.45 5 6.5 5Z";
    pub const CHECK_CIRCLE: &str = "M12 2C6.5 2 2 6.5 2 12S6.5 22 12 22 22 17.5 22 12 17.5 2 12 2M12 20C7.59 20 4 16.41 4 12S7.59 4 12 4 20 7.59 20 12 16.41 20 12 20M16.59 7.58L10 14.17L7.41 11.59L6 13L10 17L18 9L16.59 7.58Z";
    pub const CIRCLE_EDIT: &str = "M12,2A10,10 0 0,0 2,12A10,10 0 0,0 12,22A10,10 0 0,0 22,12H20A8,8 0 0,1 12,20A8,8 0 0,1 4,12A8,8 0 0,1 12,4V2M18.78,3C18.61,3 18.43,3.07 18.3,3.2L17.08,4.41L19.58,6.91L20.8,5.7C21.06,5.44 21.06,5 20.8,4.75L19.25,3.2C19.12,3.07 18.95,3 18.78,3M16.37,5.12L9,12.5V15H11.5L18.87,7.62L16.37,5.12Z";
    pub const PLUS_CIRCLE: &str = "M12,20C7.59,20 4,16.41 4,12C4,7.59 7.59,4 12,4C16.41,4 20,7.59 20,12C20,16.41 16.41,20 12,20M12,2A10,10 0 0,0 2,12A10,10 0 0,0 12,22A10,10 0 0,0 22,12A10,10 0 0,0 12,2M13,7H11V11H7V13H11V17H13V13H17V11H13V7Z";
    // File-explorer expand/collapse chevrons (the ▸/▾ glyphs tofu in the UI font).
    pub const CHEVRON_RIGHT: &str = "M8.59,16.58L13.17,12L8.59,7.41L10,6L16,12L10,18L8.59,16.58Z";
    pub const CHEVRON_DOWN: &str = "M7.41,8.58L12,13.17L16.59,8.58L18,10L12,16L6,10L7.41,8.58Z";
    // Titlebar: tab type icon (terminal), tab close, new-workspace dropdown items,
    // usage-bar refresh, and the right-side action buttons.
    pub const CONSOLE: &str = "M20,19V7H4V19H20M20,3A2,2 0 0,1 22,5V19A2,2 0 0,1 20,21H4A2,2 0 0,1 2,19V5C2,3.89 2.9,3 4,3H20M13,17V15H18V17H13M9.58,13L5.57,9H8.4L11.7,12.3C12.09,12.69 12.09,13.33 11.7,13.72L8.42,17H5.59L9.58,13Z";
    pub const CLOSE: &str = "M19,6.41L17.59,5L12,10.59L6.41,5L5,6.41L10.59,12L5,17.59L6.41,19L12,13.41L17.59,19L19,17.59L13.41,12L19,6.41Z";
    pub const REFRESH: &str = "M17.65,6.35C16.2,4.9 14.21,4 12,4A8,8 0 0,0 4,12A8,8 0 0,0 12,20C15.73,20 18.84,17.45 19.73,14H17.65C16.83,16.33 14.61,18 12,18A6,6 0 0,1 6,12A6,6 0 0,1 12,6C13.66,6 15.14,6.69 16.22,7.78L13,11H20V4L17.65,6.35Z";
    pub const VIEW_DASHBOARD: &str = "M19,5V7H15V5H19M9,5V11H5V5H9M19,13V19H15V13H19M9,17V19H5V17H9M21,3H13V9H21V3M11,3H3V13H11V3M21,11H13V21H21V11M11,15H3V21H11V15Z";
    pub const COG: &str = "M12,8A4,4 0 0,1 16,12A4,4 0 0,1 12,16A4,4 0 0,1 8,12A4,4 0 0,1 12,8M12,10A2,2 0 0,0 10,12A2,2 0 0,0 12,14A2,2 0 0,0 14,12A2,2 0 0,0 12,10M10,22C9.75,22 9.54,21.82 9.5,21.58L9.13,18.93C8.5,18.68 7.96,18.34 7.44,17.94L4.95,18.95C4.73,19.03 4.46,18.95 4.34,18.73L2.34,15.27C2.21,15.05 2.27,14.78 2.46,14.63L4.57,12.97L4.5,12L4.57,11L2.46,9.37C2.27,9.22 2.21,8.95 2.34,8.73L4.34,5.27C4.46,5.05 4.73,4.96 4.95,5.05L7.44,6.05C7.96,5.66 8.5,5.32 9.13,5.07L9.5,2.42C9.54,2.18 9.75,2 10,2H14C14.25,2 14.46,2.18 14.5,2.42L14.87,5.07C15.5,5.32 16.04,5.66 16.56,6.05L19.05,5.05C19.27,4.96 19.54,5.05 19.66,5.27L21.66,8.73C21.79,8.95 21.73,9.22 21.54,9.37L19.43,11L19.5,12L19.43,13L21.54,14.63C21.73,14.78 21.79,15.05 21.66,15.27L19.66,18.73C19.54,18.95 19.27,19.04 19.05,18.95L16.56,17.95C16.04,18.34 15.5,18.68 14.87,18.93L14.5,21.58C14.46,21.82 14.25,22 14,22H10M11.25,4L10.88,6.61C9.68,6.86 8.62,7.5 7.85,8.39L5.44,7.35L4.69,8.65L6.8,10.2C6.4,11.37 6.4,12.64 6.8,13.8L4.68,15.36L5.43,16.66L7.86,15.62C8.63,16.5 9.68,17.14 10.87,17.38L11.24,20H12.76L13.13,17.39C14.32,17.14 15.37,16.5 16.14,15.62L18.57,16.66L19.32,15.36L17.2,13.81C17.6,12.64 17.6,11.37 17.2,10.2L19.31,8.65L18.56,7.35L16.15,8.39C15.38,7.5 14.32,6.86 13.12,6.62L12.75,4H11.25Z";
    pub const ARROW_RIGHT: &str = "M4,11V13H16L10.5,18.5L11.92,19.92L19.84,12L11.92,4.08L10.5,5.5L16,11H4Z";
    // Keyboard-shortcuts button: a 4-way arrow cross (reads like the arrow keys,
    // crisp at 16px — the full keyboard glyph is too dense to read small).
    pub const ARROW_ALL: &str = "M13,11H18L16.5,9.5L17.92,8.08L21.84,12L17.92,15.92L16.5,14.5L18,13H13V18L14.5,16.5L15.92,17.92L12,21.84L8.08,17.92L9.5,16.5L11,18V13H6L7.5,14.5L6.08,15.92L2.16,12L6.08,8.08L7.5,9.5L6,11H11V6L9.5,7.5L8.08,6.08L12,2.16L15.92,6.08L14.5,7.5L13,6V11Z";
    // Usage error indicator: a "!" in a circle.
    pub const ALERT_CIRCLE: &str = "M11,15H13V17H11V15M11,7H13V13H11V7M12,2C6.47,2 2,6.5 2,12A10,10 0 0,0 12,22A10,10 0 0,0 22,12A10,10 0 0,0 12,2M12,20A8,8 0 0,1 4,12A8,8 0 0,1 12,4A8,8 0 0,1 20,12A8,8 0 0,1 12,20Z";
}

/// Style for a Windows titlebar control button: no chrome until hover, then a
/// grey wash — or Windows' red for the close button. Square corners + flush so
/// the row reads as one native caption-control strip.
#[cfg(target_os = "windows")]
fn winctl_style(close: bool) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_t, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let bg = hovered.then(|| {
            iced::Background::Color(if close {
                iced::Color::from_rgb8(0xc4, 0x2b, 0x1c)
            } else {
                iced::Color::from_rgb8(0x3a, 0x3a, 0x3a)
            })
        });
        button::Style { background: bg, ..Default::default() }
    }
}

// Win11 caption glyphs (Segoe Fluent Icons ChromeMinimize/Maximize/Close) are
// trivial 1px geometry — reproduce them exactly as thin-stroked SVG so they
// match pixel-for-pixel without depending on the system icon font: a centred
// horizontal line, a plain square outline (no title-bar notch), and an X. Drawn
// in a padded 0–12 viewBox so end-caps never clip. Each entry is
// `(path, render_px, stroke_width)` — easy knobs to tune the on-screen size.
#[cfg(target_os = "windows")]
mod caption_glyph {
    pub const MINIMIZE: (&str, f32, f32) = ("M1,6 H11", 12.0, 1.0);
    pub const MAXIMIZE: (&str, f32, f32) = ("M1.5,1.5 H10.5 V10.5 H1.5 Z", 12.0, 1.15);
    // Restore (shown when maximized): a front square + the visible L of a square
    // offset behind it, top-right — matches Segoe Fluent's ChromeRestore.
    pub const RESTORE: (&str, f32, f32) =
        ("M1.5,4 H8 V10.5 H1.5 Z M4,4 V1.5 H10.5 V8 H8", 12.0, 1.0);
    pub const CLOSE: (&str, f32, f32) = ("M1.5,1.5 L10.5,10.5 M10.5,1.5 L1.5,10.5", 12.0, 1.0);
}

/// A thin-stroked Win11 caption glyph (0–12 viewBox), stroked in `color`.
#[cfg(target_os = "windows")]
fn caption_icon(d: &str, color: iced::Color, render: f32, stroke: f32) -> Element<'static, Message> {
    let b = |v: f32| (v * 255.0).round() as u8;
    let src = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><path d="{d}" fill="none" stroke="#{:02x}{:02x}{:02x}" stroke-width="{stroke}"/></svg>"##,
        b(color.r), b(color.g), b(color.b),
    );
    svg(svg::Handle::from_memory(src.into_bytes()))
        .width(Length::Fixed(render))
        .height(Length::Fixed(render))
        .into()
}

/// A native-proportioned Windows caption button: a 46×40 hit-area (Win11 sizing)
/// with a centred glyph, square corners, hover wash via `winctl_style`. The glyph
/// is white when the window is focused and dimmed when it isn't, like native.
#[cfg(target_os = "windows")]
fn caption_button(glyph: (&str, f32, f32), msg: Message, close: bool, focused: bool) -> Element<'static, Message> {
    let color = if focused {
        iced::Color::from_rgb8(0xff, 0xff, 0xff)
    } else {
        iced::Color::from_rgb8(0x6e, 0x6e, 0x6e)
    };
    let (d, render, stroke) = glyph;
    button(
        container(caption_icon(d, color, render, stroke))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(46.0))
    .height(Length::Fixed(40.0))
    .padding(0)
    .on_press(msg)
    .style(winctl_style(close))
    .into()
}

/// Force Windows 11 rounded corners on our borderless top-level windows.
///
/// iced/winit run the window as `decorations(false)`, so Win11 doesn't auto-round
/// it. iced exposes no HWND, so we find our process's top-level windows via
/// `EnumWindows` and set `DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND` — a
/// persistent attribute, so a single call (on the first frame) is enough; DWM
/// keeps it across maximize/restore. Raw FFI to keep the dependency surface
/// unchanged. (Effect needs a real compositor — it shows on hardware but a VM
/// with software DWM may stay square.)
#[cfg(target_os = "windows")]
mod winround {
    use std::ffi::c_void;

    type Hwnd = *mut c_void;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: u32 = 2;

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(Hwnd, isize) -> i32, l: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
    }
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: Hwnd, attr: u32, val: *const c_void, sz: u32) -> i32;
    }

    extern "system" fn enum_cb(hwnd: Hwnd, _l: isize) -> i32 {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            // Match by process only (rounding a hidden helper window is a harmless
            // no-op); don't gate on visibility — the window may not be shown yet.
            if pid == GetCurrentProcessId() {
                let pref: u32 = DWMWCP_ROUND;
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    &pref as *const u32 as *const c_void,
                    std::mem::size_of::<u32>() as u32,
                );
            }
        }
        1 // keep enumerating
    }

    /// Round every top-level window owned by this process. Idempotent.
    pub fn round_our_windows() {
        unsafe {
            EnumWindows(enum_cb, 0);
        }
    }
}

/// Reposition the macOS traffic lights to match the web (Tauri's
/// `trafficLightPosition: { x: 14, y: 22 }`). iced/winit expose no API, so we
/// reach the NSWindow via `NSApplication` and inset the standard buttons — the
/// same algorithm tao uses. Must be re-applied after layout changes (resize /
/// focus), since macOS resets the buttons. No-op until the window exists.
#[cfg(target_os = "macos")]
mod trafficlights {
    use objc2::{class, msg_send, runtime::AnyObject};
    use objc2_foundation::{NSRect, NSString};
    use std::sync::atomic::{AtomicBool, Ordering};

    // Web parity: Tauri `trafficLightPosition { x: 14, y: 22 }`.
    const INSET_X: f64 = 14.0;
    const INSET_Y: f64 = 22.0;

    static OBSERVER_REGISTERED: AtomicBool = AtomicBool::new(false);

    pub fn position() {
        unsafe {
            ensure_resize_observer();
            position_inner();
        }
    }

    /// Re-inset synchronously whenever AppKit posts `NSWindowDidResize` — this runs
    /// inside AppKit's own resize pass, so the buttons never get a frame at the
    /// default position (the iced `WindowResized` message lags a frame → flicker).
    /// Registered once; the block is leaked (lives for the app's lifetime).
    unsafe fn ensure_resize_observer() {
        if OBSERVER_REGISTERED.swap(true, Ordering::SeqCst) {
            return;
        }
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        if center.is_null() {
            OBSERVER_REGISTERED.store(false, Ordering::SeqCst);
            return;
        }
        let block = block2::RcBlock::new(|_note: *mut AnyObject| unsafe { position_inner() });
        let name = NSString::from_str("NSWindowDidResizeNotification");
        let nil: *mut AnyObject = std::ptr::null_mut();
        let _: *mut AnyObject = msg_send![
            center,
            addObserverForName: &*name,
            object: nil,
            queue: nil,
            usingBlock: &*block,
        ];
        std::mem::forget(block); // the observer holds it for the app's lifetime
    }

    unsafe fn position_inner() {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        // Enumerate ALL our windows — NOT `mainWindow`/`keyWindow`, which are nil
        // while the app is inactive (e.g. launched unfocused), so the startup
        // positioning would no-op until first click. The `windows` array is always
        // available. Target our main window by its transparent titlebar (set via
        // platform_specific.titlebar_transparent) — that skips the overview popout
        // (a normal-titlebar window that shouldn't be inset).
        let windows: *mut AnyObject = msg_send![app, windows];
        if windows.is_null() {
            return;
        }
        let count: usize = msg_send![windows, count];
        for i in 0..count {
            let window: *mut AnyObject = msg_send![windows, objectAtIndex: i];
            if window.is_null() {
                continue;
            }
            let transparent: bool = msg_send![window, titlebarAppearsTransparent];
            if transparent {
                inset_window(window);
            }
        }
    }

    unsafe fn inset_window(window: *mut AnyObject) {
        // NSWindowButton: Close = 0, Miniaturize = 1, Zoom = 2.
        let close: *mut AnyObject = msg_send![window, standardWindowButton: 0usize];
        let mini: *mut AnyObject = msg_send![window, standardWindowButton: 1usize];
        let zoom: *mut AnyObject = msg_send![window, standardWindowButton: 2usize];
        if close.is_null() || mini.is_null() || zoom.is_null() {
            return;
        }
        // The titlebar container is two superviews above the close button.
        let sv: *mut AnyObject = msg_send![close, superview];
        if sv.is_null() {
            return;
        }
        let titlebar: *mut AnyObject = msg_send![sv, superview];
        if titlebar.is_null() {
            return;
        }
        let close_rect: NSRect = msg_send![close, frame];
        let win_rect: NSRect = msg_send![window, frame];
        let tb_height = close_rect.size.height + INSET_Y;
        let mut tb_rect: NSRect = msg_send![titlebar, frame];
        tb_rect.size.height = tb_height;
        tb_rect.origin.y = win_rect.size.height - tb_height;
        let _: () = msg_send![titlebar, setFrame: tb_rect];

        let mini_rect: NSRect = msg_send![mini, frame];
        let space = mini_rect.origin.x - close_rect.origin.x;
        for (i, btn) in [close, mini, zoom].into_iter().enumerate() {
            let mut r: NSRect = msg_send![btn, frame];
            r.origin.x = INSET_X + (i as f64) * space;
            let _: () = msg_send![btn, setFrameOrigin: r.origin];
        }
    }
}

/// Interactive edge/corner resize for the borderless Windows window. winit/iced
/// give a decorations-off window no resize hit-zones, so we drive the OS's own
/// modal resize loop directly: `ReleaseCapture()` then a non-client button-down
/// (`WM_NCLBUTTONDOWN`) with the edge's `HT*` code to the foreground window (the
/// one whose edge was just clicked).
///
/// We **POST** the message rather than SEND it. SendMessage would run the OS
/// modal resize loop *synchronously, nested inside iced's `update()`* — iced is
/// then mid-event (renderer/surface borrowed) and can't run its own
/// resize-render path re-entrantly, so the GPU surface never repaints and the
/// content visibly stretches until release. PostMessage returns immediately;
/// iced finishes the event, and the modal loop runs in winit's *next* dispatch
/// (fresh stack), where iced's per-`Resized` redraw (it emulates an `AboutToWait`
/// after each resize on Windows) repaints every step — smooth resize.
#[cfg(target_os = "windows")]
mod winresize {
    use std::ffi::c_void;
    type Hwnd = *mut c_void;
    const WM_NCLBUTTONDOWN: u32 = 0x00A1;

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> Hwnd;
        fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
        fn ReleaseCapture() -> i32;
        fn PostMessageW(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
    }

    /// Begin a native resize in the direction given by a Win32 `HT*` code.
    pub fn begin(ht: usize) {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != GetCurrentProcessId() {
                return;
            }
            ReleaseCapture();
            PostMessageW(hwnd, WM_NCLBUTTONDOWN, ht, 0);
        }
    }
}

fn footer_bar(session: &Session, round: iced::border::Radius) -> Element<'static, Message> {
    let muted = iced::Color::from_rgb8(0x6b, 0x7a, 0x8d);
    let primary = iced::Color::from_rgb8(0xe8, 0xea, 0xed);
    let blue = iced::Color::from_rgb8(0x56, 0x9c, 0xd6);
    let green = iced::Color::from_rgb8(0x6a, 0x99, 0x55);
    let orange = iced::Color::from_rgb8(0xe5, 0xa0, 0x3c);
    let git_orange = iced::Color::from_rgb8(0xf0, 0x50, 0x32);
    // Icons and text are each placed in an identical LINE-tall, vertically-centred
    // box, so their contents land on the same line (iced's default 1.3× text line
    // height otherwise rides the glyph lower than a centred icon of the same size).
    const LINE: f32 = 16.0;
    let lh = iced::widget::text::LineHeight::Absolute(iced::Pixels(LINE));
    let fi = move |path: &'static str, size: f32, col: iced::Color| -> Element<'static, Message> {
        container(mdi(path, size, col)).center_y(Length::Fixed(LINE)).into()
    };
    let lbl = move |s: String, col: iced::Color| text(s).size(11).color(col).line_height(lh);
    // Inter Semibold renders ~1px higher than regular in the same line box; wrap it
    // in a LINE-tall box with 1px top padding to nudge it back down to match `lbl`
    // (the box stays LINE tall, so the row height / icons don't move).
    let sbl = move |s: String, col: iced::Color| {
        container(text(s).size(11).color(col).font(ui_semibold()).line_height(lh))
            .height(Length::Fixed(LINE))
            .padding(iced::Padding { top: 1.0, right: 0.0, bottom: 0.0, left: 0.0 })
    };
    let div = move || text("|").size(11).color(iced::Color::from_rgb8(0x3a, 0x3a, 0x3a)).line_height(lh);

    let c = session.claude_status();
    let mut r = row![].spacing(6).align_y(iced::Center);

    if session.claude_running() && c.has_stats {
        if let Some(m) = &c.model {
            let mc = model_color(m);
            r = r.push(
                row![fi(mdi_path::ROBOT, 13.0, mc), sbl(m.clone(), mc)].spacing(3).align_y(iced::Center),
            );
        }
        if let Some(p) = c.used_percent {
            let size = c.context_size.map(fmt_ctx_size).unwrap_or_default();
            r = r.push(div());
            r = r.push(
                row![
                    fi(mdi_path::DATABASE, 12.0, blue),
                    sbl(format!("{p:.0}%"), blue),
                    lbl(format!("/{size}"), muted),
                ]
                .spacing(2)
                .align_y(iced::Center),
            );
        }
        r = r.push(div());
        let tin = iced::Color::from_rgb8(0x4e, 0xc9, 0xb0);
        let tout = iced::Color::from_rgb8(0xc6, 0x78, 0xdd);
        let tcr = iced::Color::from_rgb8(0xd7, 0xba, 0x7d);
        r = r.push(
            row![
                fi(mdi_path::ARROW_DOWN, 11.0, tin), lbl(fmt_k(c.input_tokens), tin),
                fi(mdi_path::ARROW_UP, 11.0, tout), lbl(fmt_k(c.output_tokens), tout),
                fi(mdi_path::CACHED, 11.0, blue), lbl(fmt_k(c.cache_write), blue),
                // The book glyph has a small left side-bearing → +1px left padding
                // restores its gap from the preceding number to match the others.
                container(mdi(mdi_path::BOOK, 11.0, tcr))
                    .center_y(Length::Fixed(LINE))
                    .padding(iced::Padding { left: 1.0, ..iced::Padding::ZERO }),
                lbl(fmt_k(c.cache_read), tcr),
            ]
            .spacing(3)
            .align_y(iced::Center),
        );

        r = r.push(horizontal_space());
        if let Some(f) = session.folder() {
            r = r.push(
                row![fi(mdi_path::FOLDER, 12.0, muted), lbl(f, primary)].spacing(4).align_y(iced::Center),
            );
        }
        if let Some(b) = session.git().and_then(|g| g.branch) {
            r = r.push(div());
            r = r.push(
                row![fi(mdi_path::BRANCH, 13.0, git_orange), sbl(b, green)]
                    .spacing(3)
                    .align_y(iced::Center),
            );
        }
    } else {
        // Not running: compact git status on the left; folder/branch on the right.
        if let Some(g) = session.git() {
            let count = |path, n: u32, col| {
                row![fi(path, 14.0, col), lbl(n.to_string(), col)].spacing(2).align_y(iced::Center)
            };
            if g.staged > 0 {
                r = r.push(count(mdi_path::CHECK_CIRCLE, g.staged, green));
            }
            if g.unstaged > 0 {
                r = r.push(count(mdi_path::CIRCLE_EDIT, g.unstaged, orange));
            }
            if g.untracked > 0 {
                r = r.push(count(mdi_path::PLUS_CIRCLE, g.untracked, blue));
            }
        }
        r = r.push(horizontal_space());
        if let Some(f) = session.folder() {
            let mut fs =
                row![fi(mdi_path::FOLDER, 12.0, muted), lbl(f, primary)].spacing(4).align_y(iced::Center);
            if let Some(b) = session.git().and_then(|g| g.branch) {
                fs = fs.push(lbl("[".into(), muted));
                fs = fs.push(fi(mdi_path::BRANCH, 12.0, git_orange));
                fs = fs.push(lbl(b, green));
                fs = fs.push(lbl("]".into(), muted));
            }
            r = r.push(fs);
        }
    }
    // Web `.terminal-footer`: 26px tall, 0 8px padding. `center_y` fixes the height
    // AND vertically centres the content (matching the header's treatment).
    container(r)
        .width(Length::Fill)
        .center_y(Length::Fixed(26.0))
        .padding([0, 8])
        .style(move |t: &iced::Theme| container::Style {
            border: iced::Border { radius: round, ..Default::default() },
            ..footer_style(t)
        })
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

/// The Arbiter "A" mark (blue-gradient SVG, the web's assets/logo.svg).
const ARBITER_LOGO_SVG: &[u8] = include_bytes!("../../assets/logo.svg");

/// Rasterize the logo SVG to RGBA at an exact pixel size using resvg (the same
/// engine iced uses internally). iced rasterizes its `svg` widget at `ceil(scale·
/// size)` and then draws it with the NEAREST-neighbour sampler, so it only looks
/// crisp when the raster exactly matches the on-screen pixels — otherwise it
/// aliases ("pixelated"). By rendering at the window's exact physical pixel size
/// and displaying it 1:1, there's no resampling at all: crisp + anti-aliased like
/// the browser, regardless of iced's sampler. Re-render when the scale changes.
fn render_logo(px: u32) -> iced::widget::image::Handle {
    use resvg::{tiny_skia, usvg};
    let px = px.max(1);
    let tree = usvg::Tree::from_data(ARBITER_LOGO_SVG, &usvg::Options::default())
        .expect("logo.svg parses");
    let mut pm = tiny_skia::Pixmap::new(px, px).expect("pixmap");
    let s = tree.size();
    let scale = (px as f32 / s.width()).min(px as f32 / s.height());
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pm.as_mut());
    // tiny_skia produces premultiplied RGBA; iced expects straight RGBA.
    let mut rgba = pm.data().to_vec();
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 {
            p[0] = (p[0] as u32 * 255 / a) as u8;
            p[1] = (p[1] as u32 * 255 / a) as u8;
            p[2] = (p[2] as u32 * 255 / a) as u8;
        }
    }
    iced::widget::image::Handle::from_rgba(px, px, rgba)
}

/// Logical size of the titlebar logo (web `.titlebar-logo` = 28px).
const LOGO_LOGICAL: f32 = 28.0;

/// Sample the titlebar azure gradient at `t` (wrapped to [0,1)) — the web's
/// `title-shimmer` stops: baby→azure→deep→tropical→baby.
fn azure_at(t: f32) -> iced::Color {
    const STOPS: [(f32, (u8, u8, u8)); 5] = [
        (0.00, (0x88, 0xD1, 0xF1)),
        (0.25, (0x33, 0x99, 0xFF)),
        (0.50, (0x02, 0x7D, 0xFF)),
        (0.75, (0x41, 0xAA, 0xDE)),
        (1.00, (0x88, 0xD1, 0xF1)),
    ];
    let t = t - t.floor();
    let mut i = 0;
    while i + 1 < STOPS.len() - 1 && t > STOPS[i + 1].0 {
        i += 1;
    }
    let (p0, c0) = STOPS[i];
    let (p1, c1) = STOPS[i + 1];
    let f = if p1 > p0 { (t - p0) / (p1 - p0) } else { 0.0 };
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
    iced::Color::from_rgb8(mix(c0.0, c1.0), mix(c0.1, c1.1), mix(c0.2, c1.2))
}

/// The animated "Arbiter" wordmark: an azure gradient shimmering across the
/// letters (the web's `title-shimmer`). Iced can't gradient-fill text, so each
/// letter samples the gradient at a phase that eases back and forth over ~12s.
fn arbiter_wordmark() -> Element<'static, Message> {
    let p = (now_ms() % 12_000) as f32 / 6_000.0; // 0→2 over 12s
    let tri = if p <= 1.0 { p } else { 2.0 - p }; // triangle 0→1→0
    let phase = tri * tri * (3.0 - 2.0 * tri); // smoothstep (ease-in-out)
    const WORD: &str = "Arbiter";
    let n = WORD.chars().count() as f32;
    // Match the web `.titlebar-title`: DM Sans 700, 15px, letter-spacing 0.06em
    // (≈0.9px at 15px → the per-letter row gap). Per-letter is required because
    // iced can't gradient-fill a single text run.
    let mut r = row![].spacing(0.9).align_y(iced::Center);
    for (i, ch) in WORD.chars().enumerate() {
        let col = azure_at(phase * 0.6 + (i as f32 / n) * 0.6);
        r = r.push(text(ch.to_string()).size(15).color(col).font(wordmark_font()));
    }
    r.into()
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
            text(g).size(size + 1).color(c).font(symbols_font()).into()
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
    round: iced::border::Radius,
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
    let title = container(text(name.to_string()).size(11).color(color)).center_x(Length::Fill);
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
    // 2px top / 0 bottom nudges the content (dot + title + shell button) down 1px:
    // `center_y` below absorbs half the 2px asymmetry, netting +1px at any height.
    .padding(iced::Padding { top: 2.0, right: 6.0, bottom: 0.0, left: 6.0 });
    // Web `.pane-toolbar`: 34px tall, #181818, 1px #2c2c2c bottom border (the
    // border is the `hline()` added below the header in the pane's content column).
    // `center_y` fixes the height AND vertically centres the content.
    container(header)
        .width(Length::Fill)
        .center_y(Length::Fixed(34.0))
        .style(move |_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x18, 0x18, 0x18))),
            border: iced::Border { radius: round, ..Default::default() },
            ..Default::default()
        })
        .into()
}

fn subscription(_state: &State) -> Subscription<Message> {
    let tick = iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick);
    // Only the main window's keys drive the terminal (not the overview window),
    // and not when a widget already consumed the key — e.g. a focused text input
    // in the new-worktree modal (else the branch name leaks into the terminal).
    let keys = iced::event::listen_with(|event, status, id| {
        if MAIN_WINDOW.get().copied() != Some(id) {
            return None;
        }
        // Track the cursor only over the titlebar (top ~44px) so the "+" dropdown
        // can anchor under the click — cheap, since it's off during terminal use.
        if let iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) = &event {
            return (position.y < 44.0).then(|| Message::CursorMoved(*position));
        }
        if status == iced::event::Status::Captured {
            return None;
        }
        handle_key(event)
    });
    let closes = iced::window::close_events().map(Message::WindowClosed);
    // Track each window's geometry (no move_events(), so filter the event stream).
    let geom = iced::window::events().map(|(id, ev)| match ev {
        iced::window::Event::Moved(p) => Message::WindowMoved(id, p),
        iced::window::Event::Resized(s) => Message::WindowResized(id, s),
        iced::window::Event::Opened { position, size } => Message::WindowOpened(id, position, size),
        iced::window::Event::Focused => Message::WindowFocusChanged(id, true),
        iced::window::Event::Unfocused => Message::WindowFocusChanged(id, false),
        _ => Message::Noop,
    });
    Subscription::batch([tick, keys, closes, geom, usage_subscription()])
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
/// The titlebar "Arbiter" wordmark font: DM Sans, but pinned to a STATIC instance
/// at optical-size 15 + weight 700 and renamed "DM Sans Arbiter". The web's
/// `.titlebar-title` is DM Sans 700/15px, and browsers auto-apply optical sizing
/// (opsz≈15) at that size — but cosmic-text uses a variable font's DEFAULT opsz
/// (9pt, designed for tiny text), which rendered visibly wrong. Pinning opsz=15
/// at build time makes native match the web's "Arbiter" width (~58.5px) exactly.
const ARBITER_WORDMARK_FONT: &[u8] = include_bytes!("../../assets/DMSans-Arbiter.ttf");
/// 3KB subset of Noto Sans Symbols 2 (the `·✢✳✶✻✽` working-animation dingbats),
/// renamed "ArbiterSymbols" — bundled so the ✻ is identical on macOS + Windows.
const ARBITER_SYMBOLS_FONT: &[u8] = include_bytes!("../../assets/ArbiterSymbols.ttf");

/// The base UI font (Inter), matching the web's `font-family: 'Inter', …`.
fn ui_font() -> iced::Font {
    iced::Font::with_name("Inter")
}

/// Inter SemiBold — for the overview's "ARBITER" header (the web's `.overview-title`
/// is Inter 600, uppercase).
fn ui_semibold() -> iced::Font {
    iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::with_name("Inter") }
}

/// The titlebar "Arbiter" wordmark font — our pinned DM Sans instance (700,
/// opsz 15), matching the web's `.titlebar-title`. Weight is baked into the
/// instance; the explicit Bold keeps cosmic-text's face matching exact.
fn wordmark_font() -> iced::Font {
    iced::Font { weight: iced::font::Weight::Bold, ..iced::Font::with_name("DM Sans Arbiter") }
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
        .font(ARBITER_WORDMARK_FONT)
        .font(ARBITER_SYMBOLS_FONT)
        .default_font(ui_font())
        .run_with(move || {
            // daemon starts with no windows — open the main one here.
            let saved = arbiter_native::persist::load();
            let main_geom = saved.as_ref().and_then(|s| s.main_window);
            let overview_geom = saved.as_ref().and_then(|s| s.overview_window);
            let overview_was_open = saved.as_ref().map(|s| s.overview_visible).unwrap_or(false);
            let saved_usage_org = saved.as_ref().and_then(|s| s.usage_org.clone());
            let saved_settings = saved.as_ref().map(|s| s.settings.clone()).unwrap_or_default();
            // Apply the saved scrollback before any terminal spawns so restored
            // sessions get the configured history depth.
            arbiter_native::term::SCROLLBACK
                .store(saved_settings.scrollback, std::sync::atomic::Ordering::Relaxed);

            // Open the main window at its saved size/position (or the default).
            let mut settings = iced::window::Settings::default();
            // macOS: unified titlebar — hide the title, make the titlebar
            // transparent, and let the content extend behind it, so the app's top
            // bar IS the titlebar (traffic lights overlay on the left).
            #[cfg(target_os = "macos")]
            {
                settings.platform_specific.title_hidden = true;
                settings.platform_specific.titlebar_transparent = true;
                settings.platform_specific.fullsize_content_view = true;
            }
            // Windows: drop the OS titlebar entirely — the app draws its own
            // unified titlebar (drag region + min/max/close). Stays resizable.
            #[cfg(target_os = "windows")]
            {
                settings.decorations = false;
                // Drop shadow for the borderless window — on Windows 11 this also
                // tends to restore the DWM rounded corners.
                settings.platform_specific.undecorated_shadow = true;
            }
            // Enforce a sane minimum so the window can never be stuck tiny (and so
            // a future drag-resize can't shrink it past usability).
            settings.min_size = Some(iced::Size::new(720.0, 480.0));
            if let Some(g) = main_geom {
                // Reject a degenerate saved size (older builds could persist a
                // minimized window's bogus dimensions, which opened it tiny) — keep
                // the 1024x768 default instead.
                if g.width >= 200.0 && g.height >= 150.0 {
                    settings.size = iced::Size::new(g.width, g.height);
                }
                if let (Some(x), Some(y)) = (g.x, g.y) {
                    let p = iced::Point::new(x, y);
                    // Ignore a saved off-screen sentinel (older builds could persist
                    // the -32000 minimized position) — let the WM place the window so
                    // it can't open invisible. Heals an already-corrupted session.json.
                    if on_screen_ish(p) {
                        settings.position = iced::window::Position::Specific(p);
                    }
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

            // Drop a saved off-screen sentinel so neither window starts tracking
            // (and re-persisting) an invisible position.
            let point = |g: persist::SavedWindow| {
                g.x.zip(g.y).map(|(x, y)| iced::Point::new(x, y)).filter(|p| on_screen_ish(*p))
            };
            let overview_size = overview_geom
                .map(|g| iced::Size::new(g.width, g.height))
                .filter(|s| s.width >= 200.0 && s.height >= 150.0)
                .unwrap_or(iced::Size::new(720.0, 520.0));
            let overview_pos = overview_geom.and_then(point);

            // Reopen the overview popout if it was open at quit (matches the web).
            let mut tasks = vec![open.map(|_| Message::Noop)];
            let overview_window = if overview_was_open {
                let (ov_id, ov_task) = open_overview(overview_size, overview_pos);
                // The overview opens after the main window and grabs focus on
                // startup; chain a focus-back so the MAIN window is active (and its
                // traffic lights coloured) — chained (not batched) so it runs after
                // the overview has actually opened, winning the focus race.
                tasks.push(ov_task.chain(iced::window::gain_focus(main_id)));
                Some(ov_id)
            } else {
                None
            };
            // Learn the real display scale so the logo is rasterized 1:1 for it.
            tasks.push(iced::window::get_scale_factor(main_id).map(Message::ScaleChanged));

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
                main_focused: true,
                main_maximized: false,
                chrome_init: false,
                // Render for a 2× display initially (the common Mac case); the
                // startup get_scale_factor query corrects it for the real display.
                logo_scale: 2.0,
                logo: render_logo((LOGO_LOGICAL * 2.0).round() as u32),
                worktree_dialog: None,
                worktree_menu: None,
                new_ws_menu: false,
                cursor: iced::Point::ORIGIN,
                new_ws_menu_x: 0.0,
                usage: UsageData::default(),
                usage_updated_ms: 0,
                usage_org: saved_usage_org,
                usage_org_menu: false,
                settings: saved_settings,
                settings_open: false,
                settings_tab: SettingsTab::General,
            };
            (state, iced::Task::batch(tasks))
        })
}

#[cfg(test)]
mod tests {
    use super::{clean_model, hsl_to_rgb, worktree_avatar};

    #[test]
    fn worktree_avatar_draws_without_panic() {
        // Exercises every feature branch + each animation frame (the GUI smoke test
        // starts with no project, so this path is otherwise unexercised). A handful
        // of varied seeds covers the different head/eye/antenna/mouth selectors.
        for seed in ["swift-otter", "brave-fox", "lucky-koala", "main", "", "a"] {
            for frame in 0..super::ANIM_FRAMES {
                let _ = worktree_avatar(seed, frame);
            }
        }
        // HSL endpoints map into range.
        assert_eq!(hsl_to_rgb(0.0, 0.0, 0.0), (0, 0, 0));
        assert_eq!(hsl_to_rgb(0.0, 0.0, 1.0), (255, 255, 255));
    }

    #[test]
    fn clean_model_strips_context_suffix() {
        // Plain names pass through untouched.
        assert_eq!(clean_model("Opus 4.8"), "Opus 4.8");
        assert_eq!(clean_model("Claude Sonnet 4.6"), "Claude Sonnet 4.6");
        // Parenthetical "(… context)" forms.
        assert_eq!(clean_model("Opus 4.8 (1M context)"), "Opus 4.8");
        assert_eq!(clean_model("Sonnet 4.6 (200k context)"), "Sonnet 4.6");
        // Inline forms, with and without a separator.
        assert_eq!(clean_model("Opus 4.8 · 1M context"), "Opus 4.8");
        assert_eq!(clean_model("Opus 4.8 1M context"), "Opus 4.8");
        // The trailing-size drop must not eat a real version token (no "context").
        assert_eq!(clean_model("Haiku 4.5"), "Haiku 4.5");
    }
}
