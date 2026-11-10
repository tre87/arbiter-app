//! A test window for the agent banner (`arbiter_native::agent_banner`): one fake
//! terminal pane with the banner under its working bar, and a menu to set what the
//! pane's agent is doing. Nothing here reads a `Session`. Resize the window to see
//! the banner follow the pane, and give way when the pane is too small for it.
//! Opened with `ARBITER_OPEN_BANNER=1`; for working on the art, where a real pane
//! (`pane_banner`) only ever shows what its agent happens to be doing.

use arbiter_native::agent_banner as banner;

use super::*;

use iced::widget::{column, row};

const HEADER_H: f32 = 22.0;
const FRAME_MS: u64 = pane_banner::FRAME_MS;
// Scene seconds per second with "Fast forward" on: a day in ten seconds.
const FAST: f32 = 24.0;
const OPEN_SIZE: iced::Size = iced::Size::new(720.0, 420.0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fold {
    None,
    State,
    Time,
    Weather,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Toggle,
    Tick,
    Scale(f32),
    Menu(bool),
    Fold(Fold),
    State(banner::State),
    Helpers(usize),
    // None is the cycle.
    Time(Option<banner::Daytime>),
    // None drifts.
    Weather(Option<banner::Weather>),
    Fast(bool),
    Freeze(bool),
}

pub struct Preview {
    pub window: Option<iced::window::Id>,
    size: iced::Size,
    scale: f32,
    state: banner::State,
    helpers: usize,
    time: Option<banner::Daytime>,
    weather: Option<banner::Weather>,
    fast: bool,
    frozen: bool,
    // Scene seconds. Advances only while the scene animates.
    t: f32,
    // Frames of the clock so far, and how many of them `frame` has seen: a robot
    // flying to its place moves on the clock even while scene time stands still.
    ticks: u64,
    drawn_ticks: u64,
    troupe: banner::Troupe,
    frame: Option<iced::widget::image::Handle>,
    // What `frame` was drawn for.
    key: Option<(banner::Scene, i32, u32, u64)>,
    menu: bool,
    fold: Fold,
}

impl Default for Preview {
    fn default() -> Self {
        Preview {
            window: None,
            size: OPEN_SIZE,
            scale: 1.0,
            state: banner::State::Working,
            helpers: 0,
            time: None,
            weather: None,
            fast: false,
            frozen: false,
            t: 0.0,
            ticks: 0,
            drawn_ticks: 0,
            troupe: banner::Troupe::default(),
            frame: None,
            key: None,
            menu: false,
            fold: Fold::None,
        }
    }
}

use pane_banner::Hidden;

impl Preview {
    // Logical size of one art pixel.
    fn zoom(&self) -> f32 {
        pane_banner::zoom(self.scale)
    }

    // The banner's width in art pixels, or why the pane has no room for it: the
    // rule the real panes use, with the window as the pane.
    fn fit(&self) -> Result<i32, Hidden> {
        pane_banner::fit(self.size.width, self.size.height, self.scale)
    }

    // Scene seconds one frame of the clock is worth.
    fn frame_secs(&self) -> f32 {
        FRAME_MS as f32 / 1000.0 * if self.fast { FAST } else { 1.0 }
    }

    fn scene(&self) -> banner::Scene {
        banner::Scene {
            state: self.state,
            helpers: self.helpers,
            weather: self.weather.unwrap_or_else(|| banner::Weather::drifting(self.t)),
            day: self.time.map_or_else(|| banner::day_phase(self.t), banner::Daytime::phase),
        }
    }
}

pub fn update(state: &mut State, msg: Msg) -> Task<Message> {
    let p = &mut state.banner;
    match msg {
        Msg::Toggle => {
            if let Some(id) = p.window.take() {
                return iced::window::close(id);
            }
            let (id, open) = iced::window::open(iced::window::Settings {
                size: p.size,
                min_size: Some(iced::Size::new(120.0, 90.0)),
                ..Default::default()
            });
            p.window = Some(id);
            return open.map(|_| Message::Noop);
        }
        Msg::Tick => {
            p.ticks += 1;
            if p.scene().animates() {
                p.t += p.frame_secs();
            }
        }
        Msg::Scale(s) => {
            if s > 0.0 {
                p.scale = s;
            }
        }
        Msg::Menu(open) => {
            p.menu = open;
            p.fold = Fold::None;
        }
        Msg::Fold(f) => p.fold = f,
        Msg::State(s) => p.state = s,
        Msg::Helpers(n) => p.helpers = n.min(banner::MAX_HELPERS),
        Msg::Time(v) => p.time = v,
        Msg::Weather(v) => p.weather = v,
        Msg::Fast(v) => p.fast = v,
        Msg::Freeze(v) => p.frozen = v,
    }
    Task::none()
}

// The window opened or changed size. None when `id` is not the preview.
pub fn on_window(state: &mut State, id: iced::window::Id, size: iced::Size) -> Option<Task<Message>> {
    if state.banner.window != Some(id) {
        return None;
    }
    state.banner.size = size;
    Some(iced::window::get_scale_factor(id).map(|s| Message::Banner(Msg::Scale(s))))
}

pub fn on_closed(state: &mut State, id: iced::window::Id) {
    if state.banner.window == Some(id) {
        state.banner.window = None;
    }
}

// Draw the frame the window is about to show, if it is not the one already held.
pub fn refresh(state: &mut State) {
    let p = &mut state.banner;
    let fit = p.fit();
    let (Some(_), Ok(w)) = (p.window, fit) else {
        p.frame = None;
        p.key = None;
        if p.window.is_none() {
            p.menu = false;
            p.troupe = banner::Troupe::default();
        }
        return;
    };
    let key = (p.scene(), w, p.t.to_bits(), p.ticks);
    if p.key != Some(key) {
        let dt = (p.ticks - p.drawn_ticks) as f32 * p.frame_secs();
        p.drawn_ticks = p.ticks;
        let buf = p.troupe.render(&key.0, w, p.t, dt);
        p.frame = Some(iced::widget::image::Handle::from_rgba(buf.w as u32, buf.h as u32, buf.px));
        p.key = Some(key);
    }
}

pub fn subscription(state: &State) -> Subscription<Message> {
    let p = &state.banner;
    // A window nobody can see (minimised reaches us as a resize to nothing) and a
    // scene that holds still both run no clock.
    let seen = p.window.is_some_and(|id| !window_hidden(id)) && p.size.width >= 1.0;
    if seen && !p.frozen && (p.scene().animates() || p.troupe.settling()) {
        iced::time::every(Duration::from_millis(FRAME_MS)).map(|_| Message::Banner(Msg::Tick))
    } else {
        Subscription::none()
    }
}

pub fn view(state: &State) -> Element<'_, Message> {
    let p = &state.banner;
    let k = p.zoom();
    let fit = p.fit();
    let dark = |c: iced::Color| {
        move |_: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(c)),
            ..Default::default()
        }
    };

    let dot = match p.state {
        banner::State::Working => Dot::Working,
        banner::State::Attention => Dot::Attention,
        banner::State::Idle | banner::State::Done => Dot::Ready,
    };
    let title = row![header_dot(dot), text("Claude").size(11).color(iced::Color::from_rgb8(0x4d, 0xa6, 0xff))]
        .spacing(5)
        .align_y(iced::Center);
    let mut right = row![].spacing(4).align_y(iced::Center);
    if let Err(hidden) = &fit {
        right = right.push(banner_hidden_icon(hidden.why()));
    }
    right = right.push(
        button(cmdi(mdi_path::COG, 12.0, TXT_SECONDARY))
            .padding([2, 4])
            .on_press(Message::Banner(Msg::Menu(!p.menu)))
            .style(|_: &iced::Theme, s: button::Status| button::Style {
                background: matches!(s, button::Status::Hovered | button::Status::Pressed)
                    .then(|| iced::Background::Color(white_a(0.12))),
                border: iced::Border { radius: 3.0.into(), ..Default::default() },
                ..Default::default()
            }),
    );
    let header = container(stack![
        container(title).center_x(Length::Fill).center_y(Length::Fill),
        container(right)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right)
            .align_y(iced::alignment::Vertical::Center)
            .padding([0, 4]),
    ])
    .width(Length::Fill)
    .height(Length::Fixed(HEADER_H))
    .style(dark(iced::Color::from_rgb8(0x16, 0x1b, 0x22)));

    let mut pane = column![header];
    let bar: Element<Message> = if p.state == banner::State::Working {
        container(working_bar(p.size.width)).height(Length::Fixed(3.0)).into()
    } else {
        Space::new(Length::Fill, Length::Fixed(3.0)).into()
    };
    pane = pane.push(bar);
    if let (Ok(w), Some(frame)) = (&fit, &p.frame) {
        let art = iced::widget::image(frame.clone())
            .width(*w as f32 * k)
            .height(banner::H as f32 * k)
            .content_fit(iced::ContentFit::Fill)
            .filter_method(iced::widget::image::FilterMethod::Nearest);
        pane = pane.push(container(art).width(Length::Fill).height(Length::Fixed(banner::H as f32 * k)).clip(true));
    }

    let line = |s: String, c: iced::Color| text(s).size(12).font(iced::Font::MONOSPACE).color(c);
    let scene = p.scene();
    let mut body = column![
        line(format!("pane {:.0} x {:.0}", p.size.width, p.size.height), TXT_SECONDARY),
        line(
            match &fit {
                Ok(w) => format!("banner {w} x {} art px, {:.2} px each", banner::H, k * p.scale),
                Err(Hidden::Narrow) => "banner hidden: too narrow".to_string(),
                Err(Hidden::Low) => "banner hidden: too low".to_string(),
            },
            TXT_SECONDARY,
        ),
        line(
            format!("{} · {} · day {:.2}", p.state.title(), scene.weather.title(), scene.day),
            TXT_MUTED,
        ),
    ]
    .spacing(3);
    if let Ok(w) = &fit {
        let shown = banner::helpers_shown(*w, p.helpers);
        body = body.push(line(format!("agents {shown} of {} shown", p.helpers), TXT_MUTED));
    }
    pane = pane.push(
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(10)
            .clip(true),
    );
    let pane = container(pane)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(dark(iced::Color::from_rgb8(0x0d, 0x11, 0x17)));
    if !p.menu {
        return pane.into();
    }

    let catcher: Element<Message> = mouse_area(Space::new(Length::Fill, Length::Fill))
        .on_press(Message::Banner(Msg::Menu(false)))
        .into();
    let panel = container(menu(p))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .padding(iced::Padding { top: HEADER_H + 2.0, right: 4.0, ..Default::default() });
    stack![pane, catcher, panel].into()
}

fn menu(p: &Preview) -> Element<'_, Message> {
    let check = |on: bool| if on { "✓" } else { " " };
    let item = |label: String, msg: Msg| {
        button(text(label).size(11))
            .width(Length::Fill)
            .padding([3, 7])
            .on_press(Message::Banner(msg))
            .style(|_: &iced::Theme, s: button::Status| button::Style {
                text_color: TXT_SECONDARY,
                background: matches!(s, button::Status::Hovered | button::Status::Pressed)
                    .then(|| iced::Background::Color(white_a(0.08))),
                ..Default::default()
            })
    };
    let rule = || {
        container(Space::new(Length::Fill, Length::Fixed(1.0))).style(|_: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(white_a(0.08))),
            ..Default::default()
        })
    };
    let back = |label: &str| column![item(format!("‹ {label}"), Msg::Fold(Fold::None)), rule()].spacing(1);

    let col = match p.fold {
        Fold::State => banner::State::ALL.iter().fold(back("State"), |c, s| {
            c.push(item(format!("{} {}", check(p.state == *s), s.title()), Msg::State(*s)))
        }),
        Fold::Time => banner::Daytime::ALL.iter().fold(
            back("Time of day").push(item(format!("{} Cycle", check(p.time.is_none())), Msg::Time(None))),
            |c, d| c.push(item(format!("{} {}", check(p.time == Some(*d)), d.title()), Msg::Time(Some(*d)))),
        ),
        Fold::Weather => banner::Weather::ALL.iter().fold(
            back("Weather").push(item(format!("{} Auto", check(p.weather.is_none())), Msg::Weather(None))),
            |c, w| {
                c.push(item(format!("{} {}", check(p.weather == Some(*w)), w.title()), Msg::Weather(Some(*w))))
            },
        ),
        Fold::None => {
            let time = p.time.map_or("Cycle", banner::Daytime::title);
            let weather = p.weather.map_or("Auto", banner::Weather::title);
            let agents = row![
                container(text(format!("  Agents: {}", p.helpers)).size(11).color(TXT_SECONDARY))
                    .width(Length::Fill)
                    .padding([3, 7]),
                item("−".to_string(), Msg::Helpers(p.helpers.saturating_sub(1))).width(Length::Fixed(24.0)),
                item("+".to_string(), Msg::Helpers(p.helpers + 1)).width(Length::Fixed(24.0)),
            ]
            .align_y(iced::Center);
            column![
                item(format!("  State: {} ›", p.state.title()), Msg::Fold(Fold::State)),
                agents,
                item(format!("  Time: {time} ›"), Msg::Fold(Fold::Time)),
                item(format!("  Weather: {weather} ›"), Msg::Fold(Fold::Weather)),
                rule(),
                item(format!("{} Fast forward", check(p.fast)), Msg::Fast(!p.fast)),
                item(format!("{} Freeze motion", check(p.frozen)), Msg::Freeze(!p.frozen)),
            ]
            .spacing(1)
        }
    };

    container(col)
        .width(Length::Fixed(160.0))
        .padding(3)
        .style(|_: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb8(0x16, 0x1b, 0x22))),
            border: iced::Border { radius: 6.0.into(), width: 1.0, color: white_a(0.12) },
            ..Default::default()
        })
        .into()
}
