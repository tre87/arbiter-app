//! Autosave/restore of the workspace layout — which terminals exist, their names,
//! shell, cwd, and the split tree — so a relaunch reopens where you left off (the
//! web's autosave parity). The live PTY processes can't be restored, so each saved
//! terminal is *respawned* in its saved cwd/shell (same as the web on restart).
//!
//! The bin (`iced_shell`) owns the `State`↔`SavedState` conversion (it knows the
//! `pane_grid` layout + how to spawn a `Session`); this module is just the
//! serialisable shape + the on-disk read/write.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which shell a saved terminal ran (mirrors the bin's `ShellKind`).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum SavedShell {
    PowerShell,
    GitBash,
}

/// Which secret an SSH connection asked for, read from ssh's own prompt the first time it
/// appeared, so the sign-in dialog can say which one it wants.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CredentialKind {
    /// A key's passphrase (`Enter passphrase for key '...'`).
    Passphrase,
    /// An account password (`user@host's password:`, or keyboard-interactive `Password:`).
    Password,
}

/// A machine to wake over the network (Settings, Wake on LAN): what to call it, and its
/// MAC address as typed (any spelling `wol::parse_mac` accepts; validated on entry).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct WolHost {
    pub name: String,
    pub mac: String,
}

/// The split tree of one workspace: interior `Split`s (mirroring `pane_grid::Node`)
/// and `Leaf` terminals.
#[derive(Serialize, Deserialize)]
pub enum SavedNode {
    Split {
        /// True = a vertical divider (left|right); false = horizontal (top/bottom).
        vertical: bool,
        ratio: f32,
        a: Box<SavedNode>,
        b: Box<SavedNode>,
    },
    Leaf {
        name: String,
        shell: SavedShell,
        cwd: Option<String>,
        /// Claude was running here → relaunch it on restore (`claude`, or
        /// `claude --resume <id>` if a session was bound). Defaulted so older
        /// save files (without these fields) still load.
        #[serde(default)]
        claude_running: bool,
        #[serde(default)]
        claude_session: Option<String>,
        /// Stable per-pane id keying this terminal's PRIVATE command-history file
        /// (`<data-dir>/history/<id>`), so each terminal keeps its own history across
        /// app exit + relaunch. Defaulted so older saves load (they get a fresh id on
        /// restore → an empty history, which is the intended "new terminal" behaviour).
        #[serde(default)]
        history_id: Option<String>,
        /// The command that put this terminal where it is, replayed on restore. Set
        /// when the pane is detected as remote, so a relaunch re-establishes the same
        /// ssh session instead of dropping you at a local prompt. Kept as the literal
        /// line the user typed, which is what makes it work for any host, jump-host
        /// chain or wrapper without Arbiter needing to model connections.
        #[serde(default)]
        startup_cmd: Option<String>,
        /// Whether that connection asks for a credential: `None` never observed, `Some(true)`
        /// it prompted, `Some(false)` it logged in without one. Decides whether the startup
        /// sign-in dialog has a row for it.
        #[serde(default)]
        prompts_for_credential: Option<bool>,
        /// Which secret it asked for, and for what (a key's file name, or `user@host`),
        /// read from ssh's prompt. Lets the sign-in dialog say what it wants.
        #[serde(default)]
        credential_kind: Option<CredentialKind>,
        #[serde(default)]
        credential_detail: Option<String>,
        /// The far host's working directory at save time, if its shell reported one (the
        /// opt-in snippet, `shell::REMOTE_OSC7_SNIPPET`). Only meaningful with `startup_cmd`.
        #[serde(default)]
        remote_cwd: Option<String>,
        /// Claude was running on the far host, so the replayed connection resumes it there.
        #[serde(default)]
        remote_claude: bool,
        /// That Claude's session id when Arbiter named or read it (see
        /// `ClaudeHandle::remote_session`), so the very conversation is resumed.
        #[serde(default)]
        remote_session: Option<String>,
    },
}

/// A window's saved size + (optional) position, in logical pixels.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct SavedWindow {
    pub width: f32,
    pub height: f32,
    pub x: Option<f32>,
    pub y: Option<f32>,
}

#[derive(Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub name: String,
    pub layout: SavedNode,
    /// The workspace's file explorer, once a folder has been picked for it.
    #[serde(default)]
    pub explorer: Option<SavedExplorer>,
    /// Files open in the editor, as absolute paths. Kept on the workspace rather
    /// than inside `explorer` so closing the explorer pane does not lose them.
    #[serde(default)]
    pub editor_tabs: Vec<String>,
    #[serde(default)]
    pub editor_active: Option<usize>,
}

/// Default width of the explorer pane, in logical pixels.
pub fn default_explorer_width() -> f32 {
    260.0
}

/// A workspace's file explorer. The editor's visibility is deliberately absent:
/// a restart comes back to the terminals, with the tabs still open behind them.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedExplorer {
    pub root: String,
    #[serde(default = "default_explorer_width")]
    pub width: f32,
    #[serde(default)]
    pub shown: bool,
    /// Absolute paths of the expanded folders.
    #[serde(default)]
    pub expanded: Vec<String>,
}

/// User-tweakable preferences (the Settings dialog). Kept small + serialised whole
/// so adding a field is back-compatible (missing fields fall back to `Default`).
#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    /// Hide the whole titlebar usage section (web `devStore.hideUsageBar`).
    #[serde(default)]
    pub hide_usage_bar: bool,
    /// Hide the per-model Sonnet usage meter (web `devStore.hideSonnetUsage`,
    /// default on — Sonnet is rarely the binding limit).
    #[serde(default = "default_true")]
    pub hide_sonnet_usage: bool,
    /// Show Fable's weekly usage meter in the titlebar, left of the 5h bar. Off by
    /// default; only plans that cap Fable separately have the window at all.
    #[serde(default)]
    pub show_fable_usage: bool,
    /// Overview popout lists only terminals running Claude (web
    /// `devStore.overviewClaudeOnly`). Off by default — show all terminals.
    #[serde(default)]
    pub overview_claude_only: bool,
    /// Keep the overview popout above other windows (it's a floating panel). On by
    /// default; toggle off to let it sit behind.
    #[serde(default = "default_true")]
    pub overview_topmost: bool,
    /// Show the Claude usage bars as a footer in the overview window. On by default.
    #[serde(default = "default_true")]
    pub overview_usage_footer: bool,
    /// Hide the Git Bash / shell-switch button in the terminal header (web
    /// `devStore.hideShellButton`).
    #[serde(default)]
    pub hide_shell_button: bool,
    /// Scrollback lines kept per terminal (web `devStore.scrollback`).
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
    /// Terminal font size in points. Applies to every terminal live (the renderer
    /// rebuilds + the PTY reflows). Bounded by `FONT_SIZE_MIN`/`MAX`.
    #[serde(default = "default_font_size")]
    pub font_size: u32,
    /// Show the terminal split/close buttons (+ their separator) in the titlebar.
    /// Off by default — the split/close shortcuts cover it.
    #[serde(default)]
    pub show_terminal_buttons: bool,
    /// Screenshot-attach folder override (web `filesStore.screenshotFolder`).
    /// `None` = the system default (macOS `~/Desktop`, else `~/Pictures/Screenshots`).
    #[serde(default)]
    pub screenshot_folder: Option<String>,
    /// Last folder used by "Attach files" (web `filesStore.lastDocsFolder`); sticky,
    /// not surfaced in the UI. `None` = the documents dir default.
    #[serde(default)]
    pub docs_folder: Option<String>,
    /// How bold/intense (SGR 1) text renders — mirrors Windows Terminal's
    /// `intenseTextStyle`. Default: a bold font face.
    #[serde(default)]
    pub intense_text_style: IntenseStyle,
    /// Background colour (hex `#rrggbb`) for the terminals, sidebars and overview.
    /// Presets in the UI: `#121212` (signature), `#000000`; or any custom hex.
    #[serde(default = "default_bg_hex")]
    pub background: String,
    /// Ask for confirmation before quitting the app (window close button / Cmd+Q /
    /// Alt+F4). On by default so a stray close can't silently drop every terminal.
    #[serde(default = "default_true")]
    pub confirm_on_quit: bool,
    /// Complete a `claude` typed at a far shell's prompt with `--session-id <uuid>`, so a
    /// restored SSH terminal resumes exactly that conversation (see
    /// `Session::on_remote_enter`). Off by default: it visibly edits what was typed, which
    /// is a surprise to anyone who did not ask for it.
    #[serde(default)]
    pub name_remote_claude_sessions: bool,
    /// Machines the Wake-on-LAN menu can wake (Settings, Wake on LAN), in list order.
    #[serde(default)]
    pub wol_hosts: Vec<WolHost>,
    /// Show the Wake-on-LAN button in the titlebar, right of the overview button. Off by
    /// default: the menu is also on Ctrl+Shift+M, and most people have nothing to wake.
    #[serde(default)]
    pub show_wol_button: bool,
    /// Show notification cards (lower right corner of the main monitor). Off by default:
    /// the user turns them on when wanted. Off silences the sound too, since there is
    /// nothing to announce.
    #[serde(default)]
    pub notifications: bool,
    /// Play the chime with each notification card. On by default.
    #[serde(default = "default_true")]
    pub notification_sound: bool,
    /// Announce Claude stopping to ask for input (a permission or a question). On by
    /// default.
    #[serde(default = "default_true")]
    pub notify_attention: bool,
    /// Announce Claude finishing a turn. On by default.
    #[serde(default = "default_true")]
    pub notify_finished: bool,
    /// A split opens its new terminal in the directory of the terminal being split, as
    /// Windows Terminal and iTerm2 do. On by default. Off starts it where a new
    /// workspace's first terminal starts.
    #[serde(default = "default_true")]
    pub split_keeps_cwd: bool,
    /// Offer the file explorer: a folder button in the titlebar and Ctrl+Shift+F.
    /// Off by default, like the Wake-on-LAN button, so nothing appears for anyone
    /// who only wants terminals. Off hides an open explorer without forgetting it.
    #[serde(default)]
    pub show_file_explorer: bool,
    /// Offer the Agents Office: a titlebar button and Ctrl+Shift+G opening a popout
    /// room where one desk is one running agent. Off by default, and experimental:
    /// turning it off closes the window, so nothing is left without a way to dismiss
    /// it.
    #[serde(default)]
    pub show_agents_office: bool,
}

/// Default background colour. `#0a0a0c` — near-black with a faint cool cast.
pub fn default_bg_hex() -> String {
    "#0a0a0c".to_string()
}

impl Settings {
    /// The background colour as RGB bytes, falling back to `#121212` if the stored hex
    /// is malformed (e.g. mid-edit in the custom field).
    pub fn bg_rgb(&self) -> (u8, u8, u8) {
        parse_hex(&self.background).unwrap_or((0x12, 0x12, 0x12))
    }
}

/// Parse a `#rrggbb` (or `rrggbb`) hex colour into RGB bytes. None if malformed.
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ))
}

/// How "intense" (SGR 1 / bold) terminal text is rendered. Mirrors Windows Terminal's
/// `intenseTextStyle`: `Bold` draws the bold font face, `Bright` brightens the colour at
/// regular weight (the classic xterm behaviour), `All` does both, `None` ignores it.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum IntenseStyle {
    None,
    #[default]
    Bold,
    Bright,
    All,
}

impl IntenseStyle {
    /// All variants, in menu order (for the Settings picker).
    pub const ALL: [IntenseStyle; 4] =
        [IntenseStyle::None, IntenseStyle::Bold, IntenseStyle::Bright, IntenseStyle::All];

    /// Compact code passed to the renderer's atomic (see `term::set_intense_style`):
    /// 0 = None, 1 = Bold, 2 = Bright, 3 = All.
    pub fn as_u8(self) -> u8 {
        match self {
            IntenseStyle::None => 0,
            IntenseStyle::Bold => 1,
            IntenseStyle::Bright => 2,
            IntenseStyle::All => 3,
        }
    }
}

impl std::fmt::Display for IntenseStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            IntenseStyle::None => "None",
            IntenseStyle::Bold => "Bold",
            IntenseStyle::Bright => "Bright",
            IntenseStyle::All => "Bold + Bright",
        })
    }
}

fn default_true() -> bool {
    true
}

fn default_scrollback() -> usize {
    5000
}

/// Bounds for the scrollback setting (web `SCROLLBACK_MIN`/`MAX`).
pub const SCROLLBACK_MIN: usize = 100;
pub const SCROLLBACK_MAX: usize = 100_000;

fn default_font_size() -> u32 {
    12
}

/// Bounds for the terminal font size setting (points).
pub const FONT_SIZE_MIN: u32 = 8;
pub const FONT_SIZE_MAX: u32 = 32;

impl Default for Settings {
    fn default() -> Self {
        Self {
            hide_usage_bar: false,
            hide_sonnet_usage: true,
            show_fable_usage: false,
            overview_claude_only: false,
            overview_topmost: true,
            overview_usage_footer: true,
            hide_shell_button: false,
            scrollback: default_scrollback(),
            font_size: default_font_size(),
            show_terminal_buttons: false,
            screenshot_folder: None,
            docs_folder: None,
            intense_text_style: IntenseStyle::Bold,
            background: default_bg_hex(),
            confirm_on_quit: true,
            name_remote_claude_sessions: false,
            wol_hosts: Vec::new(),
            show_wol_button: false,
            notifications: false,
            notification_sound: true,
            notify_attention: true,
            notify_finished: true,
            split_keeps_cwd: true,
            show_file_explorer: false,
            show_agents_office: false,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct SavedState {
    pub active: usize,
    pub workspaces: Vec<SavedWorkspace>,
    /// Main window geometry; defaulted so older save files still load.
    #[serde(default)]
    pub main_window: Option<SavedWindow>,
    /// Overview popout geometry; defaulted likewise.
    #[serde(default)]
    pub overview_window: Option<SavedWindow>,
    /// Whether the overview popout was open at save time → reopen it on startup.
    #[serde(default)]
    pub overview_visible: bool,
    /// Agents Office popout geometry; defaulted likewise.
    #[serde(default)]
    pub office_window: Option<SavedWindow>,
    /// Whether the Agents Office was open at save time → reopen it on startup.
    #[serde(default)]
    pub office_visible: bool,
    /// Chosen claude.ai org uuid for the usage bars (so the picker isn't re-shown).
    #[serde(default)]
    pub usage_org: Option<String>,
    /// User preferences (Settings dialog); defaulted so older saves load.
    #[serde(default)]
    pub settings: Settings,
}

fn path() -> Option<PathBuf> {
    Some(crate::shell::app_data_dir()?.join("session.json"))
}

/// Load the saved layout, or `None` if absent/unreadable/corrupt (→ fresh start).
pub fn load() -> Option<SavedState> {
    let bytes = std::fs::read(path()?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist the layout (best-effort; never panics — autosave must not break the UI).
pub fn save(state: &SavedState) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(state) {
        let _ = std::fs::write(&p, json);
    }
}

/// Delete the saved layout on disk (Settings → "Clear saved data"). Best-effort;
/// the live workspaces aren't touched — only what's remembered between launches.
pub fn clear() {
    if let Some(p) = path() {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_json() {
        let state = SavedState {
            active: 1,
            main_window: Some(SavedWindow { width: 1200.0, height: 800.0, x: Some(10.0), y: Some(20.0) }),
            overview_window: None,
            overview_visible: true,
            office_window: None,
            office_visible: false,
            usage_org: None,
            settings: Settings::default(),
            workspaces: vec![
                SavedWorkspace {
                    name: "Workspace 1".into(),
                    layout: SavedNode::Split {
                        vertical: true,
                        ratio: 0.4,
                        a: Box::new(SavedNode::Leaf {
                            name: "Terminal 1".into(),
                            shell: SavedShell::PowerShell,
                            cwd: Some("/tmp".into()),
                            claude_running: true,
                            claude_session: Some("sess-abc-123".into()),
                            history_id: Some("hist-abc-1".into()),
                            startup_cmd: None,
                            prompts_for_credential: None,
                            credential_kind: None,
                            credential_detail: None,
                            remote_cwd: None,
                            remote_claude: false,
                            remote_session: None,
                        }),
                        b: Box::new(SavedNode::Leaf {
                            name: "Terminal 2".into(),
                            shell: SavedShell::GitBash,
                            cwd: None,
                            claude_running: false,
                            claude_session: None,
                            history_id: None,
                            startup_cmd: Some("ssh mini".into()),
                            prompts_for_credential: Some(true),
                            credential_kind: Some(CredentialKind::Passphrase),
                            credential_detail: Some("id_ed25519".into()),
                            remote_cwd: Some("/home/tre/src".into()),
                            remote_claude: true,
                            remote_session: Some("abc-123".into()),
                        }),
                    },
                    explorer: Some(SavedExplorer {
                        root: "/tmp/project".into(),
                        width: 300.0,
                        shown: true,
                        expanded: vec!["/tmp/project/src".into()],
                    }),
                    editor_tabs: vec!["/tmp/project/src/main.rs".into()],
                    editor_active: Some(0),
                },
                SavedWorkspace {
                    name: "Workspace 2".into(),
                    explorer: None,
                    editor_tabs: Vec::new(),
                    editor_active: None,
                    layout: SavedNode::Leaf {
                        name: "Terminal 1".into(),
                        shell: SavedShell::PowerShell,
                        cwd: None,
                        claude_running: false,
                        claude_session: None,
                        history_id: None,
                        startup_cmd: None,
                        prompts_for_credential: None,
                        credential_kind: None,
                        credential_detail: None,
                        remote_cwd: None,
                        remote_claude: false,
                        remote_session: None,
                    },
                },
            ],
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        println!("{json}");
        let back: SavedState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.active, 1);
        assert_eq!(back.workspaces.len(), 2);
        assert_eq!(back.main_window.unwrap().width, 1200.0);
        let explorer = back.workspaces[0].explorer.as_ref().expect("explorer round-trips");
        assert_eq!(explorer.root, "/tmp/project");
        assert_eq!(explorer.width, 300.0);
        assert!(explorer.shown);
        assert_eq!(explorer.expanded, vec!["/tmp/project/src".to_string()]);
        assert_eq!(back.workspaces[0].editor_tabs, vec!["/tmp/project/src/main.rs".to_string()]);
        assert_eq!(back.workspaces[0].editor_active, Some(0));
        assert!(back.workspaces[1].explorer.is_none());
        match &back.workspaces[0].layout {
            SavedNode::Split { vertical, ratio, a, b } => {
                assert!(*vertical);
                assert!((*ratio - 0.4).abs() < 1e-6);
                match a.as_ref() {
                    SavedNode::Leaf {
                        claude_running,
                        claude_session,
                        history_id,
                        startup_cmd,
                        ..
                    } => {
                        assert!(*claude_running);
                        assert_eq!(claude_session.as_deref(), Some("sess-abc-123"));
                        assert_eq!(history_id.as_deref(), Some("hist-abc-1"));
                        assert!(startup_cmd.is_none(), "a local pane saves no startup command");
                    }
                    _ => panic!("expected a leaf"),
                }
                match b.as_ref() {
                    // The remote pane: its ssh line round-trips so a relaunch can
                    // replay it.
                    SavedNode::Leaf {
                        startup_cmd,
                        claude_running,
                        prompts_for_credential,
                        credential_kind,
                        credential_detail,
                        remote_cwd,
                        remote_claude,
                        remote_session,
                        ..
                    } => {
                        assert_eq!(startup_cmd.as_deref(), Some("ssh mini"));
                        assert!(!claude_running, "a remote pane must not ask for a local claude");
                        assert_eq!(*prompts_for_credential, Some(true));
                        assert_eq!(*credential_kind, Some(CredentialKind::Passphrase));
                        assert_eq!(credential_detail.as_deref(), Some("id_ed25519"));
                        assert_eq!(remote_cwd.as_deref(), Some("/home/tre/src"));
                        assert!(*remote_claude, "the far Claude is what the remote pane resumes");
                        assert_eq!(remote_session.as_deref(), Some("abc-123"));
                    }
                    _ => panic!("expected a leaf"),
                }
            }
            _ => panic!("expected a split"),
        }
    }

    #[test]
    fn old_file_without_new_fields_still_loads() {
        // A save from before window-geometry/claude-resume existed.
        let json = r#"{"active":0,"workspaces":[{"name":"W","next_term":2,
            "layout":{"Leaf":{"name":"T","shell":"PowerShell","cwd":null}}}]}"#;
        let s: SavedState = serde_json::from_str(json).unwrap();
        assert!(s.main_window.is_none());
        // Settings default when absent: Sonnet meter hidden, usage bar shown.
        assert!(s.settings.hide_sonnet_usage);
        assert!(!s.settings.hide_usage_bar);
        assert!(!s.settings.show_fable_usage);
        // The file explorer is opt-in, and a save from before it existed has none.
        assert!(!s.settings.show_file_explorer);
        // So is the Agents Office.
        assert!(!s.settings.show_agents_office);
        assert!(s.workspaces[0].explorer.is_none());
        assert!(s.workspaces[0].editor_tabs.is_empty());
        match &s.workspaces[0].layout {
            SavedNode::Leaf {
                claude_running,
                claude_session,
                history_id,
                startup_cmd,
                prompts_for_credential,
                credential_kind,
                remote_cwd,
                remote_claude,
                remote_session,
                ..
            } => {
                assert!(!claude_running);
                assert!(claude_session.is_none());
                assert!(history_id.is_none()); // absent in old saves → fresh id on restore
                assert!(startup_cmd.is_none());
                assert!(prompts_for_credential.is_none(), "unknown until observed");
                assert!(credential_kind.is_none());
                assert!(remote_cwd.is_none());
                assert!(!remote_claude);
                assert!(remote_session.is_none());
            }
            _ => panic!("expected a leaf"),
        }
    }

    // Saves written while project workspaces existed carry a `project` object that no
    // longer has a field to land in. Serde ignores unknown keys, so such a workspace must
    // still restore its terminal layout rather than failing the whole load (which `load()`
    // turns into a fresh start, i.e. every terminal lost).
    #[test]
    fn save_with_retired_project_data_still_loads_its_terminals() {
        let json = r#"{"active":0,"workspaces":[{"name":"W",
            "layout":{"Leaf":{"name":"T","shell":"GitBash","cwd":"/tmp",
                "claude_running":true,"claude_session":"sid-1","history_id":"h-1"}},
            "project":{"root":"/repo","active":0,"expanded":["/repo/src"],
                "worktrees":[{"branch":"main","path":"/repo","avatar_salt":3,
                    "layout":{"Leaf":{"name":"T","shell":"PowerShell","cwd":null}}}]}}]}"#;
        let s: SavedState = serde_json::from_str(json).unwrap();
        assert_eq!(s.workspaces.len(), 1);
        assert_eq!(s.workspaces[0].name, "W");
        match &s.workspaces[0].layout {
            SavedNode::Leaf { name, cwd, claude_session, history_id, .. } => {
                assert_eq!(name, "T");
                assert_eq!(cwd.as_deref(), Some("/tmp"));
                assert_eq!(claude_session.as_deref(), Some("sid-1"));
                assert_eq!(history_id.as_deref(), Some("h-1"));
            }
            _ => panic!("expected a leaf"),
        }
    }
}
