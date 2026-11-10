//! The agent banner: a strip of pixel art for the top of a terminal, in which a
//! robot works outdoors while its agent does.
//!
//! A pure function from a `Scene`, a width and a time to a straight-RGBA buffer,
//! drawn with rectangle fills only: no asset files, no stored state, no RNG. What
//! varies (which job a robot is on, where it walks next, where a cloud is) comes
//! from `hash`, so any frame can be reproduced.
//!
//! The strip is `H` art pixels tall at any width. The world is anchored on the main
//! robot's home at the centre column, so a wider pane shows more land on both sides
//! and a resize never moves the scenery that is already in view.

// Straight (non-premultiplied) RGBA, which is what `iced::widget::image` wants.
// Alpha is a blend weight on the colour; the buffer itself is always opaque.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub f32);

const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    Rgba(r, g, b, 1.0)
}

impl Rgba {
    fn a(self, alpha: f32) -> Rgba {
        Rgba(self.0, self.1, self.2, alpha)
    }
}

fn mix(a: Rgba, b: Rgba, k: f32) -> Rgba {
    let k = k.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
    Rgba(f(a.0, b.0), f(a.1, b.1), f(a.2, b.2), 1.0)
}

fn hash(a: i64, b: i64) -> u32 {
    let mut x = (a as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((b as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9));
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 29;
    (x >> 32) as u32
}

fn hashf(a: i64, b: i64) -> f32 {
    hash(a, b) as f32 / u32::MAX as f32
}

// Height of the strip, in art pixels.
pub const H: i32 = 46;
// Narrowest strip worth drawing, in art pixels: the robot, a tree and the near
// shore of the lake. A caller with less room hides the banner instead.
pub const MIN_W: i32 = 80;
// Helper robots drawn at most; a wider strip is needed to fit them all, see
// `helpers_shown`.
pub const MAX_HELPERS: usize = 6;
// Scene seconds for one day and night.
pub const DAY_SECS: f32 = 240.0;
// Scene seconds one kind of weather lasts under `Weather::drifting`.
pub const WEATHER_SECS: f32 = 40.0;

const HORIZON: i32 = 30;
// First row below the robots' feet.
const FEET: i32 = 41;
// The lake the idle robot fishes in, in world x (0 is the main robot's home).
const LAKE_X: i32 = 43;
const LAKE_RX: i32 = 18;
// Row of the lake's surface the fishing float sits on.
const LAKE_Y: i32 = HORIZON + 4;
// World width of one cell of scenery away from the centre.
const CELL: i32 = 56;
// Room a robot at the edge of its range needs towards the strip's edge: half of
// itself and the work in front of it.
const REACH: i32 = 18;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum State {
    // Agent alive, between turns. A still picture: the robot fishes.
    #[default]
    Idle,
    // A turn in flight: the only state that moves.
    Working,
    // Blocked on the user.
    Attention,
    // A turn just ended.
    Done,
}

impl State {
    pub const ALL: [State; 4] = [State::Working, State::Idle, State::Attention, State::Done];

    pub fn title(self) -> &'static str {
        match self {
            State::Idle => "Idle",
            State::Working => "Working",
            State::Attention => "Needs you",
            State::Done => "Done",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Weather {
    #[default]
    Clear,
    Cloudy,
    Rain,
    Snow,
    Fog,
}

impl Weather {
    pub const ALL: [Weather; 5] =
        [Weather::Clear, Weather::Cloudy, Weather::Rain, Weather::Snow, Weather::Fog];

    // The weather at scene time `t`, weighted towards fair.
    pub fn drifting(t: f32) -> Weather {
        const MIX: [Weather; 8] = [
            Weather::Clear,
            Weather::Clear,
            Weather::Clear,
            Weather::Cloudy,
            Weather::Cloudy,
            Weather::Rain,
            Weather::Snow,
            Weather::Fog,
        ];
        let n = (t / WEATHER_SECS).floor() as i64;
        MIX[(hash(n, 0x5121) % MIX.len() as u32) as usize]
    }

    pub fn title(self) -> &'static str {
        match self {
            Weather::Clear => "Clear",
            Weather::Cloudy => "Cloudy",
            Weather::Rain => "Rain",
            Weather::Snow => "Snow",
            Weather::Fog => "Fog",
        }
    }

    // 0 is an open sky, 1 fully grey.
    fn overcast(self) -> f32 {
        match self {
            Weather::Clear => 0.0,
            Weather::Cloudy => 0.45,
            Weather::Rain => 0.8,
            Weather::Snow => 0.65,
            Weather::Fog => 0.55,
        }
    }

    // Share of cloud slots that hold a cloud.
    fn cloud_cover(self) -> f32 {
        match self {
            Weather::Clear => 0.4,
            Weather::Fog => 0.5,
            _ => 1.0,
        }
    }
}

// A fixed time of day, for a caller that does not want the cycle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Daytime {
    Dawn,
    Day,
    Dusk,
    Night,
}

impl Daytime {
    pub const ALL: [Daytime; 4] = [Daytime::Dawn, Daytime::Day, Daytime::Dusk, Daytime::Night];

    pub fn title(self) -> &'static str {
        match self {
            Daytime::Dawn => "Dawn",
            Daytime::Day => "Day",
            Daytime::Dusk => "Dusk",
            Daytime::Night => "Night",
        }
    }

    // The value of `Scene::day` this stands for.
    pub fn phase(self) -> f32 {
        match self {
            Daytime::Dawn => 0.02,
            Daytime::Day => 0.2,
            Daytime::Dusk => 0.49,
            Daytime::Night => 0.72,
        }
    }
}

// `Scene::day` at scene time `t`. A scene opens in mid-morning.
pub fn day_phase(t: f32) -> f32 {
    (t / DAY_SECS + 0.12).rem_euclid(1.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scene {
    pub state: State,
    // Sub-agents at work beside the main robot. Clamped to `MAX_HELPERS`.
    pub helpers: usize,
    pub weather: Weather,
    // Time of day in [0, 1): 0 sunrise, 0.25 noon, 0.5 sunset, 0.75 midnight.
    pub day: f32,
}

impl Default for Scene {
    fn default() -> Self {
        Scene { state: State::Idle, helpers: 0, weather: Weather::Clear, day: 0.2 }
    }
}

impl Scene {
    // Whether the picture needs a clock. Only a turn in flight earns one, so a pane
    // parked at a prompt, however the sky looks, costs no frames.
    pub fn animates(&self) -> bool {
        self.state == State::Working
    }
}

// How many of `n` helpers a strip `w` art pixels wide has room for: one for every
// 30 pixels past the main robot's own.
pub fn helpers_shown(w: i32, n: usize) -> usize {
    n.min(MAX_HELPERS).min(((w - 20) / 30).max(0) as usize)
}

pub struct Buf {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

impl Buf {
    fn new(w: i32, h: i32) -> Self {
        let mut px = vec![0; (w * h * 4) as usize];
        for p in px.chunks_exact_mut(4) {
            p[3] = 255;
        }
        Buf { w, h, px }
    }

    fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        if c.3 <= 0.0 || w <= 0 || h <= 0 {
            return;
        }
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        let a = c.3.min(1.0);
        for yy in y0..y1 {
            for xx in x0..x1 {
                let i = ((yy * self.w + xx) * 4) as usize;
                for (k, s) in [c.0, c.1, c.2].into_iter().enumerate() {
                    let d = self.px[i + k] as f32;
                    self.px[i + k] = (d + (s as f32 - d) * a).round() as u8;
                }
            }
        }
    }
}

// The colour of the light at one time of day.
struct Light {
    top: Rgba,
    bottom: Rgba,
    // Multiplied into everything that is lit rather than glowing.
    amb: [f32; 3],
    // 0 by day, 1 in full night: stars, fireflies.
    night: f32,
}

fn light(day: f32, overcast: f32) -> Light {
    type Key = (f32, Rgba, Rgba, [f32; 3], f32);
    const DAWN: (Rgba, Rgba, [f32; 3], f32) =
        (rgb(0x4c, 0x5c, 0x9c), rgb(0xf0, 0xa8, 0x7c), [0.92, 0.80, 0.76], 0.25);
    const DAY: (Rgba, Rgba, [f32; 3], f32) =
        (rgb(0x3a, 0x80, 0xc2), rgb(0x98, 0xcc, 0xe4), [1.0, 1.0, 1.0], 0.0);
    const DUSK: (Rgba, Rgba, [f32; 3], f32) =
        (rgb(0x3c, 0x30, 0x6c), rgb(0xe6, 0x7c, 0x52), [0.86, 0.66, 0.62], 0.35);
    const NIGHT: (Rgba, Rgba, [f32; 3], f32) =
        (rgb(0x0a, 0x10, 0x24), rgb(0x1a, 0x2a, 0x4a), [0.36, 0.42, 0.60], 1.0);
    const KEYS: [Key; 7] = [
        (0.00, DAWN.0, DAWN.1, DAWN.2, DAWN.3),
        (0.07, DAY.0, DAY.1, DAY.2, DAY.3),
        (0.42, DAY.0, DAY.1, DAY.2, DAY.3),
        (0.50, DUSK.0, DUSK.1, DUSK.2, DUSK.3),
        (0.58, NIGHT.0, NIGHT.1, NIGHT.2, NIGHT.3),
        (0.93, NIGHT.0, NIGHT.1, NIGHT.2, NIGHT.3),
        (1.00, DAWN.0, DAWN.1, DAWN.2, DAWN.3),
    ];
    let day = day.rem_euclid(1.0);
    let i = KEYS.windows(2).position(|p| day <= p[1].0).unwrap_or(KEYS.len() - 2);
    let (a, b) = (KEYS[i], KEYS[i + 1]);
    let k = ((day - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
    let mut amb = [0.0; 3];
    for (j, v) in amb.iter_mut().enumerate() {
        *v = (a.3[j] + (b.3[j] - a.3[j]) * k) * (1.0 - 0.22 * overcast);
    }
    let grey = shade(rgb(0x96, 0xa2, 0xae), amb);
    Light {
        top: mix(mix(a.1, b.1, k), grey, overcast * 0.85),
        bottom: mix(mix(a.2, b.2, k), grey, overcast * 0.7),
        amb,
        night: a.4 + (b.4 - a.4) * k,
    }
}

fn shade(c: Rgba, amb: [f32; 3]) -> Rgba {
    let f = |v: u8, k: f32| (v as f32 * k).round().clamp(0.0, 255.0) as u8;
    Rgba(f(c.0, amb[0]), f(c.1, amb[1]), f(c.2, amb[2]), c.3)
}

// Where a robot, or a thing it made, stands: buffer x of its centre column, how
// far off the ground it is, and whether it faces left. Rectangles given to
// `Paint::lit` and `Paint::lamp` are in its space: x from the centre column
// towards its front, y up from its feet.
#[derive(Clone, Copy)]
struct Frame {
    ox: i32,
    lift: i32,
    flip: bool,
}

struct Paint {
    b: Buf,
    amb: [f32; 3],
    // Buffer x of world x 0.
    cx: i32,
}

impl Paint {
    // Lit by the day: tinted with the ambient light.
    fn put(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        self.b.fill(x, y, w, h, shade(c, self.amb));
    }

    // Glowing: drawn as given, whatever the hour.
    fn glow(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        self.b.fill(x, y, w, h, c);
    }

    fn lit(&mut self, f: Frame, dx: i32, dy: i32, w: i32, h: i32, c: Rgba) {
        let x = if f.flip { f.ox - dx - w + 1 } else { f.ox + dx };
        self.put(x, FEET - f.lift + dy, w, h, c);
    }

    fn lamp(&mut self, f: Frame, dx: i32, dy: i32, w: i32, h: i32, c: Rgba) {
        let x = if f.flip { f.ox - dx - w + 1 } else { f.ox + dx };
        self.glow(x, FEET - f.lift + dy, w, h, c);
    }
}

const WHITE: Rgba = rgb(0xf4, 0xf7, 0xfa);
const HILL_FAR: Rgba = rgb(0x62, 0x9c, 0x92);
const HILL_NEAR: Rgba = rgb(0x4e, 0x98, 0x5e);
const GRASS_HI: Rgba = rgb(0x74, 0xc2, 0x58);
const GRASS: Rgba = rgb(0x58, 0xa8, 0x48);
const GRASS_DK: Rgba = rgb(0x48, 0x94, 0x40);
const WATER: Rgba = rgb(0x3c, 0x82, 0xc4);
const SHORE: Rgba = rgb(0x3e, 0x80, 0x44);
const TRUNK: Rgba = rgb(0x7a, 0x56, 0x3a);
const LEAF: Rgba = rgb(0x40, 0x90, 0x46);
const LEAF_HI: Rgba = rgb(0x62, 0xb2, 0x58);
const LEAF_DK: Rgba = rgb(0x30, 0x76, 0x3e);
const PINE: Rgba = rgb(0x2c, 0x78, 0x58);
const PINE_DK: Rgba = rgb(0x22, 0x62, 0x4a);
const ROCK: Rgba = rgb(0x8c, 0x94, 0x9c);
const DIRT: Rgba = rgb(0x8a, 0x62, 0x40);
const WOOD: Rgba = rgb(0xb0, 0x84, 0x52);
const SHELL: Rgba = rgb(0xe8, 0xec, 0xf0);
const SHELL_SH: Rgba = rgb(0xb8, 0xc2, 0xcc);
const JOINT: Rgba = rgb(0x6a, 0x76, 0x84);
const VISOR: Rgba = rgb(0x1c, 0x24, 0x30);
const EYE: Rgba = rgb(0x8c, 0xec, 0xff);
const METAL: Rgba = rgb(0x9a, 0xa6, 0xb2);
const CAN: Rgba = rgb(0x4c, 0xa4, 0xb8);
const DROP: Rgba = rgb(0x9c, 0xd8, 0xf4);
const SPARK: Rgba = rgb(0xff, 0xe0, 0x70);
const PETAL: Rgba = rgb(0xff, 0xa8, 0xc0);
const POLLEN: Rgba = rgb(0xff, 0xf0, 0x90);
const PAINT_SKY: Rgba = rgb(0x7c, 0xc0, 0xe8);

// The antenna light, one per state.
pub const WORKING: Rgba = rgb(0x4d, 0xa6, 0xff);
pub const ATTENTION: Rgba = rgb(0xe5, 0xa0, 0x3c);
pub const DONE: Rgba = rgb(0x22, 0xc5, 0x5e);
const LAMP_OFF: Rgba = rgb(0x6b, 0x7a, 0x8d);

// The main robot's chest light, which glows like its eyes; each helper is a
// colour of its own instead.
const CHEST: Rgba = rgb(0x4d, 0xa6, 0xff);
const HELPER_COLOURS: [Rgba; MAX_HELPERS] = [
    rgb(0x3f, 0xb8, 0xa8),
    rgb(0xe8, 0xc0, 0x4a),
    rgb(0x9b, 0x7b, 0xe8),
    rgb(0xe8, 0x7a, 0xa8),
    rgb(0x8f, 0xc8, 0x4a),
    rgb(0x5a, 0xa8, 0xe8),
];

// One robot in one frame: who it is (None is the main robot), where its round has
// it, and how it is drawn.
struct Member {
    who: Option<usize>,
    act: Act,
    pose: Pose,
}

// Everybody in the strip at `t`, last helper first and the main robot last, so it
// is never covered.
fn cast(scene: &Scene, w: i32, t: f32) -> Vec<Member> {
    let lim = (w / 2 - REACH).max(0);
    let working = scene.state == State::Working;
    let member = |who: Option<usize>| {
        let act = act(who.map_or(-1, |i| i as i64), lim, t);
        let fishing = who.is_none() && scene.state == State::Idle && scene.helpers == 0;
        let pose = if fishing {
            // The lake is to the right of the middle, so that is where it fishes from.
            Pose { x: 0, lift: 0, flip: false, mode: Mode::Stand }
        } else if working {
            Pose { x: act.x, lift: act.lift, flip: act.now.flip, mode: act.mode }
        } else {
            // Wherever the turn left it, on the ground.
            Pose { x: act.x, lift: 0, flip: act.now.flip, mode: Mode::Stand }
        };
        Member { who, act, pose }
    };
    let mut all: Vec<Member> =
        (0..helpers_shown(w, scene.helpers)).rev().map(|i| member(Some(i))).collect();
    all.push(member(None));
    all
}

// Draw the strip `w` art pixels wide (`H` tall). `t` is scene seconds; pass a
// constant for a still frame. A pure function: a caller that resizes the strip or
// adds helpers and wants the robots to fly to their new places draws through a
// `Troupe` instead.
pub fn render(scene: &Scene, w: i32, t: f32) -> Buf {
    let w = w.max(1);
    draw(scene, w, t, &cast(scene, w, t))
}

// Draws the strip like `render`, and remembers where each robot was drawn, so one
// whose place has jumped (the strip was resized, the turn ended and it goes
// fishing, a helper has just been added) flies there instead of appearing there.
#[derive(Default)]
pub struct Troupe {
    // Where each robot was last drawn, in world x: the main robot, then the helpers.
    seen: [Option<f32>; MAX_HELPERS + 1],
    settling: bool,
}

// Art pixels a second, flying to where it should be.
const RESCUE_SPEED: f32 = 160.0;
// A jump this small is a robot's own movement, not a displacement.
const RESCUE_SNAP: f32 = 3.0;

impl Troupe {
    // `dt` is the seconds since the last frame drawn through this troupe.
    pub fn render(&mut self, scene: &Scene, w: i32, t: f32, dt: f32) -> Buf {
        let w = w.max(1);
        let mut all = cast(scene, w, t);
        let mut seen = [None; MAX_HELPERS + 1];
        self.settling = false;
        for m in &mut all {
            let slot = m.who.map_or(0, |i| i + 1);
            let want = m.pose.x as f32;
            let edge = (w / 2 + 12) as f32;
            let was = match (self.seen[slot], m.who) {
                // One left far outside by a narrowed strip starts from just past the
                // edge, so it is in view at once.
                (Some(x), _) => x.clamp(-edge, edge),
                // A new helper arrives from one side, out of frame.
                (None, Some(i)) => edge * if i % 2 == 0 { 1.0 } else { -1.0 },
                (None, None) => want,
            };
            let mut lag = was - want;
            if lag.abs() <= RESCUE_SNAP {
                lag = 0.0;
            } else {
                lag -= lag.signum() * (RESCUE_SPEED * dt).min(lag.abs());
            }
            if lag != 0.0 {
                self.settling = true;
                m.pose = Pose {
                    x: (want + lag).round() as i32,
                    // It comes down as it arrives.
                    lift: 2 + (lag.abs() / 3.0).min(11.0) as i32,
                    flip: lag > 0.0,
                    mode: Mode::Fly,
                };
            }
            seen[slot] = Some(want + lag);
        }
        self.seen = seen;
        draw(scene, w, t, &all)
    }

    // Whether a robot is still flying to its place, which needs frames even when
    // the scene itself holds still.
    pub fn settling(&self) -> bool {
        self.settling
    }
}

fn draw(scene: &Scene, w: i32, t: f32, cast: &[Member]) -> Buf {
    let overcast = scene.weather.overcast();
    let l = light(scene.day, overcast);
    let mut p = Paint { b: Buf::new(w, H), amb: l.amb, cx: w / 2 };

    sky(&mut p, scene, &l, t);
    land(&mut p, scene, &l, t);

    // Everything made goes down before anybody walks in front of it.
    for m in cast {
        let hw = if m.who.is_some() { HELPER.hw } else { MAIN.hw };
        let at = |s: Stop| Frame { ox: w / 2 + s.x, lift: 0, flip: s.flip };
        made(&mut p, at(m.act.before), hw, m.act.before.job, 1.0);
        if m.act.mode == Mode::Work {
            made(&mut p, at(m.act.now), hw, m.act.now.job, m.act.progress);
        }
    }
    for m in cast {
        robot(&mut p, scene, m, t);
    }

    if scene.animates() && l.night > 0.5 {
        fireflies(&mut p, l.night, t);
    }
    weather(&mut p, scene.weather, t);
    p.b
}

fn sky(p: &mut Paint, scene: &Scene, l: &Light, t: f32) {
    let w = p.b.w;
    for y in 0..H {
        let k = (y as f32 / HORIZON as f32).min(1.0);
        p.glow(0, y, w, 1, mix(l.top, l.bottom, k));
    }
    let overcast = scene.weather.overcast();
    let open = (1.0 - overcast * 1.2).max(0.0);

    if l.night > 0.0 && open > 0.0 {
        for x in 0..w {
            let wx = (x - p.cx) as i64;
            if hash(wx, 77) % 15 != 0 {
                continue;
            }
            let y = (hash(wx, 78) % (HORIZON as u32 - 6)) as i32;
            let twinkle = ((t * 1.3 + hashf(wx, 79) * 6.3).sin() > -0.4) as i32 as f32;
            p.glow(x, y, 1, 1, WHITE.a(l.night * open * (0.35 + 0.55 * twinkle)));
        }
    }

    // Sun and moon share one arc: up at the left edge, down at the right.
    let day = scene.day.rem_euclid(1.0);
    let (u, sun) = if day < 0.5 { (day / 0.5, true) } else { ((day - 0.5) / 0.5, false) };
    let alt = (u * std::f32::consts::PI).sin();
    let x = (8.0 + (w - 16) as f32 * u).round() as i32;
    let y = (HORIZON as f32 - 1.0 - alt * (HORIZON - 7) as f32).round() as i32;
    if open > 0.0 {
        if sun {
            let c = mix(rgb(0xff, 0x94, 0x4c), rgb(0xff, 0xdc, 0x60), alt * 1.4).a(open);
            disc(p, x, y, c);
            let pulse = (t * 1.5) as i32 % 2;
            let ray = c.a(open * 0.8);
            for (dx, dy) in [(0, -5), (5, 0), (0, 5), (-5, 0)] {
                p.glow(x + dx * (5 + pulse) / 5, y + dy * (5 + pulse) / 5, 1, 1, ray);
            }
            for (dx, dy) in [(4, -4), (4, 4), (-4, 4), (-4, -4)] {
                p.glow(x + dx, y + dy, 1, 1, ray.a(open * if pulse == 0 { 0.8 } else { 0.3 }));
            }
        } else {
            let c = rgb(0xe8, 0xec, 0xf4).a(open);
            disc(p, x, y, c);
            let sea = rgb(0xb4, 0xbe, 0xcc).a(open);
            p.glow(x - 1, y - 1, 1, 1, sea);
            p.glow(x + 1, y + 1, 2, 1, sea);
            p.glow(x + 1, y - 2, 1, 1, sea);
        }
    }

    // Clouds live on an endless line in world x and drift along it.
    const SPACING: f32 = 44.0;
    let drift = t * 2.2;
    let cloud = if overcast > 0.6 { rgb(0xb4, 0xbc, 0xc6) } else { WHITE };
    let first = ((-(p.cx as f32) - 30.0 - drift) / SPACING).floor() as i64;
    let last = (((w - p.cx) as f32 + 30.0 - drift) / SPACING).ceil() as i64;
    for k in first..=last {
        if hashf(k, 31) > scene.weather.cloud_cover() {
            continue;
        }
        let cw = 10 + (hash(k, 32) % 3) as i32 * 4;
        let x = p.cx + (k as f32 * SPACING + hashf(k, 33) * 20.0 + drift).floor() as i32;
        let y = 2 + (hash(k, 34) % 11) as i32;
        p.put(x + 2, y, cw / 3 + 1, 2, cloud);
        p.put(x + cw / 2 - 1, y - 1, cw / 3 + 1, 3, cloud);
        p.put(x, y + 2, cw, 2, cloud);
        p.put(x + 1, y + 4, cw - 2, 1, mix(cloud, rgb(0x8c, 0x98, 0xa8), 0.35));
    }
}

// A filled circle of radius 3.
fn disc(p: &mut Paint, x: i32, y: i32, c: Rgba) {
    p.glow(x - 1, y - 3, 3, 7, c);
    p.glow(x - 2, y - 2, 5, 1, c);
    p.glow(x - 2, y + 2, 5, 1, c);
    p.glow(x - 3, y - 1, 2, 3, c);
    p.glow(x + 2, y - 1, 2, 3, c);
}

fn land(p: &mut Paint, scene: &Scene, l: &Light, t: f32) {
    let w = p.b.w;
    let snow = scene.weather == Weather::Snow;
    let hz = HORIZON as f32;
    for x in 0..w {
        let wx = (x - p.cx) as f32;
        let far = hz - 6.0 - 4.0 * (wx * 0.045 + 1.0).sin() - 3.0 * (wx * 0.11 + 4.0).sin()
            - 1.8 * (wx * 0.021).sin();
        let far = (far.round() as i32).clamp(HORIZON - 16, HORIZON);
        p.put(x, far, 1, HORIZON - far + 1, HILL_FAR);
        if snow {
            p.put(x, far, 1, 1, WHITE);
        }
        let near = hz - 2.5 - 2.0 * (wx * 0.07 + 2.0).sin() - 1.4 * (wx * 0.031).sin();
        let near = (near.round() as i32).clamp(HORIZON - 7, HORIZON);
        p.put(x, near, 1, HORIZON - near + 1, HILL_NEAR);
    }
    p.put(0, HORIZON, w, H - HORIZON, GRASS);
    p.put(0, HORIZON, w, 1, if snow { WHITE } else { GRASS_HI });
    p.put(0, H - 3, w, 3, GRASS_DK);
    for x in 0..w {
        let wx = (x - p.cx) as i64;
        let h = hash(wx, 5);
        let y = HORIZON + 2 + (h >> 8) as i32 % (H - HORIZON - 3);
        match h % 23 {
            0 | 1 => p.put(x, y, 1, 1, GRASS_DK),
            2 | 3 => p.put(x, y, 1, 1, GRASS_HI),
            4 if !snow => {
                const PETALS: [Rgba; 3] = [POLLEN, PETAL, rgb(0xf8, 0xf8, 0xff)];
                p.put(x, y, 1, 1, PETALS[(h >> 16) as usize % 3]);
            }
            _ => {}
        }
        if snow && h % 5 == 0 {
            p.put(x, y, 1, 1, WHITE.a(0.7));
        }
    }

    // The centre is composed by hand, so the narrowest strip is still a picture;
    // further out each cell of land holds whatever its hash says.
    let water = mix(l.bottom, shade(WATER, l.amb), 0.6);
    lake(p, LAKE_X, LAKE_RX, water, scene, t);
    tree(p, -27, 17, 0);
    pine(p, -41, 14);
    bush(p, 68);
    let first = (-p.cx - CELL).div_euclid(CELL);
    let last = (w - p.cx + CELL).div_euclid(CELL);
    for c in first..=last {
        if (-1..=1).contains(&c) {
            continue;
        }
        let c64 = c as i64;
        let x = c * CELL + 8 + (hash(c64, 91) % 30) as i32;
        match hash(c64, 92) % 7 {
            0 => lake(p, c * CELL + CELL / 2, 12 + (hash(c64, 93) % 7) as i32, water, scene, t),
            1 | 2 => tree(p, x, 14 + (hash(c64, 94) % 5) as i32, c64),
            3 => pine(p, x, 12 + (hash(c64, 94) % 5) as i32),
            4 => {
                tree(p, x, 15, c64);
                pine(p, x + 13, 11);
            }
            5 => {
                bush(p, x);
                p.put(p.cx + x + 10, HORIZON + 3, 5, 2, ROCK);
                p.put(p.cx + x + 11, HORIZON + 2, 3, 1, ROCK);
            }
            _ => bush(p, x + 6),
        }
    }
}

fn lake(p: &mut Paint, wx: i32, rx: i32, water: Rgba, scene: &Scene, t: f32) {
    const RY: f32 = 3.5;
    let cx = p.cx + wx;
    let (top, bottom) = (HORIZON + 1, HORIZON + 7);
    let cy = (top + bottom) as f32 / 2.0;
    for y in top..=bottom {
        let k = (1.0 - ((y as f32 - cy) / RY).powi(2)).max(0.0).sqrt();
        let hw = (rx as f32 * k).round() as i32;
        if hw <= 0 {
            continue;
        }
        p.put(cx - hw - 1, y, 2 * hw + 2, 1, SHORE);
        p.glow(cx - hw, y, 2 * hw, 1, water);
        // Glints that step sideways, so the water is the one thing that never sits
        // quite still while the clock runs.
        let step = (t * 1.6) as i64;
        let span = (2 * hw - 3).max(1);
        let gx = cx - hw + 1 + (hash(y as i64 + wx as i64 * 7, step / 2) % span as u32) as i32;
        p.glow(gx, y, 2, 1, WHITE.a(0.35));
        if scene.weather == Weather::Rain {
            let rx2 = cx - hw + (hash(y as i64 * 13 + wx as i64, step) % (2 * hw) as u32) as i32;
            p.glow(rx2, y, 1, 1, WHITE.a(0.5));
        }
    }
    // Reeds on the near-left shore.
    for (dx, h) in [(-rx + 1, 5), (-rx + 3, 4)] {
        p.put(cx + dx, bottom - h, 1, h, LEAF_DK);
        p.put(cx + dx, bottom - h - 1, 1, 2, TRUNK);
    }
}

fn tree(p: &mut Paint, wx: i32, height: i32, seed: i64) {
    let x = p.cx + wx;
    let base = HORIZON + 5;
    let top = base - height;
    p.put(x - 1, base - 6, 2, 6, TRUNK);
    let rows = height - 5;
    for i in 0..rows {
        // A rounded crown: narrow at both ends, full in the middle.
        let k = (i as f32 + 0.5) / rows as f32;
        let hw = 2 + (4.6 * (1.0 - (2.0 * k - 1.0).powi(2)).sqrt()).round() as i32;
        let c = if k > 0.72 { LEAF_DK } else { LEAF };
        p.put(x - hw, top + i, 2 * hw, 1, c);
        if k < 0.5 {
            p.put(x - hw + 1, top + i, hw - 1, 1, LEAF_HI);
        }
    }
    if hash(seed, 95) % 2 == 0 {
        p.put(x + 3, top + rows / 2, 1, 1, rgb(0xe8, 0x5a, 0x4a));
        p.put(x - 4, top + rows / 2 + 2, 1, 1, rgb(0xe8, 0x5a, 0x4a));
        p.put(x, top + rows / 2 + 3, 1, 1, rgb(0xe8, 0x5a, 0x4a));
    }
}

fn pine(p: &mut Paint, wx: i32, height: i32) {
    let x = p.cx + wx;
    let base = HORIZON + 4;
    p.put(x, base - 2, 1, 2, TRUNK);
    let rows = height - 2;
    for i in 0..rows {
        // Tiers, each widening downwards then stepping back in.
        let hw = 1 + (i % 3) + i / 3;
        let c = if i % 3 == 2 { PINE_DK } else { PINE };
        p.put(x - hw + 1, base - 2 - rows + i, 2 * hw - 1, 1, c);
    }
}

fn bush(p: &mut Paint, wx: i32) {
    let x = p.cx + wx;
    let y = HORIZON + 1;
    p.put(x + 1, y, 6, 1, LEAF_HI);
    p.put(x, y + 1, 8, 3, LEAF);
    p.put(x + 1, y + 4, 6, 1, LEAF_DK);
}

// What a working robot is doing. Variety only: every job means "working".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Job {
    // A plant that grows and flowers.
    Water,
    // A fence, post by post.
    Hammer,
    // A mound, with a sapling in it at the end.
    Dig,
    // Crates, piled three high.
    Stack,
    // A landscape on an easel.
    Paint,
}

const JOBS: [Job; 5] = [Job::Water, Job::Hammer, Job::Dig, Job::Stack, Job::Paint];

// Scene seconds from one job's start to the next, the way there included.
const JOB_SECS: f32 = 11.0;
// Art pixels a second, walking.
const WALK_SPEED: f32 = 8.0;
// A stop further off than this is flown to.
const WALK_MAX: i32 = 34;
// Stops come in runs of this many near one another; the next run is somewhere else
// in the strip altogether.
const RUN: i64 = 3;
// How far either side of a run's first stop the others are.
const RUN_SPREAD: f32 = 26.0;
// Runs are placed in this much world either side of the middle. One that falls
// outside the strip is folded into it, so a run that fits a narrow strip is in the
// same place in a wide one.
const WORLD: i32 = 480;

// One robot's proportions, in art pixels.
struct Build {
    // Half the head's width; the head is `2 * hw + 1` wide.
    hw: i32,
    legs: i32,
    body: i32,
    head: i32,
    stalk: i32,
    eye_w: i32,
    arm: i32,
}

// 11 wide and 19 tall with its antenna.
const MAIN: Build = Build { hw: 5, legs: 3, body: 5, head: 8, stalk: 2, eye_w: 2, arm: 4 };
// 9 wide and 14 tall.
const HELPER: Build = Build { hw: 4, legs: 2, body: 4, head: 6, stalk: 1, eye_w: 1, arm: 3 };

// One place a robot works: where it stands, which way it faces, what it does.
#[derive(Clone, Copy)]
struct Stop {
    x: i32,
    flip: bool,
    job: Job,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    // Not working: on the ground, arms down.
    Stand,
    Walk,
    // On its jetpack.
    Fly,
    Work,
}

// A robot at one moment of its round: get to the next stop, work there, repeat.
struct Act {
    x: i32,
    // Art pixels off the ground, in flight.
    lift: i32,
    // Walk, Fly or Work.
    mode: Mode,
    now: Stop,
    // The stop before, whose finished work is still standing.
    before: Stop,
    // How far through the job at `now`, 0 to 1. 0 while still on the way to it.
    progress: f32,
}

// Where robot `who`'s `run`th run of stops starts, in the whole world.
fn run_start(who: i64, run: i64) -> i32 {
    ((hashf(who * 131 + 7, run) * 2.0 - 1.0) * WORLD as f32).round() as i32
}

// Robot `who`'s `n`th stop, anywhere within `lim` of the middle, which keeps the
// work in front of it inside the strip. It faces the way it came.
fn stop(who: i64, lim: i32, n: i64) -> Stop {
    let at = |n: i64| {
        let first = run_start(who, n.div_euclid(RUN));
        // One that is in the strip is used as it is; only one outside is folded in.
        let first = if first.abs() <= lim { first } else { (first + lim).rem_euclid(2 * lim + 1) - lim };
        let off = match n.rem_euclid(RUN) {
            0 => 0,
            _ => ((hashf(who * 57 + 1, n) * 2.0 - 1.0) * RUN_SPREAD).round() as i32,
        };
        (first + off).clamp(-lim, lim)
    };
    let pick = |n: i64| hash(who * 17 + 3, n) as usize % JOBS.len();
    let (x, from) = (at(n), at(n - 1));
    let flip = if x == from { hash(who, n * 2 + 1) % 2 == 0 } else { x < from };
    // Never the job it has just finished.
    let job = if pick(n) == pick(n - 1) { (pick(n) + 1) % JOBS.len() } else { pick(n) };
    Stop { x, flip, job: JOBS[job] }
}

// Each robot's round has its own offset, so they never set off in step. A stop
// nearby is walked to; one further off is flown to, in an arc.
fn act(who: i64, lim: i32, t: f32) -> Act {
    let at = t / JOB_SECS + hashf(who, 0x7a1);
    let n = at.floor() as i64;
    let s = at.fract() * JOB_SECS;
    let (now, before) = (stop(who, lim, n), stop(who, lim, n - 1));
    let dist = (now.x - before.x).abs();
    let fly = dist > WALK_MAX;
    let travel = if fly { (dist as f32 / 60.0).clamp(1.2, 3.0) } else { dist as f32 / WALK_SPEED };
    if s >= travel {
        let progress = (s - travel) / (JOB_SECS - travel);
        return Act { x: now.x, lift: 0, mode: Mode::Work, now, before, progress };
    }
    let k = s / travel;
    let (along, lift, mode) = if fly {
        // Eased at both ends: it takes off and sets down gently.
        let arc = (k * std::f32::consts::PI).sin() * (dist as f32 / 5.0).clamp(8.0, 14.0);
        (k * k * (3.0 - 2.0 * k), 1 + arc.round() as i32, Mode::Fly)
    } else {
        (k, 0, Mode::Walk)
    };
    let x = before.x + ((now.x - before.x) as f32 * along).round() as i32;
    Act { x, lift, mode, now, before, progress: 0.0 }
}

// How one robot is drawn in a frame. Its round's own, unless something has moved
// its place from under it (see `Troupe`).
#[derive(Clone, Copy)]
struct Pose {
    x: i32,
    lift: i32,
    flip: bool,
    mode: Mode,
}

// What a job leaves on the ground in front of a robot `hw` half-wide standing at
// `f`, `k` of the way through (1 is finished).
fn made(p: &mut Paint, f: Frame, hw: i32, job: Job, k: f32) {
    // Nearest column of the work.
    let a = hw + 6;
    match job {
        Job::Water => {
            let x = a + 1;
            let grown = 1 + ((k * 5.0) as i32).min(5);
            p.lit(f, x, -grown, 1, grown, LEAF_HI);
            if grown >= 3 {
                p.lit(f, x - 1, -grown + 1, 1, 1, LEAF);
                p.lit(f, x + 1, -grown + 2, 1, 1, LEAF);
            }
            if k > 0.75 {
                p.lit(f, x - 1, -grown - 1, 3, 1, PETAL);
                p.lit(f, x, -grown - 2, 1, 3, PETAL);
                p.lit(f, x, -grown - 1, 1, 1, POLLEN);
            }
        }
        Job::Hammer => {
            let posts = 1 + ((k * 3.0) as i32).min(3);
            for i in 0..posts {
                p.lit(f, a + i * 3, -6, 1, 6, WOOD);
            }
            if posts > 1 {
                p.lit(f, a, -5, (posts - 1) * 3 + 1, 1, WOOD);
                p.lit(f, a, -2, (posts - 1) * 3 + 1, 1, WOOD);
            }
        }
        Job::Dig => {
            let heap = 2 + ((k * 4.0) as i32).min(4);
            p.lit(f, a + 1, -1, heap + 2, 1, DIRT);
            p.lit(f, a + 2, -2, heap, 1, DIRT);
            if k >= 1.0 {
                let x = a + 2 + heap / 2;
                p.lit(f, x, -5, 1, 3, TRUNK);
                p.lit(f, x - 1, -7, 3, 2, LEAF_HI);
                p.lit(f, x, -8, 1, 1, LEAF_HI);
            }
        }
        Job::Stack => {
            let crates = 1 + ((k * 3.0) as i32).min(2);
            for (i, (dx, dy)) in [(0, -4), (5, -4), (2, -8)].into_iter().enumerate() {
                if (i as i32) < crates {
                    p.lit(f, a + dx, dy, 5, 4, WOOD);
                    p.lit(f, a + dx, dy, 5, 1, mix(WOOD, WHITE, 0.3));
                    p.lit(f, a + dx + 2, dy, 1, 4, TRUNK);
                }
            }
        }
        Job::Paint => {
            p.lit(f, a, -3, 1, 3, TRUNK);
            p.lit(f, a + 6, -3, 1, 3, TRUNK);
            p.lit(f, a, -10, 7, 7, TRUNK);
            p.lit(f, a + 1, -9, 5, 5, SHELL);
            let done = ((k * 5.0).ceil() as i32).clamp(0, 5);
            p.lit(f, a + 1, -9, done, 3, PAINT_SKY);
            p.lit(f, a + 1, -6, done, 2, GRASS_HI);
            if done >= 4 {
                p.lit(f, a + 4, -9, 1, 1, SPARK);
            }
        }
    }
}

fn robot(p: &mut Paint, scene: &Scene, m: &Member, t: f32) {
    let (helper, a, pose) = (m.who, &m.act, m.pose);
    let main = helper.is_none();
    let who = helper.map_or(-1, |i| i as i64);
    let b = if main { &MAIN } else { &HELPER };
    let hw = b.hw;
    let working = scene.state == State::Working;
    let flying = pose.mode == Mode::Fly;
    let fishing = main && scene.state == State::Idle && scene.helpers == 0 && !flying;
    let f = Frame { ox: p.cx + pose.x, lift: pose.lift, flip: pose.flip };
    let walking = pose.mode == Mode::Walk;
    let beat = t + hashf(who, 3) * 2.0;
    let stride = (beat * 5.0) as i32 % 2;
    // The body holds still: the life of a robot at work is in its arms, its lamp and
    // its antenna, whose tip sways a pixel each way on a slow beat.
    let sway = if working { [0, 1, 0, -1][(beat * 1.6) as usize % 4] } else { 0 };

    let body_top = -(b.legs + b.body);
    let head_top = body_top - b.head;
    let accent = helper.map_or(CHEST, |i| HELPER_COLOURS[i % MAX_HELPERS]);

    // The shadow stays on the ground, and shrinks under a robot in the air.
    let spread = (hw - pose.lift / 4).max(1);
    p.lamp(Frame { lift: 0, ..f }, -spread, 0, 2 * spread + 1, 1, Rgba(0, 0, 0, 0.18));
    // Walking lifts one foot, then the other; flying tucks both.
    let lift = |leg: i32| (flying || (walking && stride == leg)) as i32;
    p.lit(f, -hw + 1, -b.legs, 2, b.legs - lift(0), JOINT);
    p.lit(f, hw - 2, -b.legs, 2, b.legs - lift(1), JOINT);

    let torso = hw - 1;
    if flying {
        // The jetpack on its back, and the flame under it.
        p.lit(f, -torso - 2, body_top, 2, b.body - 1, JOINT);
        p.lit(f, -torso - 2, body_top, 2, 1, METAL);
        let flare = (beat * 11.0) as i32 % 2;
        p.lamp(f, -torso - 2, body_top + b.body - 1, 2, 2 + flare, rgb(0xff, 0x8a, 0x3c));
        p.lamp(f, -torso - 2, body_top + b.body - 1, 1, 1 + flare, SPARK);
        p.lamp(f, -torso - 2 + flare, body_top + b.body + 2 + flare, 1, 1, rgb(0xff, 0x8a, 0x3c).a(0.5));
    }
    if main {
        p.lit(f, -torso, body_top, 2 * torso + 1, b.body, SHELL);
        p.lit(f, -torso, body_top + b.body - 1, 2 * torso + 1, 1, SHELL_SH);
        p.lamp(f, -1, body_top + 1, 3, 2, CHEST);
    } else {
        p.lit(f, -torso, body_top, 2 * torso + 1, b.body, accent);
    }

    p.lit(f, -hw + 1, head_top, 2 * hw - 1, 1, SHELL);
    p.lit(f, -hw, head_top + 1, 2 * hw + 1, b.head - 1, SHELL);
    p.lit(f, -hw, head_top + b.head - 1, 2 * hw + 1, 1, SHELL_SH);
    let visor_h = if main { b.head - 4 } else { b.head - 3 };
    p.lit(f, -hw + 1, head_top + 2, 2 * hw - 1, visor_h, VISOR);
    if main {
        p.lit(f, -hw - 1, head_top + 3, 1, 2, JOINT);
        p.lit(f, hw + 1, head_top + 3, 1, 2, JOINT);
    }

    // The face. Eyes glow, so a robot at night is two lit dots and a lamp. The back
    // eye sits a column nearer the middle than the front one, which makes it look
    // where it is going while both keep a column of visor around them.
    let eye_y = head_top + 3;
    let (left, right) = (-b.eye_w, 2);
    let blink = working && (beat * 0.9).fract() < 0.05;
    match scene.state {
        State::Done if main => {
            for x in [-3, 3] {
                p.lamp(f, x - 1, eye_y + 1, 1, 1, EYE);
                p.lamp(f, x, eye_y, 1, 1, EYE);
                p.lamp(f, x + 1, eye_y + 1, 1, 1, EYE);
            }
        }
        _ if blink => {
            p.lamp(f, left, eye_y + 1, b.eye_w, 1, EYE);
            p.lamp(f, right, eye_y + 1, b.eye_w, 1, EYE);
        }
        _ => {
            p.lamp(f, left, eye_y, b.eye_w, 2, EYE);
            p.lamp(f, right, eye_y, b.eye_w, 2, EYE);
        }
    }
    if main {
        p.lamp(f, 0, head_top + 6, 3, 1, EYE.a(0.75));
    }

    // The antenna lamp is the state, in the status dot's own colours.
    let lamp = match scene.state {
        State::Working => {
            if (beat * 2.0) as i32 % 2 == 0 {
                WORKING
            } else {
                mix(WORKING, VISOR, 0.5)
            }
        }
        State::Attention => ATTENTION,
        State::Done => DONE,
        State::Idle => LAMP_OFF,
    };
    p.lit(f, 0, head_top - b.stalk, 1, b.stalk, JOINT);
    p.lamp(f, sway, head_top - b.stalk - 1, 1, 1, lamp);
    if scene.state != State::Idle {
        p.lamp(f, sway - 1, head_top - b.stalk - 1, 3, 1, lamp.a(0.3));
    }

    // Arms and whatever they hold. `sh` is the shoulder row, `hd` the row of a hand
    // held out in front.
    let sh = body_top;
    let hd = sh + 2;
    let arm = METAL;
    let back_arm = |p: &mut Paint| p.lit(f, -hw, sh, 1, b.arm, arm);
    // The front arm held out, hand at `(hw + 1, hd)`.
    let reach = |p: &mut Paint| {
        p.lit(f, hw, sh, 1, 2, arm);
        p.lit(f, hw, hd, 2, 1, arm);
    };
    match scene.state {
        _ if flying => {
            // The back arm steadies the pack, the front one points the way.
            p.lit(f, -hw, sh, 1, b.arm - 1, arm);
            p.lit(f, hw, sh, 1, 2, arm);
            p.lit(f, hw, sh + 1, 2, 1, arm);
        }
        State::Working if walking => {
            // Arms swing against the legs.
            p.lit(f, -hw, sh, 1, b.arm - stride, arm);
            p.lit(f, hw, sh, 1, b.arm - 1 + stride, arm);
        }
        State::Working => {
            back_arm(p);
            match a.now.job {
                Job::Water => {
                    reach(p);
                    p.lit(f, hw + 2, hd - 1, 3, 3, CAN);
                    p.lit(f, hw + 3, hd - 2, 2, 1, CAN);
                    p.lit(f, hw + 5, hd, 1, 1, CAN);
                    p.lit(f, hw + 6, hd + 1, 1, 1, CAN);
                    let mut y = hd + 2 + (beat * 7.0) as i32 % 3;
                    while y < -1 {
                        p.lamp(f, hw + 7, y, 1, 1, DROP.a(0.9));
                        y += 3;
                    }
                }
                Job::Hammer => {
                    if (beat * 2.2).fract() > 0.7 {
                        reach(p);
                        p.lit(f, hw + 2, hd - 1, 1, 2, TRUNK);
                        p.lit(f, hw + 3, hd - 2, 3, 2, JOINT);
                        p.lamp(f, hw + 6, hd - 3, 1, 1, SPARK);
                        p.lamp(f, hw + 7, hd - 1, 1, 1, SPARK);
                    } else {
                        p.lit(f, hw, sh - 1, 1, 2, arm);
                        p.lit(f, hw + 1, sh - 3, 1, 2, TRUNK);
                        p.lit(f, hw, sh - 5, 3, 2, JOINT);
                    }
                }
                Job::Dig => {
                    if (beat * 1.6).fract() > 0.55 {
                        p.lit(f, hw, sh - 1, 1, 2, arm);
                        for i in 1..=3 {
                            p.lit(f, hw + i, sh - 1 - i, 1, 1, TRUNK);
                        }
                        p.lit(f, hw + 4, sh - 6, 2, 2, METAL);
                        p.lit(f, hw + 6, sh - 6, 1, 1, DIRT);
                        p.lit(f, hw + 7, sh - 4, 1, 1, DIRT);
                    } else {
                        reach(p);
                        // The handle runs from the hand down to the blade in the soil.
                        let len = (-3 - hd).max(1);
                        for i in 0..len {
                            p.lit(f, hw + 2 + i, hd + i, 1, 1, TRUNK);
                        }
                        p.lit(f, hw + 2 + len, -3, 2, 2, METAL);
                    }
                }
                Job::Stack => {
                    if (beat * 1.2).fract() > 0.5 {
                        p.lit(f, hw, sh, 2, 1, arm);
                        p.lit(f, hw + 2, sh - 2, 4, 3, WOOD);
                        p.lit(f, hw + 3, sh - 2, 1, 3, TRUNK);
                    } else {
                        reach(p);
                    }
                }
                Job::Paint => {
                    let y = sh + (beat * 2.5) as i32 % 2;
                    p.lit(f, hw, y, 2, 1, arm);
                    p.lit(f, hw + 2, y, 2, 1, TRUNK);
                    p.lit(f, hw + 4, y, 1, 1, accent);
                }
            }
        }
        State::Attention if main => {
            // One arm up, and the amber the status dot uses, as a speech bubble.
            back_arm(p);
            p.lit(f, hw, sh - 1, 1, 2, arm);
            p.lit(f, hw + 1, sh - 3, 1, 2, arm);
            let by = head_top - 5;
            p.lamp(f, hw + 3, by, 5, 7, ATTENTION);
            p.lamp(f, hw + 2, by + 5, 1, 1, ATTENTION);
            p.lamp(f, hw + 5, by + 1, 1, 3, VISOR);
            p.lamp(f, hw + 5, by + 5, 1, 1, VISOR);
        }
        State::Done if main => {
            // Both arms up, and a green flag planted beside the work.
            for s in [-1, 1] {
                p.lit(f, s * hw, sh - 1, 1, 2, arm);
                p.lit(f, s * (hw + 1), sh - 3, 1, 2, arm);
            }
            p.lit(f, hw + 4, -14, 1, 14, SHELL_SH);
            p.lamp(f, hw + 5, -14, 5, 4, DONE);
            p.lamp(f, hw + 5, -10, 2, 1, DONE.a(0.6));
        }
        State::Idle if fishing => {
            back_arm(p);
            reach(p);
            let tip = LAKE_X - LAKE_RX + 10;
            let from = hw + 2;
            let rise = |x: i32| hd - 1 - (x - from) * 2 / 5;
            for x in from..=tip {
                p.lit(f, x, rise(x), 1, 1, TRUNK);
            }
            let float = LAKE_Y - FEET;
            p.lamp(f, tip + 1, rise(tip), 1, float - rise(tip), WHITE.a(0.55));
            p.lamp(f, tip + 1, float, 1, 1, rgb(0xf0, 0x50, 0x48));
        }
        _ => {
            back_arm(p);
            p.lit(f, hw, sh, 1, b.arm, arm);
        }
    }
}

fn fireflies(p: &mut Paint, night: f32, t: f32) {
    let w = p.b.w;
    for x in 0..w {
        let wx = (x - p.cx) as i64;
        if hash(wx, 61) % 29 != 0 {
            continue;
        }
        let ph = t * 0.8 + hashf(wx, 62) * 6.3;
        if ph.sin() < 0.2 {
            continue;
        }
        let y = HORIZON - 3 + (hash(wx, 63) % 11) as i32 + (ph * 0.7).sin().round() as i32;
        let dx = (ph * 0.5).cos().round() as i32;
        p.glow(x + dx, y, 1, 1, rgb(0xf4, 0xf0, 0x8c).a(night * 0.9));
    }
}

fn weather(p: &mut Paint, weather: Weather, t: f32) {
    let w = p.b.w;
    match weather {
        Weather::Rain => {
            for x in 0..w {
                let wx = (x - p.cx) as i64;
                for k in 0..2 {
                    if hash(wx, 40 + k) % 5 != 0 {
                        continue;
                    }
                    let fall = hashf(wx, 42 + k) * H as f32 + t * 52.0;
                    let y = fall.rem_euclid((H + 4) as f32) as i32 - 2;
                    // Slanted: a drop is further left the lower it has fallen.
                    p.put(x - y / 6, y, 1, 2, DROP.a(0.55));
                }
            }
        }
        Weather::Snow => {
            for x in 0..w {
                let wx = (x - p.cx) as i64;
                if hash(wx, 50) % 4 != 0 {
                    continue;
                }
                let fall = hashf(wx, 51) * H as f32 + t * 8.0;
                let y = fall.rem_euclid((H + 2) as f32) as i32 - 1;
                let sway = ((t * 0.9 + hashf(wx, 52) * 6.3).sin() * 1.5).round() as i32;
                p.put(x + sway, y, 1, 1, WHITE.a(0.9));
            }
        }
        Weather::Fog => {
            let mist = rgb(0xd4, 0xdc, 0xe4);
            p.put(0, HORIZON - 14, w, H - HORIZON + 14, mist.a(0.16));
            for (i, dy) in [-11, -7, -3, 1, 6, 11].into_iter().enumerate() {
                let y = HORIZON + dy;
                let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
                for x in 0..w {
                    let wx = (x - p.cx) as f32 + t * 3.0 * dir;
                    if (wx * 0.07 + y as f32).sin() + (wx * 0.023).sin() * 0.6 > 0.1 {
                        p.put(x, y, 1, 2, mist.a(0.3));
                    }
                }
            }
        }
        Weather::Clear | Weather::Cloudy => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(state: State, helpers: usize, weather: Weather, day: Daytime) -> Scene {
        Scene { state, helpers, weather, day: day.phase() }
    }

    #[test]
    fn a_frame_is_reproducible_and_fully_opaque() {
        let s = scene(State::Working, 3, Weather::Rain, Daytime::Dusk);
        let (a, b) = (render(&s, 200, 12.3), render(&s, 200, 12.3));
        assert_eq!((a.w, a.h), (200, H));
        assert_eq!(a.px, b.px);
        assert!(a.px.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn every_state_weather_and_hour_draws_at_any_width() {
        for state in State::ALL {
            for weather in Weather::ALL {
                for day in Daytime::ALL {
                    for w in [1, MIN_W, 101, 640] {
                        for helpers in [0, MAX_HELPERS, MAX_HELPERS + 3] {
                            let b = render(&scene(state, helpers, weather, day), w, 3.7);
                            assert_eq!(b.px.len(), (w * H * 4) as usize);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn only_a_turn_in_flight_moves() {
        for state in [State::Idle, State::Attention, State::Done] {
            assert!(!scene(state, 2, Weather::Clear, Daytime::Day).animates());
        }
        let s = scene(State::Working, 0, Weather::Clear, Daytime::Day);
        assert!(s.animates());
        assert_ne!(render(&s, 160, 0.0).px, render(&s, 160, 0.4).px);
    }

    #[test]
    fn widening_the_strip_keeps_the_scenery_where_it_was() {
        let s = scene(State::Idle, 0, Weather::Clear, Daytime::Night);
        let (narrow, wide) = (render(&s, 200, 5.0), render(&s, 300, 5.0));
        // The moon rides an arc scaled to the width and the robots' work is placed by
        // it, so compare the grass in front of their feet.
        for y in FEET + 1..H {
            for x in 0..200 {
                let a = ((y * 200 + x) * 4) as usize;
                let b = ((y * 300 + x + 50) * 4) as usize;
                assert_eq!(narrow.px[a..a + 4], wide.px[b..b + 4], "at {x},{y}");
            }
        }
    }

    #[test]
    fn helpers_fit_by_width() {
        assert_eq!(helpers_shown(MIN_W, 0), 0);
        assert_eq!(helpers_shown(MIN_W, 2), 2);
        assert!(helpers_shown(MIN_W, MAX_HELPERS) < MAX_HELPERS);
        assert_eq!(helpers_shown(200, MAX_HELPERS), MAX_HELPERS);
        assert_eq!(helpers_shown(400, MAX_HELPERS + 5), MAX_HELPERS);
    }

    // World x each robot is drawn at, main robot first.
    fn places(tr: &Troupe) -> Vec<f32> {
        tr.seen.iter().flatten().copied().collect()
    }

    #[test]
    fn a_narrowed_strip_brings_everybody_back_into_frame_by_air() {
        let s = scene(State::Idle, 3, Weather::Clear, Daytime::Day);
        let mut tr = Troupe::default();
        // Let the helpers that fly in at the start arrive.
        for _ in 0..80 {
            tr.render(&s, 600, 50.0, 0.05);
        }
        assert!(!tr.settling());
        let wide = places(&tr);
        assert!(wide.iter().any(|x| x.abs() > 60.0), "nobody was out wide: {wide:?}");

        // Narrowed: whoever is outside waits just past the edge, and the scene,
        // though idle, wants frames.
        tr.render(&s, 120, 50.0, 0.0);
        assert!(places(&tr).iter().any(|x| x.abs() > 60.0));
        assert!(places(&tr).iter().all(|x| x.abs() <= 72.0));
        assert!(tr.settling());
        let mut frames = 0;
        while tr.settling() {
            tr.render(&s, 120, 50.0, 0.05);
            frames += 1;
            assert!(frames < 100, "never settled");
        }
        let lim = (120 / 2 - REACH) as f32;
        assert!(places(&tr).iter().all(|x| x.abs() <= lim), "{:?}", places(&tr));
        // It took flying, not a jump.
        assert!(frames > 3);
    }

    #[test]
    fn a_new_helper_flies_in_from_outside() {
        let mut s = scene(State::Working, 0, Weather::Clear, Daytime::Day);
        let mut tr = Troupe::default();
        tr.render(&s, 300, 1.0, 0.05);
        assert!(!tr.settling());
        s.helpers = 1;
        tr.render(&s, 300, 1.0, 0.0);
        assert!(tr.settling());
        assert_eq!(places(&tr)[1].abs(), 162.0);
    }

    #[test]
    fn a_robot_at_work_is_never_taken_for_displaced() {
        let s = scene(State::Working, MAX_HELPERS, Weather::Clear, Daytime::Day);
        let mut tr = Troupe::default();
        for i in 0..200 {
            tr.render(&s, 400, i as f32 * 0.05, 0.05);
        }
        for i in 200..3000 {
            tr.render(&s, 400, i as f32 * 0.05, 0.05);
            assert!(!tr.settling(), "frame {i}");
        }
    }

    #[test]
    fn the_day_wraps() {
        assert!((day_phase(0.0) - 0.12).abs() < 1e-6);
        assert!((day_phase(DAY_SECS) - 0.12).abs() < 1e-4);
        let (a, b) = (light(0.0, 0.0), light(1.0, 0.0));
        assert_eq!(a.top, b.top);
    }

    #[test]
    fn a_robot_walks_or_flies_to_each_job_across_the_whole_strip() {
        let lim = 300 / 2 - REACH;
        let (mut walked, mut flew, mut worked, mut jobs) = (false, false, false, Vec::new());
        let (mut left, mut right) = (0, 0);
        let mut last = act(-1, lim, 0.0).x;
        for i in 0..12000 {
            let a = act(-1, lim, i as f32 * 0.05);
            assert!(a.x.abs() <= lim, "strayed to {}", a.x);
            // No teleporting: a frame moves it a few pixels at most, flying.
            assert!((a.x - last).abs() <= 6, "jumped {} to {}", last, a.x);
            assert_eq!(a.lift > 0, a.mode == Mode::Fly);
            last = a.x;
            walked |= a.mode == Mode::Walk;
            flew |= a.mode == Mode::Fly;
            worked |= a.progress > 0.9;
            (left, right) = (left.min(a.x), right.max(a.x));
            if !jobs.contains(&a.now.job) {
                jobs.push(a.now.job);
            }
        }
        assert!(walked && flew && worked);
        assert_eq!(jobs.len(), JOBS.len());
        assert!(left < -lim * 2 / 3 && right > lim * 2 / 3, "used {left} to {right} of {lim}");
        // A strip too narrow to roam in pins the robot to the middle.
        assert_eq!(act(-1, 0, 7.0).x, 0);
    }

    #[test]
    fn a_run_that_fits_the_narrow_strip_is_in_the_same_place_in_a_wide_one() {
        let mut fitting = 0;
        for run in 0..200 {
            let x = run_start(-1, run);
            if x.abs() <= 80 {
                fitting += 1;
                assert_eq!(stop(-1, 80, run * RUN).x, x);
                assert_eq!(stop(-1, 200, run * RUN).x, x);
            }
            assert!(stop(-1, 80, run * RUN).x.abs() <= 80);
        }
        assert!(fitting > 10);
    }

    #[test]
    fn helpers_keep_different_rounds() {
        let at = |who: i64| (0..200).map(|i| act(who, 80, i as f32).x).collect::<Vec<_>>();
        assert_ne!(at(0), at(1));
        assert_ne!(at(1), at(2));
    }

    fn save(b: &Buf, zoom: i32, name: &str) {
        let (w, h) = (b.w * zoom, b.h * zoom);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let s = (((y / zoom) * b.w + x / zoom) * 4) as usize;
                let d = ((y * w + x) * 4) as usize;
                px[d..d + 4].copy_from_slice(&b.px[s..s + 4]);
            }
        }
        let dir = std::env::temp_dir().join("agent-banner");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        tiny_skia::Pixmap::from_vec(px, tiny_skia::IntSize::from_wh(w as u32, h as u32).unwrap())
            .unwrap()
            .save_png(&path)
            .unwrap();
        println!("{}", path.display());
    }

    // Frames to judge by eye, in the temp dir:
    // `cargo test --lib agent_banner::tests::sheet -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn sheet() {
        let frames = [
            ("working-day", State::Working, 0, Weather::Clear, Daytime::Day, 220, 2.0),
            ("working-day-b", State::Working, 0, Weather::Clear, Daytime::Day, 220, 13.3),
            ("working-day-c", State::Working, 0, Weather::Clear, Daytime::Day, 220, 30.0),
            ("working-helpers", State::Working, 6, Weather::Cloudy, Daytime::Day, 300, 7.5),
            ("working-helpers-b", State::Working, 6, Weather::Clear, Daytime::Day, 300, 19.0),
            ("working-dusk-rain", State::Working, 2, Weather::Rain, Daytime::Dusk, 220, 4.1),
            ("working-night", State::Working, 3, Weather::Clear, Daytime::Night, 220, 9.0),
            ("idle-dawn", State::Idle, 0, Weather::Clear, Daytime::Dawn, 220, 0.0),
            ("attention-day", State::Attention, 1, Weather::Clear, Daytime::Day, 220, 0.0),
            ("done-snow", State::Done, 0, Weather::Snow, Daytime::Day, 220, 1.0),
            ("idle-fog", State::Idle, 0, Weather::Fog, Daytime::Day, 220, 0.0),
            ("narrow", State::Working, 4, Weather::Clear, Daytime::Day, MIN_W, 3.0),
            ("wide", State::Working, 6, Weather::Clear, Daytime::Day, 640, 21.0),
        ];
        for (name, state, helpers, weather, day, w, t) in frames {
            save(&render(&scene(state, helpers, weather, day), w, t), 4, name);
        }
        // The main robot in mid flight, and everybody flying in after a resize.
        let s = scene(State::Working, 2, Weather::Clear, Daytime::Day);
        let lim = 300 / 2 - REACH;
        let aloft = (0..4000).map(|i| i as f32 * 0.05).find(|t| act(-1, lim, *t).lift > 8).unwrap();
        save(&render(&s, 300, aloft), 4, "flying");
        let mut tr = Troupe::default();
        for _ in 0..80 {
            tr.render(&s, 600, 50.0, 0.05);
        }
        tr.render(&s, 140, 50.0, 0.0);
        save(&tr.render(&s, 140, 50.0, 0.1), 4, "rescue");
    }
}
