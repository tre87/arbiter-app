//! Panes of the workspaces not on screen get their real size at launch.
//!
//! A terminal learns its size when it is first drawn, and only the workspace on
//! screen is drawn, so every other pane sat at the 80x24 it was created with until
//! its workspace was first opened: an agent resumed there started at that size and
//! repainted on the first visit, and a restored ssh command, which waits for the
//! real size (`StartupGate`), went out on its five second fallback.
//!
//! Nothing is worked out before the window exists: the display scale and the size
//! the window really got are not known then. The first terminal drawn reports what
//! it measured (`note_first_frame`), and from that one drawing every other pane's
//! size follows: its rectangle from the same `pane_regions` the grid lays out with,
//! less the pane's chrome (the reporting pane's rectangle less its terminal), in
//! cells of the size the renderer built. A size that turns out wrong is put right
//! when the pane is first drawn, as every size was before.

use std::sync::OnceLock;

use super::*;

/// The grid's `.spacing(2)`.
const SPACING: f32 = 2.0;

/// A pane's chrome beside and above its terminal is a 2 px gutter and a header.
/// More than this means the window was not the size `grid_size` took it for.
const CHROME_MAX: iced::Size = iced::Size { width: 8.0, height: 80.0 };

/// What the first terminal drawn was given, and the cells the renderer built.
pub struct FirstFrame {
    /// `Session::id` of the pane.
    pub id: u64,
    /// The terminal widget's size, logical px.
    pub size: iced::Size,
    pub scale: f32,
    /// A cell, device px.
    pub cell: (u32, u32),
}

static FIRST_FRAME: OnceLock<FirstFrame> = OnceLock::new();

pub fn first_frame_known() -> bool {
    FIRST_FRAME.get().is_some()
}

/// From the renderer, once. Wakes the app so `run` follows the frame.
pub fn note_first_frame(frame: FirstFrame) {
    if FIRST_FRAME.set(frame).is_ok() {
        arbiter_native::session::wake_ui();
    }
}

/// The columns and rows of a terminal widget `size` logical px large. The one place
/// this is worked out, for the renderer and for the panes sized before they are drawn.
pub fn cells(size: iced::Size, scale: f32, cell: (u32, u32)) -> (usize, usize) {
    let pw = (size.width * scale).max(1.0) as u32;
    let ph = (size.height * scale).max(1.0) as u32;
    ((pw / cell.0.max(1)).max(1) as usize, (ph / cell.1.max(1)).max(1) as usize)
}

/// The pane grid's size in workspace `ws`: the window less the titlebar (40), the
/// 6 px frame on the other three sides, and that workspace's file explorer.
pub fn grid_size(state: &State, ws: &Workspace) -> iced::Size {
    let explorer = ws.explorer.as_ref().filter(|e| e.shown && state.settings.show_file_explorer);
    let w = state.main_size.width - 12.0 - explorer.map_or(0.0, |e| e.width + files_pane::HANDLE_W);
    let h = state.main_size.height - 46.0;
    iced::Size::new(w.max(1.0), h.max(1.0))
}

// The editor takes the grid's place while it shows, and the terminals a share of it.
fn editor_shows(state: &State, ws: &Workspace) -> bool {
    state.settings.show_file_explorer
        && ws.explorer.as_ref().is_some_and(|e| e.shown)
        && ws.editor.visible
        && !ws.editor.tabs.is_empty()
}

/// Size every pane that has not been drawn. Runs after each message and does its
/// work once, on the first one after the first terminal frame.
pub fn run(state: &mut State) {
    if state.presized {
        return;
    }
    let Some(frame) = FIRST_FRAME.get() else { return };
    state.presized = true;
    let drawn = state.workspaces.iter().enumerate().find_map(|(wi, ws)| {
        let (pane, _) = ws.panes.iter().find(|(_, d)| d.session.id() == frame.id)?;
        Some((wi, *pane))
    });
    let Some((drawn_ws, drawn_pane)) = drawn else { return };
    let regions_of = |ws: &Workspace| ws.panes.layout().pane_regions(SPACING, grid_size(state, ws));
    let Some(region) = regions_of(&state.workspaces[drawn_ws]).get(&drawn_pane).copied() else { return };
    let chrome = iced::Size::new(region.width - frame.size.width, region.height - frame.size.height);
    let debug = arbiter_native::debug::debug_enabled();
    if !(0.0..=CHROME_MAX.width).contains(&chrome.width) || !(0.0..=CHROME_MAX.height).contains(&chrome.height) {
        if debug {
            arbiter_native::debug::debug_log(&format!(
                "presize: skipped, pane {} of {region:?} drew {:?}",
                frame.id, frame.size
            ));
        }
        return;
    }
    let (mut sized, mut off) = (0, 0);
    for (wi, ws) in state.workspaces.iter().enumerate() {
        if editor_shows(state, ws) {
            continue;
        }
        let regions = regions_of(ws);
        for (pane, d) in ws.panes.iter() {
            let Some(r) = regions.get(pane) else { continue };
            let terminal = iced::Size::new(r.width - chrome.width, r.height - chrome.height);
            let want = cells(terminal, frame.scale, frame.cell);
            let have = d.session.term().lock().map(|t| t.size()).ok();
            if have == Some(want) {
                continue;
            }
            // The workspace on screen is sized by its own drawing. What it got is the
            // check on this arithmetic: a pane there that differs says it is wrong.
            if wi == drawn_ws {
                off += 1;
                if debug {
                    arbiter_native::debug::debug_log(&format!(
                        "presize: pane {} is {have:?} on screen, worked out as {want:?}",
                        d.session.id()
                    ));
                }
                continue;
            }
            d.session.presize(want.0 as u16, want.1 as u16);
            sized += 1;
        }
    }
    if debug {
        arbiter_native::debug::debug_log(&format!(
            "presize: sized {sized} panes from pane {} (chrome {chrome:?}, scale {}, cell {:?}), {off} on screen differ",
            frame.id, frame.scale, frame.cell
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::cells;

    #[test]
    fn cells_are_whole_ones_that_fit_and_never_none() {
        let size = |w, h| iced::Size::new(w, h);
        assert_eq!(cells(size(800.0, 600.0), 1.0, (8, 16)), (100, 37));
        assert_eq!(cells(size(800.0, 600.0), 2.0, (15, 34)), (106, 35));
        assert_eq!(cells(size(799.9, 15.9), 1.0, (8, 16)), (99, 1), "a part cell is not a cell, and one row stays");
        assert_eq!(cells(size(0.0, 0.0), 1.5, (8, 16)), (1, 1));
    }
}
