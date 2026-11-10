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
    // The banner is in the pane.
    shown: bool,
    // It has rows of its own above the terminal, which is that much lower for it.
    // Otherwise a banner in the pane lies over the terminal's first rows.
    pushing: bool,
    // The picture is moving (a robot flying to its place, sub-agents at work), which
    // wants frames whatever the agent itself is doing.
    settling: bool,
    // When `shown` last changed.
    flipped_ms: u64,
    // Since when the opposite of `shown` has been asked for without a break.
    asked_ms: Option<u64>,
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
        // An agent that starts its sub-agents in the background ends its turn and is
        // woken as each reports: by its own lifecycle it is idle in between, and the
        // work is going on all the same.
        _ if status.subagents > 0 => banner::State::Working,
        _ if done.get(&d.history_id).is_some_and(|until| now < *until) => banner::State::Done,
        _ => banner::State::Idle,
    };
    // "Idle" for hiding starts when the flag of the last turn comes down. An agent
    // waiting on background work has a turn of a second or two every so often, and a
    // banner that left at each turn's end came and went the whole time.
    let present = !settings.banner_hide_idle || matches!(state, banner::State::Working | banner::State::Done);
    present.then_some((state, status.subagents as usize))
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
            if wi != active {
                b.settling = false;
                b.asked_ms = None;
                continue;
            }
            let fitted = want.zip(sizes.get(pane)).map(|((state, helpers), size)| {
                fit(size.width, size.height, scale).map(|w| (state, helpers, w))
            });
            b.hidden = match fitted {
                Some(Err(why)) => Some(why),
                _ => None,
            };
            if !b.settle(matches!(fitted, Some(Ok(_))), !settings.banner_hide_idle, now, d) {
                b.frame = None;
                b.key = None;
                b.settling = false;
                continue;
            }
            // Held in the pane a moment past its time: the last frame stays as it is.
            let Some(Ok((showing, helpers, w))) = fitted else {
                continue;
            };
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
                // Sub-agents at work under an agent between turns move the picture
                // while nothing else keeps the app's clock running.
                b.settling = b.troupe.settling() || scene.animates();
            }
        }
    }
    banners.retain(|id, _| live.contains(id.as_str()));
}

// How long the agent's repaint after the banner's resize is kept from reading as work.
const REPAINT_MS: u64 = 800;
// A banner put in or taken out this recently is suspect of having caused whatever
// asks for the opposite now: its own resize makes the agent repaint, and a repaint
// can read as the agent working (or as it having stopped).
const RECENT_FLIP_MS: u64 = 4000;
// So the opposite must be asked for this long without a break before it is believed.
// Longer than a misread lasts (the working TTL, 2 s), or the banner would go in and
// out for as long as the pane sat there, resizing the terminal each time.
const STEADY_MS: u64 = 3000;

impl PaneBanner {
    // Whether the banner is in the pane, given whether it should be, and whether it
    // takes rows of its own (`rows`) or lies over the terminal's first ones.
    //
    // Over the terminal it comes and goes freely: nothing is resized. With rows of its
    // own, putting it in or taking it out changes the terminal's height and the agent
    // repaints, so it follows at once only if it has not just changed (see
    // `RECENT_FLIP_MS`).
    fn settle(&mut self, wanted: bool, rows: bool, now: u64, d: &PaneData) -> bool {
        if rows {
            self.follow(wanted, now);
        } else {
            self.shown = wanted;
            self.asked_ms = None;
        }
        let pushing = self.shown && rows;
        if pushing != self.pushing {
            self.pushing = pushing;
            d.session.suppress_activity(REPAINT_MS);
        }
        self.shown
    }

    // Move `shown` towards `wanted` as the rule above allows; true if it changed.
    fn follow(&mut self, wanted: bool, now: u64) -> bool {
        if wanted == self.shown {
            self.asked_ms = None;
            return false;
        }
        let asked = *self.asked_ms.get_or_insert(now);
        // Never yet changed is not recent, whatever the clock says.
        let recent = self.flipped_ms != 0 && now.saturating_sub(self.flipped_ms) < RECENT_FLIP_MS;
        if recent && now.saturating_sub(asked) < STEADY_MS {
            return false;
        }
        self.shown = wanted;
        self.flipped_ms = now;
        self.asked_ms = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The loop this guards against: the banner goes, the terminal is resized, the
    // agent's repaint reads as work for a couple of seconds, the banner comes back,
    // the terminal is resized again, and so on for as long as the pane sits idle.
    #[test]
    fn a_banner_that_just_changed_does_not_change_back_on_a_passing_signal() {
        let mut b = PaneBanner::default();
        let t = 1_000_000;
        // A turn starts, then ends a minute later: both followed at once.
        assert!(b.follow(true, t));
        assert!(b.follow(false, t + 60_000));
        // The repaint reads as work for 2.5 s: not followed, and forgotten after.
        let misread = t + 60_100;
        for ms in (0..2500).step_by(50) {
            assert!(!b.follow(true, misread + ms), "{ms} ms in");
            assert!(b.asked_ms.is_some());
        }
        assert!(!b.follow(false, misread + 2500));
        assert!(b.asked_ms.is_none() && !b.shown);
        // Real work started right after a turn end is followed once it has lasted.
        let again = t + 70_000;
        assert!(b.follow(true, again));
        assert!(b.follow(false, again + 5000), "a turn of five seconds ends at once");
        assert!(!b.follow(true, again + 5100));
        assert!(!b.follow(true, again + 5100 + STEADY_MS - 1));
        assert!(b.follow(true, again + 5100 + STEADY_MS));
    }
}

// Whether a banner on screen needs frames though no agent is working: its picture
// is moving, or a change of mind is waiting to be believed.
pub fn settling(state: &State) -> bool {
    state.banners.values().any(|b| b.settling || b.asked_ms.is_some())
}

// Whether pane `d` has a banner drawn in it.
pub fn shown(state: &State, d: &PaneData) -> bool {
    state.banners.get(&d.history_id).is_some_and(|b| b.frame.is_some())
}

// Why pane `d` wants a banner and shows none, for its header.
pub fn hidden(state: &State, d: &PaneData) -> Option<&'static str> {
    state.banners.get(&d.history_id)?.hidden.map(Hidden::why)
}

// The banner's place in a pane's column, between the header and the terminal, for
// a banner with rows of its own. It is always there, at no height otherwise, so the
// terminal under it keeps its place in the widget tree.
pub fn slot<'a>(state: &'a State, d: &PaneData, working: bool, pane_w: f32) -> Element<'a, Message> {
    art(state, d, true, working, pane_w)
        .unwrap_or_else(|| Space::new(Length::Fill, Length::Fixed(0.0)).into())
}

// The banner as a layer over the terminal's first rows, for one without rows of its
// own. The terminal is not resized for it; what it covers is hidden while it shows.
pub fn overlay<'a>(state: &'a State, d: &PaneData, working: bool, pane_w: f32) -> Option<Element<'a, Message>> {
    art(state, d, false, working, pane_w)
}

// The banner's picture, if pane `d` has one of the asked kind (`rows`: with rows of
// its own). `working` puts the working bar on top.
fn art<'a>(
    state: &'a State,
    d: &PaneData,
    rows: bool,
    working: bool,
    pane_w: f32,
) -> Option<Element<'a, Message>> {
    let b = state.banners.get(&d.history_id).filter(|b| b.pushing == rows)?;
    let (frame, w) = (b.frame.as_ref()?, b.key?.1);
    let k = zoom(ui_scale());
    let h = banner::H as f32 * k;
    let art = iced::widget::image(frame.clone())
        .width(w as f32 * k)
        .height(h)
        .content_fit(iced::ContentFit::Fill)
        .filter_method(iced::widget::image::FilterMethod::Nearest);
    let art = container(art).width(Length::Fill).height(Length::Fixed(h)).clip(true);
    Some(iced::widget::Stack::new().push(art).push_maybe(working.then(|| working_bar(pane_w))).into())
}
