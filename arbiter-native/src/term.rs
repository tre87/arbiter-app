//! Headless VT terminal: `alacritty_terminal` Term + parser + colour palette.
//! Same approach as the shipping app's `termgrid::HeadlessTerm`.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};

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
            palette: build_xterm_256_palette(),
            default_fg: Rgb { r: 0xcc, g: 0xcc, b: 0xcc },
            default_bg: Rgb { r: 0x14, g: 0x14, b: 0x16 },
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.term.resize(Size { cols, rows });
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

    /// Walk the visible screen, yielding (row, col, char, fg, bg) per cell.
    pub fn for_each_cell(&self, mut f: impl FnMut(usize, usize, char, [f32; 3], [f32; 3])) {
        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        let grid = self.term.grid();
        for row in 0..rows {
            let line = &grid[Line(row as i32)];
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
                f(row, col, cell.c, rgbf(fg), rgbf(bg));
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
