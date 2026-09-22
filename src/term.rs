//! Headless VT terminal: `alacritty_terminal` Term + parser + colour palette.
//! Same approach as the shipping app's `termgrid::HeadlessTerm`.

use std::collections::HashSet;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};

/// App-wide "intense text" (SGR 1 / bold) rendering mode, mirroring Windows Terminal's
/// `intenseTextStyle` (see [`crate::persist::IntenseStyle`]): 0 = None, 1 = Bold,
/// 2 = Bright, 3 = All. Read per-cell in `for_each_cell`; set from Settings on load and
/// when changed. Defaults to Bold (a bold font face).
static INTENSE_STYLE: AtomicU8 = AtomicU8::new(1);

/// Set the intense-text rendering mode from Settings (`IntenseStyle::as_u8`).
pub fn set_intense_style(v: u8) {
    INTENSE_STYLE.store(v, Ordering::Relaxed);
}

/// App-wide terminal background colour, packed `0x00RRGGBB`. Drives the terminal cells
/// here AND the iced surfaces (read via [`bg`]) so they stay in lock-step. Set from
/// Settings on load + change. Default `#0a0a0c`.
static TERM_BG: AtomicU32 = AtomicU32::new(0x000a_0a0c);

/// Set the terminal/app background colour (packed `0x00RRGGBB`) from Settings.
pub fn set_bg(rgb: u32) {
    TERM_BG.store(rgb & 0x00ff_ffff, Ordering::Relaxed);
}

/// The current background colour as RGB bytes (terminal cells + iced chrome read this).
pub fn bg() -> (u8, u8, u8) {
    let v = TERM_BG.load(Ordering::Relaxed);
    ((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn term_bg() -> Rgb {
    let (r, g, b) = bg();
    Rgb { r, g, b }
}

/// App-wide terminal font size in points (CSS-style em; the GPU renderer multiplies
/// this by the display DPR and rasterises at that resolution — see `crate::gpu`).
/// Set from Settings on load + change; the renderer reads it when it (re)builds, and
/// the host rebuilds every pane's renderer when this changes (like a DPI change), so
/// cell size + the PTY grid stay in lock-step. Default 12pt.
static FONT_PX: AtomicU32 = AtomicU32::new(12);

/// Set the terminal font size (points) from Settings.
pub fn set_font_px(n: u32) {
    FONT_PX.store(n, Ordering::Relaxed);
}

/// The current terminal font size in points.
pub fn font_px() -> u32 {
    FONT_PX.load(Ordering::Relaxed)
}

/// Active in-terminal find: the matched cell ranges (incl. scrollback), which is
/// the current one, and the precomputed cell sets for O(1) render highlighting.
struct Search {
    matches: Vec<RangeInclusive<Point>>,
    current: usize,
    cells: HashSet<(i32, usize)>,
    cur_cells: HashSet<(i32, usize)>,
}

/// Selection granularity for a fresh selection (single/double/triple click).
pub enum SelectKind {
    Simple,
    Word,
    Line,
}

/// Snapshot of the terminal's mouse-reporting + scroll modes (a TUI toggles these
/// via DECSET/DECRST). The renderer reads them to decide whether a click is sent
/// to the app or handled locally (selection / scrollback).
#[derive(Clone, Copy, Default)]
pub struct MouseModes {
    /// Any of ?1000/?1002/?1003 — the app wants click events.
    pub reporting: bool,
    /// ?1003 — report motion even with no button held.
    pub report_motion: bool,
    /// ?1002 — report motion while a button is held (drag).
    pub report_drag: bool,
    /// ?1006 — SGR (`CSI < … M/m`) encoding; else the legacy `CSI M` byte form.
    pub sgr: bool,
    /// ?1005 — UTF-8 extended coordinates (legacy encoding only).
    pub utf8: bool,
    /// ?1007 — wheel sends arrow keys on the alternate screen.
    pub alternate_scroll: bool,
    /// Alternate screen active (vim/less/htop) — gates `alternate_scroll`.
    pub alt_screen: bool,
    /// DECCKM — cursor keys send SS3 (`ESC O`) not CSI; used by alternate scroll.
    pub app_cursor: bool,
}

#[derive(Clone, Copy, Default)]
pub struct NoopListener;
impl EventListener for NoopListener {}

/// Captures the terminal's PTY replies into a shared buffer. alacritty emits a
/// program's expected responses (cursor-position report for `ESC[6n`, device
/// attributes for `ESC[c`, mode/status reports, …) as `Event::PtyWrite`; the
/// reader loop drains this after each `feed` and writes it back to the PTY. With
/// the old `NoopListener` these replies were dropped, so query-driven programs
/// (vim, many .NET / Spectre.Console UIs) would hang or misread their own input.
#[derive(Clone, Default)]
pub struct Responder {
    buf: Arc<Mutex<Vec<u8>>>,
}
impl EventListener for Responder {
    fn send_event(&self, event: alacritty_terminal::event::Event) {
        if let alacritty_terminal::event::Event::PtyWrite(text) = event {
            self.buf.lock().unwrap().extend_from_slice(text.as_bytes());
        }
    }
}

/// Scrollback lines kept per terminal (Settings → "Terminal scrollback lines").
/// A global so new terminals pick up the setting without threading it through
/// every `Session::spawn`/`VtTerm::new` call site; existing grids keep their size.
pub static SCROLLBACK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(5000);

/// Scrollback kept while Claude owns a pane (see the reader loop in `session.rs`): its
/// wheel goes to Claude, so Arbiter's history is unreachable there and only holds redraw
/// churn. Enough to keep the shell's recent output around Claude's launch.
pub const CLAUDE_SCROLLBACK: usize = 1000;

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize { self.rows }
    fn screen_lines(&self) -> usize { self.rows }
    fn columns(&self) -> usize { self.cols }
}

/// How long the cursor stays drawn after a program hides it. Claude's UI hides the cursor
/// while it redraws and shows it again after; over ssh, at the pace of its working
/// animation, the hide and the show land in separate frames, and drawing exactly what the
/// grid said made the cursor flicker at that pace. A program that means to hide its
/// cursor still loses it after this; the reader schedules the frame that does.
pub const CURSOR_HIDE_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

pub struct VtTerm {
    term: Term<Responder>,
    parser: Processor,
    palette: [Rgb; 256],
    default_fg: Rgb,
    search: Option<Search>,
    /// Where the cursor was when the program last had it shown, and when the program hid
    /// it, for `CURSOR_HIDE_GRACE`. `hidden_since` is `None` while it is shown.
    cursor_shown_at: (usize, usize),
    hidden_since: Option<std::time::Instant>,
    /// When the view was last scrolled BY THE USER (wheel / drag-autoscroll), to
    /// fade the scroll indicator out. Not set by output or jump-to-bottom, so the
    /// indicator never flashes while text merely streams in.
    last_scroll: Option<std::time::Instant>,
    /// PTY replies the term produced (query responses), drained by the reader loop
    /// and written back to the PTY. Shared with the `Responder` event sink.
    responses: Arc<Mutex<Vec<u8>>>,
    /// Bumped by every change that can alter what a frame shows: output, resize, scroll,
    /// selection, search, history depth. The renderer skips rebuilding a frame whose
    /// generation it already drew (see `gpu::TermGpu::prepare`), so a mutating method
    /// that forgets to bump it leaves a stale frame until the next change.
    generation: u64,
}

impl VtTerm {
    pub fn new(cols: usize, rows: usize) -> Self {
        let mut config = Config::default();
        config.scrolling_history = SCROLLBACK.load(std::sync::atomic::Ordering::Relaxed);
        let responses: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let term = Term::new(config, &Size { cols, rows }, Responder { buf: responses.clone() });
        Self {
            term,
            parser: Processor::new(),
            palette: build_palette(),
            default_fg: default_fg(),
            // Background is the app-wide configurable colour (TERM_BG / Settings), read
            // live via term_bg() so changing it updates every terminal. The Iced shell's
            // surfaces read the same value so skipped (empty, default-bg) cells blend.
            search: None,
            cursor_shown_at: (0, 0),
            hidden_since: None,
            last_scroll: None,
            responses,
            generation: 0,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Change how many lines of history the primary screen keeps. Shrinking drops the
    /// oldest lines and frees their rows; growing only raises the cap.
    pub fn set_history(&mut self, lines: usize) {
        let mut config = Config::default();
        config.scrolling_history = lines;
        self.term.set_options(config);
        self.generation += 1;
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.generation += 1;
        self.parser.advance(&mut self.term, bytes);
        if self.term.mode().contains(TermMode::SHOW_CURSOR) {
            let p = self.term.grid().cursor.point;
            self.cursor_shown_at = (p.line.0.max(0) as usize, p.column.0);
            self.hidden_since = None;
        } else if self.hidden_since.is_none() {
            self.hidden_since = Some(std::time::Instant::now());
        }
    }

    /// Drain the terminal's pending PTY replies (responses to queries the running
    /// program sent, e.g. cursor-position or device-attribute requests) so the
    /// caller can write them back to the PTY. Empty when nothing was asked.
    pub fn take_responses(&self) -> Vec<u8> {
        std::mem::take(&mut *self.responses.lock().unwrap())
    }

    /// Set the find query: collect all matches over the grid (incl. scrollback)
    /// via the crate's `RegexSearch`, then scroll the first match into view. An
    /// empty/invalid query clears the search. Case-insensitive unless the query has
    /// an uppercase letter (alacritty's smart-case).
    pub fn set_search(&mut self, query: &str) {
        self.generation += 1;
        if query.is_empty() {
            self.search = None;
            return;
        }
        let Ok(mut dfas) = RegexSearch::new(query) else {
            self.search = None;
            return;
        };
        let cols = self.term.columns();
        let start = Point::new(self.term.topmost_line(), Column(0));
        let end = Point::new(self.term.bottommost_line(), Column(cols.saturating_sub(1)));
        // Cap to keep a pathological query ("a") from collecting huge match lists.
        let matches: Vec<RangeInclusive<Point>> =
            RegexIter::new(start, end, Direction::Right, &self.term, &mut dfas).take(2000).collect();
        let mut cells = HashSet::new();
        for m in &matches {
            expand_match(m, cols, &mut cells);
        }
        self.search = Some(Search { matches, current: 0, cells, cur_cells: HashSet::new() });
        self.update_cur_cells();
        self.scroll_to_current();
    }

    /// Move to the next (or previous) match, wrapping, and scroll it into view.
    pub fn search_jump(&mut self, forward: bool) {
        self.generation += 1;
        let len = self.search.as_ref().map_or(0, |s| s.matches.len());
        if len == 0 {
            return;
        }
        if let Some(s) = self.search.as_mut() {
            s.current = if forward { (s.current + 1) % len } else { (s.current + len - 1) % len };
        }
        self.update_cur_cells();
        self.scroll_to_current();
    }

    pub fn clear_search(&mut self) {
        self.generation += 1;
        self.search = None;
    }

    /// `(current 1-based, total)` for the find bar; `None` when no search is active,
    /// `(0, 0)` when the query matched nothing.
    pub fn search_status(&self) -> Option<(usize, usize)> {
        self.search.as_ref().map(|s| {
            let n = s.matches.len();
            (if n == 0 { 0 } else { s.current + 1 }, n)
        })
    }

    /// Recompute the highlighted cells for the current match.
    fn update_cur_cells(&mut self) {
        let cols = self.term.columns();
        if let Some(s) = self.search.as_mut() {
            s.cur_cells.clear();
            if let Some(m) = s.matches.get(s.current) {
                expand_match(m, cols, &mut s.cur_cells);
            }
        }
    }

    /// Scroll so the current match is on screen (≈1/3 from the top), but leave the
    /// view alone if it's already visible.
    fn scroll_to_current(&mut self) {
        let Some(l) =
            self.search.as_ref().and_then(|s| s.matches.get(s.current)).map(|m| m.start().line.0)
        else {
            return;
        };
        let rows = self.term.screen_lines() as i32;
        let off = self.term.grid().display_offset() as i32;
        // Visible absolute lines are [-off, rows-1-off]; only scroll if off-screen.
        if l < -off || l > rows - 1 - off {
            let target_off = (rows / 3 - l).max(0);
            self.term.scroll_display(Scroll::Delta(target_off - off));
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.generation += 1;
        self.term.resize(Size { cols, rows });
    }

    /// Scroll the display by `lines` into scrollback (positive = up/older),
    /// clamped to the history. The next `for_each_cell` renders the new view.
    /// Records the scroll time so the scroll indicator shows then fades.
    pub fn scroll(&mut self, lines: i32) {
        self.generation += 1;
        self.term.scroll_display(Scroll::Delta(lines));
        self.last_scroll = Some(std::time::Instant::now());
    }

    /// Jump back to the live bottom (display offset 0). Does NOT mark a user
    /// scroll, so typing/jump-to-bottom never flashes the scroll indicator.
    pub fn scroll_to_bottom(&mut self) {
        self.generation += 1;
        self.term.scroll_display(Scroll::Bottom);
    }

    /// `(display_offset, history_size, screen_lines)` for sizing/placing the
    /// scroll indicator thumb.
    pub fn scroll_state(&self) -> (usize, usize, usize) {
        let g = self.term.grid();
        (g.display_offset(), g.history_size(), self.term.screen_lines())
    }

    /// Milliseconds since the last user scroll, or `None` if there hasn't been
    /// one — drives the scroll indicator's fade-out.
    pub fn scroll_age_ms(&self) -> Option<u64> {
        self.last_scroll.map(|t| t.elapsed().as_millis() as u64)
    }

    /// Map a visible row to an absolute grid line (accounting for scrollback).
    fn abs_line(&self, row: usize) -> Line {
        Line(row as i32 - self.term.grid().display_offset() as i32)
    }

    /// Begin a selection at a visible (row, col); `right` = cursor in the cell's
    /// right half (which edge the selection snaps to). `kind` sets the
    /// granularity (single/double/triple click → char/word/line).
    pub fn start_selection(&mut self, row: usize, col: usize, right: bool, kind: SelectKind) {
        self.generation += 1;
        let point = Point::new(self.abs_line(row), Column(col));
        let side = if right { Side::Right } else { Side::Left };
        let ty = match kind {
            SelectKind::Simple => SelectionType::Simple,
            SelectKind::Word => SelectionType::Semantic,
            SelectKind::Line => SelectionType::Lines,
        };
        self.term.selection = Some(Selection::new(ty, point, side));
    }

    /// Extend the active selection to a visible (row, col).
    pub fn update_selection(&mut self, row: usize, col: usize, right: bool) {
        self.generation += 1;
        let point = Point::new(self.abs_line(row), Column(col));
        let side = if right { Side::Right } else { Side::Left };
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        if self.term.selection.is_some() {
            self.generation += 1;
        }
        self.term.selection = None;
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.is_some()
    }

    /// Select the entire buffer (scrollback + visible screen) — the terminal
    /// context menu's "Select All".
    pub fn select_all(&mut self) {
        self.generation += 1;
        let history = self.term.grid().history_size() as i32;
        let cols = self.term.grid().columns();
        let lines = self.term.screen_lines() as i32;
        let mut sel =
            Selection::new(SelectionType::Simple, Point::new(Line(-history), Column(0)), Side::Left);
        sel.update(Point::new(Line(lines - 1), Column(cols.saturating_sub(1))), Side::Right);
        self.term.selection = Some(sel);
    }

    /// Wipe the visible screen and the scrollback — the context menu's "Clear
    /// Buffer". Leaves the cursor where it is (the running program owns it).
    pub fn clear(&mut self) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        self.generation += 1;
        self.term.clear_screen(ClearMode::All);
        self.term.grid_mut().clear_history();
        self.term.scroll_display(Scroll::Bottom);
        self.term.selection = None;
    }

    /// The selected text, if any (for copy).
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// True if the app enabled bracketed-paste mode (paste should be wrapped in
    /// `ESC[200~` … `ESC[201~`).
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Whether the program asked for focus events (DECSET 1004), to be told of a focus
    /// change as `CSI I` (in) / `CSI O` (out).
    pub fn reports_focus(&self) -> bool {
        self.term.mode().contains(TermMode::FOCUS_IN_OUT)
    }


    /// Snapshot the active mouse-reporting + scroll modes (see [`MouseModes`]).
    pub fn mouse_modes(&self) -> MouseModes {
        let m = self.term.mode();
        MouseModes {
            reporting: m.intersects(TermMode::MOUSE_MODE),
            report_motion: m.contains(TermMode::MOUSE_MOTION),
            report_drag: m.contains(TermMode::MOUSE_DRAG),
            sgr: m.contains(TermMode::SGR_MOUSE),
            utf8: m.contains(TermMode::UTF8_MOUSE),
            alternate_scroll: m.contains(TermMode::ALTERNATE_SCROLL),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            app_cursor: m.contains(TermMode::APP_CURSOR),
        }
    }

    /// Classify Claude's visible screen for status. Scanning the *rendered grid*
    /// (vs the raw byte stream) is robust to chunk splits and is level-triggered,
    /// so a menu reads as attention only while it's actually on screen — it clears
    /// the instant the user escapes/answers. Returns `None` for plain output
    /// (typing, redraws), which is neither working nor attention.
    /// True if a menu / approval prompt is currently on the visible screen
    /// (AskUserQuestion, plan mode, "proceed?"). Level-triggered on the rendered
    /// grid, so attention clears the instant the prompt leaves (escape/answer).
    /// Working is NOT detected here — it's keyed off the live byte stream (see
    /// `session.rs`), so a spinner star left on screen can't pin it to "working".
    pub fn visible_menu(&self) -> bool {
        self.rows_from_bottom(MENU_ROWS, is_menu_row)
    }

    /// True while Claude's status row says it is working: a spinner glyph, then text
    /// ending in the interrupt hint. Level-triggered, unlike the spinner-frame stream,
    /// so it reads correctly when the row is FROZEN (the text is still there) and when
    /// a read stalls — neither of which says the turn ended.
    pub fn visible_working(&self) -> bool {
        self.any_visible_row(is_working_row)
    }

    /// True while Claude's fullscreen UI is scrolled away from its live bottom: it then
    /// shows a "Jump to bottom (ctrl+End) ↓" bar (or "N new messages (ctrl+End) ↓") and
    /// stops drawing its status row, so no spinner frames say anything about the turn.
    pub fn visible_scrolled(&self) -> bool {
        self.screen_contains(&["(ctrl+End)"])
    }

    /// True while Claude's status row reads "✳ Waiting for N background agents to finish":
    /// its turn is over by every other sign (the spinner stands still, the Stop hook has
    /// fired), yet it picks the work up by itself when the agents report, so the pane is
    /// not idle. The row is told from the same words in the transcript by its shape: a
    /// spinner glyph, a space, the phrase; Claude's prose starts with a bullet or an indent.
    pub fn visible_waiting_agents(&self) -> bool {
        self.any_visible_row(is_waiting_agents_row)
    }

    /// Whether any of the last 40 visible rows contains one of `needles`.
    fn screen_contains(&self, needles: &[&str]) -> bool {
        self.any_visible_row(|row| needles.iter().any(|m| row.contains(m)))
    }

    /// Whether `matches` holds for the text of any of the last 40 visible rows.
    fn any_visible_row(&self, matches: impl Fn(&str) -> bool) -> bool {
        self.rows_from_bottom(40, matches)
    }

    /// Whether `matches` holds for any of the last `n` visible rows. A narrow window
    /// is how a LIVE element is told from the same words sitting in the transcript:
    /// Claude anchors its input box and menus to the bottom of the screen.
    fn rows_from_bottom(&self, n: usize, matches: impl Fn(&str) -> bool) -> bool {
        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        let grid = self.term.grid();
        let off = grid.display_offset() as i32;
        let mut buf = String::with_capacity(cols);
        for row in rows.saturating_sub(n)..rows {
            buf.clear();
            let line = &grid[Line(row as i32 - off)];
            for col in 0..cols {
                buf.push(line[Column(col)].c);
            }
            if matches(&buf) {
                return true;
            }
        }
        false
    }

    /// True if Claude Code's own UI chrome is on the visible screen. This is how a
    /// pane running Claude on a REMOTE host is recognised: the markers ride the PTY
    /// like any other output, so it works identically over SSH, where the local
    /// process scan sees only `ssh` and no statusLine capture is ever written.
    ///
    /// The markers are Claude's hint line, which reads `? for shortcuts` in the
    /// default mode and shows the mode label in each of the other three (verified
    /// against the shipped CLI's mode table: default / accept edits on / plan mode
    /// on / auto mode on).
    ///
    /// Only rows from the cursor DOWN are scanned, which is the load-bearing detail.
    /// Claude renders its input box at the cursor with the hint line just below, so
    /// anchoring there finds it wherever the box currently sits: a fixed window at
    /// the screen bottom would miss a freshly-started Claude, whose transcript is
    /// still short and whose box is near the top. Anchoring also makes the signal
    /// self-clearing. Once Claude exits, its last screen stays in the transcript
    /// ABOVE the returning shell prompt, and text above the cursor is never scanned,
    /// so a stale hint line cannot keep the pane marked as running Claude.
    ///
    /// During a turn Claude replaces the hint with the interrupt hint, so a single
    /// absent scan does not mean it exited (see `ClaudeHandle::note_screen`).
    pub fn claude_chrome(&self) -> bool {
        const CHROME: &[&str] = &[
            "? for shortcuts",
            "manual mode on",
            "accept edits on",
            "plan mode on",
            "auto mode on",
            "bypass permissions on",
        ];
        /// Rows below the cursor to include. Measured against a real session: the
        /// cursor sits in the input box, and below it come the box's bottom border, an
        /// optional warning line, the user's statusLine, and finally the mode line, so
        /// the marker was 4 rows down. This is set well past that because everything
        /// between is optional and a future release could add another line; the window
        /// only ever extends DOWNWARD, which is what keeps stale chrome above the
        /// cursor from matching, so widening it costs no precision.
        const BELOW: usize = 8;

        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        let grid = self.term.grid();
        // Absolute grid line, deliberately NOT display-offset adjusted: whether a pane
        // is running Claude must not change just because the user scrolled up.
        let cursor = grid.cursor.point.line.0.max(0) as usize;
        let mut buf = String::with_capacity(cols);
        for row in cursor..rows.min(cursor + BELOW + 1) {
            buf.clear();
            let line = &grid[Line(row as i32)];
            for col in 0..cols {
                buf.push(line[Column(col)].c);
            }
            if CHROME.iter().any(|m| buf.contains(m)) {
                return true;
            }
        }
        false
    }


    /// The text of the row the cursor is on, trailing blanks trimmed. Read at Enter, it
    /// is the command line exactly as the shell showed it: completed, recalled, edited.
    /// Absolute grid line, like `claude_chrome`, so scrolling does not change it.
    pub fn cursor_row_text(&self) -> String {
        let cols = self.term.columns();
        let grid = self.term.grid();
        let cursor = grid.cursor.point.line.0.max(0) as usize;
        if cursor >= self.term.screen_lines() {
            return String::new();
        }
        let line = &grid[Line(cursor as i32)];
        let mut buf = String::with_capacity(cols);
        for col in 0..cols {
            buf.push(line[Column(col)].c);
        }
        buf.trim_end().to_string()
    }

    /// Whether the row the cursor is on is a slash command in Claude's input box.
    /// Read at Enter, this says the user is about to open a chooser themselves
    /// (`/model`, `/config`), which no hook reports and which must not read as Claude
    /// asking for something.
    pub fn input_row_is_slash(&self) -> bool {
        row_is_slash_command(&self.cursor_row_text())
    }

    pub fn default_bg(&self) -> [f32; 3] { rgbf(term_bg()) }
    pub fn size(&self) -> (usize, usize) { (self.term.columns(), self.term.screen_lines()) }

    /// (row, col, visible) for the block cursor. A cursor the program has hidden stays
    /// visible, where it last was, for `CURSOR_HIDE_GRACE`.
    pub fn cursor(&self) -> (usize, usize, bool) {
        let p = self.term.grid().cursor.point;
        let at_bottom = self.term.grid().display_offset() == 0;
        if self.term.mode().contains(TermMode::SHOW_CURSOR) {
            return (p.line.0.max(0) as usize, p.column.0, at_bottom);
        }
        let (row, col) = self.cursor_shown_at;
        (row, col, at_bottom && self.cursor_grace_remaining().is_some())
    }

    /// How much longer a hidden cursor is still drawn, if it is (see `CURSOR_HIDE_GRACE`).
    pub fn cursor_grace_remaining(&self) -> Option<std::time::Duration> {
        CURSOR_HIDE_GRACE.checked_sub(self.hidden_since?.elapsed())
    }

    /// The http(s) URL at a visible (row, col), or `None`. Single-row detection
    /// (web parity: a link does not span wrapped rows).
    pub fn link_at(&self, row: usize, col: usize) -> Option<String> {
        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        if row >= rows || col >= cols {
            return None;
        }
        let grid = self.term.grid();
        let off = grid.display_offset() as i32;
        let line = &grid[Line(row as i32 - off)];
        let mut text: Vec<char> = Vec::with_capacity(cols);
        for c in 0..cols {
            let ch = line[Column(c)].c;
            text.push(if (ch as u32) < 0x20 { ' ' } else { ch });
        }
        url_spans(&text)
            .into_iter()
            .find(|&(s, e)| col >= s && col < e)
            .map(|(s, e)| text[s..e].iter().collect())
    }

    /// Walk the visible screen, yielding (row, col, char, fg, bg, bold, wide,
    /// selected, search_hit, link) per cell. `selected` = inside the active
    /// selection; `search_hit` = 0 none / 1 a find match / 2 the current find
    /// match; `link` = inside a detected http(s) URL.
    pub fn for_each_cell(
        &self,
        mut f: impl FnMut(usize, usize, char, [f32; 3], [f32; 3], bool, bool, bool, u8, bool),
    ) {
        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        let sel = self.term.selection.as_ref().and_then(|s| s.to_range(&self.term));
        let grid = self.term.grid();
        // Offset by the scrollback position so a scrolled-up view shows history
        // (negative line indices address the scrollback).
        let off = grid.display_offset() as i32;
        // Precompute http(s)-link cells per visible row (single-row, web parity).
        // Quick-reject rows lacking both ':' and '/' before the regex-ish scan.
        let mut link_cells: HashSet<(usize, usize)> = HashSet::new();
        let mut rowbuf: Vec<char> = Vec::with_capacity(cols);
        for row in 0..rows {
            let line = &grid[Line(row as i32 - off)];
            rowbuf.clear();
            let (mut colon, mut slash) = (false, false);
            for col in 0..cols {
                let ch = line[Column(col)].c;
                let ch = if (ch as u32) < 0x20 { ' ' } else { ch };
                colon |= ch == ':';
                slash |= ch == '/';
                rowbuf.push(ch);
            }
            if colon && slash {
                for (s, e) in url_spans(&rowbuf) {
                    for col in s..e.min(cols) {
                        link_cells.insert((row, col));
                    }
                }
            }
        }
        for row in 0..rows {
            let line_idx = row as i32 - off;
            let line = &grid[Line(line_idx)];
            for col in 0..cols {
                let cell = &line[Column(col)];
                // The dummy cell after a wide char carries no glyph — the wide
                // glyph (drawn 2 cells wide) covers it.
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let mut fg = self.resolve(cell.fg, true);
                let mut bg = self.resolve(cell.bg, false);
                // Intense/bold (SGR 1): mirror Windows Terminal's intenseTextStyle.
                // style 2 (Bright) / 3 (All) brighten the colour; style 1 (Bold) / 3 (All)
                // use the bold font face. So "Bright" gives crisp regular-weight text in a
                // brighter colour (the classic xterm look), "Bold" the bold font, etc.
                let intense = cell.flags.contains(Flags::BOLD);
                let style = INTENSE_STYLE.load(Ordering::Relaxed);
                if intense && (style == 2 || style == 3) {
                    fg = self.intense_fg(cell.fg);
                }
                // Faint/dim (SGR 2): darken the foreground, like other terminals
                // (Alacritty uses 2/3). Without it, Claude's dim status line and hints
                // rendered at full brightness — far lighter than Windows Terminal etc.
                // Only DIM cells are touched, so normal text is unaffected.
                if cell.flags.contains(Flags::DIM) {
                    fg = Rgb {
                        r: (fg.r as f32 * 0.66).round() as u8,
                        g: (fg.g as f32 * 0.66).round() as u8,
                        b: (fg.b as f32 * 0.66).round() as u8,
                    };
                }
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.flags.contains(Flags::HIDDEN) {
                    fg = bg;
                }
                let bold = intense && (style == 1 || style == 3);
                let wide = cell.flags.contains(Flags::WIDE_CHAR);
                let selected =
                    sel.as_ref().is_some_and(|r| r.contains(Point::new(Line(line_idx), Column(col))));
                let search_hit = match &self.search {
                    Some(s) if s.cur_cells.contains(&(line_idx, col)) => 2,
                    Some(s) if s.cells.contains(&(line_idx, col)) => 1,
                    _ => 0,
                };
                let link = link_cells.contains(&(row, col));
                f(row, col, cell.c, rgbf(fg), rgbf(bg), bold, wide, selected, search_hit, link);
            }
        }
    }

    fn resolve(&self, color: Color, is_fg: bool) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Indexed(i) => self.palette[i as usize],
            Color::Named(named) => match named {
                NamedColor::Foreground => self.default_fg,
                NamedColor::Background => term_bg(),
                other => {
                    let idx = other as usize;
                    if idx < 256 {
                        self.palette[idx]
                    } else if is_fg {
                        self.default_fg
                    } else {
                        term_bg()
                    }
                }
            },
        }
    }

    /// The "intense" (bright) foreground for a colour, à la xterm/Windows Terminal:
    /// the eight standard ANSI colours (0–7) map to their bright variants (8–15), the
    /// default foreground is lightened toward white, and everything else (already-bright
    /// palette entries, 256-cube, and explicit RGB) is left as the user set it.
    fn intense_fg(&self, color: Color) -> Rgb {
        match color {
            // Default foreground → bright white (palette 15), the classic "bold = bright"
            // behaviour. The default fg is already light (#ccc), so merely lightening it
            // was barely visible; the bright-white entry is clearly brighter.
            Color::Named(NamedColor::Foreground) => self.palette[15],
            Color::Named(n) if (n as usize) < 8 => self.palette[n as usize + 8],
            Color::Indexed(i) if (i as usize) < 8 => self.palette[i as usize + 8],
            other => self.resolve(other, true),
        }
    }
}

fn rgbf(c: Rgb) -> [f32; 3] {
    [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0]
}

/// Rows from the bottom that a LIVE menu can occupy. Claude anchors its input box and
/// its choosers to the bottom of the screen, so a marker further up is transcript: an
/// old approval box scrolled back into view, or prose quoting the words. Scanning the
/// whole screen for them raised a card every time the user scrolled past one.
const MENU_ROWS: usize = 12;

/// The text after a spinner glyph at the start of `row`, if the row is one of Claude's
/// status rows. The glyph is one of the spinner's frames (Claude's ✢✳✶✻✽ range, or its
/// `·` frame) followed by a space, which no transcript line begins with.
fn status_row_text(row: &str) -> Option<&str> {
    let trimmed = row.trim_start();
    let mut chars = trimmed.chars();
    let glyph = chars.next()?;
    let is_frame = matches!(glyph as u32, 0x2722..=0x273F) || glyph == '·';
    if !is_frame || chars.next() != Some(' ') {
        return None;
    }
    Some(chars.as_str())
}

/// Whether a screen row is Claude's "✳ Waiting for N background agents to finish" status
/// row (see `VtTerm::visible_waiting_agents`).
fn is_waiting_agents_row(row: &str) -> bool {
    let Some(rest) = status_row_text(row) else { return false };
    rest.starts_with("Waiting for ") && rest.contains(" background agent") && rest.contains(" to finish")
}

/// Whether a screen row is Claude's working status row: a spinner frame and a line
/// carrying the interrupt hint ("✻ Thinking… (esc to interrupt)"). The shape is what
/// separates it from prose containing the same words, including Arbiter's own source
/// shown in a diff.
fn is_working_row(row: &str) -> bool {
    status_row_text(row).is_some_and(|rest| rest.contains("esc to interrupt"))
}

/// Whether a row is the footer of a live chooser. Claude draws the same hint under
/// AskUserQuestion, plan-mode approval and its own slash-command menus.
fn is_menu_row(row: &str) -> bool {
    if row.contains("Would you like to proceed") {
        return true;
    }
    // Claude's footer reads "↑/↓ to navigate · Enter to select · Esc to cancel". Any
    // ONE of those fragments also turns up in ordinary prose ("explains how to
    // navigate the tree"), which is how scrolling a transcript used to raise a card,
    // so a footer has to carry at least two of them.
    const HINTS: &[&str] = &["to navigate", "to select", "Esc to cancel", "Enter to"];
    HINTS.iter().filter(|h| row.contains(**h)).count() >= 2
}

/// Whether a rendered input row holds a slash command. Claude's box draws a border and
/// a prompt marker before the text, so those are stripped first; everything after the
/// first character must still look like a command, not prose that happens to start
/// with a slash.
fn row_is_slash_command(row: &str) -> bool {
    let body = row.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '│' | '|' | '>' | '\u{276f}')
    });
    let Some(rest) = body.strip_prefix('/') else { return false };
    // `/usr/bin/x` or `//` is a path or a comment, not a command.
    let Some(first) = rest.chars().next() else { return false };
    first.is_ascii_alphabetic()
        && rest.chars().take_while(|c| !c.is_whitespace()).all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

/// http(s):// scheme length at `t[i]` (8 for `https://`, 7 for `http://`), else
/// `None`.
fn url_scheme_len(t: &[char], i: usize) -> Option<usize> {
    let at = |p: &[char]| t.len() - i >= p.len() && t[i..i + p.len()] == *p;
    if at(&['h', 't', 't', 'p', 's', ':', '/', '/']) {
        Some(8)
    } else if at(&['h', 't', 't', 'p', ':', '/', '/']) {
        Some(7)
    } else {
        None
    }
}

/// Spans `(start, end_exclusive)` of http(s) URLs in a single row's chars,
/// matching the web `URL_RE = /(https?:\/\/[^\s'"<>` ` ` `]+)/` plus its trailing-
/// punctuation trim. Single-row only (a URL does not wrap across rows).
fn url_spans(text: &[char]) -> Vec<(usize, usize)> {
    // Trailing chars stripped from a match end (web `/[.,;:!?)\]}'"]+$/`).
    const TRAIL: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"'];
    // A URL body stops at whitespace or any of `'"<>` plus a backtick.
    let is_url_char = |c: char| !c.is_whitespace() && !matches!(c, '\'' | '"' | '<' | '>' | '`');
    let n = text.len();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < n {
        if let Some(len) = url_scheme_len(text, i) {
            let body = i + len;
            let mut j = body;
            while j < n && is_url_char(text[j]) {
                j += 1;
            }
            if j > body {
                let mut end = j;
                while end > body && TRAIL.contains(&text[end - 1]) {
                    end -= 1;
                }
                spans.push((i, end));
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    spans
}

/// Expand an inclusive match range (line-major) into its `(abs_line, col)` cells,
/// clamped to the grid width — for O(1) per-cell highlight lookups.
fn expand_match(m: &RangeInclusive<Point>, cols: usize, out: &mut HashSet<(i32, usize)>) {
    let (s, e) = (m.start(), m.end());
    let last = cols.saturating_sub(1);
    let mut line = s.line.0;
    while line <= e.line.0 {
        let c0 = if line == s.line.0 { s.column.0 } else { 0 };
        let c1 = if line == e.line.0 { e.column.0 } else { last };
        for c in c0..=c1.min(last) {
            out.insert((line, c));
        }
        if line - s.line.0 > 100_000 {
            break; // safety against a degenerate range
        }
        line += 1;
    }
}

/// Default foreground — the platform terminal default, matching the web themes:
/// iTerm2 white on macOS, Campbell light-grey on Windows.
fn default_fg() -> Rgb {
    #[cfg(windows)]
    {
        Rgb { r: 0xcc, g: 0xcc, b: 0xcc }
    }
    #[cfg(not(windows))]
    {
        Rgb { r: 0xff, g: 0xff, b: 0xff }
    }
}

fn build_palette() -> [Rgb; 256] {
    let mut p = [Rgb { r: 0, g: 0, b: 0 }; 256];
    // ANSI 0-15 = the platform palette the web app uses (iTerm2 on macOS,
    // Campbell on Windows). Order: black, red, green, yellow, blue, magenta,
    // cyan, white, then the eight bright variants.
    #[cfg(not(windows))]
    const ANSI: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00), (0xc9, 0x1b, 0x00), (0x00, 0xc2, 0x00), (0xc7, 0xc4, 0x00),
        (0x02, 0x25, 0xc7), (0xca, 0x30, 0xc7), (0x00, 0xc5, 0xc7), (0xc7, 0xc7, 0xc7),
        (0x68, 0x68, 0x68), (0xff, 0x6e, 0x67), (0x5f, 0xfa, 0x68), (0xff, 0xfc, 0x67),
        (0x68, 0x71, 0xff), (0xff, 0x77, 0xff), (0x60, 0xfd, 0xff), (0xff, 0xff, 0xff),
    ];
    #[cfg(windows)]
    const ANSI: [(u8, u8, u8); 16] = [
        (0x0c, 0x0c, 0x0c), (0xc5, 0x0f, 0x1f), (0x13, 0xa1, 0x0e), (0xc1, 0x9c, 0x00),
        (0x00, 0x37, 0xda), (0x88, 0x17, 0x98), (0x3a, 0x96, 0xdd), (0xcc, 0xcc, 0xcc),
        (0x76, 0x76, 0x76), (0xe7, 0x48, 0x56), (0x16, 0xc6, 0x0c), (0xf9, 0xf1, 0xa5),
        (0x3b, 0x78, 0xff), (0xb4, 0x00, 0x9e), (0x61, 0xd6, 0xd6), (0xf2, 0xf2, 0xf2),
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

#[cfg(test)]
mod tests {
    use super::url_spans;

    /// Run the URL scanner over `s` and return the matched substrings.
    fn matches(s: &str) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        url_spans(&chars).into_iter().map(|(a, b)| chars[a..b].iter().collect()).collect()
    }

    #[test]
    fn detects_basic_url() {
        assert_eq!(matches("see https://example.com for info"), vec!["https://example.com"]);
        assert_eq!(matches("http://a.b/c?x=1&y=2"), vec!["http://a.b/c?x=1&y=2"]);
    }

    #[test]
    fn trims_trailing_punctuation() {
        assert_eq!(matches("visit https://example.com."), vec!["https://example.com"]);
        assert_eq!(matches("(see https://example.com)"), vec!["https://example.com"]);
        assert_eq!(matches("https://ex.com/path!?"), vec!["https://ex.com/path"]);
    }

    #[test]
    fn stops_at_quotes_and_spaces() {
        assert_eq!(matches("\"https://a.com\" and https://b.com"), vec!["https://a.com", "https://b.com"]);
        assert_eq!(matches("<https://a.com>"), vec!["https://a.com"]);
    }

    #[test]
    fn ignores_non_http_and_bare_scheme() {
        assert!(matches("ftp://a.com no match").is_empty());
        assert!(matches("just text, no colon-slash").is_empty());
        assert!(matches("http://").is_empty()); // scheme with no body
    }

    // Claude's working row, against the same words as they appear in a transcript —
    // including Arbiter's own source, which contains the phrase in these very tests.
    #[test]
    fn the_working_row_is_known_by_its_shape() {
        use super::is_working_row as row;
        assert!(row("✻ Thinking… (esc to interrupt)"));
        assert!(row("· Brewing… (esc to interrupt · ctrl+t to hide todos)"));
        assert!(row("✳ Waiting for 2 background agents to finish · esc to interrupt   "));
        // Prose, quoted text and source: same words, no status-row shape.
        assert!(!row("  the hint reads (esc to interrupt) while it works"));
        assert!(!row("● Ran tool, esc to interrupt was shown"));
        assert!(!row("+    assert!(row(\"✻ Thinking… (esc to interrupt)\"));"));
        assert!(!row("✻ Brewed for 7s"));
        assert!(!row(""));
    }

    // A live chooser's footer, against an approval box sitting in the transcript.
    #[test]
    fn a_menu_footer_is_matched_by_its_words() {
        use super::is_menu_row as row;
        assert!(row("  ↑/↓ to navigate · Enter to select · Esc to cancel"));
        assert!(row("Would you like to proceed?"));
        assert!(row("  ↑/↓ to navigate · Esc to cancel"));
        // One stray fragment is prose, which is what used to raise a card on every
        // pass while scrolling back over a plan.
        assert!(!row("  the plan explains how to navigate the tree"));
        assert!(!row("  press Esc to cancel, it said, and I did"));
        assert!(!row(""));
    }

    // Telling a slash command in Claude's input box from an ordinary prompt.
    #[test]
    fn a_slash_command_is_told_from_a_prompt() {
        use super::row_is_slash_command as slash;
        assert!(slash("> /model"));
        assert!(slash("│ ❯ /config "));
        assert!(slash("  /agents"));
        assert!(slash("> /statusline setup"));
        assert!(!slash("> fix the bug in /src/main.rs"));
        assert!(!slash("> /usr/bin/env"));
        assert!(!slash("> //"));
        assert!(!slash("> how do I use /model?"));
        assert!(!slash("> "));
        assert!(!slash(""));
    }

    // The status row while Claude waits on agents it launched, against the same words
    // where Claude writes them in its transcript.
    #[test]
    fn the_waiting_for_agents_row_is_known_by_its_shape() {
        use super::is_waiting_agents_row as row;
        assert!(row("✳ Waiting for 3 background agents to finish"));
        assert!(row("· Waiting for 1 background agent to finish"));
        assert!(row("✻ Waiting for 2 background agents to finish · esc to interrupt   "));
        assert!(!row("● Waiting for 3 background agents to finish."));
        assert!(!row("  Waiting for 3 background agents to finish"));
        assert!(!row("✳ Waiting for API response"));
        assert!(!row("✳ Brewed for 7s"));
        assert!(!row(""));
    }

    /// Scrolling (wheel or drag auto-scroll) while a selection drag is active must
    /// keep extending the marked region: scroll the view, then re-extend to the same
    /// screen row — the selection should grow to cover the lines scrolled into view.
    // Every change the renderer must draw moves the generation; the renderer relies on
    // this to skip frames whose inputs are unchanged.
    #[test]
    fn generation_moves_with_every_visible_change() {
        let mut t = super::VtTerm::new(20, 4);
        let mut last = t.generation();
        let mut step = |t: &mut super::VtTerm, what: &str| {
            assert!(t.generation() > last, "{what} did not bump the generation");
            last = t.generation();
        };
        t.feed(b"hello\r\n");
        step(&mut t, "feed");
        t.resize(30, 5);
        step(&mut t, "resize");
        t.scroll(1);
        step(&mut t, "scroll");
        t.scroll_to_bottom();
        step(&mut t, "scroll_to_bottom");
        t.start_selection(0, 0, false, super::SelectKind::Simple);
        step(&mut t, "start_selection");
        t.update_selection(0, 3, true);
        step(&mut t, "update_selection");
        t.clear_selection();
        step(&mut t, "clear_selection");
        t.set_search("hell");
        step(&mut t, "set_search");
        t.clear_search();
        step(&mut t, "clear_search");
        t.set_history(100);
        step(&mut t, "set_history");
        t.clear();
        step(&mut t, "clear");
    }

    // Shrinking the history drops the oldest lines and reports the smaller size; growing
    // it back gives capacity, not content.
    #[test]
    fn history_shrinks_and_regrows_on_demand() {
        let mut t = super::VtTerm::new(10, 2);
        for i in 0..60 {
            t.feed(format!("L{i:02}\r\n").as_bytes());
        }
        assert!(t.scroll_state().1 >= 50, "lines scrolled into history");
        t.set_history(20);
        assert_eq!(t.scroll_state().1, 20);
        t.set_history(5000);
        assert_eq!(t.scroll_state().1, 20, "growing the cap does not bring lines back");
        for i in 0..30 {
            t.feed(format!("M{i:02}\r\n").as_bytes());
        }
        assert_eq!(t.scroll_state().1, 50);
    }

    #[test]
    fn selection_extends_while_scrolling() {
        use super::{SelectKind, VtTerm};
        let mut t = VtTerm::new(10, 4);
        // 20 lines L00..L19 → last 4 visible, the rest in scrollback.
        let mut s = String::new();
        for i in 0..20 {
            if i > 0 {
                s.push_str("\r\n");
            }
            s.push_str(&format!("L{i:02}"));
        }
        t.feed(s.as_bytes());

        // Anchor at the bottom visible row, then scroll up 4 lines and re-extend to the
        // same screen row (what the wheel / auto-scroll handlers do via drag_cell).
        t.start_selection(3, 0, false, SelectKind::Simple);
        let before = t.selection_text().unwrap_or_default();
        t.scroll(4);
        t.update_selection(3, 9, true);
        let after = t.selection_text().unwrap_or_default();

        assert!(
            after.lines().count() > before.lines().count(),
            "scrolling while selecting should extend the selection: before={before:?} after={after:?}"
        );
    }

    /// Feed `s` to a fresh 80x24 term and ask whether Claude's chrome is on screen.
    /// The cursor ends up wherever `s` leaves it, which is what the scan anchors to.
    fn chrome(s: &str) -> bool {
        use super::VtTerm;
        let mut t = VtTerm::new(80, 24);
        t.feed(s.as_bytes());
        t.claude_chrome()
    }

    /// Claude's rendered shape: an input box holding the cursor, then its hint line
    /// one row below. `\x1b[A` walks the cursor back up into the box, which is where
    /// Claude leaves it.
    fn claude_screen(hint: &str) -> String {
        format!("> \r\n{hint}\x1b[A\x1b[C\x1b[C")
    }

    #[test]
    fn detects_claude_chrome_in_every_mode() {
        // The hint line Claude draws in its default mode, and the label it shows
        // instead in each of the other three (verified against the shipped CLI).
        assert!(chrome(&claude_screen("? for shortcuts")));
        assert!(chrome(&claude_screen("\u{23f5}\u{23f5} auto mode on (shift+tab to cycle)")));
        assert!(chrome(&claude_screen("\u{23f5}\u{23f5} accept edits on (shift+tab to cycle)")));
        assert!(chrome(&claude_screen("\u{23f5} plan mode on (shift+tab to cycle)")));
        // Observed in v2.1.263: the default mode names itself, and the line carries
        // both the label and the shortcuts hint.
        assert!(chrome(&claude_screen(
            "\u{23f5} manual mode on \u{b7} ? for shortcuts \u{b7} + for agents"
        )));
        // The label alone, in case a future layout drops the hint.
        assert!(chrome(&claude_screen("\u{23f5} manual mode on")));
    }

    #[test]
    fn detects_claude_chrome_with_a_status_line_between() {
        // A user statusLine renders between the box and the hint, pushing the hint
        // further from the cursor. It must still be found.
        assert!(chrome("> \r\nmdl:opus | ctx:13%/1000K\r\n? for shortcuts\x1b[A\x1b[A"));
    }

    // Transcribed from a real `claude` session captured through a PTY, cursor row
    // included: the input box, its bottom border, a warning line, the user's
    // statusLine, then the mode line. That is 4 rows between the cursor and the only
    // marker, which is what sizes the scan window, so this pins the real geometry
    // rather than an idealised version of it.
    #[test]
    fn detects_chrome_in_a_captured_real_session() {
        let screen = concat!(
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\r\n",
            "\u{276f} Try \"refactor <filepath>\"\r\n",
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\r\n",
            "  \u{26a0} Transcript saving is off\r\n",
            "  mdl:opus | ctx: no data | tokens: no data | fld:arbiter-app[main]\r\n",
            "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle)",
            // Claude leaves the cursor in the input box, 4 rows back up.
            "\x1b[4A\x1b[3C",
        );
        assert!(chrome(screen));
    }

    #[test]
    fn plain_shell_output_is_not_claude_chrome() {
        assert!(!chrome("$ cargo build --bin arbiter\r\n   Compiling arbiter-native v1.0.12"));
        assert!(!chrome("total 48\r\ndrwxr-xr-x  12 tre  staff   384 Sep  8 16:48 src"));
        // The bare prompt arrow is not enough: Claude's input box draws one, but so
        // do plenty of shell prompts.
        assert!(!chrome("\u{276f} "));
    }

    #[test]
    fn chrome_above_the_cursor_stops_counting() {
        // The exit case, and the reason the scan is cursor-anchored. Claude's last
        // screen stays in the transcript, but the shell prompt that follows puts the
        // cursor BELOW it, so the stale hint must not keep the pane marked as Claude.
        // Two rows of separation is the tight case: a bottom-anchored window would
        // still match here.
        assert!(!chrome("? for shortcuts\r\n\r\n$ "));
        // Also true when it is only one row up.
        assert!(!chrome("? for shortcuts\r\n$ "));
    }

    // A hidden cursor stays drawn where it was for the grace period, then goes; showing
    // it again puts it back where the program has it.
    #[test]
    fn a_hidden_cursor_lingers_for_the_grace_period() {
        let mut t = super::VtTerm::new(20, 5);
        t.feed(b"ab");
        assert_eq!(t.cursor(), (0, 2, true));
        t.feed(b"\x1b[?25l\x1b[H");
        assert_eq!(t.cursor(), (0, 2, true), "still drawn where it was, not where the program moved it");
        assert!(t.cursor_grace_remaining().is_some());
        t.hidden_since = Some(std::time::Instant::now() - super::CURSOR_HIDE_GRACE - std::time::Duration::from_millis(1));
        assert_eq!(t.cursor(), (0, 2, false));
        assert!(t.cursor_grace_remaining().is_none());
        t.feed(b"\x1b[?25h");
        assert_eq!(t.cursor(), (0, 0, true));
    }
}
