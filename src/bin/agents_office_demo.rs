//! Stage-1 harness for the Agents Office (`arbiter_native::agents_office`).
//!
//! Fake desks and test buttons. Nothing here touches a `Session`, a PTY or
//! `pane_dot()`: the point is to find out whether the room reads at a glance
//! before any of that is wired up. Run it with:
//!
//! ```text
//! cargo run --bin agents-office-demo --no-default-features
//! ```
//!
//! The scale readout in the footer is the thing to watch as desks are added.
//! The room is only ever blitted at a whole-number multiple, so the number steps
//! 4x, 3x, 2x, 1x rather than sliding; a fractional scale gives unevenly sized
//! pixels, which is the single tell that reads as sloppy.

use std::time::Duration;

use arbiter_native::agents_office::{self as office, Desk, Scene, Weather};
use iced::widget::{
    button, column, container, horizontal_space, image, mouse_area, row, stack, text, Space,
};

use iced::{Alignment, Element, Length, Size, Subscription};

/// Fake `(workspace, pane)` pairs, one per slot up to the planned cap of twenty,
/// so the nameplates are judged against realistic lengths rather than against
/// "Desk 1". Workspaces run two to four desks, the way they actually do.
const NAMES: [(&str, &str); 20] = [
    ("arbiter-app", "Claude"),
    ("arbiter-app", "Claude 2"),
    ("arbiter-app", "Git"),
    ("dev-webapp", "Claude"),
    ("dev-webapp", "Powershell"),
    ("tren.dk", "Claude"),
    ("tren.dk", "SSH [Mac Mini]"),
    ("tren.dk", "Claude 2"),
    ("ha-dashboard", "Claude"),
    ("ha-dashboard", "Terminal 1"),
    ("ytdownloader", "Claude"),
    ("ytdownloader", "Claude 2"),
    ("ytdownloader", "Git"),
    ("zyre-ui", "Claude"),
    ("zyre-ui", "SSH [build-box]"),
    ("claude-stats", "Claude"),
    ("claude-stats", "Powershell"),
    ("travelpack", "Claude"),
    ("travelpack", "Claude 2"),
    ("travelpack", "Terminal 1"),
];

/// Wall-clock step per tick. ~30fps, the chunky end of the range, because
/// mechanical motion reads as a machine and smooth motion reads as a cartoon.
const TICK: Duration = Duration::from_millis(33);

/// Window size the harness opens at.
const WINDOW: Size = Size::new(1280.0, 820.0);
/// Space the header, the button rows and the padding take, so the room's scale
/// is a pure function of the window size. `iced::widget::responsive` would
/// measure this instead, but it lives behind iced's `lazy` feature and that
/// pulls a crate in for a harness that does not need one.
const CHROME: Size = Size::new(32.0, 200.0);

#[derive(Debug, Clone, Copy)]
enum Msg {
    Tick,
    /// A desk was clicked. An empty one opens a pane there, which is what the
    /// real thing will do.
    Click(usize),
    Spawn(Desk),
    SpawnRow,
    RemoveLast,
    /// Set the selected desk's state, so every state can be seen on demand.
    Set(Desk),
    ToggleFreeze,
    /// Step the sky by hand, which also drops out of auto.
    CycleWeather,
    ToggleAutoWeather,
    /// Nameplates on and off, so the room can be judged both ways.
    ToggleNames,
    Resized(Size),
}

struct Demo {
    scene: Scene,
    t: f32,
    frozen: bool,
    /// Let the sky drift on its own. Off, and out of the product: the sky is a
    /// function of `t`, so drifting it needs a clock for a decoration nobody
    /// asked for. Kept in the harness only to preview the six skies quickly.
    auto_weather: bool,
    window: Size,
}

impl Default for Demo {
    fn default() -> Self {
        // Opens in a realistic working state rather than an empty room: three
        // agents, one of them blocked, which is the case the whole design is for.
        let mut scene = Scene::default();
        for (i, d) in [Desk::Working, Desk::Attention, Desk::Done, Desk::Idle]
            .into_iter()
            .enumerate()
        {
            let (ws, pane) = NAMES[i];
            scene.spawn_named(d, ws, pane);
        }
        Demo {
            scene,
            t: 0.0,
            frozen: false,
            auto_weather: false,
            window: WINDOW,
        }
    }
}

impl Demo {
    fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Tick => {
                self.t += TICK.as_secs_f32();
                if self.auto_weather {
                    self.scene.weather = Weather::drifting(self.t);
                }
            }
            Msg::Click(i) => {
                if self.scene.desks.get(i) == Some(&Desk::Empty) {
                    self.scene.desks[i] = Desk::Working;
                }
                self.scene.selected = Some(i);
            }
            Msg::Spawn(d) => {
                let n = self.scene.occupied();
                let (ws, pane) = NAMES[n % NAMES.len()];
                let i = self.scene.spawn_named(d, ws, pane);
                self.scene.selected = Some(i);
            }
            Msg::SpawnRow => {
                for _ in 0..office::COLS {
                    let n = self.scene.occupied();
                    let (ws, pane) = NAMES[n % NAMES.len()];
                    self.scene.spawn_named(Desk::Working, ws, pane);
                }
            }
            Msg::RemoveLast => self.scene.remove_last(),
            Msg::Set(d) => {
                if let Some(i) = self.scene.selected {
                    self.scene.desks[i] = d;
                }
            }
            Msg::ToggleFreeze => self.frozen = !self.frozen,
            Msg::CycleWeather => {
                self.auto_weather = false;
                let i = Weather::ALL
                    .iter()
                    .position(|w| *w == self.scene.weather)
                    .unwrap_or(0);
                self.scene.weather = Weather::ALL[(i + 1) % Weather::ALL.len()];
            }
            Msg::ToggleAutoWeather => {
                self.auto_weather = !self.auto_weather;
                if self.auto_weather {
                    self.scene.weather = Weather::drifting(self.t);
                }
            }
            Msg::ToggleNames => self.scene.show_names = !self.scene.show_names,
            Msg::Resized(size) => self.window = size,
        }
    }

    /// No clock unless something is actually moving. A room where every agent is
    /// idle is a still image and costs nothing, which is what lets an ambient
    /// display sit beside the app's no-polling rule without lying about it.
    fn subscription(&self) -> Subscription<Msg> {
        let resize = iced::window::resize_events().map(|(_id, size)| Msg::Resized(size));
        // A turn in flight is the only thing that earns a clock. Auto weather is
        // the harness's own exception, since drifting the sky is a function of `t`;
        // the product has no such option, for exactly that reason.
        if self.frozen || !(self.scene.animates() || self.auto_weather) {
            resize
        } else {
            Subscription::batch([resize, iced::time::every(TICK).map(|_| Msg::Tick)])
        }
    }

    fn view(&self) -> Element<'_, Msg> {
        column![self.readout(), self.stage(), self.controls()]
            .spacing(14)
            .padding(16)
            .into()
    }

    fn readout(&self) -> Element<'_, Msg> {
        let s = &self.scene;
        let mut parts = vec![format!("{} panes", s.occupied())];
        for d in [Desk::Working, Desk::Attention, Desk::Done, Desk::Idle] {
            let n = s.count(d);
            if n > 0 {
                parts.push(format!("{n} {}", d.label()));
            }
        }
        let sel = match s.selected {
            Some(i) => format!("slot {i} · {}", s.desks[i].label()),
            None => "click a desk".into(),
        };
        row![
            text(parts.join("  ·  ")).size(14),
            horizontal_space(),
            text(sel)
                .size(14)
                .color(iced::Color::from_rgb8(0x8a, 0x94, 0x9e)),
        ]
        .align_y(Alignment::Center)
        .into()
    }

    /// Whole-number scale only. A fractional one gives unevenly sized pixels,
    /// which is the single thing that instantly reads as sloppy, so the room
    /// steps 4x, 3x, 2x, 1x and letterboxes rather than filling the space.
    fn scale(&self) -> f32 {
        let (lw, lh) = self.scene.size();
        let w = (self.window.width - CHROME.width).max(64.0);
        let h = (self.window.height - CHROME.height).max(64.0);
        (w / lw as f32).min(h / lh as f32).floor().max(1.0)
    }

    /// The room itself: the pixel buffer with a grid of hit targets over it, so
    /// a click lands on a desk.
    fn stage(&self) -> Element<'_, Msg> {
        let k = self.scale();
        let (lw, lh) = self.scene.size();
        let (iw, ih) = (lw as f32 * k, lh as f32 * k);

        // The room is enlarged inside `render_at`, not by the image widget, so the
        // nameplates can be drawn after the enlargement at their own size.
        let buf = office::render_at(&self.scene, self.t, k as i32);
        let pixels = image::Handle::from_rgba(buf.w as u32, buf.h as u32, buf.px);
        let room = image(pixels)
            .width(iw)
            .height(ih)
            .content_fit(iced::ContentFit::Fill)
            .filter_method(image::FilterMethod::Nearest);

        // A transparent grid of hit targets over the room, built from the same
        // layout constants the painter uses so the two cannot drift apart.
        let mut grid = column![Space::new(
            Length::Fill,
            Length::Fixed(office::CEIL_H as f32 * k)
        )];
        for r in 0..office::rows(self.scene.desks.len()) {
            let mut band = row![];
            for c in 0..office::COLS {
                let i = r as usize * office::COLS + c;
                band = band.push(
                    mouse_area(Space::new(
                        Length::Fixed(office::SLOT_W as f32 * k),
                        Length::Fixed(office::ROW_H as f32 * k),
                    ))
                    .on_press(Msg::Click(i))
                    .interaction(iced::mouse::Interaction::Pointer),
                );
            }
            grid = grid.push(band);
        }

        container(container(stack![room, grid]).width(iw).height(ih))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    fn controls(&self) -> Element<'_, Msg> {
        let chip = |label: &'static str, msg: Msg| button(text(label).size(13)).on_press(msg);
        let spawn = row![
            text("spawn")
                .size(12)
                .color(iced::Color::from_rgb8(0x8a, 0x94, 0x9e)),
            chip("Working agent", Msg::Spawn(Desk::Working)),
            chip("Needs-you agent", Msg::Spawn(Desk::Attention)),
            chip("+ a full row", Msg::SpawnRow),
            chip("Remove last", Msg::RemoveLast),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let set = row![
            text("selected")
                .size(12)
                .color(iced::Color::from_rgb8(0x8a, 0x94, 0x9e)),
            chip("Working", Msg::Set(Desk::Working)),
            chip("Needs you", Msg::Set(Desk::Attention)),
            chip("Done", Msg::Set(Desk::Done)),
            chip("Ready", Msg::Set(Desk::Ready)),
            chip("Idle", Msg::Set(Desk::Idle)),
            chip("Free the desk", Msg::Set(Desk::Empty)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let (lw, lh) = self.scene.size();
        let clock = if self.frozen {
            "frozen"
        } else if self.scene.animates() {
            "ticking 30fps"
        } else {
            "still · no clock"
        };
        let status = row![
            chip(
                if self.frozen {
                    "Unfreeze"
                } else {
                    "Freeze motion"
                },
                Msg::ToggleFreeze
            ),
            chip("Weather", Msg::CycleWeather),
            chip(
                if self.auto_weather {
                    "Auto: on"
                } else {
                    "Auto: off"
                },
                Msg::ToggleAutoWeather
            ),
            chip(
                if self.scene.show_names {
                    "Names: on"
                } else {
                    "Names: off"
                },
                Msg::ToggleNames
            ),
            horizontal_space(),
            text(format!(
                "{lw}×{lh} logical · {} rows · {}x · {} · {clock}",
                office::rows(self.scene.desks.len()),
                self.scale() as i32,
                self.scene.weather.label()
            ))
            .size(12)
            .color(iced::Color::from_rgb8(0x6c, 0x76, 0x7f)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        column![spawn, set, status].spacing(8).into()
    }
}

fn main() -> iced::Result {
    iced::application("Arbiter · Agents Office (stage 1)", Demo::update, Demo::view)
        .subscription(Demo::subscription)
        .theme(|_| iced::Theme::Dark)
        .window_size(WINDOW)
        .run()
}
