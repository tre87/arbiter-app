//! The Ops Floor: a pixel-art room where one desk is one pane.
//!
//! Stage 1 — geometry and the state read. Nothing here is wired to a real
//! `Session`; `floor_demo` drives it with fake desks. The module is a pure
//! function from a `Scene` to a straight-RGBA buffer, so it unit-tests without a
//! GPU and can later feed either `iced::widget::image` or the wgpu quad pipeline.
//!
//! Drawn, not imported. Every pixel here is a `fill` call, which is why the
//! feature adds no asset files, no atlas, no dependency, and touches neither
//! glyph path (`raster.rs` CoreText/DirectWrite) that the rest of the app has to
//! be careful about.
//!
//! Three rules the rest of the design leans on:
//!
//! * **A desk never moves.** Slot `i` is always at row `i / COLS`, column
//!   `i % COLS`, for the life of the pane. Sorting desks by urgency would read
//!   better for one frame and destroy the only advantage the room has over the
//!   Overview list, which is that you learn where things are.
//! * **Colour is a reserved word.** [`WORKING`] is painted by nothing except a
//!   desk in [`Desk::Working`], [`ATTENTION`] by nothing except
//!   [`Desk::Attention`]. Furniture and weather may not borrow either.
//!   `tests::reserved_colours` holds the line across every task and every sky.
//! * **Nothing is random.** The room is a pure function of `(scene, t)`. What
//!   varies — which task a desk is doing, where a raindrop is — comes from
//!   [`hash`], never from stored state or an RNG, so any frame can be
//!   reproduced and the whole thing stays testable.
//!
//! # Overflow: what happens past five desks
//!
//! Five across is one row. The sixth desk opens a second row *below* it, the
//! eleventh a third, and so on, so the room grows downward like floors of a
//! building while every existing desk keeps its slot. The logical height grows
//! by [`ROW_H`] per row and the caller scales the whole buffer down by whole
//! integers to fit, which keeps pixels square (a non-integer scale is the one
//! thing that instantly reads as sloppy).
//!
//! That holds to roughly four rows / twenty desks, where a typical window can no
//! longer give the buffer even a 1× fit. Past that the room has to either drop
//! furniture detail or aggregate, and the honest answer is that nobody watching
//! twenty agents is reading faces anyway. Rejected alternative: scrolling the
//! room. It keeps desks large but puts some of them off-screen, which costs the
//! glanceability that is the entire point.

/// Straight (non-premultiplied) RGBA, which is what `iced::widget::image` wants.
/// Alpha is on the colour rather than the buffer: the buffer is always opaque.
#[derive(Clone, Copy, Debug)]
pub struct Rgba(pub u8, pub u8, pub u8, pub f32);

const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    Rgba(r, g, b, 1.0)
}

impl Rgba {
    const fn alpha(self, a: f32) -> Rgba {
        Rgba(self.0, self.1, self.2, a)
    }
}

/// Deterministic jitter. Everything that looks arbitrary in this room — which
/// task a desk picks, where a raindrop starts, which distant window is lit —
/// comes from here, so `render` stays a pure function of `(scene, t)` and a
/// failing frame can always be reproduced.
fn hash(a: i64, b: i64) -> u32 {
    let mut x = (a as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((b as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9));
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 29;
    (x >> 32) as u32
}

/// `hash` as a unit float.
fn hashf(a: i64, b: i64) -> f32 {
    hash(a, b) as f32 / u32::MAX as f32
}

// ---------------------------------------------------------------- palette ---
// The neutrals are deliberately low saturation and biased cool, so the room
// reads as furniture and the semantics are the only thing the eye is drawn to.
// Anchor GROUND to the app's panel background when this is wired up, so the
// scene dissolves into the chrome at its edges.

const GROUND: Rgba = rgb(0x0d, 0x12, 0x18);
const CEIL: Rgba = rgb(0x10, 0x16, 0x1d);
const WALL: Rgba = rgb(0x19, 0x20, 0x29);
const WALL_SEAM: Rgba = rgb(0x13, 0x1a, 0x22);
const TRIM: Rgba = rgb(0x2a, 0x34, 0x40);
const FLOOR: Rgba = rgb(0x14, 0x1b, 0x23);
const FLOOR_LINE: Rgba = rgb(0x1d, 0x26, 0x30);
const METAL: Rgba = rgb(0x38, 0x42, 0x4e);
const METAL_DK: Rgba = rgb(0x25, 0x2d, 0x37);
const METAL_LT: Rgba = rgb(0x51, 0x5d, 0x6c);
const DESK: Rgba = rgb(0x32, 0x2f, 0x2b);
const DESK_LT: Rgba = rgb(0x3d, 0x3a, 0x35);
const CHAIR: Rgba = rgb(0x29, 0x31, 0x3c);
const CHAIR_LT: Rgba = rgb(0x3a, 0x44, 0x51);
const SKIN: Rgba = rgb(0x9c, 0x81, 0x68);
const HAIR: Rgba = rgb(0x23, 0x26, 0x2b);
const PAPER: Rgba = rgb(0xc6, 0xcc, 0xd2);
/// Mugs are ceramic, not paper. At desk scale a paper-white mug is the
/// brightest thing in the room, which is a lot of attention for a hot drink.
const CERAMIC: Rgba = rgb(0x8d, 0x94, 0x9c);
const PLANT_DK: Rgba = rgb(0x26, 0x3b, 0x2d);
const PLANT: Rgba = rgb(0x37, 0x53, 0x3f);
const PLANT_LT: Rgba = rgb(0x4a, 0x6d, 0x51);
const PLANT_HI: Rgba = rgb(0x62, 0x8c, 0x68);
const POT: Rgba = rgb(0x5b, 0x4d, 0x3e);
const POT_DK: Rgba = rgb(0x45, 0x3a, 0x2f);

/// Desk lighting. Warm, but deliberately desaturated: a saturated warm glow at
/// this size reads as [`ATTENTION`], and the whole room depends on that colour
/// meaning exactly one thing.
const LAMP_WARM: Rgba = rgb(0xc8, 0xb7, 0x9a);
/// Daylight through the clerestory on a sunny day.
const SUNLIGHT: Rgba = rgb(0xf2, 0xe8, 0xc6);

/// The app's `TXT_MUTED`. A blue-grey that carries the house colour into the
/// furniture (light fixtures, window frames, distant city) without going
/// anywhere near the reserved [`WORKING`] azure.
const STEEL: Rgba = rgb(0x6b, 0x7a, 0x8d);

/// Shirt colours, one per slot, so neighbouring desks are told apart without
/// anybody acquiring a personality. Indexed by slot, not by pane identity.
const SHIRTS: [Rgba; 5] = [
    rgb(0x55, 0x60, 0x6d),
    rgb(0x6a, 0x61, 0x57),
    rgb(0x46, 0x56, 0x5c),
    rgb(0x5a, 0x53, 0x66),
    rgb(0x5f, 0x5a, 0x4e),
];

/// Reserved. Painted only by a desk in [`Desk::Working`]. This is the app's
/// `AZURE` (`iced_shell.rs`), the same blue as selection and focus, so a working
/// agent glows in the product's own colour rather than a near-miss of it.
pub const WORKING: Rgba = rgb(0x33, 0x99, 0xff);
/// Reserved, and the single loudest thing the room is allowed to do. The app's
/// `AMBER`, which is already the attention colour of the status dot.
pub const ATTENTION: Rgba = rgb(0xe5, 0xa0, 0x3c);
/// Transient, shown for a few seconds after a turn ends. The app's success green.
pub const DONE: Rgba = rgb(0x22, 0xc5, 0x5e);
const SCREEN_OFF: Rgba = rgb(0x39, 0x44, 0x52);

// ----------------------------------------------------------------- layout ---

/// Desks per row. Five is what fits before a face stops being legible.
pub const COLS: usize = 5;
/// Horizontal pitch of one workstation.
pub const SLOT_W: i32 = 76;
/// Logical width of the room. Fixed: only the height grows with desk count.
pub const W: i32 = COLS as i32 * SLOT_W;
/// The clerestory band: windows onto the weather, plus the ceiling lights under
/// them. Drawn once at the top however many rows there are, which is why the
/// weather costs the same at one desk and at twenty.
pub const CEIL_H: i32 = 46;
/// One row of desks: wall above, desk, floor strip below.
pub const ROW_H: i32 = 92;
/// The near floor, drawn once at the bottom: the walkway in front of the last
/// row of desks, and where a passing colleague will go in stage 3. Shallow on
/// purpose — a deep empty band under the desks reads as a missing wall.
pub const FORE_H: i32 = 18;

/// Rows needed for `slots` desks. Always at least one, so an empty room is still
/// a room.
pub fn rows(slots: usize) -> i32 {
    (slots.max(1) as i32 + COLS as i32 - 1) / COLS as i32
}

/// Logical height for `slots` desks. Grows by [`ROW_H`] per row.
pub fn height(slots: usize) -> i32 {
    CEIL_H + rows(slots) * ROW_H + FORE_H
}

/// Centre x and row-band top y of slot `i`. Depends only on the index, never on
/// state, which is what makes the room learnable.
pub fn slot_origin(i: usize) -> (i32, i32) {
    let col = (i % COLS) as i32;
    let row = (i / COLS) as i32;
    (col * SLOT_W + SLOT_W / 2, CEIL_H + row * ROW_H)
}

/// Slot under a point in logical pixels, or `None` for the clerestory, the
/// foreground and the gaps between rows. Ambient decoration is never clickable:
/// a click in this room must be unambiguous.
pub fn hit(x: i32, y: i32, slots: usize) -> Option<usize> {
    if x < 0 || x >= W || y < CEIL_H {
        return None;
    }
    let row = (y - CEIL_H) / ROW_H;
    if row < 0 || row >= rows(slots) {
        return None;
    }
    let i = row as usize * COLS + (x / SLOT_W) as usize;
    (i < rows(slots) as usize * COLS).then_some(i)
}

// ---------------------------------------------------------------- weather ---

/// What is going on outside the clerestory windows. Pure decoration: it encodes
/// nothing about any agent, and it is the one thing in the room allowed to be
/// merely pleasant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Weather {
    #[default]
    Clear,
    /// The one daylight sky. Everything else is evening, which is what a dark
    /// room wants, so this is the outlier and it is kept deliberately mid-value
    /// rather than blown out.
    Sunny,
    Cloudy,
    Rain,
    Snow,
    Fog,
}

/// How long one kind of weather lasts, in scene seconds. Long enough that it is
/// a thing you notice having changed rather than a thing you watch change.
pub const WEATHER_SECS: f32 = 22.0;

impl Weather {
    pub const ALL: [Weather; 6] = [
        Weather::Clear,
        Weather::Sunny,
        Weather::Cloudy,
        Weather::Rain,
        Weather::Snow,
        Weather::Fog,
    ];

    /// The sky over a long session. O(1) in the phase, so any `t` can be asked
    /// for without walking the history, and deterministic, so a frame can be
    /// reproduced. Two phases in a row landing on the same weather is allowed:
    /// it just means the weather held, which is what weather does.
    pub fn drifting(t: f32) -> Weather {
        let n = (t / WEATHER_SECS).floor() as i64;
        Weather::ALL[(hash(n, 0x5121) % Weather::ALL.len() as u32) as usize]
    }

    /// Whether the sky needs a clock. Precipitation moves; a clear, cloudy or
    /// foggy sky is a still image, so a quiet room under one costs nothing.
    pub fn moves(self) -> bool {
        matches!(self, Weather::Rain | Weather::Snow)
    }

    pub fn label(self) -> &'static str {
        match self {
            Weather::Clear => "clear",
            Weather::Sunny => "sunny",
            Weather::Cloudy => "cloudy",
            Weather::Rain => "rain",
            Weather::Snow => "snow",
            Weather::Fog => "fog",
        }
    }

    /// Sky gradient, top and bottom. Dim on purpose: these are windows in a dark
    /// room, and a daylight-bright rectangle would out-shout every agent in it.
    fn sky(self) -> (Rgba, Rgba) {
        match self {
            Weather::Clear => (rgb(0x0e, 0x1c, 0x33), rgb(0x1a, 0x33, 0x57)),
            Weather::Sunny => (rgb(0x2b, 0x58, 0x8a), rgb(0x6a, 0x9a, 0xbe)),
            Weather::Cloudy => (rgb(0x16, 0x1e, 0x27), rgb(0x23, 0x2d, 0x38)),
            Weather::Rain => (rgb(0x11, 0x17, 0x1f), rgb(0x1b, 0x24, 0x2e)),
            Weather::Snow => (rgb(0x16, 0x1b, 0x23), rgb(0x22, 0x2a, 0x34)),
            Weather::Fog => (rgb(0x1e, 0x23, 0x2a), rgb(0x2a, 0x31, 0x39)),
        }
    }
}

// ------------------------------------------------------------------- task ---

/// What a working agent looks busy doing. Purely cosmetic variety: every task
/// means exactly the same thing, [`Desk::Working`], and none of them is a mood.
/// The point is that five desks all mid-turn should not look like five copies of
/// one sprite.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Task {
    Typing,
    /// Leaning in, hands down, reading what came back.
    Reading,
    /// One hand on the keys, one writing on a pad.
    Noting,
    /// Back in the chair, hand to chin.
    Pondering,
    /// Mug up. Everybody does it.
    Sipping,
}

/// Weighted so typing dominates: a room where everyone is contemplating their
/// chin does not read as a room getting work done.
const TASK_MIX: [Task; 8] = [
    Task::Typing,
    Task::Typing,
    Task::Typing,
    Task::Reading,
    Task::Reading,
    Task::Noting,
    Task::Pondering,
    Task::Sipping,
];

/// How long a desk stays on one task, in scene seconds.
pub const TASK_SECS: f32 = 6.5;

/// The task slot `i` is doing at time `t`. Each desk gets its own phase offset,
/// so the room never switches in unison, which would read as choreography.
pub fn task(i: usize, t: f32) -> Task {
    let offset = hashf(i as i64, 0x7a1) * TASK_SECS;
    let phase = ((t + offset) / TASK_SECS).floor() as i64;
    TASK_MIX[(hash(i as i64, phase) % TASK_MIX.len() as u32) as usize]
}

// ------------------------------------------------------------------ state ---

/// What one desk is doing. Mirrors the app's `Dot` vocabulary
/// (`iced_shell.rs:7013`) plus the two the room adds. The floor never derives
/// status itself; it is handed whatever `pane_dot()` already computed, so the
/// dot and the room can never disagree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Desk {
    /// No pane here. A free desk, and clicking it should open one.
    Empty,
    /// Shell at a prompt, nothing running.
    Idle,
    /// A non-Claude command is running.
    Running,
    /// Claude is live but between turns.
    Ready,
    /// A turn is in flight.
    Working,
    /// Blocked on the user: a permission prompt, a plan, a question.
    Attention,
    /// A turn just ended. Transient; the caller decays this back to `Ready`.
    Done,
}

impl Desk {
    /// The screen glow, which is the room's primary state channel. `None` means
    /// a dark monitor.
    fn glow(self) -> Option<Rgba> {
        match self {
            Desk::Empty => None,
            Desk::Idle | Desk::Running | Desk::Ready => Some(SCREEN_OFF),
            Desk::Working => Some(WORKING),
            Desk::Attention => Some(ATTENTION),
            Desk::Done => Some(DONE),
        }
    }

    /// Whether this desk needs a clock. Only two states move.
    fn animates(self) -> bool {
        matches!(self, Desk::Working | Desk::Attention)
    }

    /// Short label for a readout. Not drawn in the room: no text in the art.
    pub fn label(self) -> &'static str {
        match self {
            Desk::Empty => "free desk",
            Desk::Idle => "idle",
            Desk::Running => "running",
            Desk::Ready => "ready",
            Desk::Working => "working",
            Desk::Attention => "needs you",
            Desk::Done => "done",
        }
    }
}

/// The whole room. `desks` is padded to a full row, so its length is the number
/// of visible slots and never less than [`COLS`].
#[derive(Clone, Debug)]
pub struct Scene {
    pub desks: Vec<Desk>,
    pub selected: Option<usize>,
    pub weather: Weather,
}

impl Default for Scene {
    fn default() -> Self {
        Scene {
            desks: vec![Desk::Empty; COLS],
            selected: None,
            weather: Weather::default(),
        }
    }
}

impl Scene {
    /// Fill the first free desk, opening a new row if every desk is taken.
    /// Returns the slot used.
    pub fn spawn(&mut self, state: Desk) -> usize {
        let i = match self.desks.iter().position(|d| *d == Desk::Empty) {
            Some(i) => i,
            None => {
                let i = self.desks.len();
                self.desks.extend(std::iter::repeat(Desk::Empty).take(COLS));
                i
            }
        };
        self.desks[i] = state;
        i
    }

    /// Clear the last occupied desk, dropping a trailing row once it is empty so
    /// the room shrinks back down.
    pub fn remove_last(&mut self) {
        if let Some(i) = self.desks.iter().rposition(|d| *d != Desk::Empty) {
            self.desks[i] = Desk::Empty;
        }
        while self.desks.len() > COLS
            && self.desks[self.desks.len() - COLS..]
                .iter()
                .all(|d| *d == Desk::Empty)
        {
            self.desks.truncate(self.desks.len() - COLS);
            if self.selected.is_some_and(|s| s >= self.desks.len()) {
                self.selected = None;
            }
        }
    }

    pub fn occupied(&self) -> usize {
        self.desks.iter().filter(|d| **d != Desk::Empty).count()
    }

    pub fn count(&self, state: Desk) -> usize {
        self.desks.iter().filter(|d| **d == state).count()
    }

    /// Whether anything in the room moves. When this is false the caller must
    /// stop its timer: a still room costs nothing, which is what lets an ambient
    /// display sit beside the app's no-polling rule without lying about it.
    /// Note that precipitation counts, so a rainy sky keeps the clock alive even
    /// over a room full of idle agents. That is the price of the window, and it
    /// is why [`Weather::moves`] exists rather than being assumed.
    pub fn animates(&self) -> bool {
        self.weather.moves() || self.desks.iter().any(|d| d.animates())
    }

    pub fn size(&self) -> (i32, i32) {
        (W, height(self.desks.len()))
    }
}

// ----------------------------------------------------------------- canvas ---

/// An opaque RGBA buffer that only knows how to fill rectangles.
pub struct Buf {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

impl Buf {
    fn new(w: i32, h: i32) -> Self {
        Buf {
            w,
            h,
            px: vec![0; (w * h * 4) as usize],
        }
    }

    /// Source-over a rectangle, clipped to the buffer. The destination stays
    /// opaque, so alpha here is a blend weight and never reaches the output.
    fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        if c.3 <= 0.0 || w <= 0 || h <= 0 {
            return;
        }
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        let opaque = c.3 >= 1.0;
        for yy in y0..y1 {
            for xx in x0..x1 {
                let i = ((yy * self.w + xx) * 4) as usize;
                if opaque {
                    self.px[i] = c.0;
                    self.px[i + 1] = c.1;
                    self.px[i + 2] = c.2;
                } else {
                    let src = [c.0, c.1, c.2];
                    for k in 0..3 {
                        let s = src[k] as f32 * c.3;
                        let d = self.px[i + k] as f32 * (1.0 - c.3);
                        self.px[i + k] = (s + d).round().clamp(0.0, 255.0) as u8;
                    }
                }
                self.px[i + 3] = 255;
            }
        }
    }

    /// Checkerboard fill, for the outer edge of a light pool. Dithering is the
    /// only texture this room has: a flat alpha band over a flat wall ends in a
    /// visible rectangle, which reads as a box rather than as light. The pattern
    /// is anchored to the buffer, not the viewport, so it can never crawl.
    fn fill_checker(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        for yy in y.max(0)..(y + h).min(self.h) {
            for xx in x.max(0)..(x + w).min(self.w) {
                if (xx + yy) & 1 == 0 {
                    self.fill(xx, yy, 1, 1, c);
                }
            }
        }
    }
}

// ------------------------------------------------------------------ paint ---

/// Draw the room. `t` is seconds since the scene opened; pass a constant for a
/// frozen frame.
pub fn render(scene: &Scene, t: f32) -> Buf {
    let (w, h) = scene.size();
    let mut b = Buf::new(w, h);
    room(&mut b, scene, t);
    for (i, desk) in scene.desks.iter().enumerate() {
        station(&mut b, i, *desk, scene.selected == Some(i), t);
    }
    // A faint scanline over everything, which is the cheapest way to stop a
    // field of flat rectangles reading as a chart.
    let mut y = 0;
    while y < h {
        b.fill(0, y, w, 1, Rgba(0, 0, 0, 0.05));
        y += 2;
    }
    b
}

fn room(b: &mut Buf, scene: &Scene, t: f32) {
    let (w, h) = (b.w, b.h);
    let n = scene.desks.len();
    b.fill(0, 0, w, h, GROUND);
    clerestory(b, scene.weather, t);

    // One band per row: wall, then the floor strip the desks stand on.
    for r in 0..rows(n) {
        let top = CEIL_H + r * ROW_H;
        b.fill(0, top, w, ROW_H - 12, WALL);
        let mut x = 0;
        while x < w {
            b.fill(x, top, 1, ROW_H - 12, WALL_SEAM);
            x += 38;
        }
        // A little light at the top of every row, so rows two and three read as
        // more of the same room rather than as separate strips.
        b.fill(0, top, w, 6, STEEL.alpha(0.05));
        b.fill(0, top + ROW_H - 12, w, 2, TRIM);
        b.fill(0, top + ROW_H - 10, w, 10, FLOOR);
    }

    // The near floor. It continues the last row's floor toward the viewer, so
    // it is lit a touch more and seamed horizontally; the vertical grid that used
    // to be here read as an empty tiled void rather than as a walkway, and the
    // plants standing on it read as a garden centre. Both are gone. A cable
    // trunk runs along the base, which is what an open-plan floor actually has.
    let fy = h - FORE_H;
    b.fill(0, fy, w, FORE_H, FLOOR);
    b.fill(0, fy, w, 1, TRIM);
    b.fill(0, fy + 1, w, 2, FLOOR_LINE.alpha(0.5));
    b.fill(0, fy + 8, w, 1, FLOOR_LINE.alpha(0.35));
    b.fill(0, h - 5, w, 4, rgb(0x1a, 0x21, 0x2a));
    b.fill(0, h - 5, w, 1, TRIM);
    let mut x = 6;
    while x < w {
        b.fill(x, h - 4, 3, 2, METAL_DK);
        x += 46;
    }

    // A sunny day spills past the sill. It stops above the desks on purpose:
    // reading an agent's state must never depend on the weather.
    if scene.weather == Weather::Sunny {
        b.fill(0, CEIL_H - 8, W, 6, SUNLIGHT.alpha(0.07));
        b.fill_checker(0, CEIL_H - 4, W, 8, SUNLIGHT.alpha(0.08));
        b.fill_checker(0, CEIL_H + 4, W, 8, SUNLIGHT.alpha(0.04));
    }
}

/// The top band: three windows onto the weather, with the ceiling lights slung
/// under them. Drawn once however many rows the room has.
fn clerestory(b: &mut Buf, weather: Weather, t: f32) {
    b.fill(0, 0, W, CEIL_H, CEIL);
    for i in 0..3 {
        window(b, 26 + i * 118, 6, 92, 30, weather, t, i as i64);
    }
    // Sill under the glass, then the light fixtures.
    b.fill(0, 36, W, 2, TRIM);
    trailing(b, 72, 38, 0x11);
    trailing(b, 186, 38, 0x22);
    trailing(b, 296, 38, 0x33);
    b.fill(0, CEIL_H - 3, W, 2, TRIM);
    for c in 0..COLS {
        let cx = c as i32 * SLOT_W + SLOT_W / 2;
        b.fill(cx - 14, 40, 28, 2, STEEL.alpha(0.8));
        b.fill(cx - 16, 42, 32, 4, STEEL.alpha(0.06));
    }
}

fn window(b: &mut Buf, x: i32, y: i32, w: i32, h: i32, weather: Weather, t: f32, seed: i64) {
    b.fill(x - 2, y - 2, w + 4, h + 4, METAL_DK);
    b.fill(x - 1, y - 1, w + 2, h + 2, METAL);

    // Sky, as four horizontal steps rather than a gradient: banding is honest at
    // this resolution and a smooth ramp is not available in a 12-colour room.
    let (top, bottom) = weather.sky();
    let steps = 4;
    for s in 0..steps {
        let f = s as f32 / (steps - 1) as f32;
        let c = rgb(
            (top.0 as f32 + (bottom.0 as f32 - top.0 as f32) * f) as u8,
            (top.1 as f32 + (bottom.1 as f32 - top.1 as f32) * f) as u8,
            (top.2 as f32 + (bottom.2 as f32 - top.2 as f32) * f) as u8,
        );
        b.fill(x, y + s * h / steps, w, h / steps + 1, c);
    }

    match weather {
        Weather::Clear => {
            // A few stars, fixed to the glass so they never twinkle. Twinkling
            // is motion, and motion in this room has to mean something.
            for i in 0..7 {
                let sx = x + 3 + (hash(seed, i) % (w as u32 - 6)) as i32;
                let sy = y + 2 + (hash(seed, i + 40) % (h as u32 / 2)) as i32;
                b.fill(sx, sy, 1, 1, PAPER.alpha(0.5));
            }
        }
        Weather::Sunny => {
            // The sun sits in one pane only, so the three windows read as three
            // views of one sky rather than three copies of a poster.
            if seed == 1 {
                let (dx, dy) = (x + w / 2 + 14, y + 8);
                b.fill_checker(dx - 6, dy - 6, 13, 13, SUNLIGHT.alpha(0.3));
                b.fill(dx - 2, dy - 3, 5, 9, SUNLIGHT);
                b.fill(dx - 3, dy - 2, 7, 7, SUNLIGHT);
            }
            for i in 0..2 {
                let cy = y + 4 + (hash(seed, i + 7) % (h as u32 / 3)) as i32;
                let cw = 18 + (hash(seed, i + 11) % 20) as i32;
                let cx = x + (hash(seed, i + 13) % (w as u32 - 20)) as i32;
                b.fill(cx, cy, cw.min(x + w - cx), 3, SUNLIGHT.alpha(0.5));
                b.fill(
                    cx + 3,
                    cy - 2,
                    (cw - 8).max(4).min(x + w - cx - 3),
                    2,
                    SUNLIGHT.alpha(0.3),
                );
            }
        }
        Weather::Cloudy => {
            for i in 0..3 {
                let cy = y + 3 + (hash(seed, i + 7) % (h as u32 / 2)) as i32;
                let cw = 22 + (hash(seed, i + 11) % 26) as i32;
                let cx = x + (hash(seed, i + 13) % (w as u32 - 20)) as i32;
                b.fill(cx, cy, cw.min(x + w - cx), 4, STEEL.alpha(0.16));
                b.fill(
                    cx + 4,
                    cy - 2,
                    (cw - 10).max(4).min(x + w - cx - 4),
                    3,
                    STEEL.alpha(0.1),
                );
            }
        }
        Weather::Fog => {
            for i in 0..4 {
                let fy = y + 2 + i * (h / 5);
                b.fill_checker(x, fy, w, 3, STEEL.alpha(0.22));
            }
        }
        Weather::Rain => {
            for i in 0..22 {
                let rx = x + 1 + (hash(seed, i) % (w as u32 - 2)) as i32;
                let speed = 46.0 + hashf(seed, i + 60) * 30.0;
                let ry = y + ((t * speed + hashf(seed, i + 9) * h as f32 * 4.0) % h as f32) as i32;
                b.fill(rx, ry, 1, 3, STEEL.alpha(0.7));
            }
        }
        Weather::Snow => {
            for i in 0..18 {
                let speed = 7.0 + hashf(seed, i + 70) * 6.0;
                let drift = ((t * 0.7 + hashf(seed, i + 3) * 6.0).sin() * 3.0) as i32;
                let sx = x + 1 + (hash(seed, i) % (w as u32 - 2)) as i32 + drift;
                let sy = y + ((t * speed + hashf(seed, i + 5) * h as f32 * 3.0) % h as f32) as i32;
                b.fill(sx.clamp(x, x + w - 1), sy, 1, 1, PAPER.alpha(0.8));
            }
        }
    }

    // Distant city along the bottom of the glass, then the mullions over it all.
    for i in 0..9 {
        let bw = 6 + (hash(seed, i + 21) % 10) as i32;
        let bh = 4 + (hash(seed, i + 23) % 9) as i32;
        let bx = x + (i as i32 * w / 9);
        b.fill(
            bx,
            y + h - bh,
            bw.min(x + w - bx),
            bh,
            rgb(0x10, 0x16, 0x1e),
        );
        if hash(seed, i + 31) % 3 == 0 {
            b.fill(bx + 2, y + h - bh + 2, 1, 1, STEEL.alpha(0.7));
        }
    }
    let mut m = x + w / 3;
    while m < x + w {
        b.fill(m, y, 1, h, METAL_DK);
        m += w / 3;
    }
    b.fill(x, y + h / 2, w, 1, METAL_DK.alpha(0.6));
}

/// Claude's own spinner at 7×7. The real thing cycles ✳ ✶ ✻, which at this size
/// is exactly a "+" and an "×" trading places through a full eight-point star,
/// so that is what these three frames are. Rows are bitmasks, bit 6 leftmost.
const SPINNER: [[u8; 7]; 3] = [
    [0x08, 0x08, 0x08, 0x7F, 0x08, 0x08, 0x08],
    [0x49, 0x2A, 0x1C, 0x7F, 0x1C, 0x2A, 0x49],
    [0x41, 0x22, 0x14, 0x08, 0x14, 0x22, 0x41],
];
/// Out and back, so the star reads as turning rather than flicking between two
/// poses. Four steps at 8fps is one revolution every half second.
const SPINNER_ORDER: [usize; 4] = [0, 1, 2, 1];

fn spinner(b: &mut Buf, x: i32, y: i32, c: Rgba, frame: i64) {
    let glyph = &SPINNER[SPINNER_ORDER[frame.rem_euclid(4) as usize]];
    for (row, bits) in glyph.iter().enumerate() {
        for col in 0..7 {
            if bits & (0x40 >> col) != 0 {
                b.fill(x + col, y + row as i32, 1, 1, c);
            }
        }
    }
}

/// Output accumulating on a screen. Widths come from the scroll position rather
/// than from the line's place on screen, so lines march off the top instead of
/// flickering where they stand.
fn out_lines(b: &mut Buf, x: i32, y: i32, w: i32, n: i32, c: Rgba, scroll: i64) {
    for k in 0..n as i64 {
        let lw = 4 + (hash(scroll + k, 0x1d) % (w as u32).max(2)) as i32;
        b.fill(x, y + k as i32 * 3, lw, 1, c);
    }
}

/// The monitor face. The screen carries more state than anything else in the
/// room, which is the whole reason it is turned toward the camera while the
/// figure beside it stays in profile.
fn screen(b: &mut Buf, sx: i32, sy: i32, sw: i32, sh: i32, d: Desk, t: f32, seed: i64) {
    b.fill(sx, sy, sw, sh, rgb(0x0b, 0x10, 0x16));
    let Some(g) = d.glow() else { return };
    b.fill(sx, sy, sw, sh, g.alpha(0.1));
    let body = METAL_LT.alpha(0.3);
    match d {
        Desk::Working => {
            spinner(b, sx + (sw - 7) / 2, sy + 3, g, (t * 8.0) as i64 + seed);
            out_lines(
                b,
                sx + 2,
                sy + 13,
                sw - 8,
                3,
                g.alpha(0.4),
                (t * 2.5) as i64 + seed * 5,
            );
        }
        Desk::Running => out_lines(
            b,
            sx + 2,
            sy + 3,
            sw - 8,
            7,
            body,
            (t * 3.5) as i64 + seed * 5,
        ),
        Desk::Ready => {
            out_lines(b, sx + 2, sy + 4, sw - 8, 3, body, 7);
            b.fill(sx + 2, sy + 16, 3, 2, METAL_LT.alpha(0.6));
        }
        Desk::Idle => b.fill(sx + 2, sy + 4, 3, 2, METAL_LT.alpha(0.5)),
        Desk::Attention => {
            // A choice waiting on you: two options, the first one under the
            // cursor. Static — the paddle above the monitor does the moving.
            out_lines(b, sx + 2, sy + 3, sw - 8, 2, body, 3);
            b.fill(sx + 2, sy + 12, sw - 4, 4, g);
            b.fill(sx + 2, sy + 18, sw - 4, 3, g.alpha(0.35));
        }
        Desk::Done => {
            out_lines(b, sx + 2, sy + 4, sw - 8, 4, body, 11);
            b.fill(sx + 2, sy + 17, 9, 2, g);
        }
        Desk::Empty => {}
    }
}

/// A pot on the clerestory sill with its growth hanging down the wall. The only
/// greenery that is not on a desk: the floor is kept clear so the foreground
/// reads as a walkway rather than as a shelf of pot plants.
fn trailing(b: &mut Buf, x: i32, y: i32, seed: i64) {
    b.fill(x, y, 13, 4, POT);
    b.fill(x, y, 13, 1, POT_DK);
    for i in 0..6i64 {
        let len = 9 + (hash(seed, i) % 16) as i32;
        let c = if i % 2 == 0 { PLANT } else { PLANT_LT };
        b.fill(x + 1 + i as i32 * 2, y + 4, 1, len, c);
        b.fill(x + 1 + i as i32 * 2, y + 4 + len - 2, 2, 3, c);
    }
}

/// A plant for the end of a desk, in four sizes. Which one a desk gets is fixed
/// by its slot, so the clutter on a desk is as stable as the desk's position and
/// a row of five reads as five people rather than one stamped five times.
/// `base` is the desk surface; the plant grows up from it.
fn desk_plant(b: &mut Buf, x: i32, base: i32, variant: u32, seed: i64) {
    let green = |k: i64| [PLANT_DK, PLANT, PLANT_LT, PLANT_HI][(hash(seed, k) % 4) as usize];
    match variant {
        // A succulent barely taller than the mug.
        0 => {
            b.fill(x + 1, base - 3, 5, 3, POT);
            b.fill(x + 1, base - 4, 5, 1, POT_DK);
            b.fill(x + 1, base - 6, 5, 2, PLANT);
            b.fill(x + 2, base - 7, 3, 1, PLANT_LT);
        }
        // A squat bushy thing.
        1 => {
            b.fill(x, base - 5, 7, 5, POT);
            b.fill(x, base - 6, 7, 1, POT_DK);
            for k in 0..3i64 {
                let h = 4 + (hash(seed, k) % 4) as i32;
                b.fill(x + 1 + k as i32 * 2, base - 6 - h, 2, h, green(k));
            }
            b.fill(x + 1, base - 8, 5, 2, green(9));
        }
        // Tall fronds, the one that reads from across the room.
        2 => {
            b.fill(x + 1, base - 7, 5, 7, POT);
            b.fill(x + 1, base - 8, 5, 1, POT_DK);
            for k in 0..3i64 {
                let h = 7 + (hash(seed, k) % 6) as i32;
                b.fill(x + 1 + k as i32 * 2, base - 8 - h, 1, h, green(k));
                b.fill(x + k as i32 * 2, base - 9 - h, 3, 2, green(k + 4));
            }
        }
        // Growth spilling over the desk edge.
        _ => {
            b.fill(x, base - 5, 7, 5, POT);
            b.fill(x, base - 6, 7, 1, POT_DK);
            b.fill(x + 1, base - 8, 5, 2, green(1));
            b.fill(x + 2, base - 9, 3, 1, green(2));
            for k in 0..3i64 {
                let len = 3 + (hash(seed, k + 5) % 5) as i32;
                b.fill(x + 5 + k as i32, base - 4 + k as i32, 1, len, green(k + 6));
            }
        }
    }
}

// --- one workstation, in row-relative pixels ---------------------------------
// The figure is seated, and every part of that has to be visible or it reads as
// somebody standing at a desk (which is exactly how the first version read).
// So: the hip meets the seat, the thigh runs forward under the desktop, the
// lower leg drops to the floor, and the desk is a cantilever with its pedestal
// on the far side, leaving the legs and the chair post in open view.
const HAIR_Y: i32 = 30;
const HEAD_Y: i32 = 32;
const SHOULDER_Y: i32 = 43;
const HIP_Y: i32 = 62;
const SEAT_Y: i32 = 62;
const KNEE_Y: i32 = 68;
const DESK_Y: i32 = 56;
const KB_Y: i32 = 52;
const FLOOR_Y: i32 = 80;
const MON_TOP: i32 = 20;
const MON_BOT: i32 = 49;

/// One workstation. Everything is positioned from the slot's centre `cx` and the
/// row band top `r`, so a desk is identical wherever it lands.
fn station(b: &mut Buf, i: usize, d: Desk, selected: bool, t: f32) {
    let (cx, r) = slot_origin(i);
    let shirt = SHIRTS[i % COLS];
    let trews = rgb(0x39, 0x42, 0x4d);
    // One step down from the shirt, to seam the arm off the torso it grows out
    // of; at the same value the two merged into a single slab.
    let shirt_dk = Rgba(
        (shirt.0 as f32 * 0.72) as u8,
        (shirt.1 as f32 * 0.72) as u8,
        (shirt.2 as f32 * 0.72) as u8,
        1.0,
    );
    let glow = d.glow();
    let occupied = d != Desk::Empty;
    // 12fps sprite cadence: smooth motion reads as an animation, chunky motion
    // reads as a machine.
    let frame = (t * 12.0) as i64;
    let job = (d == Desk::Working).then(|| task(i, t));
    // Idle, done and pondering slump back into the chair. Posture is a free
    // state channel and it costs no colour. Only the upper body leans: the legs
    // stay where they are, which is what makes the lean read as a lean.
    let lean = i32::from(
        matches!(d, Desk::Idle | Desk::Done | Desk::Ready) || job == Some(Task::Pondering),
    ) - i32::from(job == Some(Task::Reading) || job == Some(Task::Noting));

    // Light spill. Small on purpose: the glow says which state, the paddle says
    // "act now". An attention state that floods the desk spends the whole
    // loudness budget in one place.
    if let Some(g) = glow {
        let b0 = match d {
            Desk::Working => 0.10 + 0.03 * (t * 9.0).sin(),
            Desk::Attention => 0.07 + 0.03 * (t * 1.9).sin(),
            _ => 0.04,
        };
        b.fill_checker(cx - 18, r + 14, 50, 44, g.alpha(b0 * 0.55));
        b.fill(cx - 8, r + 18, 38, 36, g.alpha(b0 * 0.7));
        b.fill(cx, r + 20, 28, 30, g.alpha(b0));
    }

    // Task chair, drawn behind the figure. An unoccupied one is pushed in under
    // the desk, which is how a free desk reads as free rather than as somebody
    // who has gone very still.
    let ch = cx - 31 + if occupied { 0 } else { 13 };
    b.fill(ch, r + 40 + lean, 5, SEAT_Y - 40, CHAIR);
    b.fill(ch + 1, r + 42 + lean, 3, SEAT_Y - 44, CHAIR_LT);
    b.fill(ch, r + SEAT_Y, 20, 3, CHAIR_LT);
    b.fill(ch, r + SEAT_Y + 3, 20, 2, CHAIR);
    b.fill(ch + 7, r + SEAT_Y + 5, 3, 11, METAL_DK);
    b.fill(ch + 1, r + FLOOR_Y - 4, 17, 2, METAL_DK);
    b.fill(ch, r + FLOOR_Y - 2, 3, 2, METAL);
    b.fill(ch + 16, r + FLOOR_Y - 2, 3, 2, METAL);

    if occupied {
        // Head, in profile while working; turned to camera, with two eyes, when
        // the agent is blocked. The turn is a state change, not a personality.
        b.fill(cx - 24 + lean, r + HEAD_Y, 8, 10, SKIN);
        b.fill(cx - 25 + lean, r + HAIR_Y, 10, 5, HAIR);
        b.fill(cx - 25 + lean, r + HEAD_Y + 3, 2, 5, HAIR);
        if d == Desk::Attention {
            b.fill(cx - 21, r + HEAD_Y + 5, 1, 1, HAIR);
            b.fill(cx - 18, r + HEAD_Y + 5, 1, 1, HAIR);
        } else {
            b.fill(cx - 18 + lean, r + HEAD_Y + 5, 1, 1, HAIR);
        }

        // Shoulders, then a torso of about two head-heights down to the hip. Any
        // longer and the figure reads as standing behind the desk, which is
        // exactly how the first attempt at this read.
        let torso = HIP_Y - SHOULDER_Y;
        b.fill(cx - 26 + lean, r + SHOULDER_Y, 13, 4, shirt);
        b.fill(
            cx - 17 + lean,
            r + SHOULDER_Y + 2,
            2,
            HIP_Y - SHOULDER_Y - 4,
            shirt_dk,
        );
        b.fill(cx - 25 + lean, r + SHOULDER_Y, 11, torso, shirt);
        b.fill(cx - 26 + lean, r + SHOULDER_Y + 3, 2, torso - 7, shirt);

        // Seated legs: thigh forward under the desktop, shin down to the floor.
        // The thigh is darker than the seat slab beneath it and carries a lit top
        // edge, which is the only reason the two read as separate objects here.
        let trews_lit = rgb(0x4c, 0x57, 0x64);
        let shin = FLOOR_Y - 3 - KNEE_Y;
        b.fill(cx - 24, r + HIP_Y, 17, KNEE_Y - HIP_Y - 1, trews);
        b.fill(cx - 24, r + HIP_Y, 17, 1, trews_lit);
        b.fill(cx - 24, r + KNEE_Y - 1, 17, 1, rgb(0x24, 0x2a, 0x33));
        b.fill(cx - 12, r + KNEE_Y, 5, shin, trews);
        b.fill(cx - 12, r + KNEE_Y, 1, shin, trews_lit);
        b.fill(cx - 13, r + FLOOR_Y - 3, 10, 3, rgb(0x1b, 0x1f, 0x25));
        b.fill(cx - 13, r + FLOOR_Y - 3, 10, 1, rgb(0x2b, 0x31, 0x39));

        // Arms. This is where the work variety lives: every pose below means
        // exactly Working, so none may introduce a colour or a motion the other
        // states do not already have.
        match (d, job) {
            (Desk::Working, Some(Task::Typing)) | (Desk::Running, _) => {
                let k = (frame % 2) as i32;
                b.fill(cx - 16, r + 46, 10, 4, shirt);
                b.fill(cx - 7, r + 48 + k, 8, 3, SKIN);
                b.fill(cx - 16, r + 51, 9, 4, shirt);
                b.fill(cx - 8, r + 52 - k, 8, 3, SKIN);
            }
            // Hands in the lap, head in close.
            (Desk::Working, Some(Task::Reading)) => {
                b.fill(cx - 17, r + 48, 8, 4, shirt);
                b.fill(cx - 13, r + 52, 6, 4, SKIN);
            }
            // One hand on the keys, the other writing. The pen is the only thing
            // that moves, at a third the rate of typing.
            (Desk::Working, Some(Task::Noting)) => {
                let k = ((frame / 3) % 3) as i32;
                b.fill(cx - 16, r + 46, 10, 4, shirt);
                b.fill(cx - 7, r + 49, 8, 3, SKIN);
                b.fill(cx - 17, r + 52, 7, 4, shirt);
                b.fill(cx - 11 + k, r + 54, 4, 3, SKIN);
            }
            // Back in the chair, hand to chin.
            (Desk::Working, Some(Task::Pondering)) => {
                b.fill(cx - 17 + lean, r + 47, 4, 10, shirt);
                b.fill(cx - 18 + lean, r + 41, 4, 7, SKIN);
            }
            // Mug up, which is why there is no mug on the desk this phase.
            (Desk::Working, Some(Task::Sipping)) => {
                b.fill(cx - 17, r + 45, 4, 11, shirt);
                b.fill(cx - 18, r + 41, 5, 5, SKIN);
                b.fill(cx - 19, r + 38, 5, 5, CERAMIC);
            }
            // Hands off the keyboard: the agent has stopped and is waiting.
            (Desk::Attention, _) => {
                b.fill(cx - 16, r + 48, 7, 4, shirt);
                b.fill(cx - 10, r + 42, 4, 11, SKIN);
            }
            _ => {
                b.fill(cx - 16 + lean, r + 49, 9, 4, shirt);
                b.fill(cx - 8 + lean, r + 52, 6, 3, SKIN);
            }
        }
    }

    // Desk: a cantilever top with its pedestal on the far side, so the near side
    // stays open and the seated legs are actually visible.
    b.fill(cx - 16, r + DESK_Y, 52, 3, DESK_LT);
    b.fill(cx - 16, r + DESK_Y + 3, 52, 2, rgb(0x24, 0x1e, 0x18));
    b.fill(cx + 22, r + DESK_Y + 5, 14, FLOOR_Y - DESK_Y - 5, DESK);
    b.fill(cx + 22, r + DESK_Y + 5, 14, 1, DESK_LT);
    b.fill(cx + 24, r + DESK_Y + 11, 10, 1, POT_DK);
    b.fill(cx + 24, r + DESK_Y + 18, 10, 1, POT_DK);
    b.fill(cx - 16, r + FLOOR_Y, 52, 2, Rgba(0, 0, 0, 0.3));

    // Monitor. The screen faces the camera while the figure beside it stays in
    // profile: the standard side-view cheat, and the only way the screen can
    // carry state at all.
    b.fill(cx + 13, r + MON_BOT, 6, 5, METAL_DK);
    b.fill(cx + 8, r + DESK_Y - 2, 16, 2, METAL_DK);
    b.fill(cx + 3, r + MON_TOP, 26, MON_BOT - MON_TOP, METAL);
    b.fill(cx + 3, r + MON_TOP, 26, 1, METAL_LT);
    screen(b, cx + 5, r + MON_TOP + 2, 22, 25, d, t, i as i64);

    if occupied {
        // Desk light: a bar clipped over the top of the monitor, throwing light
        // down onto the desk the way the real thing does. Warm, but deliberately
        // desaturated: a saturated warm glow at this size reads as ATTENTION,
        // and the room depends on that colour meaning exactly one thing.
        b.fill(cx + 14, r + MON_TOP - 4, 4, 5, METAL_DK);
        b.fill(cx + 7, r + MON_TOP - 7, 18, 3, METAL);
        b.fill(cx + 7, r + MON_TOP - 7, 18, 1, METAL_LT);
        b.fill(cx + 8, r + MON_TOP - 4, 16, 1, LAMP_WARM.alpha(0.9));
        b.fill_checker(cx + 4, r + KB_Y - 4, 24, 8, LAMP_WARM.alpha(0.1));
        b.fill(cx + 7, r + KB_Y, 18, 4, LAMP_WARM.alpha(0.07));
    }

    // Keyboard.
    b.fill(cx - 8, r + KB_Y, 15, 4, METAL_DK);
    b.fill(cx - 7, r + KB_Y + 1, 13, 2, rgb(0x32, 0x3a, 0x42));

    if occupied {
        // Every desk keeps a plant; which one is fixed by slot.
        desk_plant(
            b,
            cx + 29,
            r + DESK_Y,
            hash(i as i64, 0x91) % 4,
            i as i64 * 7 + 3,
        );

        // The mug and the out-tray share the near end of the desk, which they
        // can because Working and Done never happen at the same desk at once.
        if d == Desk::Working && job != Some(Task::Sipping) {
            b.fill(cx - 13, r + DESK_Y - 6, 5, 6, CERAMIC);
            b.fill(cx - 8, r + DESK_Y - 5, 2, 3, CERAMIC);
        }
        if d == Desk::Done {
            b.fill(cx - 14, r + DESK_Y - 4, 10, 4, METAL_DK);
            for k in 0..3 {
                b.fill(cx - 13, r + DESK_Y - 6 - k, 8, 1, PAPER);
            }
        }

        // The one signal object: a paddle that rises clear of the monitor only
        // when the agent is blocked on you, breathing at well under 1Hz. Nothing
        // else in the room is allowed to move like this.
        if d == Desk::Attention {
            let rise = ((t * 3.0).min(1.0) * 8.0) as i32;
            let y = r + 14 - rise;
            b.fill(cx + 20, y + 6, 2, r + MON_TOP - 7 - (y + 6), METAL_DK);
            let pulse = 0.75 + 0.25 * (t * 2.2).sin();
            b.fill(cx + 16, y, 10, 7, ATTENTION.alpha(pulse));
            b.fill(cx + 18, y + 3, 2, 1, rgb(0x3a, 0x2c, 0x10));
            b.fill(cx + 22, y + 3, 2, 1, rgb(0x3a, 0x2c, 0x10));
        }
    }

    // Selection is a floor marker, not a flashing outline: it must not compete
    // with the attention signal, and it may not borrow a reserved colour.
    if selected {
        let fy = r + ROW_H - 12;
        b.fill(cx - 34, fy, SLOT_W - 8, 1, PAPER.alpha(0.7));
        b.fill(cx - 34, fy, 1, 4, PAPER.alpha(0.7));
        b.fill(cx + 33, fy, 1, 4, PAPER.alpha(0.7));
        b.fill(cx - 34, r + 10, SLOT_W - 8, ROW_H - 22, PAPER.alpha(0.03));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(states: &[Desk]) -> Scene {
        let mut s = Scene::default();
        for d in states {
            s.spawn(*d);
        }
        s
    }

    #[test]
    fn a_row_holds_five_then_the_room_grows_downward() {
        assert_eq!(rows(0), 1, "an empty room is still a room");
        assert_eq!(rows(5), 1);
        assert_eq!(rows(6), 2);
        assert_eq!(rows(20), 4);
        assert_eq!(height(5) - height(0), 0);
        assert_eq!(
            height(6) - height(5),
            ROW_H,
            "row six costs exactly one row"
        );
    }

    #[test]
    fn a_desk_never_moves() {
        // Slot 7 is row 1, column 2, and stays there however many desks exist.
        let (x, y) = slot_origin(7);
        assert_eq!(x, 2 * SLOT_W + SLOT_W / 2);
        assert_eq!(y, CEIL_H + ROW_H);
        for n in 8..40 {
            let mut s = Scene::default();
            for _ in 0..n {
                s.spawn(Desk::Working);
            }
            assert_eq!(slot_origin(7), (x, y), "slot 7 moved with {n} desks");
        }
    }

    #[test]
    fn spawning_past_a_full_row_opens_another() {
        let mut s = Scene::default();
        for i in 0..COLS {
            assert_eq!(s.spawn(Desk::Working), i);
        }
        assert_eq!(s.desks.len(), COLS);
        assert_eq!(
            s.spawn(Desk::Working),
            COLS,
            "the sixth desk starts row two"
        );
        assert_eq!(s.desks.len(), COLS * 2);
        assert_eq!(
            s.count(Desk::Empty),
            COLS - 1,
            "the new row is otherwise free"
        );
    }

    #[test]
    fn removing_the_last_desk_collapses_the_row() {
        let mut s = scene(&[Desk::Working; 6]);
        assert_eq!(s.desks.len(), COLS * 2);
        s.remove_last();
        assert_eq!(s.desks.len(), COLS, "the empty row went away");
        for _ in 0..COLS {
            s.remove_last();
        }
        assert_eq!(s.desks.len(), COLS, "the first row is never removed");
        assert_eq!(s.occupied(), 0);
    }

    #[test]
    fn hit_testing_ignores_the_clerestory_and_foreground() {
        let s = scene(&[Desk::Working; 6]);
        let n = s.desks.len();
        assert_eq!(hit(SLOT_W / 2, CEIL_H + 10, n), Some(0));
        assert_eq!(
            hit(SLOT_W / 2, CEIL_H + ROW_H + 10, n),
            Some(COLS),
            "row two"
        );
        assert_eq!(hit(W - 1, CEIL_H + 10, n), Some(COLS - 1));
        assert_eq!(hit(10, 20, n), None, "the weather is not a desk");
        assert_eq!(
            hit(10, height(n) - 4, n),
            None,
            "the foreground is not a desk"
        );
        assert_eq!(hit(-1, CEIL_H + 10, n), None);
    }

    /// The load-bearing rule. If furniture, weather or a work pose ever borrows a
    /// semantic colour, the eye stops trusting it and the loudness budget is gone.
    #[test]
    fn reserved_colours() {
        // Odd rows escape the scanline, so a reserved colour that is painted at
        // full strength survives into the buffer exactly.
        let has = |s: &Scene, t: f32, c: Rgba| {
            let b = render(s, t);
            b.px.chunks_exact(4)
                .any(|p| p[0] == c.0 && p[1] == c.1 && p[2] == c.2)
        };

        // Every sky, every quiet state, the greenery and a selection: none of it
        // may produce a colour that means "this desk is doing something".
        for w in Weather::ALL {
            let mut quiet = scene(&[Desk::Idle, Desk::Ready, Desk::Running]);
            quiet.weather = w;
            quiet.selected = Some(0); // selection must not borrow a reserved colour
            for step in 0..40 {
                let t = step as f32 * 0.9;
                for (c, name) in [(ATTENTION, "amber"), (WORKING, "azure"), (DONE, "green")] {
                    assert!(
                        !has(&quiet, t, c),
                        "{name} in a {} room at t={t}",
                        w.label()
                    );
                }
            }
        }

        // And each one must actually appear for the state it belongs to.
        assert!(
            has(&scene(&[Desk::Attention]), 0.5, ATTENTION),
            "a blocked agent must show"
        );
        assert!(
            has(&scene(&[Desk::Done]), 0.5, DONE),
            "a finished turn must show"
        );
        // Every task must still read as working, whichever pose it picked, and
        // the spinner must be on screen at every point in its cycle.
        let busy = scene(&[Desk::Working]);
        for step in 0..60 {
            let t = step as f32 * 0.7;
            assert!(
                has(&busy, t, WORKING),
                "working desk lost its glow at t={t}"
            );
        }
    }

    #[test]
    fn a_quiet_room_needs_no_clock() {
        assert!(!scene(&[Desk::Idle, Desk::Ready, Desk::Done]).animates());
        assert!(scene(&[Desk::Idle, Desk::Working]).animates());
        assert!(scene(&[Desk::Attention]).animates());
        assert!(
            !Scene::default().animates(),
            "an empty room under a clear sky is a still image"
        );

        // Precipitation is the one bit of decoration that costs a clock, which is
        // a deliberate trade and must stay visible in the type.
        for w in Weather::ALL {
            let mut s = Scene::default();
            s.weather = w;
            assert_eq!(
                s.animates(),
                w.moves(),
                "{} disagreed with Weather::moves",
                w.label()
            );
        }
        assert!(Weather::Rain.moves() && Weather::Snow.moves());
        assert!(!Weather::Clear.moves() && !Weather::Cloudy.moves() && !Weather::Fog.moves());
    }

    #[test]
    fn desks_vary_their_work_without_moving_in_unison() {
        // Every task is reachable, so none of the poses is dead code.
        let mut seen = std::collections::HashSet::new();
        for step in 0..400 {
            seen.insert(task(0, step as f32 * 1.7));
        }
        assert_eq!(seen.len(), 5, "some task never comes up: {seen:?}");

        // Deterministic: the same slot and time always give the same task.
        assert_eq!(task(3, 12.25), task(3, 12.25));

        // Typing dominates, or the room stops looking like work is happening.
        let typing = (0..600)
            .filter(|s| task(1, *s as f32 * 0.9) == Task::Typing)
            .count() as f32
            / 600.0;
        assert!(
            (0.3..0.55).contains(&typing),
            "typing share drifted to {typing}"
        );

        // Desks are phase-offset, so at some moment they are doing different
        // things. Five identical sprites was the thing this set out to fix.
        let varied = (0..200).any(|step| {
            let t = step as f32 * 0.6;
            (0..COLS)
                .map(|i| task(i, t))
                .collect::<std::collections::HashSet<_>>()
                .len()
                >= 3
        });
        assert!(varied, "every desk switched task in lockstep");
    }

    #[test]
    fn the_weather_drifts_over_a_long_session() {
        let w = |n: i64| Weather::drifting(n as f32 * WEATHER_SECS + 1.0);
        let seen: std::collections::HashSet<_> = (0..120).map(w).collect();
        assert_eq!(
            seen.len(),
            Weather::ALL.len(),
            "some weather never happens: {seen:?}"
        );
        // It has to actually change. A sky stuck on one value for half an hour
        // is the failure this is here to catch.
        let changes = (1..80).filter(|n| w(*n) != w(n - 1)).count();
        assert!(
            changes > 40,
            "the sky changed only {changes} times in 80 phases"
        );
        assert_eq!(
            Weather::drifting(3.0),
            Weather::drifting(3.0),
            "must be reproducible"
        );
    }

    #[test]
    fn the_buffer_is_sized_and_opaque() {
        let s = scene(&[Desk::Working; 7]);
        let (w, h) = s.size();
        let b = render(&s, 1.0);
        assert_eq!((b.w, b.h), (w, h));
        assert_eq!(b.px.len(), (w * h * 4) as usize);
        assert!(
            b.px.chunks_exact(4).all(|p| p[3] == 255),
            "transparent pixel in the room"
        );
    }

    #[test]
    fn drawing_stays_inside_the_buffer() {
        // Every station is drawn from its own origin, so the last slot in a row
        // is the one that would overflow if an offset were wrong. Walk time too,
        // so no task pose or raindrop reaches past an edge.
        for n in [1usize, 5, 6, 11, 20] {
            for w in Weather::ALL {
                let mut s = Scene::default();
                for _ in 0..n {
                    s.spawn(Desk::Working);
                }
                s.weather = w;
                s.selected = Some(n - 1);
                for step in 0..30 {
                    let _ = render(&s, step as f32 * 1.3); // panics on a bad index
                }
            }
        }
    }

    /// Eyeball the room without launching a GUI. Writes the raw buffer so a
    /// one-liner can turn it into an image:
    /// `cargo test --lib floor::tests::dump -- --ignored --nocapture`
    #[test]
    #[ignore = "writes a file; run it when you want to look at the room"]
    fn dump() {
        let mut s = Scene::default();
        for d in [Desk::Working; 5] {
            s.spawn(d);
        }
        for d in [Desk::Attention, Desk::Done, Desk::Idle, Desk::Ready] {
            s.spawn(d);
        }
        s.weather = std::env::var("FLOOR_WEATHER")
            .ok()
            .and_then(|v| Weather::ALL.into_iter().find(|w| w.label() == v))
            .unwrap_or(Weather::Rain);
        s.selected = Some(5);
        let t = std::env::var("FLOOR_T")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.5);
        let b = render(&s, t);
        let path = std::env::temp_dir().join(format!("ops-floor-{}x{}.rgba", b.w, b.h));
        std::fs::write(&path, &b.px).unwrap();
        println!("{}", path.display());
    }
}
