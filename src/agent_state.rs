//! Per-pane agent state, for any agent, updated event-driven (no polling): by the reader
//! thread (screen levels, spinner frames, terminal signals), by the event-log watcher
//! (`agent_watch`, the agent's hooks) and by the busy-edge process scan. `view()` reads
//! the derived lifecycle each frame.
//!
//! Each `Session` owns an `Arc<AgentHandle>`, shared with the watcher through a global
//! registry of `Weak` handles. The lifecycle is *computed* (not stored), so the threads
//! never fight over a single field; each just stamps its latest event time or level.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::agent::{AgentEvent, AgentKind};
use crate::persist::SAVE_DIRTY;
use crate::term_signal::TermSignal;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Lifecycle {
    #[default]
    Closed,
    Ready,
    Working,
    Attention,
}

/// Live agent status for one pane, as the view reads it.
#[derive(Clone, Copy, Default, Debug)]
pub struct AgentStatus {
    pub lifecycle: Lifecycle,
    pub kind: Option<AgentKind>,
    /// Moves when the pane's agent changes kind, so the UI can treat the new one as a
    /// first sighting rather than a transition of the old.
    pub epoch: u64,
    /// Shells the agent left running in the background, by its footer. Not work: the
    /// lifecycle never reads it, since a dev server can run for as long as the session.
    pub shells: u32,
}

/// What said which agent a pane runs. A weaker source never overrides a stronger one
/// while the agent is live: the hook is the agent itself speaking, the process scan saw
/// its binary, a typed command is what the user asked for, chrome is a screen reading.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum KindSource {
    Chrome = 1,
    Typed = 2,
    Scan = 3,
    Hook = 4,
}

/// One conversation the pane's agent reported.
#[derive(Clone, Debug)]
struct SessionRecord {
    id: String,
    transcript: Option<String>,
    last_active_ms: u64,
}

pub struct AgentHandle {
    /// This pane's id (== `Session.id`), set as `ARBITER_PANE_ID` on its shell, which is
    /// how the agent's hooks name the pane their events belong to.
    pub pane_id: u64,
    /// The pane's shell process, set once it is spawned; where agents are looked for.
    pub shell_pid: OnceLock<u32>,
    pub cwd: Arc<Mutex<Option<String>>>,
    /// A LOCAL agent process runs in this pane (the scan saw it, or its hook spoke).
    pub running: Arc<AtomicBool>,
    /// The bound kind as `kind as u8 + 1`, 0 for none; lock-free since the reader reads
    /// it for every chunk.
    kind: AtomicU8,
    kind_source: AtomicU8,
    epoch: AtomicU64,
    /// The conversations reported by `SessionBound`, most recently active last. One for
    /// most agents; several for one that hosts tabs.
    sessions: Mutex<Vec<SessionRecord>>,
    /// How far the watcher has applied this pane's event log.
    pub(crate) events_offset: Mutex<u64>,
    /// Last-event times (ms since epoch); 0 = never.
    activity_ms: AtomicU64, // spinner frames (working)
    stop_ms: AtomicU64,     // a turn end (hook or notification)
    /// Time of the previous detected spinner frame: entering "working" requires TWO
    /// frames an animation-gap apart, so a one-shot repaint (Shift+Tab/Enter, a single
    /// resize) that emits the star glyph once can't false-trigger working.
    last_spinner_ms: AtomicU64,
    /// Glyph fingerprint of that frame (see `agent_screen::activity_key`). The pair must
    /// draw DIFFERENT glyphs to count as an animation, so a repaint that keeps
    /// re-emitting the same static star can't false-trigger working however often it
    /// repeats. 0 = none yet.
    last_spinner_glyphs: AtomicU64,
    /// Time of the last chunk carrying any spinner glyph, suppressed or not; 0 = none. A
    /// static star in a repaint counts too, so this says whether the agent is drawing at
    /// all, not whether it is working (see `star_age_ms`).
    last_star_ms: AtomicU64,
    /// The agent's fullscreen UI is scrolled away from its live bottom (see
    /// `ScreenProfile::scrolled`), where Claude stops drawing its status row.
    scrolled: AtomicBool,
    /// Set when the transcript was scrolled away during a live turn: the turn is held as
    /// working, frames or not, until the row is back in view (then it gets a fresh TTL).
    scroll_holds_working: AtomicBool,
    /// The status row says the agent waits on agents it launched (see
    /// `ScreenProfile::waiting_rows`). Its turn has ended by every other sign, the Stop
    /// hook included, yet it resumes by itself when they report, so the pane is held as
    /// working for as long as the row shows; when the row goes, the resumed turn gets a
    /// fresh TTL to show its first frames in.
    waiting_background: AtomicBool,
    /// Background shells by the footer's count (see `AgentStatus::shells`).
    background_shells: AtomicU32,
    /// Spinner detection is ignored until this time: set briefly on app-initiated
    /// repaints (window/PTY resize) whose rapid redraws would otherwise look animated.
    suppress_until_ms: AtomicU64,
    /// A text menu/approval prompt is on the visible screen: level-triggered by the
    /// reader's grid scan. Covers AskUserQuestion / plan / proceed. It goes off only once
    /// the prompt has stayed gone `MENU_GONE_CONFIRM_MS` (see `menu_gap`).
    menu_on_screen: AtomicBool,
    /// The pending absence of the menu, by id, 0 for none. A full repaint (a resize, the
    /// wheel nudge) clears the screen before redrawing the prompt, and read at once that
    /// gap was the prompt answered and its return a new one: a second card, and a
    /// user-opened chooser turned into the agent's.
    menu_gap: AtomicU64,
    menu_gap_seq: AtomicU64,
    /// A permission hook fired, or a notification or bell said the agent needs the user
    /// (edge-triggered): cleared when the agent resumes (activity) or the turn ends.
    /// Covers tool-permission prompts that don't show a grid marker.
    hook_attention: AtomicBool,
    /// When the user last submitted a slash command here (0 = never). A chooser that
    /// appears just after one is theirs, not the agent's: no hook reports `/model`, and
    /// its footer is the one AskUserQuestion draws, so the screen alone cannot tell them
    /// apart. Cleared by activity, so a command that sets the agent working (`/init`)
    /// still reports the prompts that turn raises.
    slash_submit_ms: AtomicU64,
    /// The menu currently on screen is the one that slash command opened.
    menu_user_opened: AtomicBool,
    /// The agent says it is working (a footer hint or a working row), read
    /// level-triggered from the grid. Unlike spinner frames this survives a frozen row
    /// and a stalled read, neither of which means the turn ended.
    working_row: AtomicBool,
    /// The idle input box was on screen at the last reading (`VtTerm::chrome`).
    idle_box: AtomicBool,
    /// Moves on every edge of `working_row`. A turn end read from the row is confirmed
    /// `ROW_END_CONFIRM_MS` later, and only if this has not moved since.
    row_edge_seq: AtomicU64,
    /// When the user last sent Esc or Ctrl+C here (0 = never). A turn they interrupted
    /// did not finish, so the row leaving right after it raises nothing.
    interrupt_ms: AtomicU64,
    /// Bumped once per turn that is KNOWN to have ended: a Stop hook, a turn-end
    /// notification, or the working row leaving the screen. The notification reads this
    /// counter rather than sampling the lifecycle, so a turn end can neither be invented
    /// by a timeout nor missed by sampling at the wrong moment. Never reset: the UI
    /// compares it against the last value it saw.
    finish_seq: AtomicU64,
    /// When `finish_seq` last moved, and whether the pane has worked since. Two signals
    /// report most turn ends (the hook and the row, or the hook and the notification);
    /// the second, with no work in between, is the same finish.
    last_finish_ms: AtomicU64,
    worked_since_finish: AtomicBool,
    /// When the user last submitted a prompt, by hook (0 = never). Holds Working briefly
    /// until a level signal takes over (see `TURN_START_HOLD_MS`).
    turn_started_ms: AtomicU64,
    /// Levels from terminal signals (see `note_signal`).
    title_working: AtomicBool,
    title_attention: AtomicBool,
    progress_working: AtomicBool,
    /// The agent's own UI chrome is on this pane's screen (`VtTerm::chrome`). This is how
    /// a REMOTE agent is recognised, where the local process scan sees only `ssh` and no
    /// hook ever fires. Latched rather than timed: an idle agent produces no output, so
    /// nothing would refresh a TTL, but the moment it exits the shell prints a prompt
    /// and that scan clears this.
    on_screen: AtomicBool,
    /// The agent was on the far side when the connection ended (or, seeded on restore, at
    /// save time), so the next connection should resume it. Taken by whoever queues that
    /// connection.
    remote_pending: Mutex<Option<AgentKind>>,
    /// An agent was typed at the far shell's prompt and no other command has been typed
    /// at a shell prompt since. A second witness to a far agent, for when its screen
    /// chrome is not recognised.
    remote_typed: AtomicBool,
    /// The far agent's conversation, when Arbiter knows it: the id it completed onto a
    /// typed launch (`--session-id`), or the one typed with a resume. Kept across a drop
    /// so the next connection resumes that very conversation; cleared once the agent is
    /// seen to have exited, or says the conversation is gone.
    remote_session: Mutex<Option<String>>,
}

/// Working reverts to ready after this long without a detected spinner frame. Must
/// comfortably exceed the gap BETWEEN detected frames: Claude's ✻ bloom passes through
/// `·` frames (not in its star range) and animates slower while "thinking", so a
/// too-short TTL makes working flicker on/off between frames. The web used 2s; matching
/// it. The turn-end stays instant regardless: the Stop hook (+ post-stop guard) clears
/// working immediately; this only bounds the no-hook fallback.
const WORKING_TTL_MS: u64 = 2000;
/// After a turn end, treat the turn as over: ignore a trailing spinner frame (the final
/// redraw) and force ready for this long, so the turn-end can't flicker
/// working→ready→working.
const STOP_SUPPRESS_MS: u64 = 700;
/// Two spinner detections closer than this are one screen split across ConPTY reads,
/// not two animation frames.
const MIN_FRAME_GAP_MS: u64 = 20;
/// Two detections farther apart than this aren't a continuous animation; the later one
/// re-arms as a fresh first frame instead of confirming working. Covers the fast bloom
/// cadence; well below WORKING_TTL_MS so a one-shot can't accidentally pair with a much
/// later unrelated repaint.
const MAX_FRAME_GAP_MS: u64 = 600;
/// A chooser appearing within this long of a slash command is the one it opened.
/// Comfortably longer than an agent takes to draw it, far shorter than a turn.
const SLASH_MENU_WINDOW_MS: u64 = 3000;
/// How long a chooser has to stay off the screen to count as answered or escaped. Longer
/// than a full repaint leaves it cleared (the wheel nudge's narrower-and-back repaints
/// twice, 120 ms apart), short enough that amber still clears with the answer.
const MENU_GONE_CONFIRM_MS: u64 = 400;
/// How long the working row has to stay gone before its leaving counts as a turn end.
/// Long enough to outlast a repaint split across reads, short enough that the card
/// still arrives with the finish.
const ROW_END_CONFIRM_MS: u64 = 400;
/// A row leaving within this long of Esc or Ctrl+C is the interrupt landing, not a
/// finish. Claude takes a few hundred ms to cancel the request and redraw.
const INTERRUPT_WINDOW_MS: u64 = 1500;
/// How long after a Stop hook the background-work check is made (see `count_stop`).
/// The hook and the waiting row are written together; this covers the reader drawing it.
const STOP_CONFIRM_MS: u64 = 250;
/// A second turn end this soon after one, with no work in between, is the same one
/// reported twice (see `last_finish_ms`).
const FINISH_DEDUPE_MS: u64 = 2000;
/// How long a submitted prompt alone holds Working. Enough for the agent's first frame
/// or row to arrive; short enough that a lost turn end cannot pin the fast tick on.
const TURN_START_HOLD_MS: u64 = 5000;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Where this run's pane event logs live (see `agent_shim::EVENTS_SUBDIR`).
fn events_dir() -> Option<PathBuf> {
    Some(crate::shell::app_data_dir()?.join(crate::agent_shim::EVENTS_SUBDIR))
}

impl AgentHandle {
    pub fn new(pane_id: u64, cwd: Arc<Mutex<Option<String>>>, running: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            pane_id,
            shell_pid: OnceLock::new(),
            cwd,
            running,
            kind: AtomicU8::new(0),
            kind_source: AtomicU8::new(0),
            epoch: AtomicU64::new(0),
            sessions: Mutex::new(Vec::new()),
            events_offset: Mutex::new(0),
            activity_ms: AtomicU64::new(0),
            stop_ms: AtomicU64::new(0),
            last_spinner_ms: AtomicU64::new(0),
            last_spinner_glyphs: AtomicU64::new(0),
            last_star_ms: AtomicU64::new(0),
            scrolled: AtomicBool::new(false),
            scroll_holds_working: AtomicBool::new(false),
            waiting_background: AtomicBool::new(false),
            background_shells: AtomicU32::new(0),
            suppress_until_ms: AtomicU64::new(0),
            menu_on_screen: AtomicBool::new(false),
            menu_gap: AtomicU64::new(0),
            menu_gap_seq: AtomicU64::new(0),
            hook_attention: AtomicBool::new(false),
            slash_submit_ms: AtomicU64::new(0),
            menu_user_opened: AtomicBool::new(false),
            working_row: AtomicBool::new(false),
            idle_box: AtomicBool::new(false),
            row_edge_seq: AtomicU64::new(0),
            interrupt_ms: AtomicU64::new(0),
            finish_seq: AtomicU64::new(0),
            last_finish_ms: AtomicU64::new(0),
            worked_since_finish: AtomicBool::new(false),
            turn_started_ms: AtomicU64::new(0),
            title_working: AtomicBool::new(false),
            title_attention: AtomicBool::new(false),
            progress_working: AtomicBool::new(false),
            on_screen: AtomicBool::new(false),
            remote_pending: Mutex::new(None),
            remote_typed: AtomicBool::new(false),
            remote_session: Mutex::new(None),
        })
    }

    // ── Which agent ──────────────────────────────────────────────────────────────

    /// The agent bound to this pane, if any.
    pub fn kind(&self) -> Option<AgentKind> {
        match self.kind.load(Ordering::Relaxed) {
            0 => None,
            i => AgentKind::from_index(i - 1),
        }
    }

    fn kind_source(&self) -> u8 {
        self.kind_source.load(Ordering::Relaxed)
    }

    /// An agent is running here right now, locally or on the far side of a connection.
    fn live(&self) -> bool {
        self.running.load(Ordering::Relaxed) || self.on_screen()
    }

    /// `source` says `kind` runs here. A different kind than the one bound replaces it
    /// (resetting the lifecycle, since those readings were the other agent's), unless the
    /// bound one is still live and came from a source at least as strong. True when
    /// `kind` is the bound agent afterwards.
    pub fn bind(&self, kind: AgentKind, source: KindSource) -> bool {
        let cur = self.kind();
        let cur_source = self.kind_source();
        if cur == Some(kind) {
            if source as u8 > cur_source {
                self.kind_source.store(source as u8, Ordering::Relaxed);
            }
            return true;
        }
        if cur.is_some() && (source as u8) <= cur_source && self.live() {
            return false;
        }
        // A switch between two agents is a new agent to the UI (see `epoch`); binding
        // one where none was bound is not, or a launch that opens on a chooser would
        // read as a first sighting and raise no card.
        if cur.is_some() {
            self.reset_lifecycle();
            self.epoch.fetch_add(1, Ordering::Relaxed);
        }
        self.kind.store(kind as u8 + 1, Ordering::Relaxed);
        self.kind_source.store(source as u8, Ordering::Relaxed);
        crate::debug::debug_log(&format!("pane {}: agent {kind:?} bound by {source:?}", self.pane_id));
        true
    }

    fn clear_kind(&self) {
        self.kind.store(0, Ordering::Relaxed);
        self.kind_source.store(0, Ordering::Relaxed);
    }

    /// Every reading that belonged to the previous agent. `finish_seq` survives: the UI
    /// compares it against the last value it saw, so it must never go back.
    fn reset_lifecycle(&self) {
        for t in [
            &self.activity_ms,
            &self.stop_ms,
            &self.last_spinner_ms,
            &self.last_spinner_glyphs,
            &self.last_star_ms,
            &self.suppress_until_ms,
            &self.slash_submit_ms,
            &self.interrupt_ms,
            &self.turn_started_ms,
            &self.menu_gap,
        ] {
            t.store(0, Ordering::Relaxed);
        }
        for f in [
            &self.scrolled,
            &self.scroll_holds_working,
            &self.waiting_background,
            &self.menu_on_screen,
            &self.hook_attention,
            &self.menu_user_opened,
            &self.working_row,
            &self.idle_box,
            &self.title_working,
            &self.title_attention,
            &self.progress_working,
        ] {
            f.store(false, Ordering::Relaxed);
        }
        self.background_shells.store(0, Ordering::Relaxed);
        self.sessions.lock().unwrap().clear();
    }

    /// The local agent is gone: the shell's prompt came back. Forgets its conversations
    /// and its event log, so a late hook from the process that just exited cannot mark
    /// the next command in this pane as an agent. Under the log's lock, which the
    /// watcher applies events under: a `SessionBound` applied between the caller seeing
    /// the prompt and this would otherwise mark the pane running again for good.
    pub fn end_session(&self) {
        let mut offset = self.events_offset.lock().unwrap();
        self.running.store(false, Ordering::Relaxed);
        self.sessions.lock().unwrap().clear();
        for f in [&self.hook_attention, &self.title_working, &self.title_attention, &self.progress_working] {
            f.store(false, Ordering::Relaxed);
        }
        self.turn_started_ms.store(0, Ordering::Relaxed);
        self.background_shells.store(0, Ordering::Relaxed);
        if !self.on_screen() {
            self.clear_kind();
        }
        *offset = 0;
        if let Some(dir) = events_dir() {
            let _ = std::fs::remove_file(dir.join(format!("{}.jsonl", self.pane_id)));
        }
    }

    // ── The far side of a connection ─────────────────────────────────────────────

    /// The agent half of a connection edge (see `session::connection_edge`). A new
    /// connection is the new truth: whoever queued it took the pending flag first. At its
    /// end, an agent still on the far side is what the next connection brings back, by
    /// either witness.
    pub fn connection_edge(&self, on: bool) {
        if on {
            self.remote_typed.store(false, Ordering::Relaxed);
            *self.remote_pending.lock().unwrap() = None;
            return;
        }
        let on_screen = self.on_screen.swap(false, Ordering::Relaxed);
        let typed = self.remote_typed.swap(false, Ordering::Relaxed);
        if on_screen || typed {
            *self.remote_pending.lock().unwrap() = self.kind();
            SAVE_DIRTY.store(true, Ordering::Relaxed);
        }
        if !self.running.load(Ordering::Relaxed) {
            self.clear_kind();
        }
    }

    /// The agent half of forgetting a connection the user ended themselves.
    pub fn forget_remote(&self) {
        *self.remote_session.lock().unwrap() = None;
        *self.remote_pending.lock().unwrap() = None;
        self.remote_typed.store(false, Ordering::Relaxed);
    }

    /// Reader: `kind`'s chrome is on screen. True when that newly latches the pane.
    pub fn latch_screen(&self, kind: AgentKind) -> bool {
        self.bind(kind, KindSource::Chrome);
        let newly = !self.on_screen.swap(true, Ordering::Relaxed);
        if newly {
            SAVE_DIRTY.store(true, Ordering::Relaxed);
            crate::debug::debug_log(&format!("remote agent {kind:?}: chrome on screen"));
        }
        newly
    }

    /// Reader: no agent's chrome is on screen. Absent only latches OFF when no spinner
    /// frame is fresh, because during a turn Claude replaces its idle hint line with the
    /// interrupt hint: clearing on that would drop the pane's agent state mid-answer and
    /// take the status dot with it. A real exit has no spinner either, so it still clears
    /// on the very next chunk (the returning shell prompt).
    pub fn unlatch_screen(&self) {
        if self.activity_fresh() || !self.on_screen.swap(false, Ordering::Relaxed) {
            return;
        }
        // Its screen went away with no turn in flight: the agent exited, whatever was typed.
        self.waiting_background.store(false, Ordering::Relaxed);
        self.background_shells.store(0, Ordering::Relaxed);
        self.remote_typed.store(false, Ordering::Relaxed);
        *self.remote_session.lock().unwrap() = None;
        if !self.running.load(Ordering::Relaxed) {
            self.clear_kind();
        }
        SAVE_DIRTY.store(true, Ordering::Relaxed);
        crate::debug::debug_log("remote agent: chrome left the screen");
    }

    /// Keyboard: an agent was typed at the far prompt (`Some`), or another command was
    /// typed at a shell prompt (`None`), which is only possible once the agent has exited.
    pub fn set_remote_typed(&self, kind: Option<AgentKind>) {
        let typed = kind.is_some();
        if let Some(k) = kind {
            self.bind(k, KindSource::Typed);
        }
        if self.remote_typed.swap(typed, Ordering::Relaxed) != typed {
            SAVE_DIRTY.store(true, Ordering::Relaxed);
        }
        if !typed && !self.live() {
            self.clear_kind();
        }
    }

    /// Which agent is (or, for an ended connection, was) running on the far side, by any
    /// witness. This is what the saved layout carries for a remote pane.
    pub fn remote_agent(&self) -> Option<AgentKind> {
        if self.on_screen() || self.remote_typed.load(Ordering::Relaxed) {
            return self.kind();
        }
        *self.remote_pending.lock().unwrap()
    }

    /// Take the "resume the agent on the next connection" flag.
    pub fn take_remote_pending(&self) -> Option<AgentKind> {
        self.remote_pending.lock().unwrap().take()
    }

    /// Seed from a saved layout.
    pub fn seed_remote_agent(&self, kind: Option<AgentKind>) {
        *self.remote_pending.lock().unwrap() = kind;
    }

    /// The far agent's conversation as far as Arbiter can know it (see the field). `None`
    /// when it cannot: a continue-last typed by hand, the agent gone, or its id refused.
    pub fn set_remote_session(&self, id: Option<String>) {
        let mut cur = self.remote_session.lock().unwrap();
        if *cur != id {
            *cur = id;
            SAVE_DIRTY.store(true, Ordering::Relaxed);
        }
    }

    pub fn remote_session(&self) -> Option<String> {
        self.remote_session.lock().unwrap().clone()
    }

    /// Seed from a saved layout.
    pub fn seed_remote_session(&self, id: Option<&str>) {
        *self.remote_session.lock().unwrap() = id.map(str::to_string);
    }

    /// An agent is running in this pane as far as the SCREEN is concerned (remote panes).
    pub fn on_screen(&self) -> bool {
        self.on_screen.load(Ordering::Relaxed)
    }

    // ── Events from hooks and signals ────────────────────────────────────────────

    /// Apply one event `kind`'s hook reported, for its conversation `session`. Only a
    /// `SessionBound` is taken from an agent not (or no longer) running here: anything
    /// else arriving then is a late hook from a process that has exited.
    pub fn apply(self: &Arc<Self>, kind: AgentKind, session: Option<&str>, event: &AgentEvent) {
        if let AgentEvent::SessionBound { id, transcript } = event {
            if !self.bind(kind, KindSource::Hook) {
                return;
            }
            if !self.running.swap(true, Ordering::Relaxed) {
                SAVE_DIRTY.store(true, Ordering::Relaxed);
            }
            self.record_session(kind, id, transcript.as_deref());
            return;
        }
        if !self.running.load(Ordering::Relaxed) || self.kind() != Some(kind) {
            return;
        }
        if let Some(id) = session {
            self.touch_session(id);
        }
        match event {
            AgentEvent::SessionBound { .. } => {}
            AgentEvent::TurnStarted => {
                self.turn_started_ms.store(now_ms(), Ordering::Relaxed);
                self.hook_attention.store(false, Ordering::Relaxed);
                self.worked_since_finish.store(true, Ordering::Relaxed);
            }
            AgentEvent::NeedsAttention => self.hook_attention.store(true, Ordering::Relaxed),
            AgentEvent::TurnEnded => self.turn_ended(),
            AgentEvent::Interrupted => {
                let now = now_ms();
                self.interrupt_ms.store(now, Ordering::Relaxed);
                self.stop_ms.store(now, Ordering::Relaxed);
                self.hook_attention.store(false, Ordering::Relaxed);
            }
            AgentEvent::SessionEnded => self.hook_attention.store(false, Ordering::Relaxed),
        }
    }

    fn turn_ended(self: &Arc<Self>) {
        self.stop_ms.store(now_ms(), Ordering::Relaxed);
        self.hook_attention.store(false, Ordering::Relaxed);
        self.count_stop();
    }

    fn record_session(&self, kind: AgentKind, id: &str, transcript: Option<&str>) {
        let mut sessions = self.sessions.lock().unwrap();
        let prev = sessions.iter().position(|s| s.id == id).map(|i| sessions.remove(i));
        if !kind.agent().multi_session() {
            sessions.clear();
        }
        let transcript = transcript.map(str::to_string).or_else(|| prev.as_ref().and_then(|p| p.transcript.clone()));
        let changed = prev.as_ref().map_or(true, |p| p.transcript != transcript);
        sessions.push(SessionRecord { id: id.to_string(), transcript, last_active_ms: now_ms() });
        if changed {
            SAVE_DIRTY.store(true, Ordering::Relaxed);
        }
    }

    fn touch_session(&self, id: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(i) = sessions.iter().position(|s| s.id == id) {
            let mut s = sessions.remove(i);
            s.last_active_ms = now_ms();
            sessions.push(s);
        }
    }

    /// Reader: a title, bell, progress report or notification the agent emitted, read by
    /// its `SignalProfile`. Titles and progress are levels (each report replaces the
    /// last); a bell or a notification is an edge, like a hook.
    pub fn note_signal(self: &Arc<Self>, signal: &TermSignal) {
        let Some(kind) = self.kind() else { return };
        let p = kind.agent().signals();
        match signal {
            TermSignal::Title(title) => {
                let working = title.chars().next().is_some_and(|c| p.title_working.iter().any(|g| g.contains(c)));
                if working {
                    self.worked_since_finish.store(true, Ordering::Relaxed);
                }
                self.title_working.store(working, Ordering::Relaxed);
                let attention = p.title_attention.iter().any(|m| title.contains(m));
                self.title_attention.store(attention, Ordering::Relaxed);
            }
            TermSignal::Bell if p.bell_is_attention => self.hook_attention.store(true, Ordering::Relaxed),
            TermSignal::Progress { .. } if p.progress_is_working => {
                let busy = signal.is_busy_progress();
                if busy {
                    self.worked_since_finish.store(true, Ordering::Relaxed);
                }
                self.progress_working.store(busy, Ordering::Relaxed);
            }
            TermSignal::Notify { body, .. } => {
                if p.notify_attention.iter().any(|m| body.starts_with(m)) {
                    self.hook_attention.store(true, Ordering::Relaxed);
                } else if p.notify_is_turn_end {
                    self.turn_ended();
                }
            }
            _ => {}
        }
    }

    // ── Screen levels and spinner frames ─────────────────────────────────────────

    /// Whether a spinner frame landed recently enough to count as an in-flight turn.
    fn activity_fresh(&self) -> bool {
        let act = self.activity_ms.load(Ordering::Relaxed);
        act != 0 && now_ms().saturating_sub(act) < WORKING_TTL_MS
    }

    /// Reader: a chunk carrying spinner glyphs arrived, `glyphs` being the fingerprint of
    /// the distinct ones in it (`agent_screen::activity_key`). Also resolves any pending
    /// permission attention: the agent has resumed, so it's working, not waiting.
    pub fn note_activity(&self, glyphs: u64) {
        let now = now_ms();
        self.last_star_ms.store(now, Ordering::Relaxed);
        let stop = self.stop_ms.load(Ordering::Relaxed);
        // A spinner frame inside the post-Stop window is the turn's FINAL redraw: ignore
        // it so it can't revive "working" after Stop already ended the turn. (A genuinely
        // new turn's frames land well after the window.)
        if now.saturating_sub(stop) < STOP_SUPPRESS_MS {
            return;
        }
        let act = self.activity_ms.load(Ordering::Relaxed);
        let working_now = act > stop && now.saturating_sub(act) < WORKING_TTL_MS;
        if working_now {
            // Already working: any frame sustains it (bridging the slow `·` frames that
            // aren't in the star range), the established behaviour. Sustaining is
            // deliberately NOT suppressed: a resize or a scroll during a real turn must
            // not drop the working state the agent is genuinely in.
            self.activity_ms.store(now, Ordering::Relaxed);
            self.hook_attention.store(false, Ordering::Relaxed);
            return;
        }
        // Not working, so this pair would ENTER it. Skip frames inside an app-initiated
        // repaint window (resize, edit key, scroll), whose rapid redraws would otherwise
        // look like a cycling spinner.
        if now < self.suppress_until_ms.load(Ordering::Relaxed) {
            return;
        }
        // Entering needs a SECOND frame an animation-gap after the first, drawing a
        // DIFFERENT glyph. A one-shot repaint (Shift+Tab/Enter, single resize) emits the
        // star once and never pairs; a repeated repaint (scrolling Claude's transcript
        // past a "✻ Brewed for 7s" thinking summary) re-emits the SAME star and so never
        // pairs either. The bloom advances a frame each time, so it always does.
        let prev = self.last_spinner_ms.swap(now, Ordering::Relaxed);
        let prev_glyphs = self.last_spinner_glyphs.swap(glyphs, Ordering::Relaxed);
        let gap = now.saturating_sub(prev);
        if prev != 0 && glyphs != prev_glyphs && (MIN_FRAME_GAP_MS..=MAX_FRAME_GAP_MS).contains(&gap) {
            self.activity_ms.store(now, Ordering::Relaxed);
            self.hook_attention.store(false, Ordering::Relaxed);
            self.worked_since_finish.store(true, Ordering::Relaxed);
            // The agent has genuinely started a turn, so the last slash command is spent:
            // whatever it puts up now is its own (`/init` asking to write a file).
            // Cleared here, in the branch that ENTERS working, rather than on every
            // frame: a lone star in a repaint does not pair and so must not count, and
            // clearing on any frame at all made the latch easy to lose.
            self.slash_submit_ms.store(0, Ordering::Relaxed);
        }
    }

    /// Don't let spinner frames START working for `dur_ms`. Called when an action that
    /// doesn't start the agent working causes a repaint: a window/PTY resize, an edit key
    /// (Shift+Enter newline, Shift+Tab mode-cycle) on Windows where ConPTY repaints the
    /// region, or a wheel notch handed to a mouse-reporting agent, which redraws its
    /// whole screen. Each re-emits on-screen stars that would otherwise pair into a
    /// false "working". An already-working turn keeps being sustained (see
    /// `note_activity`), so this can never blank a genuine working state.
    pub fn suppress_activity(&self, dur_ms: u64) {
        // Extend, never shorten, an existing window: a burst of edit keys each pushes it
        // out, so every repaint stays covered.
        let until = now_ms() + dur_ms;
        if until > self.suppress_until_ms.load(Ordering::Relaxed) {
            self.suppress_until_ms.store(until, Ordering::Relaxed);
        }
        // Drop any half-formed pair so a frame landing just past the window can't pair
        // with one from before it.
        self.last_spinner_ms.store(0, Ordering::Relaxed);
        self.last_spinner_glyphs.store(0, Ordering::Relaxed);
    }

    /// Resume spinner detection immediately: called on a SUBMIT (Enter). Real working is
    /// imminent after a submit, so an edit-key/resize suppression window must not delay it.
    pub fn clear_suppression(&self) {
        self.suppress_until_ms.store(0, Ordering::Relaxed);
    }

    /// Reader: whether a menu/approval prompt is on the visible screen, and whether the
    /// input box currently holds a slash command.
    ///
    /// Two things make a menu the user's. Claude lists its slash commands as soon as `/`
    /// is typed, so a box reading `/mod` is already showing one: that is what
    /// `slash_input` catches, before Enter is pressed at all. Then Enter clears the box
    /// and the chooser proper opens, which `slash_submit_ms` covers. Once either has
    /// claimed the menu it stays claimed until the menu leaves, so the handover between
    /// the two cannot show a gap.
    ///
    /// Leaving is confirmed, not read: see `menu_gap`.
    pub fn set_menu(self: &Arc<Self>, on: bool, slash_input: bool) {
        if on {
            self.menu_gap.store(0, Ordering::Relaxed);
            let was = self.menu_on_screen.swap(true, Ordering::Relaxed);
            let submitted = self.slash_submit_ms.load(Ordering::Relaxed);
            let just_submitted = submitted != 0 && now_ms().saturating_sub(submitted) < SLASH_MENU_WINDOW_MS;
            let mine = slash_input || just_submitted;
            if mine {
                self.menu_user_opened.store(true, Ordering::Relaxed);
            }
            if !was && crate::debug::debug_enabled() {
                crate::debug::debug_log(&format!(
                    "menu on: slash_input={slash_input} just_submitted={just_submitted} \
                     submitted_ms_ago={} hook_attention={} -> user_opened={mine}",
                    if submitted == 0 { -1 } else { now_ms().saturating_sub(submitted) as i64 },
                    self.hook_attention.load(Ordering::Relaxed),
                ));
            }
            return;
        }
        if !self.menu_on_screen.load(Ordering::Relaxed) || self.menu_gap.load(Ordering::Relaxed) != 0 {
            return;
        }
        let gap = self.menu_gap_seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.menu_gap.store(gap, Ordering::Relaxed);
        let weak = Arc::downgrade(self);
        let at = std::time::Instant::now() + Duration::from_millis(MENU_GONE_CONFIRM_MS);
        crate::session::schedule(at, move || {
            if let Some(h) = weak.upgrade() {
                if h.confirm_menu_gone(gap) {
                    crate::session::wake_ui();
                }
            }
        });
    }

    /// The menu has stayed gone since `gap` began: it was answered or escaped. True when
    /// that is so; false when it came back meanwhile.
    fn confirm_menu_gone(&self, gap: u64) -> bool {
        if self.menu_gap.compare_exchange(gap, 0, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            return false;
        }
        self.menu_on_screen.store(false, Ordering::Relaxed);
        self.menu_user_opened.store(false, Ordering::Relaxed);
        // The command is spent on the chooser it opened. Anything the agent puts up
        // afterwards is its own, even within the window.
        self.slash_submit_ms.store(0, Ordering::Relaxed);
        // AskUserQuestion fires a permission hook, but escaping it produces no spinner or
        // Stop to clear that sticky hook attention, so a menu leaving clears it. A
        // markerless prompt never sets a menu, so it still clears via activity or Stop.
        self.hook_attention.store(false, Ordering::Relaxed);
        crate::debug::debug_log(&format!("pane {}: menu gone", self.pane_id));
        true
    }

    /// UI: the user submitted a slash command in this pane (see `slash_submit_ms`).
    ///
    /// Recorded only while the agent is not mid-turn. A command typed at an idle prompt
    /// opens a chooser that belongs to the user; one typed while the agent is working
    /// (or `/init`, which sets it working) is followed by prompts that are the agent's,
    /// and those must still be reported.
    pub fn note_slash_command(&self) {
        if self.lifecycle() == Lifecycle::Working {
            return;
        }
        self.slash_submit_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Reader: whether the agent's working status row is on screen, and whether its idle
    /// input box is. The row going away *and the box coming back* is a turn end we can
    /// see, which is what panes with no hooks (every remote one) rely on instead of a
    /// silence in the spinner stream.
    ///
    /// Requiring the box is what makes it safe: a resize or a reflow can catch the agent
    /// mid-redraw, with neither the row nor the box on screen, and that must not read as
    /// a finish. Claude swaps one hint for the other, so exactly one is present whenever
    /// it is drawing at all.
    ///
    /// The edge is one sampled chunk, and a repaint split across reads (ConPTY rewrites
    /// rows piecemeal) can drop the row for one of them while a mode line keeps the box
    /// reading as present. So the finish is only counted if the screen still says so
    /// `ROW_END_CONFIRM_MS` later (`confirm_row_end`).
    pub fn set_working_row(self: &Arc<Self>, on: bool, idle_box: bool) {
        if let Some(edge) = self.working_row_edge(on, idle_box) {
            let weak = Arc::downgrade(self);
            let at = std::time::Instant::now() + Duration::from_millis(ROW_END_CONFIRM_MS);
            crate::session::schedule(at, move || {
                if let Some(h) = weak.upgrade() {
                    if h.confirm_row_end(edge) {
                        crate::session::wake_ui();
                    }
                }
            });
        }
    }

    /// Records a reading of the row and the box. `Some(edge)` when it is a candidate turn
    /// end, to be passed to `confirm_row_end` once the confirmation delay has passed.
    fn working_row_edge(&self, on: bool, idle_box: bool) -> Option<u64> {
        self.idle_box.store(idle_box, Ordering::Relaxed);
        let was = self.working_row.swap(on, Ordering::Relaxed);
        if was == on {
            return None;
        }
        if on {
            self.worked_since_finish.store(true, Ordering::Relaxed);
        }
        let edge = self.row_edge_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let ended = !on
            && idle_box
            && !self.waiting_background.load(Ordering::Relaxed)
            && !self.interrupted_recently()
            && now_ms() >= self.suppress_until_ms.load(Ordering::Relaxed);
        if crate::debug::debug_enabled() {
            crate::debug::debug_log(&format!(
                "working row {}: idle_box={idle_box} candidate_end={ended}",
                if on { "on" } else { "off" }
            ));
        }
        ended.then_some(edge)
    }

    /// Counts the turn end read at `edge` if nothing has contradicted it since: the row
    /// has not come back, the box is still there, no chooser has opened (a permission
    /// prompt replacing the row is a question, not a finish) and the user did not
    /// interrupt. True when it was counted.
    fn confirm_row_end(&self, edge: u64) -> bool {
        let holds = self.row_edge_seq.load(Ordering::Relaxed) == edge
            && !self.working_row.load(Ordering::Relaxed)
            && self.idle_box.load(Ordering::Relaxed)
            && !self.menu_on_screen.load(Ordering::Relaxed)
            && !self.waiting_background.load(Ordering::Relaxed)
            && !self.interrupted_recently();
        holds && self.count_finish()
    }

    /// A reported turn end (a Stop hook, a turn-end notification): the authoritative one.
    /// Not counted while the agent is waiting on agents it launched, since Claude's hook
    /// fires then too and it resumes by itself. The hook can be handled before the reader
    /// has seen the row that says so, so the check is made `STOP_CONFIRM_MS` later rather
    /// than at once.
    fn count_stop(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let at = std::time::Instant::now() + Duration::from_millis(STOP_CONFIRM_MS);
        crate::session::schedule(at, move || {
            let Some(h) = weak.upgrade() else { return };
            if !h.waiting_background.load(Ordering::Relaxed) && h.count_finish() {
                crate::session::wake_ui();
            }
        });
    }

    /// Moves `finish_seq` for one turn end, unless it is the one just counted reported
    /// again (see `last_finish_ms`). True when counted.
    fn count_finish(&self) -> bool {
        let now = now_ms();
        let last = self.last_finish_ms.load(Ordering::Relaxed);
        if last != 0
            && now.saturating_sub(last) < FINISH_DEDUPE_MS
            && !self.worked_since_finish.load(Ordering::Relaxed)
        {
            return false;
        }
        self.finish_seq.fetch_add(1, Ordering::Relaxed);
        self.last_finish_ms.store(now, Ordering::Relaxed);
        self.worked_since_finish.store(false, Ordering::Relaxed);
        true
    }

    /// UI: the user sent Esc or Ctrl+C to this pane.
    pub fn note_interrupt(&self) {
        self.interrupt_ms.store(now_ms(), Ordering::Relaxed);
    }

    fn interrupted_recently(&self) -> bool {
        let at = self.interrupt_ms.load(Ordering::Relaxed);
        at != 0 && now_ms().saturating_sub(at) < INTERRUPT_WINDOW_MS
    }

    /// How many turns are known to have ended here (see `finish_seq`).
    pub fn finish_seq(&self) -> u64 {
        self.finish_seq.load(Ordering::Relaxed)
    }

    /// Reader: whether the transcript is scrolled away from its live bottom. While it
    /// is, Claude draws no spinner frames, so a turn that was live when the scroll began
    /// is held as working rather than read as over; when the view returns, the turn gets
    /// a fresh TTL to resume in (the nudge in the UI sees to that).
    pub fn set_scrolled(&self, on: bool) {
        let was = self.scrolled.swap(on, Ordering::Relaxed);
        if on == was {
            return;
        }
        let now = now_ms();
        let act = self.activity_ms.load(Ordering::Relaxed);
        let stop = self.stop_ms.load(Ordering::Relaxed);
        // Waiting on background work counts as live even though the Stop hook has
        // already fired: the agent resumes by itself, so scrolling away mid-wait must
        // hold the turn exactly as scrolling away mid-spinner does.
        let live = (act > stop && now.saturating_sub(act) < WORKING_TTL_MS)
            || self.waiting_background.load(Ordering::Relaxed)
            || self.working_row.load(Ordering::Relaxed);
        if on {
            self.scroll_holds_working.store(live, Ordering::Relaxed);
        } else if self.scroll_holds_working.swap(false, Ordering::Relaxed) {
            self.activity_ms.store(now, Ordering::Relaxed);
        }
    }

    /// Whether the transcript is currently scrolled away from its live bottom.
    pub fn scrolled(&self) -> bool {
        self.scrolled.load(Ordering::Relaxed)
    }

    /// Reader: whether the status row says the agent waits on agents it launched. While it
    /// does the pane is working (see `waiting_background`); when the row goes, the turn
    /// the agent resumes with gets a fresh TTL, so the moment before its first frames pair
    /// up is not read as a turn end. A wait the user broke off with Escape ends the same
    /// way, one TTL later.
    pub fn set_waiting_background(&self, on: bool) {
        let was = self.waiting_background.swap(on, Ordering::Relaxed);
        if was && !on {
            self.activity_ms.store(now_ms(), Ordering::Relaxed);
        }
    }

    /// Reader: the footer's count of background shells. `None` (no footer on screen)
    /// keeps the last count.
    pub fn set_background_shells(&self, count: Option<u32>) {
        if let Some(n) = count {
            self.background_shells.store(n, Ordering::Relaxed);
        }
    }

    /// Reader: a menu/prompt just LEFT the screen (answered or escaped): resolve any
    /// hook-set attention. AskUserQuestion fires a permission/elicitation hook but
    /// escaping it produces no spinner/Stop to clear that hook, so it would hang amber.
    /// Called only on the on→off edge, so a markerless prompt (which never sets a menu)
    /// is never cleared prematurely.
    pub fn clear_hook_attention(&self) {
        self.hook_attention.store(false, Ordering::Relaxed);
    }

    // ── What the pane shows and saves ────────────────────────────────────────────

    /// The most recently active conversation's id.
    pub fn session_id(&self) -> Option<String> {
        self.sessions.lock().unwrap().last().map(|s| s.id.clone())
    }

    /// The agent and conversation to resume on restore, ONLY if the agent will actually
    /// find the conversation: its transcript exists. A freshly launched agent has an id
    /// but no conversation yet, so resuming it errors ("no conversation found"); then
    /// this is `None` and restore launches a clean agent. Where the agent keeps it comes
    /// from its hook when it said, else from where the agent is known to keep it. With
    /// neither, only an agent whose ids arrive with a finished turn is taken at its word
    /// (`Agent::bound_after_a_turn`).
    pub fn resumable_session(&self) -> Option<(AgentKind, String)> {
        let kind = self.kind()?;
        let rec = self.sessions.lock().unwrap().last().cloned()?;
        let cwd = self.cwd.lock().unwrap().clone();
        let path = rec
            .transcript
            .map(PathBuf::from)
            .or_else(|| cwd.and_then(|c| kind.agent().derived_transcript(&rec.id, &c)));
        let exists = match path {
            Some(p) => p.exists(),
            None => kind.agent().bound_after_a_turn(),
        };
        exists.then_some((kind, rec.id))
    }

    /// Derived lifecycle: the most recent signal wins; activity counts as "working" only
    /// while fresh, then reverts to ready.
    fn lifecycle(&self) -> Lifecycle {
        // Attention is level-based: a prompt on screen, a title saying so, or an
        // unresolved hook. A menu the user opened themselves is not a prompt (see
        // `slash_submit_ms`).
        let menu = self.menu_on_screen.load(Ordering::Relaxed) && !self.menu_user_opened.load(Ordering::Relaxed);
        if menu || self.hook_attention.load(Ordering::Relaxed) || self.title_attention.load(Ordering::Relaxed) {
            return Lifecycle::Attention;
        }
        // Waiting on its own agents: the turn is over on paper (Stop hook, still spinner)
        // but the agent carries on by itself when they report.
        if self.waiting_background.load(Ordering::Relaxed) {
            return Lifecycle::Working;
        }
        // The agent's own screen, title or progress says it is working. Levels, so they
        // hold through a frozen row and a stalled read, where the spinner stream below
        // would decay into "ready" and read as a turn end.
        if self.working_row.load(Ordering::Relaxed)
            || self.title_working.load(Ordering::Relaxed)
            || self.progress_working.load(Ordering::Relaxed)
        {
            return Lifecycle::Working;
        }
        // A turn held across a scroll, where Claude draws no status row at all.
        if self.scrolled.load(Ordering::Relaxed) && self.scroll_holds_working.load(Ordering::Relaxed) {
            return Lifecycle::Working;
        }
        let act = self.activity_ms.load(Ordering::Relaxed);
        let stop = self.stop_ms.load(Ordering::Relaxed);
        let now = now_ms();
        // Just stopped: clean turn-end; ignore a trailing spinner frame so it doesn't
        // flicker working→ready→working.
        if stop != 0 && now.saturating_sub(stop) < STOP_SUPPRESS_MS {
            return Lifecycle::Ready;
        }
        // A prompt was submitted and nothing on screen has spoken yet.
        let started = self.turn_started_ms.load(Ordering::Relaxed);
        if started > stop && now.saturating_sub(started) < TURN_START_HOLD_MS {
            return Lifecycle::Working;
        }
        // Fallback for a pane whose agent draws no row we recognise: activity fresh and
        // more recent than the last turn-end. This decays on a timeout, so it drives the
        // dot only; `finish_seq` is what raises a card.
        if act > stop && now.saturating_sub(act) < WORKING_TTL_MS {
            return Lifecycle::Working;
        }
        Lifecycle::Ready
    }

    /// Milliseconds since a chunk last carried a spinner glyph, None if none has yet.
    pub fn star_age_ms(&self) -> Option<u64> {
        let t = self.last_star_ms.load(Ordering::Relaxed);
        (t != 0).then(|| now_ms().saturating_sub(t))
    }

    /// Snapshot for the view.
    pub fn snapshot(&self) -> AgentStatus {
        AgentStatus {
            lifecycle: self.lifecycle(),
            kind: self.kind(),
            epoch: self.epoch.load(Ordering::Relaxed),
            shells: self.background_shells.load(Ordering::Relaxed),
        }
    }
}

static REGISTRY: Mutex<Vec<Weak<AgentHandle>>> = Mutex::new(Vec::new());

/// Register a session's handle for the watcher to update.
pub fn register(handle: &Arc<AgentHandle>) {
    let mut reg = REGISTRY.lock().unwrap();
    reg.retain(|w| w.strong_count() > 0); // prune closed panes
    reg.push(Arc::downgrade(handle));
}

pub(crate) fn live_handles() -> Vec<Arc<AgentHandle>> {
    REGISTRY.lock().unwrap().iter().filter_map(Weak::upgrade).collect()
}

#[cfg(test)]
mod tests {
    use super::{AgentHandle, KindSource, Lifecycle};
    use crate::agent::{AgentEvent, AgentKind};
    use crate::term_signal::TermSignal;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// One animation-gap between frames: over MIN_FRAME_GAP_MS, well under MAX.
    const FRAME: Duration = Duration::from_millis(40);

    fn handle() -> Arc<AgentHandle> {
        let h = AgentHandle::new(1, Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(true)));
        h.bind(AgentKind::Claude, KindSource::Scan);
        h
    }

    /// The menu leaves the screen and stays gone past the confirmation.
    fn menu_gone(h: &Arc<AgentHandle>) {
        h.set_menu(false, false);
        assert!(h.confirm_menu_gone(h.menu_gap.load(Ordering::Relaxed)), "a gap was pending");
    }

    // A full repaint (a resize, the wheel nudge) clears the screen and redraws the prompt.
    // That is the same prompt still waiting: no new attention edge for the UI to raise a
    // card on, the hook's attention kept, and a chooser the user opened still theirs.
    #[test]
    fn a_prompt_cleared_for_a_repaint_is_the_same_prompt() {
        let h = handle();
        h.set_menu(true, false);
        h.hook_attention.store(true, Ordering::Relaxed);
        h.set_menu(false, false);
        let gap = h.menu_gap.load(Ordering::Relaxed);
        assert_ne!(gap, 0);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention, "still asking mid-repaint");
        h.set_menu(true, false);
        assert!(!h.confirm_menu_gone(gap), "it came back, so it never left");
        assert!(h.hook_attention.load(Ordering::Relaxed));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);

        let h = handle();
        h.note_slash_command();
        h.set_menu(true, false);
        h.set_menu(false, false);
        let gap = h.menu_gap.load(Ordering::Relaxed);
        h.set_menu(true, false);
        assert!(!h.confirm_menu_gone(gap));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready, "the user's /model survives a repaint");

        // Answered: gone and staying gone, which clears the hook's attention with it.
        let h = handle();
        h.set_menu(true, false);
        h.hook_attention.store(true, Ordering::Relaxed);
        menu_gone(&h);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
    }

    // Scrolling Claude's transcript redraws its screen every notch, re-emitting the
    // static star of any "thinking summary" line on it. Same glyph every time, so it
    // must never pair into working however long the user keeps scrolling.
    #[test]
    fn repeated_identical_frames_never_start_working() {
        let h = handle();
        for _ in 0..6 {
            h.note_activity(0b1000);
            std::thread::sleep(FRAME);
        }
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
    }

    // `/model` and friends draw the footer a real prompt draws, and no hook reports
    // them, so the only thing that tells them apart is that the user just typed one.
    #[test]
    fn a_chooser_the_user_opened_is_not_attention() {
        let h = handle();
        h.note_slash_command();
        h.set_menu(true, false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
        // Answered or escaped: the latch goes with the menu.
        menu_gone(&h);
        h.set_menu(true, false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
    }

    // Claude lists its commands the moment `/` is typed, long before Enter, and that
    // list carries the same footer. Typing is the whole signal here.
    #[test]
    fn the_command_list_shown_while_typing_is_not_attention() {
        let h = handle();
        h.set_menu(true, true);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
        // Enter clears the box and the chooser proper opens: the claim has to survive
        // the input row no longer holding the command.
        h.note_slash_command();
        h.set_menu(true, false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
        // Escaped: back to normal, and the next menu is Claude's.
        menu_gone(&h);
        h.set_menu(true, false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
    }

    #[test]
    fn a_chooser_claude_opened_is_attention() {
        let h = handle();
        h.set_menu(true, false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
    }

    // `/init` sets Claude working; a permission prompt later in that turn is Claude's.
    #[test]
    fn a_slash_command_that_starts_work_still_reports_its_prompts() {
        let h = handle();
        h.note_slash_command();
        h.note_activity(0b1000);
        std::thread::sleep(FRAME);
        h.note_activity(0b0100);
        h.set_menu(true, false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
    }

    // The turn end is an event, never a timeout: a lapse in the spinner stream leaves
    // the pane ready but raises nothing, because nothing said the turn ended.
    #[test]
    fn only_a_real_turn_end_counts_as_finished() {
        let h = handle();
        assert_eq!(h.finish_seq(), 0);
        // A spinner lapse: ready, but no finish.
        h.note_activity(0b1000);
        std::thread::sleep(FRAME);
        h.note_activity(0b0100);
        assert_eq!(h.finish_seq(), 0);
        // Claude's working row going, with its input box back and still so a moment
        // later: that is a turn end.
        assert_eq!(h.working_row_edge(true, false), None);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        let edge = h.working_row_edge(false, true).expect("a candidate end");
        assert_eq!(h.finish_seq(), 0, "not before it is confirmed");
        assert!(h.confirm_row_end(edge));
        assert_eq!(h.finish_seq(), 1);
    }

    // A resize catches Claude mid-redraw with neither the row nor the box on screen.
    #[test]
    fn a_redraw_without_the_input_box_is_not_a_turn_end() {
        let h = handle();
        h.working_row_edge(true, false);
        assert_eq!(h.working_row_edge(false, false), None);
        assert_eq!(h.finish_seq(), 0);
    }

    // One chunk of a split repaint drops the row while a mode line keeps the box on
    // screen; the next chunk has it back. That is not a turn end.
    #[test]
    fn a_row_that_comes_straight_back_is_not_a_turn_end() {
        let h = handle();
        h.working_row_edge(true, true);
        let edge = h.working_row_edge(false, true).expect("a candidate end");
        h.working_row_edge(true, true);
        assert!(!h.confirm_row_end(edge));
        assert_eq!(h.finish_seq(), 0);
    }

    // The row gives way to a permission prompt: a question, not a finish.
    #[test]
    fn a_row_replaced_by_a_chooser_is_not_a_turn_end() {
        let h = handle();
        h.working_row_edge(true, true);
        let edge = h.working_row_edge(false, true).expect("a candidate end");
        h.set_menu(true, false);
        assert!(!h.confirm_row_end(edge));
        assert_eq!(h.finish_seq(), 0);
    }

    // Esc stops the turn; the row going is the interrupt landing, and the user knows.
    #[test]
    fn an_interrupted_turn_is_not_a_finish() {
        let h = handle();
        h.working_row_edge(true, true);
        h.note_interrupt();
        assert_eq!(h.working_row_edge(false, true), None);
        // Nor when the Esc lands between the edge and its confirmation.
        let h = handle();
        h.working_row_edge(true, true);
        let edge = h.working_row_edge(false, true).expect("a candidate end");
        h.note_interrupt();
        assert!(!h.confirm_row_end(edge));
        assert_eq!(h.finish_seq(), 0);
    }

    // The row is gone because the user scrolled, not because the turn ended.
    #[test]
    fn a_turn_held_across_a_scroll_stays_working() {
        let h = handle();
        h.set_working_row(true, false);
        h.set_scrolled(true);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        assert_eq!(h.finish_seq(), 0);
    }

    // 1.5.1's background-agent hold, defeated by scrolling: the Stop hook has already
    // fired, so the old `act > stop` test read the pane as idle the moment it scrolled.
    #[test]
    fn scrolling_away_during_an_agent_wait_holds_the_turn() {
        let h = handle();
        h.set_waiting_background(true);
        h.set_scrolled(true);
        h.set_waiting_background(false); // the row is off screen now
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
    }

    #[test]
    fn two_differing_frames_an_animation_gap_apart_start_working() {
        let h = handle();
        h.note_activity(0b1000);
        std::thread::sleep(FRAME);
        h.note_activity(0b0100);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
    }

    #[test]
    fn suppression_blocks_entering_working() {
        let h = handle();
        h.suppress_activity(1000);
        h.note_activity(0b1000);
        std::thread::sleep(FRAME);
        h.note_activity(0b0100);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
    }

    // A scroll (or resize) during a real turn suppresses, but must not blank the
    // working state Claude is genuinely in.
    #[test]
    fn suppression_still_sustains_an_active_turn() {
        let h = handle();
        h.note_activity(0b1000);
        std::thread::sleep(FRAME);
        h.note_activity(0b0100);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.suppress_activity(1000);
        std::thread::sleep(FRAME);
        h.note_activity(0b0010);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
    }

    #[test]
    fn a_scrolled_transcript_holds_a_live_turn_but_invents_none() {
        // Idle: scrolling away does not make it working.
        let h = handle();
        h.set_scrolled(true);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
        h.set_scrolled(false);

        // Live: the hold is latched at the scroll, and survives the frames stopping.
        h.note_activity(1);
        std::thread::sleep(FRAME);
        h.note_activity(2);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.set_scrolled(true);
        assert!(h.scroll_holds_working.load(Ordering::Relaxed));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.set_scrolled(false);
        assert!(!h.scroll_holds_working.load(Ordering::Relaxed));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
    }

    // "Waiting for N background agents to finish": the turn is over by its Stop hook and
    // its still spinner, but Claude resumes on its own, so the pane stays working until
    // the row goes, and then for a TTL more so the resumed turn's first frames have time.
    #[test]
    fn waiting_for_agents_holds_working_across_the_stop() {
        let h = handle();
        h.note_activity(1);
        std::thread::sleep(FRAME);
        h.note_activity(2);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.set_waiting_background(true);
        h.stop_ms.store(super::now_ms(), Ordering::Relaxed);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        std::thread::sleep(Duration::from_millis(super::STOP_SUPPRESS_MS + 50));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.set_waiting_background(false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);

        // A prompt on screen still outranks the wait.
        h.set_waiting_background(true);
        h.set_menu(true, false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
    }

    // A shell left running (a dev server) is shown beside the pane, never held as work:
    // the turn's Stop counts as its finish, and a footer the reader cannot see (a chooser
    // over it) keeps the count it last read.
    #[test]
    fn a_background_shell_is_not_work() {
        let h = handle();
        h.set_background_shells(Some(2));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
        assert_eq!(h.snapshot().shells, 2);
        h.set_background_shells(None);
        assert_eq!(h.snapshot().shells, 2);
        h.apply(AgentKind::Claude, None, &AgentEvent::TurnEnded);
        std::thread::sleep(Duration::from_millis(super::STOP_CONFIRM_MS + 150));
        assert_eq!(h.finish_seq(), 1);
        h.end_session();
        assert_eq!(h.snapshot().shells, 0);
    }

    // A stronger witness to which agent runs here is not overridden by a weaker one
    // while it is live; a new kind resets what the old one's screen said, but never
    // moves the finish count backwards, which the UI compares against.
    #[test]
    fn a_weaker_source_cannot_rebind_a_live_agent() {
        let h = handle();
        h.bind(AgentKind::Codex, KindSource::Chrome);
        assert_eq!(h.kind(), Some(AgentKind::Claude), "the scan outranks chrome");
        let epoch = h.snapshot().epoch;

        h.working_row_edge(true, true);
        let edge = h.working_row_edge(false, true).unwrap();
        assert!(h.confirm_row_end(edge));
        h.set_menu(true, false);
        h.bind(AgentKind::Codex, KindSource::Hook);
        assert_eq!(h.kind(), Some(AgentKind::Codex));
        assert_eq!(h.finish_seq(), 1, "kept across the switch");
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready, "the old menu reading is gone");
        assert_ne!(h.snapshot().epoch, epoch);
    }

    // A second agent speaking with the same authority while the first is live (a
    // `codex exec` run by Claude's Bash tool, had it reached the pane) takes nothing over.
    #[test]
    fn a_live_hook_bound_agent_is_not_rebound_by_another_hook() {
        let h = AgentHandle::new(1, Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
        h.apply(AgentKind::Claude, Some("c1"), &AgentEvent::SessionBound { id: "c1".into(), transcript: None });
        let epoch = h.snapshot().epoch;
        h.apply(AgentKind::Codex, Some("x1"), &AgentEvent::SessionBound { id: "x1".into(), transcript: None });
        assert_eq!(h.kind(), Some(AgentKind::Claude));
        assert_eq!(h.session_id().as_deref(), Some("c1"), "the pane keeps its own conversation");
        assert_eq!(h.snapshot().epoch, epoch);
        h.apply(AgentKind::Codex, Some("x1"), &AgentEvent::TurnEnded);
        assert_eq!(h.finish_seq(), 0, "the other agent's turn end is not this pane's");
    }

    // Binding an agent where none was bound is no new agent to the UI, so a launch that
    // opens straight into a chooser still raises its card; only a switch moves the epoch.
    #[test]
    fn only_a_switch_between_agents_moves_the_epoch() {
        let h = AgentHandle::new(1, Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
        h.bind(AgentKind::Claude, KindSource::Scan);
        assert_eq!(h.snapshot().epoch, 0);
        h.end_session();
        h.bind(AgentKind::Claude, KindSource::Scan);
        assert_eq!(h.snapshot().epoch, 0);
        h.bind(AgentKind::Codex, KindSource::Hook);
        assert_eq!(h.snapshot().epoch, 1);
    }

    // An id with nothing to check it against is only trusted from an agent whose ids come
    // with a finished turn; Claude's comes at startup, before any conversation exists.
    #[test]
    fn an_unverifiable_claude_id_is_not_resumed() {
        let h = AgentHandle::new(1, Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
        h.apply(AgentKind::Claude, Some("c1"), &AgentEvent::SessionBound { id: "c1".into(), transcript: None });
        assert_eq!(h.resumable_session(), None, "no cwd, no transcript: nothing proves it exists");
        let h = AgentHandle::new(1, Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
        h.apply(AgentKind::Codex, Some("x1"), &AgentEvent::SessionBound { id: "x1".into(), transcript: None });
        assert_eq!(h.resumable_session(), Some((AgentKind::Codex, "x1".into())));
    }

    // Hooks from a process that has exited must not revive the pane.
    #[test]
    fn only_a_session_start_is_taken_from_an_agent_not_running() {
        let h = AgentHandle::new(1, Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
        h.apply(AgentKind::Claude, None, &AgentEvent::NeedsAttention);
        assert_eq!(h.kind(), None);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
        h.apply(AgentKind::Claude, Some("s1"), &AgentEvent::SessionBound { id: "s1".into(), transcript: None });
        assert!(h.running.load(Ordering::Relaxed));
        assert_eq!(h.kind(), Some(AgentKind::Claude));
        assert_eq!(h.session_id().as_deref(), Some("s1"));
        h.apply(AgentKind::Claude, Some("s1"), &AgentEvent::NeedsAttention);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
        // `/clear`: a new conversation replaces the old for a one-session agent.
        h.apply(AgentKind::Claude, Some("s2"), &AgentEvent::SessionBound { id: "s2".into(), transcript: None });
        assert_eq!(h.session_id().as_deref(), Some("s2"));
    }

    // A prompt submitted by hook holds Working until a level signal speaks, and a turn
    // end takes it straight back; only so long, so a lost end cannot pin it.
    #[test]
    fn a_submitted_prompt_holds_working_briefly() {
        let h = handle();
        h.apply(AgentKind::Claude, None, &AgentEvent::TurnStarted);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.turn_started_ms.store(super::now_ms() - super::TURN_START_HOLD_MS - 1, Ordering::Relaxed);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
        h.apply(AgentKind::Claude, None, &AgentEvent::TurnStarted);
        h.stop_ms.store(super::now_ms(), Ordering::Relaxed);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
    }

    // The hook and the screen both report most turn ends; one finish is one card's worth.
    #[test]
    fn a_turn_end_reported_twice_counts_once() {
        let h = handle();
        assert!(h.count_finish());
        assert!(!h.count_finish(), "the same finish, reported again");
        h.working_row_edge(true, true); // the next turn worked
        assert!(h.count_finish());
        assert_eq!(h.finish_seq(), 2);
    }

    // Codex's title says working and waiting; each title replaces the last.
    #[test]
    fn titles_and_notifications_are_read_by_the_agents_signals() {
        let h = handle();
        h.bind(AgentKind::Codex, KindSource::Hook);
        h.note_signal(&TermSignal::Title("\u{280b} arbiter-app".into()));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.note_signal(&TermSignal::Title("[ ! ] Action Required | arbiter-app".into()));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
        h.note_signal(&TermSignal::Title("arbiter-app".into()));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
        h.note_signal(&TermSignal::Notify { title: None, body: "Approval requested: cargo test".into() });
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
        h.clear_hook_attention();
        // Claude's signals are NONE: its title spinner means nothing here.
        let c = handle();
        c.note_signal(&TermSignal::Title("\u{280b} Claude Code".into()));
        c.note_signal(&TermSignal::Bell);
        assert_eq!(c.snapshot().lifecycle, Lifecycle::Ready);
    }

    #[test]
    fn copilot_progress_is_a_working_level() {
        let h = handle();
        h.bind(AgentKind::Copilot, KindSource::Hook);
        h.note_signal(&TermSignal::Progress { state: 3, percent: None });
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.note_signal(&TermSignal::Progress { state: 0, percent: None });
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready);
    }
}
