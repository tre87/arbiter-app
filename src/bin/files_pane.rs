//! The file explorer pane and the editor it opens files into.
//!
//! A module of `iced_shell` rather than a binary (see `autobins` in Cargo.toml).
//! The pure model lives in the library (`arbiter_native::explorer`,
//! `::editor`, `::highlight`); everything here is widgets and message handling.
//!
//! Layout: the explorer hugs the left edge of the workspace body as a sibling of
//! the `pane_grid`, which is what keeps it out of Ctrl+Shift+E while leaving it
//! draggable. While the editor is shown it takes the terminals' place to the
//! right of the explorer; the terminals keep running behind it.

use std::path::{Path, PathBuf};

use iced::widget::text_editor;

use arbiter_native::editor::{self as ed, DiskChange, EditKind, Snapshot, UndoStack};
use arbiter_native::explorer::{self as ex, DirEntry, Status, Tree};
use arbiter_native::highlight;

use super::*;

// Explicit so they win over the glob above, which would otherwise make every
// use of these macros ambiguous with the ones at the crate root.
use iced::widget::{column, row};

/// Narrowest the explorer pane may be dragged: below this the names are unusable.
pub const MIN_W: f32 = 160.0;
/// Width of the drag handle between the explorer and the content, which doubles
/// as the gap the app-wide glow shows through.
pub const HANDLE_W: f32 = 6.0;
/// Rows of the tree, the tab strip and the editor all use these.
const ROW_INDENT: f32 = 16.0;
/// Inset of a tree row's own content, and of the list inside the pane. The
/// header's left padding is their sum, so the folder name starts exactly above
/// the chevrons beneath it.
const ROW_PAD_X: f32 = 8.0;
const TREE_PAD: f32 = 4.0;
/// How long spinner detection is ignored after a layout change resizes the PTYs. The
/// same 500 ms a window resize uses, and for the same reason.
const REFLOW_SUPPRESS_MS: u64 = 500;
/// Size of a chevron, a folder and a file-type icon: one slot, so names line up.
const ICON_PX: f32 = 16.0;
/// Folders are deliberately a neutral slate rather than another bright colour:
/// the file-type icons beside them carry the colour, and a vivid folder would
/// compete with them for attention at every level of the tree.
const FOLDER_TINT: iced::Color = iced::Color::from_rgb(0.541, 0.592, 0.659);
/// Height of the explorer's header and of the editor's tab strip. They sit side
/// by side at the top of the workspace, so they share one height.
const HEADER_H: f32 = 34.0;
const FOOTER_H: f32 = 22.0;
const EDITOR_FONT_PX: f32 = 13.0;
/// Absolute, and shared with the gutter: the two columns only stay in step
/// because neither is allowed to derive its line height from the font.
const EDITOR_LINE_H: f32 = 20.0;
/// Rough advance width of Cascadia Mono at 13 px, for sizing the gutter.
const MONO_ADVANCE: f32 = 7.7;
/// Gap between the line numbers and the first character of the line.
const GUTTER_GAP: f32 = 10.0;
/// Above this many lines, a buffer is not carried over to the tab being opened.
///
/// Putting a different file into a buffer means clearing it first, and
/// cosmic-text's delete builds an undo record by pushing every removed line onto
/// the FRONT of a vector, so it costs far more than the lines are worth: 8 ms at
/// 2,500 lines, 48 ms at 10,500. Starting from an empty buffer instead costs one
/// frame, and a frame is 16 ms. Below this the carry-over wins, above it the
/// frame does.
const CARRY_MAX_LINES: usize = 2_000;

/// Lines pasted at a time when a whole file goes into the buffer.
///
/// cosmic-text inserts each pasted line into the buffer's line vector at the
/// same index, so one paste of a whole file is quadratic in its length: a 10,000
/// line file spent 191 ms there. Pasting in blocks keeps that quadratic term
/// inside the block, and a block this large is still few enough that the
/// re-shaping every paste triggers is paid a couple of dozen times rather than
/// hundreds.
const PASTE_LINES: usize = 512;

/// The editor's own padding. The gutter is drawn from the same value, which is
/// what puts the first number level with the first line.
const EDITOR_PAD: f32 = 5.0;
pub const PROMPT_INPUT: &str = "explorer-prompt-input";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// A workspace's file explorer, once a folder has been picked for it.
pub struct Explorer {
    pub root: PathBuf,
    pub shown: bool,
    pub width: f32,
    pub tree: Tree,
    pub selected: HashSet<PathBuf>,
    /// Where a Shift-click range starts: the last click without a modifier.
    pub anchor: Option<PathBuf>,
    /// Last plain click, for telling a double-click from two single ones.
    last_click: Option<(PathBuf, std::time::Instant)>,
    watcher: Option<ex::Watcher>,
    /// Bumped whenever the root changes, so a git status that was already
    /// running for the old root is discarded when it arrives.
    git_epoch: u64,
}

impl Explorer {
    fn new(root: PathBuf, width: f32) -> Self {
        Explorer {
            root,
            shown: true,
            width,
            tree: Tree::default(),
            selected: HashSet::new(),
            anchor: None,
            last_click: None,
            watcher: None,
            git_epoch: 0,
        }
    }

    fn arm_watcher(&mut self) {
        self.watcher = ex::watch(&self.root);
    }

    fn rows(&self) -> Vec<(DirEntry, usize)> {
        self.tree.rows(&self.root)
    }
}

/// One open file.
pub struct EditorTab {
    path: PathBuf,
    /// None until the tab is first shown: a restored session remembers its tabs
    /// but reads nothing from disk while the terminals are up.
    content: Option<text_editor::Content>,
    lang: String,
    /// Identifies this buffer's fill; bumped on every replacement, so a read that
    /// lands after another one was started is dropped.
    doc: u64,
    /// Identifies the file to the highlighter, whose cache a change clears. Moves only
    /// when the buffer takes on a different file: a reload or an undo puts back
    /// mostly the same lines, and each cached line is checked against its text and
    /// parse state before it is reused, so keeping the cache is safe and saves
    /// re-parsing from the top down to the caret.
    hl_doc: u64,
    /// The buffer has held a line too long to colour (`highlight::has_long_line`).
    /// Set when text arrives rather than measured in `view`; never cleared while the
    /// tab lives, since a file with one such line is not one to colour anyway.
    long_line: bool,
    dirty: bool,
    saved_hash: u64,
    undo: UndoStack,
    /// Same bounded stack as `undo`: a redo entry is another whole-document copy.
    redo: UndoStack,
    disk: Option<ed::DiskStamp>,
    eol: ed::Eol,
    trailing_newline: bool,
    bom: bool,
    /// The file has gone from disk; the buffer is kept so Save can write it back.
    missing: bool,
    /// A watcher event arrived for a tab that was not on screen, so the disk
    /// check waits until it is activated.
    pending_check: bool,
    /// A read is in flight for this tab: its buffer is the empty one the widget
    /// is being laid out with, and nothing else may touch it until it lands.
    loading: bool,
    load_error: Option<String>,
}

impl EditorTab {
    fn unloaded(path: PathBuf) -> Self {
        let lang = ed::lang_for_path(&path);
        EditorTab {
            path,
            content: None,
            lang,
            doc: 0,
            hl_doc: 0,
            long_line: false,
            dirty: false,
            saved_hash: 0,
            undo: UndoStack::default(),
            redo: UndoStack::default(),
            disk: None,
            eol: ed::Eol::Lf,
            trailing_newline: true,
            bom: false,
            missing: false,
            pending_check: false,
            loading: false,
            load_error: None,
        }
    }

    fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }

    fn lines(&self) -> Vec<String> {
        match &self.content {
            Some(c) => c.lines().map(|l| l.to_string()).collect(),
            None => Vec::new(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        snapshot_of(self.content.as_ref())
    }

    /// `ed::hash_lines` of the buffer, which `saved_hash` is compared with.
    fn content_hash(&self) -> u64 {
        match &self.content {
            Some(c) => ed::hash_lines(c.lines()),
            None => ed::hash_lines(std::iter::empty::<&str>()),
        }
    }
}

/// Free of `EditorTab` so an undo entry can be taken from the content alone, while
/// the tab's undo stack is borrowed for the `begin` that decides whether it is needed.
fn snapshot_of(content: Option<&text_editor::Content>) -> Snapshot {
    let lines: Vec<String> =
        content.map(|c| c.lines().map(|l| l.to_string()).collect()).unwrap_or_default();
    Snapshot {
        text: ed::to_buffer_text(&lines),
        cursor: content.map(|c| c.cursor_position()).unwrap_or((0, 0)),
    }
}

/// The editor for one workspace.
#[derive(Default)]
pub struct Editor {
    pub tabs: Vec<EditorTab>,
    pub active: Option<usize>,
    /// Shown in place of the terminal grid. Never restored as true.
    pub visible: bool,
    /// The tab whose unsaved changes are being asked about before it closes.
    close_confirm: Option<usize>,
    /// The tab that changed on disk while it had unsaved edits.
    reload_prompt: Option<usize>,
    doc_seq: u64,
}

impl Editor {
    fn tab(&self) -> Option<&EditorTab> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    fn tab_mut(&mut self) -> Option<&mut EditorTab> {
        match self.active {
            Some(i) => self.tabs.get_mut(i),
            None => None,
        }
    }

    fn next_doc(&mut self) -> u64 {
        self.doc_seq += 1;
        self.doc_seq
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.tabs.iter().position(|t| t.path == path)
    }

    /// Whether a modal of the editor's own is up, which the key bindings check:
    /// a focused editor sits under every modal in the view stack and would
    /// otherwise swallow the arrows meant for a dialog.
    fn modal_up(&self) -> bool {
        self.close_confirm.is_some() || self.reload_prompt.is_some()
    }
}

/// The open explorer context menu: where it was opened, and on what. A `None`
/// target is the empty space below the tree, which acts on the root.
pub struct Menu {
    pub x: f32,
    pub y: f32,
    pub target: Option<PathBuf>,
}

pub enum PromptKind {
    Rename(PathBuf),
    NewFile(PathBuf),
    NewFolder(PathBuf),
}

/// The one-field dialog behind Rename, New file and New folder.
pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
    pub error: Option<String>,
}

pub struct Delete {
    pub paths: Vec<PathBuf>,
    pub label: String,
}

/// An explorer-width drag in progress: where it started, and how wide the pane
/// was then.
#[derive(Clone, Copy)]
pub struct Drag {
    pub start_x: f32,
    pub start_w: f32,
}

/// The "which terminal should this go to" picker.
pub struct SendTarget {
    text: String,
    rows: Vec<(pane_grid::Pane, String, String)>,
    selected: usize,
    /// No Claude was running, so every terminal in the workspace is offered.
    all_terminals: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAnswer {
    Save,
    Discard,
    Cancel,
}

/// Everything the explorer and editor can be asked to do. Nested so the app's
/// own `Message` grows by one variant rather than forty.
#[derive(Debug, Clone)]
pub enum Msg {
    ShowSetting(bool),
    Toggle,
    PickFolder,
    FolderPicked(Option<String>),
    Hide,
    Select(PathBuf, bool),
    ClearSelection,
    MenuOpen(Option<PathBuf>),
    MenuClose,
    OpenSelection,
    OpenExternal,
    Reveal(PathBuf),
    New(bool),
    RenameStart,
    PromptInput(String),
    PromptCommit,
    PromptCancel,
    CopyPath(bool),
    DeleteStart,
    DeleteConfirm,
    DeleteCancel,
    DragStart,
    DragMove(iced::Point),
    DragEnd,
    FsChanged(PathBuf, Vec<PathBuf>),
    GitStatus(PathBuf, u64, HashMap<String, String>),

    EditorOpen(PathBuf),
    EditorToggle,
    EditorAction(text_editor::Action),
    /// The trash finished with these paths; the strings are the ones it refused.
    Deleted(Vec<PathBuf>, Vec<String>),
    /// A tab whose buffer had to be laid out first is ready for its text: which
    /// tab (its path and `doc`), where its caret goes, and the file.
    Fill(PathBuf, u64, (usize, usize), ReadFile),
    TabSelect(usize),
    TabClose(usize),
    CloseDirtyAnswer(CloseAnswer),
    Save,
    Undo,
    Redo,
    IndentKey,
    EditorMenuOpen,
    EditorMenuClose,
    Cut,
    Copy,
    Paste,
    Pasted(Option<String>),
    DeleteSelection,
    SelectAll,
    SendToAgent,
    SendPick(usize),
    SendMove(i32),
    SendConfirm,
    SendCancel,
    ReloadAnswer(bool),
}

/// `settings_toggle` takes a plain `fn`, which cannot be a closure over the variant.
pub fn show_setting_msg(v: bool) -> Message {
    Message::Files(Msg::ShowSetting(v))
}

// ---------------------------------------------------------------------------
// Queries the shell asks
// ---------------------------------------------------------------------------

/// The active workspace's explorer, if the setting is on and it is shown.
pub fn shown(state: &State) -> Option<&Explorer> {
    if !state.settings.show_file_explorer {
        return None;
    }
    state.active().explorer.as_ref().filter(|e| e.shown)
}

/// Whether the editor is on screen, and so whether keys belong to it rather than
/// to a terminal.
pub fn editor_visible(state: &State) -> bool {
    shown(state).is_some()
        && state.active().editor.visible
        && !state.active().editor.tabs.is_empty()
}

pub fn clamp_width(w: f32, win_w: f32) -> f32 {
    let max = (win_w * 0.5).max(MIN_W);
    w.clamp(MIN_W, max)
}

/// Rebuild an explorer from a saved session. `None` when its folder has gone.
pub fn from_saved(saved: &persist::SavedExplorer, win_w: f32, enabled: bool) -> Option<Explorer> {
    let root = PathBuf::from(&saved.root);
    if !root.is_dir() {
        return None;
    }
    let mut e = Explorer::new(ex::normalize(&root), clamp_width(saved.width, win_w));
    e.shown = saved.shown;
    for d in &saved.expanded {
        let p = ex::normalize(Path::new(d));
        if p.is_dir() {
            e.tree.expanded.insert(p);
        }
    }
    e.tree.reload(&e.root);
    if enabled && e.shown {
        e.arm_watcher();
    }
    Some(e)
}

pub fn to_saved(e: &Explorer) -> persist::SavedExplorer {
    let mut expanded: Vec<String> =
        e.tree.expanded.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    expanded.sort();
    persist::SavedExplorer {
        root: e.root.to_string_lossy().into_owned(),
        width: e.width,
        shown: e.shown,
        expanded,
    }
}

pub fn saved_tabs(ed: &Editor) -> (Vec<String>, Option<usize>) {
    (ed.tabs.iter().map(|t| t.path.to_string_lossy().into_owned()).collect(), ed.active)
}

pub fn editor_from_saved(tabs: &[String], active: Option<usize>) -> Editor {
    let tabs: Vec<EditorTab> = tabs
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .map(|p| EditorTab::unloaded(ex::normalize(&p)))
        .collect();
    let active = match (active, tabs.is_empty()) {
        (_, true) => None,
        (Some(i), false) => Some(i.min(tabs.len() - 1)),
        (None, false) => Some(0),
    };
    Editor { tabs, active, ..Editor::default() }
}

/// Every explorer watcher stops or starts when the setting is switched.
pub fn set_enabled(state: &mut State, on: bool) {
    for ws in &mut state.workspaces {
        let Some(e) = ws.explorer.as_mut() else { continue };
        if on && e.shown {
            e.tree.reload(&e.root);
            e.arm_watcher();
        } else if !on {
            e.watcher = None;
        }
    }
}

/// Work to start as soon as the app is up, so it is not paid for in the frame
/// that first needs it. Linking the syntax grammars takes a noticeable moment,
/// and the first file opened would otherwise wait for all of it.
pub fn boot_tasks(state: &State) -> Vec<Task<Message>> {
    if state.settings.show_file_explorer {
        // Under the open-latency diagnostic, wait for it: otherwise the first
        // open races the warm-up and its number is really the grammar decode.
        if ed::timing::enabled() {
            let _ = highlight::syntaxes();
        }
        highlight::warm();
    }
    boot_git_tasks(state)
}

/// One `git status` per restored explorer that is on screen.
fn boot_git_tasks(state: &State) -> Vec<Task<Message>> {
    if !state.settings.show_file_explorer {
        return Vec::new();
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut tasks = Vec::new();
    for ws in &state.workspaces {
        let Some(e) = ws.explorer.as_ref() else { continue };
        if !e.shown || seen.contains(&e.root) {
            continue;
        }
        seen.push(e.root.clone());
        tasks.push(git_task(e.root.clone(), e.git_epoch));
    }
    tasks
}

/// Ignore spinner detection briefly in the panes about to be re-laid-out.
///
/// Anything that changes the grid's width resizes their PTYs, and ConPTY answers with a
/// repaint that re-emits whatever stars were on screen. Two of those in a row look like
/// an animation and read as Claude working, which then "finishes" a couple of seconds
/// later. Only the active workspace is affected: nothing else is being laid out.
pub fn suppress_reflow(state: &State) {
    for (_, d) in state.active().panes.iter() {
        d.session.suppress_claude_activity(REFLOW_SUPPRESS_MS);
    }
}

/// Escape on one of the editor's own dialogs. Both answer the conservative way:
/// keep the tab, keep the edits. True when one was open.
pub fn dismiss_dialog(state: &mut State) -> bool {
    if state.active().editor.close_confirm.is_some() {
        let _ = update(state, Msg::CloseDirtyAnswer(CloseAnswer::Cancel));
        return true;
    }
    if state.active().editor.reload_prompt.is_some() {
        let _ = update(state, Msg::ReloadAnswer(false));
        return true;
    }
    false
}

/// Unsaved files, for the quit confirmation.
pub fn unsaved_names(state: &State) -> Vec<String> {
    let mut out = Vec::new();
    for ws in &state.workspaces {
        for t in &ws.editor.tabs {
            if t.dirty {
                out.push(t.name());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Filesystem work
// ---------------------------------------------------------------------------

/// Run `git status` for an explorer root off the UI thread. The epoch is handed
/// back untouched so a result for a root that has since changed is dropped.
/// Roots with a `git status` running, each with whether another was asked for while
/// it ran. One at a time per root: on a repository where a status outlasts the
/// watcher's debounce, overlapping runs piled up and whichever finished last set the
/// colours, which could be the oldest.
static GIT_RUNS: std::sync::Mutex<Option<HashMap<PathBuf, bool>>> = std::sync::Mutex::new(None);

/// Start a `git status` for `root`, or, if one is already running, have one more
/// follow it (`git_done`).
fn git_task(root: PathBuf, epoch: u64) -> Task<Message> {
    {
        let mut runs = GIT_RUNS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let runs = runs.get_or_insert_with(HashMap::new);
        if let Some(again) = runs.get_mut(&root) {
            *again = true;
            return Task::none();
        }
        runs.insert(root.clone(), false);
    }
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    let for_thread = root.clone();
    std::thread::spawn(move || {
        let status = arbiter_native::git::file_status(&for_thread.to_string_lossy());
        let _ = tx.send(status);
    });
    Task::perform(async move { rx.await.unwrap_or_default() }, move |status| {
        Message::Files(Msg::GitStatus(root.clone(), epoch, status))
    })
}

fn refresh(e: &mut Explorer) -> Task<Message> {
    e.tree.reload(&e.root);
    git_task(e.root.clone(), e.git_epoch)
}

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

pub fn update(state: &mut State, msg: Msg) -> Task<Message> {
    match msg {
        Msg::ShowSetting(on) => {
            state.settings.show_file_explorer = on;
            set_enabled(state, on);
            save_session(state);
            if on {
                highlight::warm();
                let roots: Vec<(PathBuf, u64)> = state
                    .workspaces
                    .iter()
                    .filter_map(|w| w.explorer.as_ref())
                    .filter(|e| e.shown)
                    .map(|e| (e.root.clone(), e.git_epoch))
                    .collect();
                return Task::batch(roots.into_iter().map(|(r, ep)| git_task(r, ep)));
            }
        }
        Msg::Toggle => {
            if !state.settings.show_file_explorer {
                return Task::none();
            }
            // Reaching for the explorer is the earliest reliable sign a file is
            // about to be opened.
            highlight::warm();
            // Showing or hiding the pane moves the grid's edge by its whole width.
            suppress_reflow(state);
            match state.active_mut().explorer.as_mut() {
                None => return update(state, Msg::PickFolder),
                Some(e) => {
                    e.shown = !e.shown;
                    if e.shown {
                        e.arm_watcher();
                        let task = refresh(e);
                        save_session(state);
                        return task;
                    }
                    e.watcher = None;
                    state.active_mut().editor.visible = false;
                    release_background_tabs(state);
                    save_session(state);
                }
            }
        }
        Msg::Hide => {
            suppress_reflow(state);
            if let Some(e) = state.active_mut().explorer.as_mut() {
                e.shown = false;
                e.watcher = None;
            }
            // The editor's toggle lives in the explorer header, so leaving it
            // armed would make the terminals vanish again the moment the pane
            // came back. Closing the pane means "show me the terminals".
            state.active_mut().editor.visible = false;
            release_background_tabs(state);
            save_session(state);
        }
        Msg::PickFolder => {
            let ws = state.active();
            let start = ws
                .panes
                .get(ws.focus)
                .and_then(|p| p.session.cwd())
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .or_else(dirs::home_dir);
            return Task::perform(
                async move {
                    let mut d = rfd::AsyncFileDialog::new().set_title("Open folder");
                    if let Some(s) = start {
                        d = d.set_directory(s);
                    }
                    d.pick_folder().await.map(|h| h.path().to_string_lossy().into_owned())
                },
                |p| Message::Files(Msg::FolderPicked(p)),
            );
        }
        Msg::FolderPicked(Some(path)) => {
            let root = ex::normalize(Path::new(&path));
            let win_w = state.main_size.width;
            let ws = state.active_mut();
            match ws.explorer.as_mut() {
                Some(e) => {
                    // A new root invalidates everything keyed by the old one.
                    e.root = root;
                    e.shown = true;
                    e.tree = Tree::default();
                    e.selected.clear();
                    e.anchor = None;
                    e.git_epoch += 1;
                    e.width = clamp_width(e.width, win_w);
                }
                None => ws.explorer = Some(Explorer::new(root, clamp_width(260.0, win_w))),
            }
            let e = state.active_mut().explorer.as_mut().expect("just set");
            e.arm_watcher();
            let task = refresh(e);
            save_session(state);
            return task;
        }
        Msg::FolderPicked(None) => {}
        Msg::Select(path, is_dir) => return select(state, path, is_dir),
        Msg::ClearSelection => {
            if let Some(e) = state.active_mut().explorer.as_mut() {
                e.selected.clear();
                e.anchor = None;
            }
        }
        Msg::MenuOpen(target) => {
            // The selection is deliberately left alone: a right-click outside it
            // acts on the row it landed on (see `menu_targets`) without throwing
            // away what was picked, which is the whole point of picking it.
            let at = last_cursor();
            state.term_menu = None;
            state.ws_tab_menu = None;
            state.explorer_menu = Some(Menu { x: at.x, y: at.y, target });
        }
        Msg::MenuClose => state.explorer_menu = None,
        Msg::OpenSelection => {
            let files: Vec<PathBuf> =
                menu_targets(state).into_iter().filter(|p| p.is_file()).collect();
            state.explorer_menu = None;
            let mut tasks = Vec::new();
            for f in files {
                tasks.push(update(state, Msg::EditorOpen(f)));
            }
            return Task::batch(tasks);
        }
        Msg::OpenExternal => {
            let targets = menu_targets(state);
            state.explorer_menu = None;
            for p in targets {
                open_path(&p.to_string_lossy());
            }
        }
        Msg::Reveal(path) => {
            state.explorer_menu = None;
            reveal_path(&path.to_string_lossy());
        }
        Msg::CopyPath(relative) => {
            let root = state.active().explorer.as_ref().map(|e| e.root.clone());
            // On the background there is no row, so the root's own path is what
            // "Copy path" means there.
            let mut sel = menu_targets(state);
            if sel.is_empty() {
                sel.extend(root.clone());
            }
            state.explorer_menu = None;
            let text: Vec<String> = sel
                .iter()
                .map(|p| match (relative, root.as_ref()) {
                    (true, Some(r)) => ex::relative_path(r, p)
                        .unwrap_or_else(|| p.to_string_lossy().into_owned()),
                    _ => p.to_string_lossy().into_owned(),
                })
                .collect();
            if !text.is_empty() {
                return iced::clipboard::write(text.join("\n"));
            }
        }
        Msg::New(is_dir) => {
            let dir = new_entry_dir(state);
            state.explorer_menu = None;
            let Some(dir) = dir else { return Task::none() };
            state.explorer_prompt = Some(Prompt {
                kind: if is_dir { PromptKind::NewFolder(dir) } else { PromptKind::NewFile(dir) },
                text: String::new(),
                error: None,
            });
            return text_input::focus(text_input::Id::new(PROMPT_INPUT));
        }
        Msg::RenameStart => {
            let sel = menu_targets(state);
            state.explorer_menu = None;
            let Some(path) = sel.into_iter().next() else { return Task::none() };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let caret = ex::stem_len(&name);
            let is_dir = path.is_dir();
            state.explorer_prompt =
                Some(Prompt { kind: PromptKind::Rename(path), text: name, error: None });
            let id = text_input::Id::new(PROMPT_INPUT);
            // iced 0.13 cannot select a range in a text input, so a rename puts
            // the caret after the stem instead of highlighting it.
            return if is_dir {
                text_input::focus(id.clone()).chain(text_input::select_all(id))
            } else {
                text_input::focus(id.clone()).chain(text_input::move_cursor_to(id, caret))
            };
        }
        Msg::PromptInput(t) => {
            if let Some(p) = state.explorer_prompt.as_mut() {
                p.text = t;
                p.error = None;
            }
        }
        Msg::PromptCancel => state.explorer_prompt = None,
        Msg::PromptCommit => return prompt_commit(state),
        Msg::DeleteStart => {
            let paths = menu_targets(state);
            state.explorer_menu = None;
            if paths.is_empty() {
                return Task::none();
            }
            let label = if paths.len() == 1 {
                format!(
                    "Move \u{201c}{}\u{201d} to trash?",
                    paths[0].file_name().unwrap_or_default().to_string_lossy()
                )
            } else {
                format!("Move {} items to trash?", paths.len())
            };
            state.explorer_delete = Some(Delete { paths, label });
        }
        Msg::DeleteCancel => state.explorer_delete = None,
        Msg::DeleteConfirm => return delete_confirm(state),
        Msg::DragStart => {
            let w = state.active().explorer.as_ref().map(|e| e.width);
            if let Some(start_w) = w {
                state.explorer_drag = Some(Drag { start_x: last_cursor().x, start_w });
                suppress_reflow(state);
            }
        }
        Msg::DragMove(p) => {
            let Some(d) = state.explorer_drag else { return Task::none() };
            let win_w = state.main_size.width;
            if let Some(e) = state.active_mut().explorer.as_mut() {
                e.width = clamp_width(d.start_w + (p.x - d.start_x), win_w);
            }
            // The suppression that covers the resulting PTY resizes is armed once at
            // DragStart and once at DragEnd. Doing it per mouse move re-armed a
            // 500 ms window continuously, and a window that never closes stops a
            // genuinely working pane from being seen as working again.
        }
        Msg::DragEnd => {
            if state.explorer_drag.take().is_some() {
                suppress_reflow(state);
                save_session(state);
            }
        }
        Msg::FsChanged(root, paths) => return fs_changed(state, root, paths),
        Msg::GitStatus(root, epoch, status) => {
            let mut current = None;
            for ws in &mut state.workspaces {
                let Some(e) = ws.explorer.as_mut() else { continue };
                if e.root == root {
                    current = Some(e.git_epoch);
                    if e.git_epoch == epoch {
                        e.tree.set_status(&status, &root);
                    }
                }
            }
            let again = GIT_RUNS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_mut()
                .and_then(|runs| runs.remove(&root))
                .unwrap_or(false);
            if let (true, Some(epoch)) = (again, current) {
                return git_task(root, epoch);
            }
        }

        Msg::EditorOpen(path) => return open_file(state, path),
        Msg::EditorToggle => {
            let has_tabs = !state.active().editor.tabs.is_empty();
            if !has_tabs {
                return Task::none();
            }
            let now = !state.active().editor.visible;
            // Hiding the editor is the important one: the terminals are out of the
            // view tree while it shows, so they resize on the frame they come back,
            // long after the window event that would otherwise have covered it.
            suppress_reflow(state);
            state.active_mut().editor.visible = now;
            if now {
                let task = ensure_active_loaded(state);
                return Task::batch([task, iced::widget::focus_next()]);
            }
            release_background_tabs(state);
        }
        Msg::EditorAction(action) => return editor_action(state, action),
        Msg::Deleted(paths, failed) => return deleted(state, paths, failed),
        Msg::Fill(path, doc, caret, read) => return finish_fill(state, path, doc, caret, read),
        Msg::TabSelect(i) => {
            let ed = &mut state.active_mut().editor;
            if i >= ed.tabs.len() {
                return Task::none();
            }
            ed.active = Some(i);
            let task = ensure_active_loaded(state);
            save_session(state);
            return Task::batch([task, iced::widget::focus_next()]);
        }
        Msg::TabClose(i) => return close_tab(state, i),
        Msg::CloseDirtyAnswer(answer) => return close_dirty_answer(state, answer),
        Msg::Save => return save_tab(state),
        Msg::Undo => return undo_redo(state, true),
        Msg::Redo => return undo_redo(state, false),
        Msg::IndentKey => return indent(state),
        Msg::EditorMenuOpen => {
            let at = last_cursor();
            state.term_menu = None;
            state.editor_menu = Some((at.x, at.y));
        }
        Msg::EditorMenuClose => state.editor_menu = None,
        Msg::Copy => {
            state.editor_menu = None;
            if let Some(s) = selection_text(state) {
                return iced::clipboard::write(s);
            }
        }
        Msg::Cut => {
            state.editor_menu = None;
            if let Some(s) = selection_text(state) {
                let del = editor_action(state, text_editor::Action::Edit(text_editor::Edit::Delete));
                return Task::batch([iced::clipboard::write(s), del]);
            }
        }
        Msg::Paste => {
            state.editor_menu = None;
            return iced::clipboard::read().map(|t| Message::Files(Msg::Pasted(t)));
        }
        Msg::Pasted(Some(t)) if !t.is_empty() => {
            return editor_action(
                state,
                text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(t))),
            );
        }
        Msg::Pasted(_) => {}
        Msg::DeleteSelection => {
            state.editor_menu = None;
            return editor_action(state, text_editor::Action::Edit(text_editor::Edit::Delete));
        }
        Msg::SelectAll => {
            state.editor_menu = None;
            return editor_action(state, text_editor::Action::SelectAll);
        }
        Msg::SendToAgent => return send_to_agent(state),
        Msg::SendMove(delta) => {
            if let Some(t) = state.send_target.as_mut() {
                let n = t.rows.len();
                if n > 0 {
                    let d = delta.rem_euclid(n as i32) as usize;
                    t.selected = (t.selected + d) % n;
                }
            }
        }
        Msg::SendPick(i) => {
            if let Some(t) = state.send_target.as_mut() {
                t.selected = i;
            }
            return update(state, Msg::SendConfirm);
        }
        Msg::SendConfirm => {
            let Some(t) = state.send_target.take() else { return Task::none() };
            let Some((pane, _, _)) = t.rows.get(t.selected).cloned() else { return Task::none() };
            return deliver(state, pane, t.text);
        }
        Msg::SendCancel => state.send_target = None,
        Msg::ReloadAnswer(reload) => {
            let Some(i) = state.active_mut().editor.reload_prompt.take() else {
                return Task::none();
            };
            if reload {
                return reload_tab(state, i);
            }
            // Keeping the edits means accepting this version as the baseline, or
            // the same change would be reported again on the next check.
            if let Some(t) = state.active_mut().editor.tabs.get_mut(i) {
                t.disk = ed::DiskStamp::read(&t.path);
            }
        }
    }
    Task::none()
}

fn selection(state: &State) -> Vec<PathBuf> {
    let Some(e) = state.active().explorer.as_ref() else { return Vec::new() };
    let order: Vec<PathBuf> = e.rows().into_iter().map(|(en, _)| en.path).collect();
    order.into_iter().filter(|p| e.selected.contains(p)).collect()
}

/// What the open context menu acts on. Right-clicking inside the selection acts
/// on all of it; right-clicking any other row acts on that row alone, leaving
/// the selection untouched; the empty space below the tree acts on nothing, so
/// the item-specific entries grey out instead of applying to a stale selection.
fn menu_targets(state: &State) -> Vec<PathBuf> {
    let Some(m) = state.explorer_menu.as_ref() else { return Vec::new() };
    let Some(target) = m.target.as_ref() else { return Vec::new() };
    let sel = selection(state);
    if sel.contains(target) {
        sel
    } else {
        vec![target.clone()]
    }
}

/// Where a new file or folder goes: into the selected folder, beside a selected
/// file, or at the root.
fn new_entry_dir(state: &State) -> Option<PathBuf> {
    let e = state.active().explorer.as_ref()?;
    Some(match menu_targets(state).first() {
        Some(p) if p.is_dir() => p.clone(),
        Some(p) => p.parent().map(Path::to_path_buf).unwrap_or_else(|| e.root.clone()),
        None => e.root.clone(),
    })
}

fn select(state: &mut State, path: PathBuf, is_dir: bool) -> Task<Message> {
    let (shift, ctrl) = (state.modifiers.shift(), state.modifiers.command());
    let now = std::time::Instant::now();
    let Some(e) = state.active_mut().explorer.as_mut() else { return Task::none() };

    if !shift && !ctrl && !is_dir {
        let double = matches!(&e.last_click, Some((p, t))
            if *p == path && t.elapsed() <= CLICK_THRESHOLD);
        if double {
            e.last_click = None;
            return update(state, Msg::EditorOpen(path));
        }
        e.last_click = Some((path.clone(), now));
    } else {
        e.last_click = None;
    }

    if shift {
        if let Some(anchor) = e.anchor.clone() {
            let order: Vec<PathBuf> = e.rows().into_iter().map(|(en, _)| en.path).collect();
            let a = order.iter().position(|p| *p == anchor);
            let b = order.iter().position(|p| *p == path);
            if let (Some(a), Some(b)) = (a, b) {
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                e.selected = order[lo..=hi].iter().cloned().collect();
                return Task::none();
            }
        }
        e.selected.insert(path.clone());
        e.anchor = Some(path);
        return Task::none();
    }
    if ctrl {
        if !e.selected.remove(&path) {
            e.selected.insert(path.clone());
        }
        e.anchor = Some(path);
        return Task::none();
    }
    e.selected.clear();
    e.selected.insert(path.clone());
    e.anchor = Some(path.clone());
    if is_dir {
        e.tree.toggle_expand(&path);
        save_session(state);
    }
    Task::none()
}

fn prompt_commit(state: &mut State) -> Task<Message> {
    let Some(p) = state.explorer_prompt.as_ref() else { return Task::none() };
    let name = p.text.trim().to_string();
    if let Err(e) = ex::valid_entry_name(&name) {
        if let Some(p) = state.explorer_prompt.as_mut() {
            p.error = Some(e.to_string());
        }
        return Task::none();
    }
    let (dir, old): (PathBuf, Option<PathBuf>) = match &p.kind {
        PromptKind::Rename(old) => (
            old.parent().map(Path::to_path_buf).unwrap_or_else(|| old.clone()),
            Some(old.clone()),
        ),
        PromptKind::NewFile(d) | PromptKind::NewFolder(d) => (d.clone(), None),
    };
    let target = ex::normalize(&dir.join(&name));
    let case_only = old.as_ref().is_some_and(|old| is_same_entry(old, &target));
    if target.exists() && Some(&target) != old.as_ref() && !case_only {
        if let Some(p) = state.explorer_prompt.as_mut() {
            p.error = Some("Something with that name is already here.".into());
        }
        return Task::none();
    }
    let make_dir = matches!(p.kind, PromptKind::NewFolder(_));
    let is_new_file = matches!(p.kind, PromptKind::NewFile(_));
    let result = match &old {
        Some(old) => std::fs::rename(old, &target),
        None if make_dir => std::fs::create_dir(&target),
        None => std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map(|_| ()),
    };
    if let Err(err) = result {
        state.explorer_prompt = None;
        state.notice = Some(Notice {
            title: "Could not do that".into(),
            body: format!("{}: {err}", target.display()),
        });
        return Task::none();
    }
    state.explorer_prompt = None;

    let mut task = Task::none();
    if let Some(e) = state.active_mut().explorer.as_mut() {
        if let Some(old) = &old {
            e.tree.remap_expanded(old, &target);
            e.selected.remove(old);
        }
        e.tree.expand(&dir);
        e.tree.reload_dir(&dir);
        e.selected.clear();
        e.selected.insert(target.clone());
        e.anchor = Some(target.clone());
        task = git_task(e.root.clone(), e.git_epoch);
    }
    // An open file follows its new name, and so does every open file inside a
    // renamed folder; left behind, a tab reads as deleted and cannot be saved.
    if let Some(old) = &old {
        for ws in &mut state.workspaces {
            for t in &mut ws.editor.tabs {
                let Ok(rest) = t.path.strip_prefix(old) else { continue };
                let path = if rest.as_os_str().is_empty() { target.clone() } else { target.join(rest) };
                t.lang = ed::lang_for_path(&path);
                t.disk = ed::DiskStamp::read(&path);
                t.path = path;
            }
        }
    }
    save_session(state);
    if is_new_file {
        let open = update(state, Msg::EditorOpen(target));
        return Task::batch([task, open]);
    }
    task
}

/// Whether `target` names the same file as `old`, which on a case-insensitive file
/// system is what a rename that only changes case looks like: `target.exists()` is
/// true, yet nothing would be overwritten.
fn is_same_entry(old: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::symlink_metadata(old), std::fs::symlink_metadata(target)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    // NTFS is case-insensitive, and std has no stable file identity on Windows.
    #[cfg(not(unix))]
    {
        old.parent() == target.parent()
            && old.file_name().map(|n| n.to_string_lossy().to_lowercase())
                == target.file_name().map(|n| n.to_string_lossy().to_lowercase())
    }
}

fn delete_confirm(state: &mut State) -> Task<Message> {
    let Some(d) = state.explorer_delete.take() else { return Task::none() };
    // Off the UI thread: on macOS the trash goes through Finder, one AppleScript per
    // item, and on Windows a large folder takes as long as the Recycle Bin needs.
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    let paths = d.paths;
    std::thread::spawn(move || {
        let failed: Vec<String> = paths
            .iter()
            .filter_map(|p| trash::delete(p).err().map(|e| format!("{}: {e}", p.display())))
            .collect();
        let _ = tx.send((paths, failed));
    });
    Task::perform(async move { rx.await.unwrap_or_default() }, |(paths, failed)| {
        Message::Files(Msg::Deleted(paths, failed))
    })
}

fn deleted(state: &mut State, paths: Vec<PathBuf>, failed: Vec<String>) -> Task<Message> {
    let mut tasks = Vec::new();
    for ws in &mut state.workspaces {
        let Some(e) = ws.explorer.as_mut() else { continue };
        if !paths.iter().any(|p| p.starts_with(&e.root)) {
            continue;
        }
        for p in &paths {
            e.selected.remove(p);
            e.tree.expanded.remove(p);
            if let Some(parent) = p.parent() {
                e.tree.reload_dir(parent);
            }
        }
        tasks.push(git_task(e.root.clone(), e.git_epoch));
    }
    if !failed.is_empty() {
        state.notice =
            Some(Notice { title: "Could not delete everything".into(), body: failed.join("\n") });
    }
    save_session(state);
    Task::batch(tasks)
}

fn fs_changed(state: &mut State, root: PathBuf, paths: Vec<PathBuf>) -> Task<Message> {
    let mut tasks = Vec::new();
    let mut touched = false;
    for ws in &mut state.workspaces {
        let Some(e) = ws.explorer.as_mut() else { continue };
        if e.root != root || !e.shown {
            continue;
        }
        touched = true;
        // Once per folder, not once per path: a checkout touching 500 files in one
        // folder read and sorted it 500 times, on the UI thread.
        let mut dirs: HashSet<&Path> = HashSet::new();
        for p in &paths {
            if e.tree.entries.contains_key(p) {
                dirs.insert(p);
            }
            if let Some(parent) = p.parent() {
                dirs.insert(parent);
            }
        }
        for d in dirs {
            e.tree.reload_dir(d);
        }
        e.tree.expanded.retain(|d| d.is_dir());
    }
    if touched {
        let epoch = state
            .workspaces
            .iter()
            .filter_map(|w| w.explorer.as_ref())
            .find(|e| e.root == root)
            .map(|e| e.git_epoch)
            .unwrap_or(0);
        tasks.push(git_task(root, epoch));
    }
    tasks.push(editor_fs_changed(state, &paths));
    Task::batch(tasks)
}

/// A file under a watched root changed: reload the tab showing it when it has no
/// unsaved edits, ask when it has, and defer the check for tabs off screen.
fn editor_fs_changed(state: &mut State, paths: &[PathBuf]) -> Task<Message> {
    let active_ws = state.active;
    let mut check: Option<usize> = None;
    for (wi, ws) in state.workspaces.iter_mut().enumerate() {
        let visible_tab = ws.editor.visible.then_some(ws.editor.active).flatten();
        for (ti, t) in ws.editor.tabs.iter_mut().enumerate() {
            if !paths.contains(&t.path) {
                continue;
            }
            if wi == active_ws && visible_tab == Some(ti) {
                check = Some(ti);
            } else {
                t.pending_check = true;
            }
        }
    }
    match check {
        Some(i) => check_disk(state, i),
        None => Task::none(),
    }
}

// ---------------------------------------------------------------------------
// Editor actions
// ---------------------------------------------------------------------------

fn open_file(state: &mut State, path: PathBuf) -> Task<Message> {
    let path = ex::normalize(&path);
    ed::timing::open(
        &path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    );
    highlight::warm();
    if let Some(i) = state.active().editor.index_of(&path) {
        state.active_mut().editor.active = Some(i);
        state.active_mut().editor.visible = true;
        let task = ensure_active_loaded(state);
        save_session(state);
        return Task::batch([task, iced::widget::focus_next()]);
    }
    let read = match read_file(&path) {
        Ok(read) => read,
        Err(body) => {
            state.notice = Some(Notice { title: "Cannot open this file".into(), body });
            return Task::none();
        }
    };
    let carried = take_spare_buffer(state, None);
    let mut tab = EditorTab::unloaded(path.clone());
    tab.doc = state.active_mut().editor.next_doc();
    tab.hl_doc = tab.doc;
    tab.content = Some(carried.unwrap_or_default());
    let ws = state.active;
    let pane = &mut state.active_mut().editor;
    pane.tabs.push(tab);
    let i = pane.tabs.len() - 1;
    pane.active = Some(i);
    pane.visible = true;
    release_background_tabs(state);
    Task::batch([fill_tab(state, ws, i, (0, 0), read), iced::widget::focus_next()])
}

/// What one read brings back.
#[derive(Clone)]
pub struct ReadFile {
    loaded: ed::Loaded,
    disk: Option<ed::DiskStamp>,
}

// By hand so that debug-printing the message it travels in does not print the
// whole file.
impl std::fmt::Debug for ReadFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadFile").field("bytes", &self.loaded.text.len()).finish()
    }
}

/// Read one file. `Err` carries what to tell the user.
fn read_file(path: &Path) -> Result<ReadFile, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > ed::MAX_FILE_BYTES {
        return Err(format!(
            "{} is {:.1} MiB. The editor opens files up to {} MiB; use Open in default app instead.",
            path.display(),
            meta.len() as f64 / (1024.0 * 1024.0),
            ed::MAX_FILE_BYTES / (1024 * 1024),
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    ed::timing::phase("read");
    let loaded = ed::load_bytes(&bytes).map_err(|e| e.message().to_string())?;
    Ok(ReadFile { loaded, disk: ed::DiskStamp::read(path) })
}

/// Take a laid-out buffer off a tab other than `keep`, for the tab being opened
/// to carry on with.
///
/// A fresh `Content` has a buffer with no height until the widget has been laid
/// out once, and text pasted into a buffer with no height is laid out in full
/// (see `set_text`). A buffer that has been on screen is sized to this very pane,
/// so handing it on is what lets a file's text appear in the same frame the
/// editor does rather than the frame after. The tab it comes from re-reads on the
/// way back, which is what `release_background_tabs` would have done to it
/// anyway. For the same reason, a tab holding anything that is not on disk
/// keeps its buffer.
fn take_spare_buffer(state: &mut State, keep: Option<usize>) -> Option<text_editor::Content> {
    let pane = &mut state.active_mut().editor;
    for (i, tab) in pane.tabs.iter_mut().enumerate() {
        if Some(i) == keep || tab.dirty || tab.missing || tab.loading {
            continue;
        }
        if !tab.undo.is_empty() || !tab.redo.is_empty() {
            continue;
        }
        let carryable = tab
            .content
            .as_ref()
            .is_some_and(|c| buffer_is_sized(c) && c.line_count() <= CARRY_MAX_LINES);
        if carryable {
            return tab.content.take();
        }
    }
    None
}

/// Whether a buffer has been through a layout, and so knows its viewport.
fn buffer_is_sized(content: &text_editor::Content) -> bool {
    content.editor().buffer().size().1.is_some_and(|h| h > 0.0)
}

/// Put a file that has been read into its tab, now if the buffer is ready for it
/// and on the next frame if it is not. `caret` is where the caret goes afterwards:
/// a reload keeps its place, anything else starts at the top, since the buffer may
/// have been carried over from another tab, whose caret it still holds.
fn fill_tab(
    state: &mut State,
    ws: usize,
    i: usize,
    caret: (usize, usize),
    read: ReadFile,
) -> Task<Message> {
    let Some(tab) = state.workspaces.get_mut(ws).and_then(|w| w.editor.tabs.get_mut(i)) else {
        return Task::none();
    };
    let ready = tab.content.as_ref().is_some_and(buffer_is_sized);
    if !ready {
        // Nothing on screen to carry a buffer over from, so the empty one this
        // tab was given has to be laid out once before the text goes in. One
        // frame, and the editor is already up while it passes.
        tab.loading = true;
        let msg = Msg::Fill(tab.path.clone(), tab.doc, caret, read);
        return Task::done(Message::Files(msg));
    }
    apply_read(tab, caret, read);
    save_session(state);
    Task::none()
}

/// The second half of a deferred fill: the buffer has been laid out by now.
///
/// The tab is found again by its path and `doc`, and only while it is still
/// waiting. Its workspace is not held by index: closing an earlier workspace
/// during the frame in between would shift it, and a same-path tab there could
/// have its unsaved edits replaced. `doc` alone is only unique within one
/// workspace, hence the path beside it.
fn finish_fill(
    state: &mut State,
    path: PathBuf,
    doc: u64,
    caret: (usize, usize),
    read: ReadFile,
) -> Task<Message> {
    let tab = state
        .workspaces
        .iter_mut()
        .flat_map(|w| w.editor.tabs.iter_mut())
        .find(|t| t.loading && t.doc == doc && t.path == path);
    let Some(tab) = tab else { return Task::none() };
    tab.loading = false;
    apply_read(tab, caret, read);
    save_session(state);
    Task::none()
}

fn apply_read(tab: &mut EditorTab, caret: (usize, usize), read: ReadFile) {
    if tab.content.is_none() {
        tab.content = Some(text_editor::Content::new());
    }
    set_text(tab, &read.loaded.text);
    tab.long_line = highlight::has_long_line(&read.loaded.text);
    ed::timing::phase("fill");
    ed::timing::lines(tab.content.as_ref().map(|c| c.line_count()).unwrap_or(0));
    place_caret(tab, caret);
    tab.saved_hash = tab.content_hash();
    tab.eol = read.loaded.eol;
    tab.trailing_newline = read.loaded.trailing_newline;
    tab.bom = read.loaded.bom;
    tab.disk = read.disk;
    tab.dirty = false;
    tab.missing = false;
    tab.load_error = None;
    tab.undo.clear();
    tab.redo.clear();
}

/// Replace a tab's buffer with `text`, in place.
///
/// Pasting over a select-all rather than building a fresh `Content`.
/// `Content::with_text` makes a buffer with no height, so cosmic-text lays out
/// every line of the file, and the first layout then throws all of it away when
/// it applies the editor's own font. Measured on a 2,400 line file that was 40%
/// of what an open cost. A buffer that knows its viewport shapes only the lines
/// on screen.
///
/// A paste ending in `\n` opens an empty last line, where `Buffer::set_text` (and
/// `ed::buffer_lines`, which the rest of the editor reasons with) reads that newline
/// as closing the line before it. One is stripped so the two agree; without it every
/// save added a blank line to a file ending in a newline.
fn set_text(tab: &mut EditorTab, text: &str) {
    let Some(c) = tab.content.as_mut() else { return };
    let text = text.strip_suffix('\n').unwrap_or(text);
    c.perform(text_editor::Action::SelectAll);
    let mut blocks = line_blocks(text, PASTE_LINES);
    // The first paste replaces the selection; the rest land at the caret, which
    // each one leaves at the end of the document.
    let first = blocks.next().unwrap_or("");
    c.perform(text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(first.to_string()))));
    for block in blocks {
        c.perform(text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(
            block.to_string(),
        ))));
    }
}

/// `text` cut into runs of at most `lines` whole lines, each keeping its own
/// line terminator. Empty for empty text.
fn line_blocks(text: &str, lines: usize) -> impl Iterator<Item = &str> {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= text.len() {
            return None;
        }
        let mut end = start;
        for _ in 0..lines {
            match text[end..].find('\n') {
                Some(i) => end += i + 1,
                None => {
                    end = text.len();
                    break;
                }
            }
            if end >= text.len() {
                break;
            }
        }
        let block = &text[start..end];
        start = end;
        Some(block)
    })
}

/// Load the active tab if it has not been read yet, and check the disk if a
/// watcher event arrived while it was off screen.
/// Let go of the buffers of tabs that are not on screen.
///
/// A tab's buffer is not just its text: cosmic-text keeps a line of its own for
/// every line of the file, and the highlighter keeps a parsed copy of every line
/// it has reached. That would otherwise be held for every tab at once, for as
/// long as the app is open.
///
/// Only untouched tabs are released: one with unsaved edits obviously cannot be
/// re-read from disk, and one with an undo history would lose it, which is not
/// a trade to make silently. They reload on the way back in, the same lazy path
/// a restored session already uses.
/// While the editor is hidden nothing is on screen, so even the active tab's
/// buffer is dead weight until it comes back.
fn release_background_tabs(state: &mut State) {
    let visible = state.active().editor.visible;
    let active = state.active().editor.active.filter(|_| visible);
    for (i, t) in state.active_mut().editor.tabs.iter_mut().enumerate() {
        if Some(i) == active || t.content.is_none() {
            continue;
        }
        if t.dirty || t.missing || t.loading || !t.undo.is_empty() || !t.redo.is_empty() {
            continue;
        }
        t.content = None;
    }
}

fn ensure_active_loaded(state: &mut State) -> Task<Message> {
    release_background_tabs(state);
    let Some(i) = state.active().editor.active else { return Task::none() };
    let needs_load = state
        .active()
        .editor
        .tabs
        .get(i)
        .map(|t| t.content.is_none() && t.load_error.is_none() && !t.loading)
        .unwrap_or(false);
    if !needs_load {
        return check_disk(state, i);
    }
    highlight::warm();
    let Some(path) = state.active().editor.tabs.get(i).map(|t| t.path.clone()) else {
        return Task::none();
    };
    let read = match read_file(&path) {
        Ok(read) => read,
        Err(body) => {
            if let Some(t) = state.active_mut().editor.tabs.get_mut(i) {
                t.load_error = Some(body);
            }
            return Task::none();
        }
    };
    let carried = take_spare_buffer(state, Some(i));
    let doc = state.active_mut().editor.next_doc();
    let ws = state.active;
    let Some(t) = state.active_mut().editor.tabs.get_mut(i) else { return Task::none() };
    t.doc = doc;
    t.hl_doc = doc;
    t.content = Some(carried.unwrap_or_default());
    fill_tab(state, ws, i, (0, 0), read)
}

/// Compare one tab with its file: reload silently when clean, ask when dirty.
fn check_disk(state: &mut State, i: usize) -> Task<Message> {
    let Some(t) = state.active().editor.tabs.get(i) else { return Task::none() };
    if t.content.is_none() || t.loading {
        return Task::none();
    }
    let change = ed::check_disk(&t.path, t.disk);
    let dirty = t.dirty;
    if let Some(t) = state.active_mut().editor.tabs.get_mut(i) {
        t.pending_check = false;
    }
    match change {
        DiskChange::Unchanged => Task::none(),
        DiskChange::Missing => {
            if let Some(t) = state.active_mut().editor.tabs.get_mut(i) {
                t.missing = true;
            }
            Task::none()
        }
        DiskChange::Changed if !dirty => reload_tab(state, i),
        DiskChange::Changed => {
            state.active_mut().editor.reload_prompt = Some(i);
            Task::none()
        }
    }
}

fn reload_tab(state: &mut State, i: usize) -> Task<Message> {
    let Some(path) = state.active().editor.tabs.get(i).map(|t| t.path.clone()) else {
        return Task::none();
    };
    ed::timing::open(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let read = match read_file(&path) {
        Ok(read) => read,
        Err(body) => {
            if let Some(t) = state.active_mut().editor.tabs.get_mut(i) {
                t.load_error = Some(body);
                t.content = None;
            }
            return Task::none();
        }
    };
    let doc = state.active_mut().editor.next_doc();
    let ws = state.active;
    let mut caret = (0, 0);
    if let Some(t) = state.active_mut().editor.tabs.get_mut(i) {
        t.doc = doc;
        caret = t.content.as_ref().map(|c| c.cursor_position()).unwrap_or((0, 0));
    }
    fill_tab(state, ws, i, caret, read)
}

/// Put the caret back at `(line, byte)` after the buffer was replaced. iced 0.13
/// has no "set cursor" action, so it is walked there; `Click` cannot be used
/// because the new buffer has not been laid out yet.
fn place_caret(tab: &mut EditorTab, (line, byte): (usize, usize)) {
    let Some(c) = tab.content.as_mut() else { return };
    c.perform(text_editor::Action::Move(text_editor::Motion::DocumentStart));
    // Whole pages first: every step re-shapes around the caret, so walking a line
    // at a time to line 9,000 shapes the file on the way past it.
    loop {
        let at = c.cursor_position().0;
        c.perform(text_editor::Action::Move(text_editor::Motion::PageDown));
        let now = c.cursor_position().0;
        if now > line || now == at {
            if now > line {
                c.perform(text_editor::Action::Move(text_editor::Motion::PageUp));
            }
            break;
        }
    }
    while c.cursor_position().0 < line {
        let at = c.cursor_position();
        c.perform(text_editor::Action::Move(text_editor::Motion::Down));
        if c.cursor_position() == at {
            break;
        }
    }
    // Right steps by grapheme, so walk to the recorded byte rather than
    // computing it. The guard is against a caret that cannot move any further,
    // which would otherwise spin here forever.
    let on_line = c.cursor_position().0;
    let mut last = c.cursor_position();
    while last.1 < byte {
        c.perform(text_editor::Action::Move(text_editor::Motion::Right));
        let now = c.cursor_position();
        if now.0 != on_line {
            c.perform(text_editor::Action::Move(text_editor::Motion::Left));
            break;
        }
        if now == last {
            break;
        }
        last = now;
    }
}

fn editor_action(state: &mut State, action: text_editor::Action) -> Task<Message> {
    let now = now_ms();
    // The buffer works in LF throughout (`ed::load_bytes`), and a Windows clipboard
    // holds CRLF: pasted as is, every line kept a stray `\r` that the display, the
    // modified check and Send to Agent all carried. Ctrl+V reaches here as a key
    // binding and the menu's Paste as a message, so both are caught here.
    let action = match action {
        text_editor::Action::Edit(text_editor::Edit::Paste(t)) if t.contains('\r') => {
            let t = t.replace("\r\n", "\n").replace('\r', "\n");
            text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(t)))
        }
        other => other,
    };
    let Some(tab) = state.active_mut().editor.tab_mut() else { return Task::none() };
    if tab.content.is_none() {
        return Task::none();
    }
    let kind = match &action {
        text_editor::Action::Edit(e) => Some(match e {
            text_editor::Edit::Insert(c) => EditKind::Insert(*c),
            text_editor::Edit::Paste(_) => EditKind::Paste,
            text_editor::Edit::Enter => EditKind::Enter,
            text_editor::Edit::Backspace => EditKind::Backspace,
            text_editor::Edit::Delete => EditKind::Delete,
        }),
        _ => None,
    };
    match kind {
        Some(kind) => {
            // Only an edit that opens a new undo entry copies the document; a key
            // joining the current run does not.
            if let text_editor::Action::Edit(text_editor::Edit::Paste(pasted)) = &action {
                tab.long_line |= highlight::has_long_line(pasted);
            }
            let content = tab.content.as_ref();
            if tab.undo.begin(kind, now, || snapshot_of(content)) {
                tab.redo.clear();
            }
            if let Some(c) = tab.content.as_mut() {
                c.perform(action);
            }
            tab.dirty = tab.content_hash() != tab.saved_hash;
        }
        None => {
            // A move, click or selection ends the run, so the next keystroke
            // opens a fresh undo entry rather than joining the last word.
            tab.undo.clear_run();
            if let Some(c) = tab.content.as_mut() {
                c.perform(action);
            }
        }
    }
    Task::none()
}

fn undo_redo(state: &mut State, undo: bool) -> Task<Message> {
    state.editor_menu = None;
    let doc = state.active_mut().editor.next_doc();
    let Some(tab) = state.active_mut().editor.tab_mut() else { return Task::none() };
    if tab.content.is_none() {
        return Task::none();
    }
    let current = tab.snapshot();
    let restore = if undo { tab.undo.pop() } else { tab.redo.pop() };
    let Some(restore) = restore else { return Task::none() };
    if undo {
        tab.redo.push(current);
    } else {
        tab.undo.push(current);
    }
    tab.doc = doc;
    set_text(tab, &restore.text);
    tab.long_line |= highlight::has_long_line(&restore.text);
    place_caret(tab, restore.cursor);
    tab.dirty = tab.content_hash() != tab.saved_hash;
    Task::none()
}

/// Tab key: spaces to the next multiple of four, as one undo entry.
fn indent(state: &mut State) -> Task<Message> {
    let col = match state.active().editor.tab().and_then(|t| t.content.as_ref()) {
        Some(c) => c.cursor_position().1,
        None => return Task::none(),
    };
    let n = 4 - (col % 4);
    let mut tasks = Vec::new();
    for _ in 0..n {
        tasks.push(editor_action(
            state,
            text_editor::Action::Edit(text_editor::Edit::Insert(' ')),
        ));
    }
    Task::batch(tasks)
}

fn save_tab(state: &mut State) -> Task<Message> {
    state.editor_menu = None;
    let active = state.active().editor.active;
    let Some(tab) = state.active_mut().editor.tab_mut() else { return Task::none() };
    if tab.content.is_none() {
        return Task::none();
    }
    // Written since it was read, and the watcher's debounce has not reported it yet:
    // ask, as the watcher would have, rather than overwrite it unseen. "Keep mine"
    // takes the new version as the baseline, so saving again then goes through.
    if ed::check_disk(&tab.path, tab.disk) == DiskChange::Changed {
        state.active_mut().editor.reload_prompt = active;
        return Task::none();
    }
    let lines = tab.lines();
    let bytes = ed::serialize(lines.clone(), tab.eol, tab.trailing_newline, tab.bom);
    match ed::write_file(&tab.path, &bytes) {
        Ok(()) => {
            tab.saved_hash = ed::hash_lines(lines.iter().map(String::as_str));
            tab.dirty = false;
            tab.missing = false;
            tab.disk = ed::DiskStamp::read(&tab.path);
        }
        Err(e) => {
            let body = format!("{}: {e}", tab.path.display());
            state.notice = Some(Notice { title: "Could not save".into(), body });
        }
    }
    Task::none()
}

fn close_tab(state: &mut State, i: usize) -> Task<Message> {
    state.editor_menu = None;
    let dirty = state.active().editor.tabs.get(i).map(|t| t.dirty).unwrap_or(false);
    if dirty {
        state.active_mut().editor.close_confirm = Some(i);
        return Task::none();
    }
    drop_tab(state, i);
    save_session(state);
    Task::none()
}

fn drop_tab(state: &mut State, i: usize) {
    let ed = &mut state.active_mut().editor;
    if i >= ed.tabs.len() {
        return;
    }
    let was = ed.active.unwrap_or(0);
    ed.tabs.remove(i);
    if ed.tabs.is_empty() {
        ed.visible = false;
        ed.active = None;
        return;
    }
    // Everything after the closed tab shifts down one; closing the active tab
    // hands the slot to its right-hand neighbour, or the new last tab.
    let next = if was > i { was - 1 } else { was };
    ed.active = Some(next.min(ed.tabs.len() - 1));
}

fn close_dirty_answer(state: &mut State, answer: CloseAnswer) -> Task<Message> {
    let Some(i) = state.active_mut().editor.close_confirm.take() else { return Task::none() };
    match answer {
        CloseAnswer::Cancel => Task::none(),
        CloseAnswer::Discard => {
            drop_tab(state, i);
            save_session(state);
            Task::none()
        }
        CloseAnswer::Save => {
            let was = state.active().editor.active;
            state.active_mut().editor.active = Some(i);
            let task = save_tab(state);
            let saved = state.active().editor.tabs.get(i).map(|t| !t.dirty).unwrap_or(false);
            if saved {
                drop_tab(state, i);
            } else {
                state.active_mut().editor.active = was;
            }
            save_session(state);
            task
        }
    }
}

fn selection_text(state: &State) -> Option<String> {
    state.active().editor.tab()?.content.as_ref()?.selection()
}

fn send_to_agent(state: &mut State) -> Task<Message> {
    state.editor_menu = None;
    let Some(tab) = state.active().editor.tab() else { return Task::none() };
    let Some(content) = tab.content.as_ref() else { return Task::none() };
    let Some(selected) = content.selection() else { return Task::none() };
    let caret = content.cursor_position();
    let range = ed::selection_lines(|i| content.line(i).map(|l| l.to_string()), caret, &selected);
    let (range, code) = ed::trim_dangling_line(range, &selected);
    let text = ed::agent_message(
        &tab.path,
        range.0 + 1,
        range.1 + 1,
        &ed::fence_tag(&tab.path),
        &code,
    );

    let claude: Vec<(pane_grid::Pane, String, String)> = state
        .active()
        .panes
        .iter()
        .filter(|(_, d)| d.session.claude_running())
        .map(|(p, d)| (*p, d.name.clone(), pane_where(d)))
        .collect();
    if claude.len() == 1 {
        let pane = claude[0].0;
        return deliver(state, pane, text);
    }
    let all_terminals = claude.is_empty();
    let rows = if all_terminals {
        state
            .active()
            .panes
            .iter()
            .map(|(p, d)| (*p, d.name.clone(), pane_where(d)))
            .collect()
    } else {
        claude
    };
    if rows.is_empty() {
        return Task::none();
    }
    state.send_target = Some(SendTarget { text, rows, selected: 0, all_terminals });
    Task::none()
}

fn pane_where(d: &PaneData) -> String {
    let mut s = d.session.cwd().unwrap_or_default();
    if d.session.is_remote() {
        s = if s.is_empty() { "(remote)".into() } else { format!("{s} (remote)") };
    }
    s
}

/// Paste the message into a pane and get out of the way.
fn deliver(state: &mut State, pane: pane_grid::Pane, text: String) -> Task<Message> {
    paste_into(state.active_mut(), pane, &text);
    state.active_mut().editor.visible = false;
    release_background_tabs(state);
    super::update(state, Message::Focus(pane))
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// The explorer pane: header, then the tree.
pub fn pane_view<'a>(state: &'a State, e: &'a Explorer) -> Element<'a, Message> {
    let title = e
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| e.root.to_string_lossy().into_owned());
    let has_tabs = !state.active().editor.tabs.is_empty();
    let header = container(
        row![
            text(title.to_uppercase()).size(12).font(ui_semibold()).color(TXT_SECONDARY),
            horizontal_space(),
            header_btn(
                mdi_path::FILE_DOCUMENT_EDIT_OUTLINE,
                has_tabs.then_some(Message::Files(Msg::EditorToggle)),
                state.active().editor.visible,
            ),
            header_btn(mdi_path::FOLDER_OPEN, Some(Message::Files(Msg::PickFolder)), false),
            header_btn(mdi_path::CLOSE, Some(Message::Files(Msg::Hide)), false),
        ]
        .spacing(2)
        .align_y(iced::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(HEADER_H))
    // A container stacks its content at the top unless told otherwise, which
    // left the title and the buttons riding high in the 34 px bar.
    .align_y(iced::Center)
    .padding(iced::Padding {
        top: 0.0,
        right: 6.0,
        bottom: 0.0,
        left: TREE_PAD + ROW_PAD_X,
    })
    .style(|_t: &iced::Theme| container::Style {
        background: Some(iced::Background::Color(app_header_bg())),
        border: iced::Border {
            radius: iced::border::Radius {
                top_left: 8.0,
                top_right: 8.0,
                bottom_right: 0.0,
                bottom_left: 0.0,
            },
            ..Default::default()
        },
        ..Default::default()
    });

    let mut rows = column![].spacing(0).padding(TREE_PAD);
    // A right-click no longer moves the selection, so the row the open menu is
    // about is marked here instead; otherwise there is nothing saying which one
    // "Rename" would rename.
    let menu_on = state.explorer_menu.as_ref().and_then(|m| m.target.clone());
    for (entry, depth) in e.rows() {
        let targeted = menu_on.as_deref() == Some(entry.path.as_path());
        rows = rows.push(tree_row(e, &entry, depth, targeted));
    }
    // The press falls through to here only when no row swallowed it, which is
    // how a click on the empty space below the tree clears the selection.
    let body = mouse_area(scrollable(rows).width(Length::Fill).height(Length::Fill))
        .on_press(Message::Files(Msg::ClearSelection))
        .on_right_press(Message::Files(Msg::MenuOpen(None)));

    container(column![header, hline(), body].width(Length::Fill).height(Length::Fill))
        .width(Length::Fixed(e.width))
        .height(Length::Fill)
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(app_bg())),
            border: iced::Border { radius: 8.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

/// A 14 px icon button for the explorer header. `None` renders it greyed out.
fn header_btn(path: &'static str, msg: Option<Message>, active: bool) -> Element<'static, Message> {
    let color = match (&msg, active) {
        (None, _) => iced::Color::from_rgb8(0x5a, 0x5a, 0x5a),
        (Some(_), true) => AZURE,
        (Some(_), false) => TXT_SECONDARY,
    };
    let enabled = msg.is_some();
    let mut b = button(cmdi(path, 14.0, color)).padding([4, 5]).style(move |_t: &iced::Theme, s| {
        let hovered = enabled && matches!(s, button::Status::Hovered);
        button::Style {
            background: if active {
                Some(iced::Background::Color(iced::Color::from_rgba8(0x33, 0x99, 0xff, 0.15)))
            } else if hovered {
                Some(iced::Background::Color(iced::Color::from_rgb8(0x25, 0x25, 0x25)))
            } else {
                None
            },
            border: iced::Border { radius: 4.0.into(), ..Default::default() },
            ..Default::default()
        }
    });
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

/// Text colour for a row by its git state. A folder carries the strongest state
/// beneath it, so a collapsed tree still shows where the changes are.
fn status_color(status: Option<&Status>) -> iced::Color {
    match status {
        Some(Status::Modified) => iced::Color::from_rgb8(0xe2, 0xc0, 0x8d),
        Some(Status::Added) | Some(Status::Untracked) | Some(Status::Renamed) => {
            iced::Color::from_rgb8(0x73, 0xc9, 0x91)
        }
        Some(Status::Deleted) => iced::Color::from_rgb8(0xc7, 0x4e, 0x39),
        Some(Status::Conflicted) => iced::Color::from_rgb8(0xe5, 0xc0, 0x7b),
        None => iced::Color::from_rgb8(0xc8, 0xcc, 0xd4),
    }
}

fn tree_row<'a>(
    e: &Explorer,
    entry: &DirEntry,
    depth: usize,
    targeted: bool,
) -> Element<'a, Message> {
    let color = status_color(e.tree.status.get(&entry.path));
    let selected = e.selected.contains(&entry.path);
    // Directories get a chevron and no folder icon, as the web explorer had it.
    let open = entry.is_dir && e.tree.expanded.contains(&entry.path);
    // A file takes the chevron's slot as blank space, so every name on a level
    // starts at the same x whether or not its row can be expanded.
    let chevron: Element<'a, Message> = if entry.is_dir {
        let p = if open { mdi_path::CHEVRON_DOWN } else { mdi_path::CHEVRON_RIGHT };
        mdi(p, ICON_PX, iced::Color::from_rgb8(0x9c, 0x9c, 0x9c))
    } else {
        Space::with_width(Length::Fixed(ICON_PX)).into()
    };
    let icon: Element<'a, Message> = if entry.is_dir {
        let p = if open { mdi_path::FOLDER_OPEN } else { mdi_path::FOLDER };
        mdi(p, ICON_PX, FOLDER_TINT)
    } else {
        let (p, (r, g, b)) = file_icons::file_icon(&entry.name);
        mdi(p, ICON_PX, iced::Color::from_rgb8(r, g, b))
    };
    let content = row![
        Space::with_width(Length::Fixed(depth as f32 * ROW_INDENT)),
        chevron,
        icon,
        text(entry.name.clone()).size(13).color(color),
    ]
    .spacing(4)
    .align_y(iced::Center);
    let btn = button(content)
        .width(Length::Fill)
        .padding(iced::Padding { top: 2.0, right: ROW_PAD_X, bottom: 2.0, left: ROW_PAD_X })
        .on_press(Message::Files(Msg::Select(entry.path.clone(), entry.is_dir)))
        .style(move |_t: &iced::Theme, s| {
            let hovered = matches!(s, button::Status::Hovered);
            let bg = if selected {
                Some(iced::Color::from_rgba8(0x33, 0x99, 0xff, 0.18))
            } else if targeted || hovered {
                Some(iced::Color::from_rgb8(0x25, 0x25, 0x25))
            } else {
                None
            };
            button::Style {
                background: bg.map(iced::Background::Color),
                border: iced::Border {
                    color: if targeted {
                        iced::Color::from_rgba8(0x33, 0x99, 0xff, 0.45)
                    } else {
                        iced::Color::TRANSPARENT
                    },
                    width: if targeted { 1.0 } else { 0.0 },
                    radius: 4.0.into(),
                },
                ..Default::default()
            }
        });
    mouse_area(btn)
        .on_right_press(Message::Files(Msg::MenuOpen(Some(entry.path.clone()))))
        .into()
}

/// The draggable gap between the explorer and the content.
pub fn handle_view() -> Element<'static, Message> {
    mouse_area(Space::new(Length::Fixed(HANDLE_W), Length::Fill))
        .on_press(Message::Files(Msg::DragStart))
        .interaction(iced::mouse::Interaction::ResizingHorizontally)
        .into()
}

/// While a drag runs, one window-wide layer collects the moves and the release.
/// `mouse_area::on_move` reports only while the cursor is inside it, and the
/// app's own cursor messages stop below the titlebar, so the handle alone would
/// lose the drag on the first fast pixel.
pub fn drag_overlay() -> Element<'static, Message> {
    mouse_area(Space::new(Length::Fill, Length::Fill))
        .on_move(|p| Message::Files(Msg::DragMove(p)))
        .on_release(Message::Files(Msg::DragEnd))
        .interaction(iced::mouse::Interaction::ResizingHorizontally)
        .into()
}

/// The editor, in the terminal grid's place.
pub fn editor_view(state: &State) -> Element<'_, Message> {
    let ed = &state.active().editor;
    let body: Element<Message> = match ed.tab() {
        Some(tab) => match (&tab.content, &tab.load_error) {
            (_, Some(err)) => container(text(err.clone()).size(13).color(TXT_MUTED))
                .center(Length::Fill)
                .padding(24)
                .into(),
            (Some(content), None) => text_area(state, tab, content),
            (None, None) => container(text("Loading\u{2026}").size(13).color(TXT_MUTED))
                .center(Length::Fill)
                .into(),
        },
        None => container(text("No file open").size(13).color(TXT_MUTED)).center(Length::Fill).into(),
    };
    container(column![tab_strip(ed), hline(), body, hline(), footer(ed)].width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(app_bg())),
            border: iced::Border { radius: 8.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

fn text_area<'a>(
    state: &'a State,
    tab: &'a EditorTab,
    content: &'a text_editor::Content,
) -> Element<'a, Message> {
    ed::timing::frame();
    let mono = iced::Font::with_name(highlight::MONO_FAMILY);
    let lh = iced::widget::text::LineHeight::Absolute(iced::Pixels(EDITOR_LINE_H));
    let count = content.line_count().max(1);
    let digits = count.to_string().len();
    let gutter_w = (digits as f32 * MONO_ADVANCE + 8.0).ceil();

    // Where the editor has actually scrolled to, read from its own buffer. The
    // numbers are derived from it every frame rather than tracked beside it, so
    // the two columns cannot come apart. Before the first layout the buffer has
    // no height yet; the window's own is an over-estimate, and drawing a few
    // numbers too many costs nothing because the container clips them.
    let (first, offset, visible, page_lines) = {
        let editor = content.editor();
        let buffer = editor.buffer();
        let scroll = buffer.scroll();
        let view_h = match buffer.size().1 {
            Some(h) if h > 0.0 => h,
            _ => state.main_size.height,
        };
        let (first, offset, visible) = ed::gutter_window(
            scroll.line,
            scroll.vertical,
            view_h,
            EDITOR_LINE_H,
            content.line_count(),
        );
        (first, offset, visible, ((view_h / EDITOR_LINE_H) as usize).max(1))
    };
    let gutter = gutter::Gutter::new(
        first,
        visible,
        EDITOR_PAD + offset,
        gutter_w,
        EDITOR_LINE_H,
        EDITOR_FONT_PX,
        mono,
        TXT_MUTED,
    );

    let modal_up = state.active().editor.modal_up() || modal_is_open(state);
    let active = state.active().editor.active.unwrap_or(0);

    // A file past the cap is shown plain: the widget re-highlights from the
    // edited line to the last visible one on every keystroke. So is one with a
    // line too long to parse in a frame.
    let token = if count > highlight::MAX_HIGHLIGHT_LINES || tab.long_line {
        "txt"
    } else {
        tab.lang.as_str()
    };
    let editor = text_editor(content)
        .font(mono)
        .size(EDITOR_FONT_PX)
        .line_height(lh)
        // Room on the left for the numbers drawn beneath, so the editor's own
        // bounds still cover them and a drag across them keeps selecting. This
        // needs the editor's padding to be asymmetric, which upstream iced 0.13
        // hit-tests wrongly (it applies the padding to swapped axes). That is one
        // of the two things `vendor/iced_widget` exists to fix.
        .padding(iced::Padding {
            top: EDITOR_PAD,
            right: EDITOR_PAD,
            bottom: EDITOR_PAD,
            left: gutter_w + GUTTER_GAP,
        })
        .wrapping(iced::widget::text::Wrapping::None)
        // The editor is the viewport and scrolls itself, so cosmic-text shapes
        // and iced highlights only the lines on screen. Laying the whole document
        // out to keep a gutter beside it in step costs a shaping pass over every
        // line of the file, three times over, in the frame that opens it.
        .height(Length::Fill)
        .on_action(|a| Message::Files(Msg::EditorAction(a)))
        .key_binding(move |kp| key_binding(kp, modal_up, active, page_lines))
        .style(editor_style)
        .highlight_with::<highlight::Syntax>(
            highlight::Settings { token: token.to_string(), doc: tab.hl_doc },
            highlight::format,
        );

    let editor = mouse_area(editor).on_right_press(Message::Files(Msg::EditorMenuOpen));
    container(iced::widget::stack![editor, gutter])
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .into()
}

fn editor_style(_t: &iced::Theme, _s: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: iced::Background::Color(app_bg()),
        border: iced::Border::default(),
        icon: TXT_MUTED,
        placeholder: TXT_MUTED,
        value: TXT_PRIMARY,
        selection: iced::Color { a: 0.35, ..AZURE },
    }
}

/// Which keys the editor takes. Returning `None` leaves the key to the app's own
/// handler, which is how the workspace chords keep working while editing.
fn key_binding(
    kp: text_editor::KeyPress,
    modal_up: bool,
    active: usize,
    page_lines: usize,
) -> Option<text_editor::Binding<Message>> {
    use iced::keyboard::key::Named;
    use iced::keyboard::Key;
    use text_editor::{Binding, Motion};

    // A focused editor sits under every modal in the view stack, so while one is
    // open it must let the arrows and Enter through to the dialog.
    if modal_up || !matches!(kp.status, text_editor::Status::Focused) {
        return None;
    }
    // Ctrl on every platform, as the rest of the app's shortcuts are, and Cmd too on
    // macOS, where iced's own bindings and every other editor expect it.
    let cmd = kp.modifiers.command() || kp.modifiers.control();
    let (shift, alt) = (kp.modifiers.shift(), kp.modifiers.alt());
    let files = |m: Msg| Some(Binding::Custom(Message::Files(m)));
    match kp.key.as_ref() {
        // Page motion is computed from the buffer height, which is unbounded
        // here, so the built-in binding would jump to the ends of the document.
        Key::Named(Named::PageUp) => {
            Some(Binding::Sequence(vec![Binding::Move(Motion::Up); page_lines]))
        }
        Key::Named(Named::PageDown) => {
            Some(Binding::Sequence(vec![Binding::Move(Motion::Down); page_lines]))
        }
        Key::Named(Named::Tab) if cmd => None, // Ctrl+Tab still switches workspace
        Key::Named(Named::Tab) if shift => Some(Binding::Custom(Message::Noop)),
        Key::Named(Named::Tab) => files(Msg::IndentKey),
        Key::Character(c) if cmd && !alt => match c.to_lowercase().as_str() {
            "z" if shift => files(Msg::Redo),
            "z" => files(Msg::Undo),
            "y" => files(Msg::Redo),
            "s" if !shift => files(Msg::Save),
            // Closing a hidden terminal by accident would be unrecoverable, so
            // both spellings close the tab instead.
            "w" => files(Msg::TabClose(active)),
            // Spelled out: iced's defaults answer to Cmd alone on macOS.
            "c" => Some(Binding::Copy),
            "x" => Some(Binding::Cut),
            "v" => Some(Binding::Paste),
            "a" => Some(Binding::SelectAll),
            // Ctrl+1..9 and the Ctrl+Shift chords stay with the app.
            d if d.chars().all(|ch| ch.is_ascii_digit()) => None,
            _ if shift => None,
            // Anything else would reach the terminal as a control byte.
            _ => Some(Binding::Custom(Message::Noop)),
        },
        _ => Binding::from_key_press(kp),
    }
}

fn tab_strip(ed: &Editor) -> Element<'_, Message> {
    // The row keeps its natural height and the bar centres it, rather than the
    // row filling: `scrollable` validates its content against the DEFAULT
    // vertical direction when it is constructed, before `.direction()` makes it
    // horizontal, and a filling height there trips its assertion.
    let mut tabs = row![].spacing(4).padding([0, 6]).align_y(iced::Center);
    for (i, t) in ed.tabs.iter().enumerate() {
        let active = ed.active == Some(i);
        let (fg, bg) = if active {
            (TXT_PRIMARY, Some(iced::Color::from_rgba8(0xff, 0xff, 0xff, 0.08)))
        } else {
            (TXT_SECONDARY, None)
        };
        let mark = if t.dirty { mdi_path::CIRCLE_MEDIUM } else { mdi_path::CLOSE };
        let label = row![
            text(t.name()).size(12).color(fg),
            button(cmdi(mark, 11.0, fg))
                .padding(2)
                .on_press(Message::Files(Msg::TabClose(i)))
                .style(|_t: &iced::Theme, s| button::Style {
                    background: matches!(s, button::Status::Hovered)
                        .then(|| iced::Background::Color(iced::Color::from_rgb8(0x3a, 0x3a, 0x3a))),
                    border: iced::Border { radius: 3.0.into(), ..Default::default() },
                    ..Default::default()
                }),
        ]
        .spacing(6)
        .align_y(iced::Center);
        let pill = container(label).padding([2, 6]).style(move |_t: &iced::Theme| container::Style {
            background: bg.map(iced::Background::Color),
            border: iced::Border { radius: 6.0.into(), ..Default::default() },
            ..Default::default()
        });
        tabs = tabs.push(
            iced::widget::tooltip(
                mouse_area(pill).on_press(Message::Files(Msg::TabSelect(i))),
                container(text(t.path.to_string_lossy().into_owned()).size(11).color(TXT_SECONDARY))
                    .padding([4, 8])
                    .style(|_t: &iced::Theme| container::Style {
                        background: Some(iced::Background::Color(iced::Color::from_rgb8(0x25, 0x25, 0x25))),
                        border: iced::Border {
                            color: iced::Color::from_rgb8(0x2c, 0x2c, 0x2c),
                            width: 1.0,
                            radius: 4.0.into(),
                        },
                        ..Default::default()
                    }),
                iced::widget::tooltip::Position::Bottom,
            ),
        );
    }
    container(scrollable(tabs).direction(scrollable::Direction::Horizontal(
        scrollable::Scrollbar::new().width(2).scroller_width(2),
    )))
    .width(Length::Fill)
    .height(Length::Fixed(HEADER_H))
    .align_y(iced::Center)
    .style(|_t: &iced::Theme| container::Style {
        background: Some(iced::Background::Color(app_header_bg())),
        border: iced::Border {
            radius: iced::border::Radius {
                top_left: 8.0,
                top_right: 8.0,
                bottom_right: 0.0,
                bottom_left: 0.0,
            },
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

fn footer(ed: &Editor) -> Element<'_, Message> {
    let mut items = row![].spacing(14).align_y(iced::Center);
    if let Some(t) = ed.tab() {
        if let Some(c) = t.content.as_ref() {
            let (line, byte) = c.cursor_position();
            let col = c.line(line).map(|l| l[..byte.min(l.len())].chars().count()).unwrap_or(0) + 1;
            items = items.push(text(format!("Ln {}, Col {}", line + 1, col)).size(11).color(TXT_MUTED));
        }
        items = items.push(text(t.eol.label()).size(11).color(TXT_MUTED));
        items = items.push(text(t.lang.clone()).size(11).color(TXT_MUTED));
        if t.missing {
            items = items.push(
                text("Deleted on disk").size(11).color(iced::Color::from_rgb8(0xe5, 0x6b, 0x6f)),
            );
        } else if t.dirty {
            items = items.push(text("Modified").size(11).color(TXT_MUTED));
        }
        items = items.push(horizontal_space());
        items = items.push(text(t.path.to_string_lossy().into_owned()).size(11).color(TXT_MUTED));
    }
    container(items)
        .width(Length::Fill)
        .height(Length::Fixed(FOOTER_H))
        .padding(iced::Padding { top: 0.0, right: 10.0, bottom: 0.0, left: 10.0 })
        .into()
}

// ---------------------------------------------------------------------------
// Menus and dialogs
// ---------------------------------------------------------------------------

pub fn menu_view<'a>(state: &State, m: &Menu) -> Element<'a, Message> {
    let sel = menu_targets(state);
    let n = sel.len();
    let on_root = m.target.is_none();
    let single = n == 1;
    let all_files = n >= 1 && sel.iter().all(|p| p.is_file());
    let files = |msg: Msg| Some(Message::Files(msg));
    let reveal_target = sel
        .first()
        .cloned()
        .or_else(|| state.active().explorer.as_ref().map(|e| e.root.clone()));

    let open_label =
        if n > 1 { format!("Open {n} files") } else { "Open".to_string() };
    let mut items = column![].spacing(0).padding([4, 0]);
    items = items.push(menu_item(
        mdi_path::OPEN_IN_APP,
        open_label,
        all_files.then(|| Message::Files(Msg::OpenSelection)),
        false,
    ));
    items = items.push(menu_item(
        mdi_path::OPEN_IN_APP,
        "Open in default app".into(),
        if single { files(Msg::OpenExternal) } else { None },
        false,
    ));
    items = items.push(menu_item(
        mdi_path::FOLDER_OPEN,
        reveal_label().to_string(),
        reveal_target.map(|p| Message::Files(Msg::Reveal(p))),
        false,
    ));
    items = items.push(menu_divider());
    items = items.push(menu_item(
        mdi_path::FILE_PLUS_OUTLINE,
        "New file".into(),
        files(Msg::New(false)),
        false,
    ));
    items = items.push(menu_item(
        mdi_path::FOLDER_PLUS_OUTLINE,
        "New folder".into(),
        files(Msg::New(true)),
        false,
    ));
    items = items.push(menu_divider());
    items = items.push(menu_item(
        mdi_path::CONTENT_COPY,
        "Copy path".into(),
        if n >= 1 || on_root { files(Msg::CopyPath(false)) } else { None },
        false,
    ));
    items = items.push(menu_item(
        mdi_path::LINK_VARIANT,
        "Copy relative path".into(),
        if n >= 1 && !on_root { files(Msg::CopyPath(true)) } else { None },
        false,
    ));
    items = items.push(menu_divider());
    items = items.push(menu_item(
        mdi_path::PENCIL,
        "Rename".into(),
        if single { files(Msg::RenameStart) } else { None },
        false,
    ));
    let delete_label = if n > 1 { format!("Delete {n} items") } else { "Delete".to_string() };
    items = items.push(menu_item(
        mdi_path::DELETE,
        delete_label,
        if n >= 1 { files(Msg::DeleteStart) } else { None },
        true,
    ));

    let est_h = 10.0 * 30.0 + 3.0 * 9.0 + 8.0;
    context_menu_card(items, 224.0, est_h, m.x, m.y, state.main_size, Message::Files(Msg::MenuClose))
}

pub fn editor_menu_view<'a>(state: &State, x: f32, y: f32) -> Element<'a, Message> {
    let ed = &state.active().editor;
    let tab = ed.tab();
    let has_sel = tab
        .and_then(|t| t.content.as_ref())
        .and_then(|c| c.selection())
        .is_some();
    let can_undo = tab.map(|t| !t.undo.is_empty()).unwrap_or(false);
    let can_redo = tab.map(|t| !t.redo.is_empty()).unwrap_or(false);
    let dirty = tab.map(|t| t.dirty).unwrap_or(false);
    let active = ed.active.unwrap_or(0);
    let files = |m: Msg| Some(Message::Files(m));

    let mut items = column![].spacing(0).padding([4, 0]);
    let mut rows = 0.0;
    if has_sel {
        items = items.push(menu_item(mdi_path::SEND, "Send to Agent".into(), files(Msg::SendToAgent), false));
        items = items.push(menu_divider());
        rows += 1.0;
    }
    items = items.push(menu_item(mdi_path::UNDO, "Undo".into(), can_undo.then(|| Message::Files(Msg::Undo)), false));
    items = items.push(menu_item(mdi_path::REDO, "Redo".into(), can_redo.then(|| Message::Files(Msg::Redo)), false));
    items = items.push(menu_divider());
    items = items.push(menu_item(mdi_path::CONTENT_CUT, "Cut".into(), has_sel.then(|| Message::Files(Msg::Cut)), false));
    items = items.push(menu_item(mdi_path::CONTENT_COPY, "Copy".into(), has_sel.then(|| Message::Files(Msg::Copy)), false));
    items = items.push(menu_item(mdi_path::CONTENT_PASTE, "Paste".into(), files(Msg::Paste), false));
    items = items.push(menu_item(mdi_path::DELETE, "Delete".into(), has_sel.then(|| Message::Files(Msg::DeleteSelection)), false));
    items = items.push(menu_divider());
    items = items.push(menu_item(mdi_path::SELECT_ALL, "Select All".into(), files(Msg::SelectAll), false));
    items = items.push(menu_item(mdi_path::CONTENT_SAVE, "Save".into(), dirty.then(|| Message::Files(Msg::Save)), false));
    items = items.push(menu_divider());
    items = items.push(menu_item(mdi_path::CLOSE, "Close tab".into(), files(Msg::TabClose(active)), true));
    rows += 9.0;

    let est_h = rows * 30.0 + 4.0 * 9.0 + 8.0;
    context_menu_card(items, 224.0, est_h, x, y, state.main_size, Message::Files(Msg::EditorMenuClose))
}

pub fn prompt_view(p: &Prompt) -> Element<'static, Message> {
    let (title, action) = match p.kind {
        PromptKind::Rename(_) => ("Rename", "Rename"),
        PromptKind::NewFile(_) => ("New file", "Create"),
        PromptKind::NewFolder(_) => ("New folder", "Create"),
    };
    let input = text_input("Name", &p.text)
        .id(text_input::Id::new(PROMPT_INPUT))
        .on_input(|t| Message::Files(Msg::PromptInput(t)))
        .on_submit(Message::Files(Msg::PromptCommit))
        .padding([7, 9])
        .size(13);
    let mut panel = column![text(title).size(15).font(ui_semibold()), input].spacing(14);
    if let Some(e) = &p.error {
        panel = panel.push(text(e.clone()).size(11).color(iced::Color::from_rgb8(0xe5, 0x6b, 0x6f)));
    }
    panel = panel.push(
        row![
            horizontal_space(),
            button(text("Cancel").size(13))
                .on_press(Message::Files(Msg::PromptCancel))
                .style(button::secondary)
                .padding([6, 14]),
            button(text(action).size(13))
                .on_press(Message::Files(Msg::PromptCommit))
                .style(primary_btn_style)
                .padding([6, 14]),
        ]
        .spacing(8)
        .align_y(iced::Center),
    );
    modal_scrim(
        modal_panel(panel.padding(18).width(Length::Fixed(340.0)).into()),
        Message::Files(Msg::PromptCancel),
    )
}

pub fn delete_view(d: &Delete) -> Element<'static, Message> {
    let body = if d.paths.len() == 1 && d.paths[0].is_dir() {
        "The folder and everything in it will be moved to the OS trash."
    } else if d.paths.len() == 1 {
        "The item will be moved to the OS trash."
    } else {
        "The selected items will be moved to the OS trash."
    };
    let panel = column![
        text(d.label.clone()).size(15).font(ui_semibold()),
        text(body).size(13).color(TXT_SECONDARY),
        row![
            horizontal_space(),
            button(text("Cancel").size(13))
                .on_press(Message::Files(Msg::DeleteCancel))
                .style(button::secondary)
                .padding([6, 14]),
            button(text("Delete").size(13))
                .on_press(Message::Files(Msg::DeleteConfirm))
                .style(danger_btn_style)
                .padding([6, 14]),
        ]
        .spacing(8)
        .align_y(iced::Center),
    ]
    .spacing(14)
    .padding(18)
    .width(Length::Fixed(380.0));
    modal_scrim(modal_panel(panel.into()), Message::Files(Msg::DeleteCancel))
}

pub fn reload_view(state: &State) -> Option<Element<'static, Message>> {
    let ed = &state.active().editor;
    let t = ed.tabs.get(ed.reload_prompt?)?;
    let panel = column![
        text(format!("\u{201c}{}\u{201d} changed on disk", t.name())).size(15).font(ui_semibold()),
        text("Reload it and lose your unsaved edits?").size(13).color(TXT_SECONDARY),
        row![
            horizontal_space(),
            button(text("Keep mine").size(13))
                .on_press(Message::Files(Msg::ReloadAnswer(false)))
                .style(button::secondary)
                .padding([6, 14]),
            button(text("Reload").size(13))
                .on_press(Message::Files(Msg::ReloadAnswer(true)))
                .style(danger_btn_style)
                .padding([6, 14]),
        ]
        .spacing(8)
        .align_y(iced::Center),
    ]
    .spacing(14)
    .padding(18)
    .width(Length::Fixed(420.0));
    Some(modal_panel_only(panel.into()))
}

pub fn close_confirm_view(state: &State) -> Option<Element<'static, Message>> {
    let ed = &state.active().editor;
    let t = ed.tabs.get(ed.close_confirm?)?;
    let panel = column![
        text(format!("Save changes to \u{201c}{}\u{201d}?", t.name())).size(15).font(ui_semibold()),
        text("Your edits are lost if you do not save them.").size(13).color(TXT_SECONDARY),
        row![
            horizontal_space(),
            button(text("Cancel").size(13))
                .on_press(Message::Files(Msg::CloseDirtyAnswer(CloseAnswer::Cancel)))
                .style(button::secondary)
                .padding([6, 14]),
            button(text("Don't save").size(13))
                .on_press(Message::Files(Msg::CloseDirtyAnswer(CloseAnswer::Discard)))
                .style(danger_btn_style)
                .padding([6, 14]),
            button(text("Save").size(13))
                .on_press(Message::Files(Msg::CloseDirtyAnswer(CloseAnswer::Save)))
                .style(primary_btn_style)
                .padding([6, 14]),
        ]
        .spacing(8)
        .align_y(iced::Center),
    ]
    .spacing(14)
    .padding(18)
    .width(Length::Fixed(440.0));
    Some(modal_panel_only(panel.into()))
}

pub fn send_target_view(t: &SendTarget) -> Element<'static, Message> {
    let title = if t.all_terminals {
        "No Claude is running here. Send to which terminal?"
    } else {
        "Send to which Claude?"
    };
    let mut panel = column![text(title).size(15).font(ui_semibold())].spacing(10);
    for (i, (_, name, where_)) in t.rows.iter().enumerate() {
        let highlighted = i == t.selected;
        let (n_c, w_c) = if highlighted {
            (iced::Color::WHITE, iced::Color::from_rgba8(0xff, 0xff, 0xff, 0.7))
        } else {
            (TXT_SECONDARY, TXT_MUTED)
        };
        let mut labels = column![text(name.clone()).size(13).color(n_c)].spacing(1);
        if !where_.is_empty() {
            labels = labels.push(text(where_.clone()).size(11).color(w_c));
        }
        panel = panel.push(
            button(row![labels, horizontal_space()].align_y(iced::Center))
                .width(Length::Fill)
                .padding([8, 12])
                .on_press(Message::Files(Msg::SendPick(i)))
                .style(move |_t: &iced::Theme, s| {
                    let hovered = matches!(s, button::Status::Hovered);
                    let bg = if highlighted {
                        Some(AZURE)
                    } else if hovered {
                        Some(iced::Color::from_rgb8(0x2c, 0x2c, 0x2c))
                    } else {
                        None
                    };
                    button::Style {
                        background: bg.map(iced::Background::Color),
                        border: iced::Border { radius: 6.0.into(), ..Default::default() },
                        ..Default::default()
                    }
                }),
        );
    }
    panel = panel.push(
        text("\u{2191}\u{2193} choose \u{00b7} Enter sends \u{00b7} Esc cancels")
            .size(10)
            .color(TXT_MUTED),
    );
    modal_scrim(
        modal_panel(panel.padding(18).width(Length::Fixed(420.0)).into()),
        Message::Files(Msg::SendCancel),
    )
}

/// Arrows and Enter drive the picker; nothing else reaches the terminals behind it.
pub fn send_target_input(state: &mut State, bytes: &[u8]) -> Option<Task<Message>> {
    state.send_target.as_ref()?;
    Some(match bytes {
        b"\x1b[A" | b"\x1bOA" => update(state, Msg::SendMove(-1)),
        b"\x1b[B" | b"\x1bOB" => update(state, Msg::SendMove(1)),
        b"\r" => update(state, Msg::SendConfirm),
        _ => Task::none(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_width_stays_usable_and_never_takes_half_the_window() {
        assert_eq!(clamp_width(260.0, 1600.0), 260.0);
        assert_eq!(clamp_width(40.0, 1600.0), MIN_W);
        assert_eq!(clamp_width(1400.0, 1600.0), 800.0);
        // A window narrower than twice the minimum still yields the minimum,
        // rather than a pane too small to read.
        assert_eq!(clamp_width(200.0, 100.0), MIN_W);
    }

    #[test]
    fn a_restored_explorer_needs_its_folder_to_still_exist() {
        let saved = persist::SavedExplorer {
            root: "/definitely/not/here/arbiter-test".into(),
            width: 300.0,
            shown: true,
            expanded: Vec::new(),
        };
        assert!(from_saved(&saved, 1600.0, false).is_none());
    }

    #[test]
    fn a_rename_that_only_changes_case_is_the_same_file() {
        let dir = std::env::temp_dir().join(format!("arbiter-case-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lower = dir.join("readme.md");
        let other = dir.join("other.md");
        std::fs::write(&lower, "x").unwrap();
        std::fs::write(&other, "y").unwrap();
        let upper = dir.join("README.md");
        // True only where the file system folds case (macOS, Windows by default).
        if upper.exists() {
            assert!(is_same_entry(&lower, &upper));
        }
        assert!(!is_same_entry(&lower, &other));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tab_with(text: &str) -> EditorTab {
        let mut tab = EditorTab::unloaded(PathBuf::from("t.txt"));
        tab.content = Some(text_editor::Content::new());
        set_text(&mut tab, text);
        tab
    }

    #[test]
    fn a_file_read_into_the_widget_saves_back_byte_for_byte() {
        for original in ["a\nb\n", "a\nb\n\n", "a\nb", "", "\n", "x\r\ny\r\n"] {
            let loaded = ed::load_bytes(original.as_bytes()).expect("text");
            let tab = tab_with(&loaded.text);
            assert_eq!(tab.lines(), ed::buffer_lines(&loaded.text), "lines of {original:?}");
            let back = ed::serialize(tab.lines(), loaded.eol, loaded.trailing_newline, loaded.bom);
            assert_eq!(back, original.as_bytes(), "round trip of {original:?}");
        }
    }

    // Typing a character and deleting it again leaves the buffer as saved.
    #[test]
    fn a_reverted_edit_is_not_modified() {
        for original in ["a\nb\n", "a\nb", "a\n\n"] {
            let mut tab = tab_with(original);
            tab.saved_hash = tab.content_hash();
            let c = tab.content.as_mut().unwrap();
            c.perform(text_editor::Action::Edit(text_editor::Edit::Insert('x')));
            assert_ne!(tab.content_hash(), tab.saved_hash, "{original:?} edited");
            let c = tab.content.as_mut().unwrap();
            c.perform(text_editor::Action::Edit(text_editor::Edit::Backspace));
            assert_eq!(tab.content_hash(), tab.saved_hash, "{original:?} reverted");
        }
    }

    #[test]
    fn an_undo_snapshot_restores_the_same_lines() {
        for original in ["a\nb\n", "a\nb\n\n", "a\n\n\n", "a"] {
            let mut tab = tab_with(original);
            let before = tab.lines();
            let snap = tab.snapshot();
            set_text(&mut tab, &snap.text);
            assert_eq!(tab.lines(), before, "snapshot of {original:?}");
        }
    }

    #[test]
    fn restored_tabs_drop_files_that_have_gone_and_clamp_the_active_one() {
        let ed = editor_from_saved(&["/definitely/not/here/a.rs".to_string()], Some(0));
        assert!(ed.tabs.is_empty());
        assert_eq!(ed.active, None);
        assert!(!ed.visible, "a relaunch opens on the terminals");
    }
}
