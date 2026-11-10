//! The agent banner (`arbiter_native::agent_banner`) at the top of each terminal
//! that runs an agent: under the pane's header and working bar, over the terminal,
//! which gives up the rows.
//!
//! Frames are drawn in `update` (`refresh`), since `view` cannot cache, and only
//! when something moved. The clock is the app's own fast tick, which runs while any
//! agent works; a banner whose agent is not working holds still and asks for
//! nothing, except for the moment a robot is flying to a new place.

use arbiter_native::agent_banner as banner;

use super::*;

// How long the banner shows a turn that just ended before it goes back to idle.
pub const DONE_MS: u64 = 30_000;
// Art pixels are this many logical pixels, rounded to whole physical ones.
const ZOOM: f32 = 2.0;
// 20 fps: the art moves in steps of a whole pixel, so more frames show nothing new.
pub const FRAME_MS: u64 = 50;
// A pane lower than this, header included, keeps all of its rows for the terminal.
const MIN_PANE_H: f32 = 280.0;
// The most a banner's clock moves in one frame, so the first frame after a pause
// (another workspace, a minimised window) is not a leap.
const MAX_CATCH_UP: u64 = 5;

// The header icon for a pane whose banner does not fit: a picture, struck through.
pub const OFF_ICON: &str = "M3,5H21V19H3Z M5,7V17H19V7Z M6.5,15.5L10,11L12.5,14L14.5,12L17.5,15.5Z \
                            M2.4,3.8L3.8,2.4L21.6,20.2L20.2,21.6Z";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hidden {
    Narrow,
    Low,
}

impl Hidden {
    pub fn why(self) -> &'static str {
        match self {
            Hidden::Narrow => "The animated banner is hidden: this terminal is too narrow to show it.",
            Hidden::Low => "The animated banner is hidden: this terminal is too low to show it.",
        }
    }
}

// Logical size of one art pixel on a display of scale factor `scale`.
pub fn zoom(scale: f32) -> f32 {
    let s = if scale > 0.0 { scale } else { 1.0 };
    (ZOOM * s).round().max(1.0) / s
}

// The banner's width in art pixels for a pane `w` by `h` logical pixels (header
// included), or why the pane has no room for it.
pub fn fit(w: f32, h: f32, scale: f32) -> Result<i32, Hidden> {
    let art = w / zoom(scale);
    if art < banner::MIN_W as f32 {
        Err(Hidden::Narrow)
    } else if h < MIN_PANE_H {
        Err(Hidden::Low)
    } else {
        Ok(art.ceil() as i32)
    }
}

#[derive(Default)]
pub struct PaneBanner {
    troupe: banner::Troupe,
    // Scene seconds: the time this pane's agent has spent working since the app
    // started. Never reset, so the day and the weather carry on from turn to turn.
    t: f32,
    // The frame of the clock (`now_ms / FRAME_MS`) this was last brought up to.
    at_frame: u64,
    frame: Option<iced::widget::image::Handle>,
    // What `frame` was drawn for.
    key: Option<(banner::Scene, i32)>,
    // The pane wants a banner and has no room for one.
    hidden: Option<Hidden>,
    // The banner is in the pane, taking rows from its terminal.
    shown: bool,
    // A robot is flying to its place, which wants frames whatever the agent does.
    settling: bool,
}

// What the banner of pane `d` should show, and how many sub-agents its agent has
// running, or None for no banner.
fn wanted(
    settings: &persist::Settings,
    done: &HashMap<String, u64>,
    d: &PaneData,
    now: u64,
) -> Option<(banner::State, usize)> {
    if !settings.show_agent_banner || !d.session.agent_running() {
        return None;
    }
    let status = d.session.agent_status();
    let state = match status.lifecycle {
        Lifecycle::Working => banner::State::Working,
        Lifecycle::Attention => banner::State::Attention,
        _ if done.get(&d.history_id).is_some_and(|until| now < *until) => banner::State::Done,
        _ => banner::State::Idle,
    };
    (state == banner::State::Working || !settings.banner_hide_idle)
        .then_some((state, status.subagents as usize))
}

// Each pane of the active workspace, with its size in the grid.
fn pane_sizes(state: &State) -> HashMap<pane_grid::Pane, iced::Size> {
    // The grid is the window less the titlebar (40), the 6 px frame on the other
    // three sides, and the file explorer.
    let w = state.main_size.width
        - 12.0
        - files_pane::shown(state).map_or(0.0, |e| e.width + files_pane::HANDLE_W);
    let h = state.main_size.height - 46.0;
    state
        .active()
        .panes
        .layout()
        .pane_regions(2.0, iced::Size::new(w.max(1.0), h.max(1.0)))
        .into_iter()
        .map(|(p, r)| (p, r.size()))
        .collect()
}

// Bring every pane's banner up to now, and draw the frames that changed.
pub fn refresh(state: &mut State) {
    let now = now_ms();
    let frame = now / FRAME_MS;
    state.turn_done.retain(|_, until| now < *until);
    if state.banners.is_empty() && !state.settings.show_agent_banner {
        return;
    }
    let scale = ui_scale();
    let sizes = pane_sizes(state);
    let active = state.active;
    let State { workspaces, banners, settings, turn_done, .. } = state;
    let mut live: HashSet<&str> = HashSet::new();
    for (wi, ws) in workspaces.iter().enumerate() {
        for (pane, d) in ws.panes.iter() {
            let want = wanted(settings, turn_done, d, now);
            // A pane that has had a banner keeps its clock for as long as it lives,
            // so an agent started again there carries on with the same day.
            if want.is_none() && !banners.contains_key(&d.history_id) {
                continue;
            }
            live.insert(&d.history_id);
            let b = banners.entry(d.history_id.clone()).or_default();
            let ticks = frame.saturating_sub(b.at_frame).min(MAX_CATCH_UP);
            let dt = ticks as f32 * FRAME_MS as f32 / 1000.0;
            b.at_frame = frame;
            if want.is_some_and(|(state, _)| state == banner::State::Working) {
                b.t += dt;
            }
            // Only the workspace on screen is drawn; the others just keep time.
            let size = if wi == active { sizes.get(pane) } else { None };
            let (Some((showing, helpers)), Some(size)) = (want, size) else {
                if wi == active {
                    b.hidden = None;
                    show(b, d, false);
                }
                b.settling = false;
                continue;
            };
            let w = match fit(size.width, size.height, scale) {
                Ok(w) => w,
                Err(why) => {
                    b.hidden = Some(why);
                    b.settling = false;
                    show(b, d, false);
                    continue;
                }
            };
            b.hidden = None;
            show(b, d, true);
            let scene = banner::Scene {
                state: showing,
                helpers,
                weather: banner::Weather::drifting(b.t),
                day: banner::day_phase(b.t),
            };
            let key = (scene, w);
            let moved = ticks > 0 && (scene.animates() || b.settling);
            if b.key != Some(key) || moved || b.frame.is_none() {
                let buf = b.troupe.render(&scene, w, b.t, dt);
                b.frame = Some(iced::widget::image::Handle::from_rgba(buf.w as u32, buf.h as u32, buf.px));
                b.key = Some(key);
                b.settling = b.troupe.settling();
            }
        }
    }
    banners.retain(|id, _| live.contains(id.as_str()));
}

// Put the banner in the pane or take it out. Either way the terminal under it
// changes height and its agent repaints, which must not read as the agent working.
fn show(b: &mut PaneBanner, d: &PaneData, shown: bool) {
    if b.shown != shown {
        b.shown = shown;
        d.session.suppress_activity(500);
    }
    if !shown {
        b.frame = None;
        b.key = None;
    }
}

// Whether a banner on screen needs frames though no agent is working.
pub fn settling(state: &State) -> bool {
    state.banners.values().any(|b| b.settling)
}

// Whether pane `d` has a banner drawn in it.
pub fn shown(state: &State, d: &PaneData) -> bool {
    state.banners.get(&d.history_id).is_some_and(|b| b.frame.is_some())
}

// Why pane `d` wants a banner and shows none, for its header.
pub fn hidden(state: &State, d: &PaneData) -> Option<&'static str> {
    state.banners.get(&d.history_id)?.hidden.map(Hidden::why)
}

// The banner's place in a pane's column, between the header and the terminal. It
// is always there, at no height when the pane has no banner, so the terminal under
// it keeps its place in the widget tree. `working` puts the working bar on top.
pub fn slot<'a>(state: &'a State, d: &PaneData, working: bool, pane_w: f32) -> Element<'a, Message> {
    let drawn = state.banners.get(&d.history_id).and_then(|b| Some((b.frame.as_ref()?, b.key?.1)));
    let Some((frame, w)) = drawn else {
        return Space::new(Length::Fill, Length::Fixed(0.0)).into();
    };
    let k = zoom(ui_scale());
    let h = banner::H as f32 * k;
    let art = iced::widget::image(frame.clone())
        .width(w as f32 * k)
        .height(h)
        .content_fit(iced::ContentFit::Fill)
        .filter_method(iced::widget::image::FilterMethod::Nearest);
    let art = container(art).width(Length::Fill).height(Length::Fixed(h)).clip(true);
    iced::widget::Stack::new().push(art).push_maybe(working.then(|| working_bar(pane_w))).into()
}
