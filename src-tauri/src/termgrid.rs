// Production single-canvas terminal renderer — backend.
//
// Parses real PTY sessions with `alacritty_terminal` OFF the webview main
// thread (the existing PTY reader feeds a per-session HeadlessTerm), and ships
// compact binary cell-diffs over a Tauri `Channel` to one WebGL canvas in the
// webview. This is the productionised version of the validated spike (see
// memory: terminal-renderer-architecture) — it removes the two costs the
// per-terminal xterm path pays: N WebGL compositing layers and main-thread VT
// parsing.
//
// Non-breaking: a session's grid is None/inactive until the frontend calls
// `termgrid_attach`, so the existing xterm path is unaffected when the GPU
// renderer is off.
//
// ── Wire format (little-endian), one blob per frame ──────────────────────────
//   u8   version (=1)
//   u16  slotCount
//   repeat slotCount:
//     u16  slot                 (frontend-assigned, maps to a pane)
//     u16  cols
//     u16  rows
//     u16  cursorRow
//     u16  cursorCol
//     u8   cursorVisible (0/1)
//     u16  dirtyLineCount
//     repeat dirtyLineCount:
//       u16 row, u16 left, u16 right        (inclusive cols)
//       repeat (right-left+1) cells:
//         u32 codepoint
//         u8  fgR,fgG,fgB, bgR,bgG,bgB
//         u8  flags  (bit0 INVERSE,1 BOLD,2 ITALIC,3 UNDERLINE,4 HIDDEN,5 WIDE,6 WIDE_SPACER)

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::pty::Sessions;

#[derive(serde::Serialize)]
pub struct SearchMatch {
    pub line: i32,
    pub col: u32,
    pub len: u32,
}

// ── HeadlessTerm: alacritty Term + parser + base palette ─────────────────────

#[derive(Clone, Copy, Default)]
struct NoopListener;
impl EventListener for NoopListener {}

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct HeadlessTerm {
    term: Term<NoopListener>,
    parser: Processor,
    palette: [Rgb; 256],
    default_fg: Rgb,
    default_bg: Rgb,
    // Force a full frame on the next pack (after a scroll or resize, where
    // alacritty's per-line damage doesn't capture the wholesale view change).
    force_full: bool,
}

impl HeadlessTerm {
    pub fn new(cols: usize, rows: usize) -> Self {
        let size = Size { cols, rows };
        let mut config = Config::default();
        config.scrolling_history = 5000;
        let term = Term::new(config, &size, NoopListener);
        HeadlessTerm {
            term,
            parser: Processor::new(),
            palette: build_xterm_256_palette(),
            default_fg: Rgb { r: 0xcc, g: 0xcc, b: 0xcc },
            default_bg: Rgb { r: 0x14, g: 0x14, b: 0x16 },
            force_full: false,
        }
    }

    /// Feed raw PTY bytes. alacritty's vte parser handles UTF-8 internally.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.term.resize(Size { cols, rows });
        self.force_full = true;
    }

    /// Scroll the viewport into scrollback history by `delta` lines (positive =
    /// older/up). Forces a full frame so the whole scrolled view is re-sent.
    pub fn scroll(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
        self.force_full = true;
    }

    /// Case-insensitive search over the whole grid (scrollback included).
    /// Returns matches as (content-line, start-col, length), capped.
    pub fn search(&self, query: &str) -> Vec<SearchMatch> {
        let q: Vec<char> = query.to_lowercase().chars().collect();
        let ql = q.len();
        let mut out = Vec::new();
        if ql == 0 {
            return out;
        }
        let grid = self.term.grid();
        let cols = self.term.columns();
        let top = -(grid.history_size() as i32);
        let bottom = self.term.screen_lines() as i32 - 1;
        let mut line = top;
        while line <= bottom {
            let row = &grid[Line(line)];
            let mut chars: Vec<char> = Vec::with_capacity(cols);
            for c in 0..cols {
                let ch = row[Column(c)].c;
                let lc = if ch == '\0' { ' ' } else { ch.to_lowercase().next().unwrap_or(ch) };
                chars.push(lc);
            }
            if chars.len() >= ql {
                let mut start = 0;
                while start + ql <= chars.len() {
                    if chars[start..start + ql] == q[..] {
                        out.push(SearchMatch { line, col: start as u32, len: ql as u32 });
                        if out.len() >= 5000 {
                            return out;
                        }
                        start += ql;
                    } else {
                        start += 1;
                    }
                }
            }
            line += 1;
        }
        out
    }

    /// Extract selected text. Lines are content-line coords (0 = top of the
    /// active screen, negative = scrollback history), so a selection can span
    /// content scrolled out of view. Trailing spaces per line are trimmed.
    pub fn selection_text(&self, sl: i32, sc: usize, el: i32, ec: usize) -> String {
        let grid = self.term.grid();
        let cols = self.term.columns();
        if cols == 0 {
            return String::new();
        }
        let top = -(grid.history_size() as i32);
        let bottom = self.term.screen_lines() as i32 - 1;
        // Order endpoints lexicographically (line, then column).
        let (first, last) = if (sl, sc) <= (el, ec) { ((sl, sc), (el, ec)) } else { ((el, ec), (sl, sc)) };
        let (fl, fc) = first;
        let (ll, lc) = last;
        let mut line = fl.clamp(top, bottom);
        let last_line = ll.clamp(top, bottom);
        let mut out = String::new();
        while line <= last_line {
            let s = if line == fl { fc.min(cols - 1) } else { 0 };
            let e = if line == ll { lc.min(cols - 1) } else { cols - 1 };
            let row = &grid[Line(line)];
            let mut text = String::new();
            let mut col = s;
            while col <= e {
                let c = row[Column(col)].c;
                text.push(if c == '\0' { ' ' } else { c });
                col += 1;
            }
            out.push_str(text.trim_end());
            if line < last_line {
                out.push('\n');
            }
            line += 1;
        }
        out
    }

    /// Apply the frontend's xterm theme so resolved colors match the old
    /// per-terminal renderer exactly: default fg/bg + the 16 ANSI colors. The
    /// 6×6×6 cube and grayscale ramp (indices 16–255) keep their standard values.
    pub fn set_theme(&mut self, fg: &[u8], bg: &[u8], ansi: &[u8]) {
        if fg.len() >= 3 {
            self.default_fg = Rgb { r: fg[0], g: fg[1], b: fg[2] };
        }
        if bg.len() >= 3 {
            self.default_bg = Rgb { r: bg[0], g: bg[1], b: bg[2] };
        }
        for i in 0..16 {
            if ansi.len() >= i * 3 + 3 {
                self.palette[i] = Rgb { r: ansi[i * 3], g: ansi[i * 3 + 1], b: ansi[i * 3 + 2] };
            }
        }
    }

    fn resolve(&self, color: Color, is_fg: bool) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Indexed(i) => self.palette[i as usize],
            Color::Named(named) => match named {
                NamedColor::Foreground => self.default_fg,
                NamedColor::Background => self.default_bg,
                other => {
                    let idx = other as usize;
                    if idx < 256 {
                        self.palette[idx]
                    } else if is_fg {
                        self.default_fg
                    } else {
                        self.default_bg
                    }
                }
            },
        }
    }

    /// Pack this terminal's per-frame payload into `out`, prefixed by `slot`.
    /// Consumes and resets damage. Returns false if nothing changed.
    pub fn pack_into(&mut self, slot: u16, out: &mut Vec<u8>, force_full: bool) -> bool {
        let cols = self.term.columns();
        let rows = self.term.screen_lines();
        // How far the viewport is scrolled into history (0 = at the bottom).
        let offset = self.term.grid().display_offset() as i32;
        // A scrolled view (or a pending scroll/resize) changes every visible
        // line, so send a full frame; otherwise use per-line damage.
        let full = force_full || self.force_full || offset != 0;
        self.force_full = false;

        let mut dirty: Vec<(usize, usize, usize)> = Vec::new();
        if full {
            for line in 0..rows {
                dirty.push((line, 0, cols.saturating_sub(1)));
            }
            self.term.reset_damage();
        } else {
            match self.term.damage() {
                TermDamage::Full => {
                    for line in 0..rows {
                        dirty.push((line, 0, cols.saturating_sub(1)));
                    }
                }
                TermDamage::Partial(iter) => {
                    for b in iter {
                        if b.line < rows {
                            dirty.push((b.line, b.left, b.right.min(cols.saturating_sub(1))));
                        }
                    }
                }
            }
            self.term.reset_damage();
            if dirty.is_empty() {
                return false;
            }
        }

        let cursor = self.term.grid().cursor.point;
        let cur_row = cursor.line.0.max(0) as u16;
        let cur_col = cursor.column.0 as u16;
        // Hide the cursor while scrolled back — it lives in the active screen,
        // below the viewport.
        let cur_vis = (offset == 0 && self.term.mode().contains(TermMode::SHOW_CURSOR)) as u8;

        out.extend_from_slice(&slot.to_le_bytes());
        out.extend_from_slice(&(cols as u16).to_le_bytes());
        out.extend_from_slice(&(rows as u16).to_le_bytes());
        out.extend_from_slice(&cur_row.to_le_bytes());
        out.extend_from_slice(&cur_col.to_le_bytes());
        out.push(cur_vis);
        // Scroll offset (lines into history) so the frontend can map visible
        // rows to content lines for content-anchored selection.
        out.extend_from_slice(&(offset.max(0) as u16).to_le_bytes());
        out.extend_from_slice(&(dirty.len() as u16).to_le_bytes());

        let grid = self.term.grid();
        for (line, left, right) in dirty {
            out.extend_from_slice(&(line as u16).to_le_bytes());
            out.extend_from_slice(&(left as u16).to_le_bytes());
            out.extend_from_slice(&(right as u16).to_le_bytes());
            // Apply the scroll offset: visible row `line` maps to grid line
            // `line - offset` (negative indices read scrollback history).
            let row = &grid[Line(line as i32 - offset)];
            for col in left..=right {
                let cell = &row[Column(col)];
                let fg = self.resolve(cell.fg, true);
                let bg = self.resolve(cell.bg, false);
                let mut flags = 0u8;
                let f = cell.flags;
                if f.contains(Flags::INVERSE) { flags |= 1 << 0; }
                if f.contains(Flags::BOLD) { flags |= 1 << 1; }
                if f.contains(Flags::ITALIC) { flags |= 1 << 2; }
                if f.contains(Flags::UNDERLINE) { flags |= 1 << 3; }
                if f.contains(Flags::HIDDEN) { flags |= 1 << 4; }
                if f.contains(Flags::WIDE_CHAR) { flags |= 1 << 5; }
                if f.contains(Flags::WIDE_CHAR_SPACER) { flags |= 1 << 6; }
                out.extend_from_slice(&(cell.c as u32).to_le_bytes());
                out.extend_from_slice(&[fg.r, fg.g, fg.b, bg.r, bg.g, bg.b, flags]);
            }
        }
        true
    }
}

// ── State + commands ─────────────────────────────────────────────────────────

struct Attached {
    sid: String,
    slot: u16,
    grid: Arc<Mutex<Option<HeadlessTerm>>>,
    active: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
}

pub struct TermGridState {
    attached: Arc<Mutex<Vec<Attached>>>,
    // Generation counter: bumping it retires the previous frame thread, so
    // `termgrid_start` can re-bind a fresh Channel without leaking threads.
    gen: Arc<AtomicU64>,
}

impl TermGridState {
    pub fn new() -> Self {
        TermGridState {
            attached: Arc::new(Mutex::new(Vec::new())),
            gen: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Wakes the frame thread when a grid goes dirty so it can BLOCK while idle
/// instead of polling every 8ms (which spun ~125 idle wake-ups/sec on the main
/// process). The PTY reader / scroll / resize / attach paths call
/// `notify_frame_dirty()` after marking a grid dirty.
static FRAME_WAKE: OnceLock<Arc<(Mutex<bool>, Condvar)>> = OnceLock::new();
fn frame_wake() -> &'static Arc<(Mutex<bool>, Condvar)> {
    FRAME_WAKE.get_or_init(|| Arc::new((Mutex::new(false), Condvar::new())))
}
pub fn notify_frame_dirty() {
    let (lock, cvar) = &**frame_wake();
    *lock.lock().unwrap() = true;
    cvar.notify_one();
}

/// Begin streaming grid diffs to the frontend over `channel`. Spawns one frame
/// thread (~120 Hz) that packs damage for all attached sessions into one blob.
/// Re-callable: a new call retires the old thread and binds the new channel.
#[tauri::command]
pub fn termgrid_start(state: State<TermGridState>, channel: Channel<InvokeResponseBody>) {
    let my_gen = state.gen.fetch_add(1, Ordering::SeqCst) + 1;
    let attached = state.attached.clone();
    let gen = state.gen.clone();
    // Wake any prior frame thread so it re-checks `gen` and exits promptly.
    notify_frame_dirty();
    std::thread::spawn(move || loop {
        if gen.load(Ordering::SeqCst) != my_gen {
            break;
        }
        // Block until a grid goes dirty (reader calls notify_frame_dirty), with a
        // 1s safety timeout in case a wake is ever missed. Idle = ~1 wake/sec
        // instead of 125. Then an 8ms batch window coalesces a burst of output
        // into a single frame (~120fps cap) before packing.
        {
            let (lock, cvar) = &**frame_wake();
            let mut dirty = lock.lock().unwrap();
            if !*dirty {
                let (g, _) = cvar.wait_timeout(dirty, Duration::from_secs(1)).unwrap();
                dirty = g;
            }
            *dirty = false;
        }
        if gen.load(Ordering::SeqCst) != my_gen {
            break;
        }
        std::thread::sleep(Duration::from_millis(8));
        let mut body: Vec<u8> = Vec::with_capacity(4096);
        let mut count: u16 = 0;
        {
            let list = attached.lock().unwrap();
            for a in list.iter() {
                if !a.active.load(Ordering::Relaxed) {
                    continue;
                }
                if !a.dirty.swap(false, Ordering::Relaxed) {
                    continue;
                }
                if let Ok(mut g) = a.grid.lock() {
                    if let Some(t) = g.as_mut() {
                        if t.pack_into(a.slot, &mut body, false) {
                            count += 1;
                        }
                    }
                }
            }
        }
        if count == 0 {
            continue;
        }
        let mut frame: Vec<u8> = Vec::with_capacity(body.len() + 3);
        frame.push(1u8);
        frame.extend_from_slice(&count.to_le_bytes());
        frame.extend_from_slice(&body);
        if channel.send(InvokeResponseBody::Raw(frame)).is_err() {
            break;
        }
    });
}

/// Start GPU rendering for a session at `slot`. Creates the session's grid,
/// replays its buffered history so the current screen is reconstructed, and
/// registers it with the frame thread.
#[tauri::command]
pub fn termgrid_attach(
    sessions: State<Sessions>,
    state: State<TermGridState>,
    session_id: String,
    slot: u16,
    cols: u16,
    rows: u16,
    fg: Vec<u8>,
    bg: Vec<u8>,
    ansi: Vec<u8>,
) {
    let arcs = {
        let map = sessions.0.lock().unwrap();
        match map.get(&session_id) {
            Some(s) => s.attach_grid(cols, rows, &fg, &bg, &ansi),
            None => return,
        }
    };
    let (grid, active, dirty) = arcs;
    let mut list = state.attached.lock().unwrap();
    list.retain(|a| a.sid != session_id && a.slot != slot);
    list.push(Attached { sid: session_id, slot, grid, active, dirty });
    drop(list);
    // Pane is registered + dirty (replayed history) — wake the frame thread to
    // send the initial frame now rather than after the 1s safety timeout.
    notify_frame_dirty();
}

/// Scroll a session's viewport into scrollback by `delta` lines (positive = up).
#[tauri::command]
pub fn termgrid_scroll(sessions: State<Sessions>, session_id: String, delta: i32) {
    if let Some(s) = sessions.0.lock().unwrap().get(&session_id) {
        s.scroll_grid(delta);
        notify_frame_dirty();
    }
}

/// Extract selected text (content-line coords) — used for copy, so a selection
/// can span scrolled-out-of-view history.
#[tauri::command]
pub fn termgrid_selection_text(
    sessions: State<Sessions>,
    session_id: String,
    s_line: i32,
    s_col: usize,
    e_line: i32,
    e_col: usize,
) -> String {
    if let Some(s) = sessions.0.lock().unwrap().get(&session_id) {
        return s.selection_text(s_line, s_col, e_line, e_col);
    }
    String::new()
}

/// Search a session's grid (scrollback included); returns matches in
/// content-line coords for highlighting and scroll-to.
#[tauri::command]
pub fn termgrid_search(sessions: State<Sessions>, session_id: String, query: String) -> Vec<SearchMatch> {
    if let Some(s) = sessions.0.lock().unwrap().get(&session_id) {
        return s.search_grid(&query);
    }
    Vec::new()
}

/// Stop GPU rendering for a session (frees its grid).
#[tauri::command]
pub fn termgrid_detach(sessions: State<Sessions>, state: State<TermGridState>, session_id: String) {
    if let Some(s) = sessions.0.lock().unwrap().get(&session_id) {
        s.detach_grid();
    }
    state.attached.lock().unwrap().retain(|a| a.sid != session_id);
}

// ── xterm 256-color palette ──────────────────────────────────────────────────

fn build_xterm_256_palette() -> [Rgb; 256] {
    let mut p = [Rgb { r: 0, g: 0, b: 0 }; 256];
    const ANSI: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00), (0xcd, 0x31, 0x31), (0x0d, 0xbc, 0x79), (0xe5, 0xe5, 0x10),
        (0x24, 0x72, 0xc8), (0xbc, 0x3f, 0xbc), (0x11, 0xa8, 0xcd), (0xe5, 0xe5, 0xe5),
        (0x66, 0x66, 0x66), (0xf1, 0x4c, 0x4c), (0x23, 0xd1, 0x8b), (0xf5, 0xf5, 0x43),
        (0x3b, 0x8e, 0xea), (0xd6, 0x70, 0xd6), (0x29, 0xb8, 0xdb), (0xff, 0xff, 0xff),
    ];
    for (i, &(r, g, b)) in ANSI.iter().enumerate() {
        p[i] = Rgb { r, g, b };
    }
    let steps = [0u8, 95, 135, 175, 215, 255];
    let mut idx = 16usize;
    for r in 0..6 {
        for g in 0..6 {
            for b in 0..6 {
                p[idx] = Rgb { r: steps[r], g: steps[g], b: steps[b] };
                idx += 1;
            }
        }
    }
    for i in 0..24u8 {
        let v = 8 + i * 10;
        p[232 + i as usize] = Rgb { r: v, g: v, b: v };
    }
    p
}
