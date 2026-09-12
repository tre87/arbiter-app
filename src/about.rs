//! `arbiter about`: the mark drawn in the terminal, then what this build is and where it
//! runs. The terminal counterpart of `neofetch`, reached from any pane as plain `arbiter`
//! (the shim directory on every pane's PATH carries a launcher for it).
//!
//! The mark is not hand-drawn: the two strokes of `assets/logo.svg` are rasterised from
//! their geometry into quadrant block characters (four sub-pixels per cell) and coloured
//! along the SVG's own gradient axes, so it is the logo at terminal resolution and can be
//! drawn at any angle. On a colour terminal it turns once about its vertical axis, like a
//! coin, easing to a stop face-on, then the facts appear beside it. Anywhere else (a pipe,
//! `NO_COLOR`, `TERM=dumb`) the face-on picture prints once, in plain text.

use std::io::{IsTerminal, Write};
use std::time::Duration;

/// The two strokes of the mark as polygons in the SVG's 800x800 space. The rounded
/// corners are dropped: a terminal sub-pixel is far coarser than they are.
const LEFT: [(f32, f32); 9] = [
    (405.9, 96.4),
    (383.3, 103.3),
    (91.8, 613.1),
    (106.4, 641.8),
    (220.7, 641.8),
    (238.1, 631.8),
    (467.3, 223.1),
    (467.1, 200.8),
    (416.9, 107.4),
];
const RIGHT: [(f32, f32); 8] = [
    (493.2, 252.1),
    (421.7, 376.7),
    (421.8, 397.4),
    (559.6, 632.0),
    (576.9, 641.8),
    (688.2, 641.8),
    (702.5, 612.0),
    (501.8, 252.4),
];

/// The mark's bounding box in that space: left, top, right, bottom.
const BOX: (f32, f32, f32, f32) = (91.0, 96.0, 704.0, 642.0);

/// The gradient axes from the SVG (`gradientUnits="userSpaceOnUse"`), start to end, and
/// their stops. A point's colour is where its projection onto the axis falls.
const LEFT_AXIS: ((f32, f32), (f32, f32)) = ((91.0, 641.0), (468.0, 96.0));
const LEFT_STOPS: [(f32, [u8; 3]); 4] = [
    (0.0, [0x88, 0xD1, 0xF1]),
    (0.18, [0x41, 0xAA, 0xDE]),
    (0.55, [0x33, 0x99, 0xFF]),
    (1.0, [0x88, 0xD1, 0xF1]),
];
const RIGHT_AXIS: ((f32, f32), (f32, f32)) = ((421.0, 378.0), (704.0, 640.0));
const RIGHT_STOPS: [(f32, [u8; 3]); 3] = [
    (0.0, [0x02, 0x7D, 0xFF]),
    (0.38, [0x13, 0x92, 0xD3]),
    (1.0, [0x00, 0x39, 0xA9]),
];

/// The mark's size in cells. A cell is about twice as tall as wide, so 27 by 12 keeps the
/// box's 613:546 proportions within a percent.
const MARK_COLS: usize = 27;
const MARK_ROWS: usize = 12;

/// Samples per sub-pixel edge; a sub-pixel is inked when more than half its samples are.
const SUPERSAMPLE: usize = 3;

/// Quadrant blocks indexed by which sub-pixels are inked: bit 0 top-left, bit 1 top-right,
/// bit 2 bottom-left, bit 3 bottom-right.
const QUADRANTS: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

/// One turn: frames and their spacing. Forty-eight frames at 40 ms is just under two
/// seconds, slow enough to read as a turn rather than a flicker.
const SPIN_FRAMES: usize = 48;
const FRAME: Duration = Duration::from_millis(40);

/// How much the back face is dimmed while it shows, so the turn reads as depth.
const BACK_FACE: f32 = 0.6;

/// What this binary is, fixed at build time (see `build.rs`).
pub struct Build {
    pub version: &'static str,
    pub commit: &'static str,
    pub date: &'static str,
    pub profile: &'static str,
    pub target: &'static str,
    pub rustc: &'static str,
}

impl Build {
    pub fn current() -> Build {
        Build {
            version: env!("CARGO_PKG_VERSION"),
            commit: option_env!("ARBITER_GIT_SHA").unwrap_or("unknown"),
            date: option_env!("ARBITER_GIT_DATE").unwrap_or("unknown"),
            profile: option_env!("ARBITER_PROFILE").unwrap_or("unknown"),
            target: option_env!("ARBITER_TARGET").unwrap_or("unknown"),
            rustc: option_env!("ARBITER_RUSTC").unwrap_or("unknown"),
        }
    }

    /// The one-line form for `arbiter --version`.
    pub fn one_line(&self) -> String {
        format!("arbiter {} ({}, {})", self.version, self.commit, self.date)
    }
}

/// Which stroke a point of the mark belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stroke {
    Left,
    Right,
}

/// Even-odd point-in-polygon.
fn inside(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut hit = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > y) != (yj > y) && x < xj + (y - yj) * (xi - xj) / (yi - yj) {
            hit = !hit;
        }
        j = i;
    }
    hit
}

/// The stroke under a point of the UNROTATED mark, if any.
fn stroke_at(x: f32, y: f32) -> Option<Stroke> {
    if inside(&LEFT, x, y) {
        Some(Stroke::Left)
    } else if inside(&RIGHT, x, y) {
        Some(Stroke::Right)
    } else {
        None
    }
}

/// Colour at `t` along a gradient's stops.
fn gradient(stops: &[(f32, [u8; 3])], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    for pair in stops.windows(2) {
        let (t0, c0) = pair[0];
        let (t1, c1) = pair[1];
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
            return [mix(c0[0], c1[0]), mix(c0[1], c1[1]), mix(c0[2], c1[2])];
        }
    }
    stops[stops.len() - 1].1
}

/// The colour of a stroke at a point of the unrotated mark: the point projected onto the
/// stroke's gradient axis.
fn colour_at(stroke: Stroke, x: f32, y: f32) -> [u8; 3] {
    let (((ax, ay), (bx, by)), stops): (_, &[(f32, [u8; 3])]) = match stroke {
        Stroke::Left => (LEFT_AXIS, &LEFT_STOPS),
        Stroke::Right => (RIGHT_AXIS, &RIGHT_STOPS),
    };
    let (dx, dy) = (bx - ax, by - ay);
    let t = ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy);
    gradient(stops, t)
}

/// The mark turned by `theta` about its vertical axis, as rows of cells. Each cell is
/// four sub-pixels, sampled on the screen grid and mapped back through the turn to the
/// unrotated mark for the hit test and the colour. `color` adds truecolor escapes.
fn mark(theta: f32, color: bool) -> Vec<String> {
    let (x0, y0, x1, y1) = BOX;
    let cx = (x0 + x1) / 2.0;
    // Screen x = cx + (x - cx) * cos; near edge-on the mark is a sliver either way, so
    // the inverse stays finite.
    let cos = theta.cos();
    let inv = 1.0 / if cos.abs() < 0.04 { 0.04f32.copysign(cos) } else { cos };
    let back = cos < 0.0;
    let sub_w = (x1 - x0) / (MARK_COLS * 2) as f32;
    let sub_h = (y1 - y0) / (MARK_ROWS * 2) as f32;
    let mut rows = Vec::with_capacity(MARK_ROWS);
    for row in 0..MARK_ROWS {
        let mut line = String::with_capacity(MARK_COLS * 4);
        let mut last: Option<[u8; 3]> = None;
        for col in 0..MARK_COLS {
            let mut bits = 0u8;
            let (mut left, mut right) = (0u32, 0u32);
            let (mut sum_x, mut sum_y, mut n) = (0.0f32, 0.0f32, 0u32);
            for q in 0..4 {
                let sx = col * 2 + (q & 1);
                let sy = row * 2 + (q >> 1);
                let mut inked = 0;
                let mut owner = (0u32, 0u32);
                for i in 0..SUPERSAMPLE {
                    for j in 0..SUPERSAMPLE {
                        let px = x0 + (sx as f32 + (i as f32 + 0.5) / SUPERSAMPLE as f32) * sub_w;
                        let py = y0 + (sy as f32 + (j as f32 + 0.5) / SUPERSAMPLE as f32) * sub_h;
                        let ux = cx + (px - cx) * inv;
                        match stroke_at(ux, py) {
                            Some(Stroke::Left) => {
                                inked += 1;
                                owner.0 += 1;
                                sum_x += ux;
                                sum_y += py;
                                n += 1;
                            }
                            Some(Stroke::Right) => {
                                inked += 1;
                                owner.1 += 1;
                                sum_x += ux;
                                sum_y += py;
                                n += 1;
                            }
                            None => {}
                        }
                    }
                }
                if inked * 2 > SUPERSAMPLE * SUPERSAMPLE {
                    bits |= 1 << q;
                    left += owner.0;
                    right += owner.1;
                }
            }
            if bits == 0 {
                line.push(' ');
                continue;
            }
            if color {
                let stroke = if left >= right { Stroke::Left } else { Stroke::Right };
                let mut rgb = colour_at(stroke, sum_x / n as f32, sum_y / n as f32);
                if back {
                    rgb = rgb.map(|c| (c as f32 * BACK_FACE).round() as u8);
                }
                if last != Some(rgb) {
                    line.push_str(&format!("\x1b[38;2;{};{};{}m", rgb[0], rgb[1], rgb[2]));
                    last = Some(rgb);
                }
            }
            line.push(QUADRANTS[bits as usize]);
        }
        if color && last.is_some() {
            line.push_str("\x1b[0m");
        }
        rows.push(line);
    }
    rows
}

/// One fact for the panel: a short label and its value. A `None` value skips the line.
type Fact = (&'static str, Option<String>);

fn facts(build: &Build) -> Vec<Fact> {
    // The build number on Windows, the kernel version elsewhere: what a bug report wants.
    let os = sysinfo::System::long_os_version().map(|name| match sysinfo::System::kernel_version() {
        Some(k) if !name.contains(&k) => {
            format!("{name} · {}{k}", if cfg!(windows) { "build " } else { "kernel " })
        }
        _ => name,
    });
    let host = sysinfo::System::host_name();
    let pane = std::env::var("ARBITER_PANE_ID").ok();
    // `$SHELL` names a POSIX shell (Git Bash sets it on Windows too); without it, a
    // Windows pane is PowerShell.
    let shell = std::env::var("SHELL")
        .ok()
        .map(|s| {
            let base = s.rsplit(['/', '\\']).next().unwrap_or(&s);
            base.strip_suffix(".exe").unwrap_or(base).to_string()
        })
        .or_else(|| cfg!(windows).then(|| "PowerShell".to_string()));
    let data = crate::shell::app_data_dir().map(|p| p.display().to_string());
    vec![
        ("Build", Some(format!("{} · {} · {}", build.commit, build.date, build.profile))),
        ("Target", Some(build.target.to_string())),
        ("Rust", Some(build.rustc.to_string())),
        ("OS", os),
        ("Host", host),
        (
            "Terminal",
            Some(match pane {
                Some(id) => format!("Arbiter pane {id} · GPU renderer"),
                None => "not inside Arbiter".to_string(),
            }),
        ),
        ("Shell", shell),
        ("Layout", layout_summary()),
        ("Data", data),
        ("Claude", claude_version()),
    ]
}

/// "2 workspaces · 5 terminals · 3 over ssh", from the saved layout.
fn layout_summary() -> Option<String> {
    use crate::persist::SavedNode;
    fn count(node: &SavedNode, total: &mut usize, remote: &mut usize) {
        match node {
            SavedNode::Split { a, b, .. } => {
                count(a, total, remote);
                count(b, total, remote);
            }
            SavedNode::Leaf { startup_cmd, .. } => {
                *total += 1;
                if startup_cmd.is_some() {
                    *remote += 1;
                }
            }
        }
    }
    let saved = crate::persist::load()?;
    let (mut total, mut remote) = (0, 0);
    for ws in &saved.workspaces {
        count(&ws.layout, &mut total, &mut remote);
    }
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    let mut s = format!("{} · {}", plural(saved.workspaces.len(), "workspace"), plural(total, "terminal"));
    if remote > 0 {
        s.push_str(&format!(" · {remote} over ssh"));
    }
    Some(s)
}

/// `claude --version`, from the real Claude when a pane's shim names it, else whatever
/// `claude` is on PATH. Given two seconds; a missing or slow Claude just drops the line.
fn claude_version() -> Option<String> {
    let real = std::env::var("ARBITER_REAL_CLAUDE").ok();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = if cfg!(windows) {
            let target = real.unwrap_or_else(|| "claude".to_string());
            std::process::Command::new("cmd").args(["/c", &target, "--version"]).output()
        } else {
            match real {
                Some(path) => std::process::Command::new(path).arg("--version").output(),
                None => std::process::Command::new("claude").arg("--version").output(),
            }
        };
        let _ = tx.send(
            output
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()),
        );
    });
    rx.recv_timeout(Duration::from_secs(2)).ok().flatten().filter(|s| !s.is_empty())
}

/// Lay the facts beside the mark: the title on the first row, a rule, then label and
/// value pairs; rows past the mark's height continue below it, indented to the same column.
fn compose(mark: &[String], build: &Build, facts: &[Fact], color: bool) -> Vec<String> {
    // The title takes the logo's saturated blue (#3399FF).
    let (bold, dim, blue, reset) =
        if color { ("\x1b[1m", "\x1b[2m", "\x1b[38;2;51;153;255m", "\x1b[0m") } else { ("", "", "", "") };
    let mut right: Vec<String> = Vec::new();
    right.push(format!("{bold}{blue}Arbiter {}{reset}", build.version));
    right.push(format!("{dim}{}{reset}", "─".repeat(12)));
    for (label, value) in facts {
        if let Some(v) = value {
            right.push(format!("{dim}{label:<9}{reset}{v}"));
        }
    }
    let rows = mark.len().max(right.len());
    let blank_mark = " ".repeat(MARK_COLS);
    (0..rows)
        .map(|i| {
            let left = mark.get(i).cloned().unwrap_or_else(|| blank_mark.clone());
            match right.get(i) {
                Some(r) => format!("{left}  {r}"),
                None => left.trim_end().to_string(),
            }
        })
        .collect()
}

/// Whether stdout wants colour and can take an animation.
fn fancy() -> bool {
    std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map_or(true, |t| t != "dumb")
}

/// The turn's angle at frame `i` of `n`: one full revolution, eased at both ends so it
/// starts and stops gently, ending face-on.
fn spin_angle(i: usize, n: usize) -> f32 {
    let t = i as f32 / n as f32;
    let eased = t * t * (3.0 - 2.0 * t);
    eased * std::f32::consts::TAU
}

/// Print it. Turning where the terminal allows, plain otherwise.
pub fn run() {
    let build = Build::current();
    let color = fancy();
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out);
    if color {
        // Redraw in place: cursor up by the mark's height after each frame. The cursor is
        // hidden meanwhile (DECTCEM), or it would be drawn wherever the redraw left it.
        let _ = write!(out, "\x1b[?25l");
        for i in 0..SPIN_FRAMES {
            for row in mark(spin_angle(i, SPIN_FRAMES), true) {
                let _ = writeln!(out, "{row}");
            }
            let _ = out.flush();
            std::thread::sleep(FRAME);
            let _ = write!(out, "\x1b[{}A", MARK_ROWS);
        }
        let _ = write!(out, "\x1b[?25h");
    }
    let facts = facts(&build);
    for line in compose(&mark(0.0, color), &build, &facts, color) {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out);
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inked(rows: &[String]) -> usize {
        rows.iter().flat_map(|r| r.chars()).filter(|c| *c != ' ').count()
    }

    // Face-on, the mark is the logo: an apex near the top centre, two feet at the bottom,
    // and daylight between the strokes at mid-height.
    #[test]
    fn face_on_it_is_two_strokes_with_a_gap() {
        let rows = mark(0.0, false);
        // Visible with `--nocapture`, which is how the rendering gets eyeballed.
        for r in &rows {
            println!("|{r}|");
        }
        assert_eq!(rows.len(), MARK_ROWS);
        assert!(rows.iter().all(|r| r.chars().count() == MARK_COLS));
        let top: Vec<usize> = rows[0].chars().enumerate().filter(|(_, c)| *c != ' ').map(|(i, _)| i).collect();
        assert!(!top.is_empty() && top.iter().all(|&i| (11..=16).contains(&i)), "apex near the centre: {top:?}");
        let bottom = &rows[MARK_ROWS - 1];
        assert_ne!(bottom.chars().nth(2), Some(' '), "left foot");
        assert_ne!(bottom.chars().nth(MARK_COLS - 2), Some(' '), "right foot");
        assert_eq!(bottom.chars().nth(MARK_COLS / 2), Some(' '), "open between the feet");
        let mid: Vec<char> = rows[MARK_ROWS / 2].chars().collect();
        let first = mid.iter().position(|c| *c != ' ').unwrap();
        let last = mid.iter().rposition(|c| *c != ' ').unwrap();
        assert!(mid[first..=last].contains(&' '), "a gap between the strokes at mid-height");
    }

    // Edge-on it is a sliver; turned around it is the mirror image, no bigger and no
    // smaller than the front.
    #[test]
    fn it_turns() {
        let front = inked(&mark(0.0, false));
        let edge = inked(&mark(std::f32::consts::FRAC_PI_2, false));
        let back = inked(&mark(std::f32::consts::PI, false));
        assert!(edge * 4 < front, "edge-on: {edge} of {front}");
        assert!((back as i32 - front as i32).abs() <= front as i32 / 10, "back {back} vs front {front}");
    }

    #[test]
    fn the_turn_starts_and_ends_face_on() {
        assert_eq!(spin_angle(0, SPIN_FRAMES), 0.0);
        assert!((spin_angle(SPIN_FRAMES, SPIN_FRAMES) - std::f32::consts::TAU).abs() < 1e-5);
        let mid = spin_angle(SPIN_FRAMES / 2, SPIN_FRAMES);
        assert!((mid - std::f32::consts::PI).abs() < 1e-4, "half way round at half time");
    }

    #[test]
    fn gradients_hit_their_stops() {
        assert_eq!(gradient(&LEFT_STOPS, 0.0), [0x88, 0xD1, 0xF1]);
        assert_eq!(gradient(&LEFT_STOPS, 0.55), [0x33, 0x99, 0xFF]);
        assert_eq!(gradient(&RIGHT_STOPS, 1.0), [0x00, 0x39, 0xA9]);
        assert_eq!(gradient(&RIGHT_STOPS, 5.0), [0x00, 0x39, 0xA9], "clamped");
        // The axis ends carry the end stops exactly; the apex, at 0.94 of the way along,
        // is nearly the light end.
        assert_eq!(colour_at(Stroke::Left, 468.0, 96.0), [0x88, 0xD1, 0xF1]);
        assert_eq!(colour_at(Stroke::Left, 91.0, 641.0), [0x88, 0xD1, 0xF1]);
        let apex = colour_at(Stroke::Left, 405.0, 100.0);
        assert!(apex[0] > 0x70 && apex[2] > 0xE8, "nearly the light end: {apex:?}");
        assert_eq!(colour_at(Stroke::Right, 704.0, 640.0), [0x00, 0x39, 0xA9]);
    }

    #[test]
    fn facts_sit_beside_the_mark_and_continue_below() {
        let build = Build::current();
        let facts: Vec<Fact> = (0..14).map(|i| ("Label", Some(format!("value {i}")))).collect();
        let lines = compose(&mark(0.0, false), &build, &facts, false);
        assert_eq!(lines.len(), 16, "title, rule, fourteen facts");
        assert!(lines[0].contains(&format!("Arbiter {}", build.version)));
        assert!(lines[15].starts_with(&" ".repeat(MARK_COLS)), "past the mark, indented to its width");
        let plain: Vec<Fact> = vec![("Skipped", None)];
        assert_eq!(compose(&mark(0.0, false), &build, &plain, false).len(), MARK_ROWS);
    }
}
