//! Headless VT terminal: `alacritty_terminal` Term + parser + colour palette.
//! Same approach as the shipping app's `termgrid::HeadlessTerm`.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};

/// Selection granularity for a fresh selection (single/double/triple click).
pub enum SelectKind {
    Simple,
    Word,
    Line,
}

#[derive(Clone, Copy, Default)]
pub struct NoopListener;
impl EventListener for NoopListener {}

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

pub struct VtTerm {
    term: Term<NoopListener>,
    parser: Processor,
    palette: [Rgb; 256],
    default_fg: Rgb,
    default_bg: Rgb,
}

impl VtTerm {
    pub fn new(cols: usize, rows: usize) -> Self {
        let mut config = Config::default();
        config.scrolling_history = 5000;
        let term = Term::new(config, &Size { cols, rows }, NoopListener);
        Self {
            term,
            parser: Processor::new(),
            palette: build_palette(),
            default_fg: default_fg(),
            // Arbiter's signature terminal background (CUSTOM_TERMINAL_BG in the
            // web app). The Iced shell's surface is this colour too, so skipped
            // (empty, default-bg) cells show through seamlessly.
            default_bg: Rgb { r: 0x12, g: 0x12, b: 0x12 },
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.term.resize(Size { cols, rows });
    }

    /// Scroll the display by `lines` into scrollback (positive = up/older),
    /// clamped to the history. The next `for_each_cell` renders the new view.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Jump back to the live bottom (display offset 0).
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Map a visible row to an absolute grid line (accounting for scrollback).
    fn abs_line(&self, row: usize) -> Line {
        Line(row as i32 - self.term.grid().display_offset() as i32)
    }

    /// Begin a selection at a visible (row, col); `right` = cursor in the cell's
    /// right half (which edge the selection snaps to). `kind` sets the
    /// granularity (single/double/triple click → char/word/line).
    pub fn start_selection(&mut self, row: usize, col: usize, right: bool, kind: SelectKind) {
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
        let point = Point::new(self.abs_line(row), Column(col));
        let side = if right { Side::Right } else { Side::Left };
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.is_some()
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

    /// True if the visible screen shows a Claude menu / approval prompt (which no
    /// hook covers — AskUserQuestion, plan mode, "proceed?"). Scanning the
    /// rendered grid (vs the raw byte stream) is robust to chunk splits and
    /// catches the prompt for as long as it stays on screen.
    pub fn visible_has_menu(&self) -> bool {
        // The exact markers the web used (AskUserQuestion / plan-mode menus).
        const MARKERS: &[&str] = &["to navigate", "Esc to cancel", "Would you like to proceed"];
        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        let grid = self.term.grid();
        let off = grid.display_offset() as i32;
        let mut buf = String::with_capacity(cols);
        for row in rows.saturating_sub(24)..rows {
            buf.clear();
            let line = &grid[Line(row as i32 - off)];
            for col in 0..cols {
                buf.push(line[Column(col)].c);
            }
            if MARKERS.iter().any(|m| buf.contains(m)) {
                return true;
            }
        }
        false
    }

    pub fn default_bg(&self) -> [f32; 3] { rgbf(self.default_bg) }
    pub fn size(&self) -> (usize, usize) { (self.term.columns(), self.term.screen_lines()) }

    /// (row, col, visible) for the block cursor.
    pub fn cursor(&self) -> (usize, usize, bool) {
        let p = self.term.grid().cursor.point;
        let vis = self.term.grid().display_offset() == 0
            && self.term.mode().contains(TermMode::SHOW_CURSOR);
        (p.line.0.max(0) as usize, p.column.0, vis)
    }

    /// Walk the visible screen, yielding (row, col, char, fg, bg, bold, selected)
    /// per cell. `selected` is true for cells inside the active selection.
    pub fn for_each_cell(&self, mut f: impl FnMut(usize, usize, char, [f32; 3], [f32; 3], bool, bool)) {
        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        let sel = self.term.selection.as_ref().and_then(|s| s.to_range(&self.term));
        let grid = self.term.grid();
        // Offset by the scrollback position so a scrolled-up view shows history
        // (negative line indices address the scrollback).
        let off = grid.display_offset() as i32;
        for row in 0..rows {
            let line_idx = row as i32 - off;
            let line = &grid[Line(line_idx)];
            for col in 0..cols {
                let cell = &line[Column(col)];
                let mut fg = self.resolve(cell.fg, true);
                let mut bg = self.resolve(cell.bg, false);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.flags.contains(Flags::HIDDEN) {
                    fg = bg;
                }
                let bold = cell.flags.contains(Flags::BOLD);
                let selected =
                    sel.as_ref().is_some_and(|r| r.contains(Point::new(Line(line_idx), Column(col))));
                f(row, col, cell.c, rgbf(fg), rgbf(bg), bold, selected);
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
}

fn rgbf(c: Rgb) -> [f32; 3] {
    [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0]
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
