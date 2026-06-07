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
    },
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
                        }),
                        b: Box::new(SavedNode::Leaf {
                            name: "Terminal 2".into(),
                            shell: SavedShell::GitBash,
                            cwd: None,
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
        match &back.workspaces[0].layout {
            SavedNode::Split { vertical, ratio, .. } => {
                assert!(*vertical);
                assert!((*ratio - 0.4).abs() < 1e-6);
            }
            _ => panic!("expected a split"),
        }
    }
}
