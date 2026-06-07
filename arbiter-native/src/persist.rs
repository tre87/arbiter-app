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

#[derive(Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub name: String,
    pub next_term: usize,
    pub layout: SavedNode,
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
            workspaces: vec![
                SavedWorkspace {
                    name: "Workspace 1".into(),
                    next_term: 3,
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
                },
                SavedWorkspace {
                    name: "Workspace 2".into(),
                    next_term: 2,
                    layout: SavedNode::Leaf {
                        name: "Terminal 1".into(),
                        shell: SavedShell::PowerShell,
                        cwd: None,
                        claude_running: false,
                        claude_session: None,
                    },
                },
            ],
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        println!("{json}");
        let back: SavedState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.active, 1);
        assert_eq!(back.workspaces.len(), 2);
        assert_eq!(back.workspaces[0].next_term, 3);
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
        match &s.workspaces[0].layout {
            SavedNode::Leaf { claude_running, claude_session, .. } => {
                assert!(!claude_running);
                assert!(claude_session.is_none());
            }
            _ => panic!("expected a leaf"),
        }
    }
}
