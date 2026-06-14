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

/// One worktree of a saved project workspace: its branch, path, and split tree.
#[derive(Serialize, Deserialize)]
pub struct SavedWorktree {
    pub branch: String,
    pub path: String,
    pub layout: SavedNode,
    /// Avatar reroll counter (see `Worktree::avatar_salt`); defaulted for back-compat.
    #[serde(default)]
    pub avatar_salt: u32,
}

/// A saved project workspace (git repo + its worktrees + explorer state).
#[derive(Serialize, Deserialize)]
pub struct SavedProject {
    pub root: String,
    pub active: usize,
    pub worktrees: Vec<SavedWorktree>,
    #[serde(default)]
    pub expanded: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub name: String,
    /// The active worktree's split tree (project) or the workspace's tree (terminal).
    pub layout: SavedNode,
    /// Present → this is a project workspace; restore its sidebars + worktrees.
    /// Defaulted so older save files (terminal-only) still load.
    #[serde(default)]
    pub project: Option<SavedProject>,
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
}

/// Default background colour. `#050a0f` — a near-black with a faint blue cast.
pub fn default_bg_hex() -> String {
    "#050a0f".to_string()
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

impl Default for Settings {
    fn default() -> Self {
        Self {
            hide_usage_bar: false,
            hide_sonnet_usage: true,
            overview_claude_only: false,
            overview_topmost: true,
            overview_usage_footer: true,
            hide_shell_button: false,
            scrollback: default_scrollback(),
            show_terminal_buttons: false,
            screenshot_folder: None,
            docs_folder: None,
            intense_text_style: IntenseStyle::Bold,
            background: default_bg_hex(),
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
                        }),
                        b: Box::new(SavedNode::Leaf {
                            name: "Terminal 2".into(),
                            shell: SavedShell::GitBash,
                            cwd: None,
                            claude_running: false,
                            claude_session: None,
                        }),
                    },
                    project: None,
                },
                SavedWorkspace {
                    name: "Workspace 2".into(),
                    layout: SavedNode::Leaf {
                        name: "Terminal 1".into(),
                        shell: SavedShell::PowerShell,
                        cwd: None,
                        claude_running: false,
                        claude_session: None,
                    },
                    project: None,
                },
            ],
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        println!("{json}");
        let back: SavedState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.active, 1);
        assert_eq!(back.workspaces.len(), 2);
        assert_eq!(back.main_window.unwrap().width, 1200.0);
        match &back.workspaces[0].layout {
            SavedNode::Split { vertical, ratio, a, .. } => {
                assert!(*vertical);
                assert!((*ratio - 0.4).abs() < 1e-6);
                match a.as_ref() {
                    SavedNode::Leaf { claude_running, claude_session, .. } => {
                        assert!(*claude_running);
                        assert_eq!(claude_session.as_deref(), Some("sess-abc-123"));
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
        match &s.workspaces[0].layout {
            SavedNode::Leaf { claude_running, claude_session, .. } => {
                assert!(!claude_running);
                assert!(claude_session.is_none());
            }
            _ => panic!("expected a leaf"),
        }
    }
}
