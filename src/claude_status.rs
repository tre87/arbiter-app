//! Per-pane Claude status, updated event-driven (no polling) by a single `notify`
//! watcher over the capture + hook dirs, the same model the webview used and
//! the native git status uses.
//!
//! Each `Session` owns an `Arc<ClaudeHandle>` (shared with the watcher via a
//! global registry of `Weak` handles) holding its live `ClaudeStatus`; `view()`
//! reads it each frame. A capture (`<data>/claude-sessions/<pane-id>.json`) existing
//! means Claude launched in that pane; hook signals (`<data>/claude-hooks/<sid>.json`)
//! flip the lifecycle (Stop→ready, Permission/elicitation→attention).

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};

pub type Watcher = Debouncer<RecommendedWatcher>;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Lifecycle {
    #[default]
    Closed,
    Ready,
    Working,
    Attention,
}

/// Live Claude status for one pane. Only the lifecycle now: the per-pane token /
/// context / cost readouts were retired because Claude's own statusLine already
/// renders them, and unlike this it does so over SSH too.
#[derive(Clone, Default)]
pub struct ClaudeStatus {
    pub lifecycle: Lifecycle,
}

/// The watcher's view of one session: how to match it (cwd + alive flag + bound
/// session id), the captured stats, and the timestamps the lifecycle is derived
/// from. The lifecycle is *computed* (not stored) so the reader thread (activity/
/// menu) and the watcher thread (hooks) never fight over a single field — each
/// just stamps its latest event time.
pub struct ClaudeHandle {
    /// This pane's id (== `Session.id`), set as `PANE_ID_ENV` on its shell — the
    /// primary key a capture binds by (exact, robust under load / shared cwds).
    pub pane_id: u64,
    pub shell_pid: Option<u32>,
    pub cwd: Arc<Mutex<Option<String>>>,
    pub claude_running: Arc<AtomicBool>,
    stats: Mutex<ClaudeStatus>,
    /// Claude session id once a capture binds it (so hooks route here).
    session_id: Mutex<Option<String>>,
    /// Highest hook nonce applied (so a repeated signal still fires once).
    last_nonce: Mutex<u128>,
    /// Last-event times (ms since epoch); 0 = never.
    activity_ms: AtomicU64, // spinner / "esc to interrupt" (working)
    stop_ms: AtomicU64,     // Stop hook (turn end)
    /// Time of the previous detected spinner frame — entering "working" requires TWO
    /// frames an animation-gap apart, so a one-shot repaint (Shift+Tab/Enter, a single
    /// resize) that emits the star glyph once can't false-trigger working.
    last_spinner_ms: AtomicU64,
    /// Glyph fingerprint of that frame (see `session::chunk_spinner_key`). The pair
    /// must draw DIFFERENT glyphs to count as an animation, so a repaint that keeps
    /// re-emitting the same static star can't false-trigger working however often it
    /// repeats. 0 = none yet.
    last_spinner_glyphs: AtomicU64,
    /// Time of the last chunk carrying any spinner glyph, suppressed or not; 0 = none. A
    /// static star in a repaint counts too, so this says whether Claude is drawing at all,
    /// not whether it is working (see `star_age_ms`).
    last_star_ms: AtomicU64,
    /// Claude's fullscreen UI is scrolled away from its live bottom (see
    /// `VtTerm::visible_scrolled`), where it stops drawing its status row.
    scrolled: AtomicBool,
    /// Set when the transcript was scrolled away during a live turn: the turn is held as
    /// working, frames or not, until the row is back in view (then it gets a fresh TTL).
    scroll_holds_working: AtomicBool,
    /// Claude's status row says it is waiting for background agents it launched (see
    /// `VtTerm::visible_waiting_agents`). Its turn has ended by every other sign, the
    /// Stop hook included, yet it resumes by itself when they report, so the pane is held
    /// as working for as long as the row shows; when the row goes, the resumed turn gets
    /// a fresh TTL to show its first frames in.
    waiting_agents: AtomicBool,
    /// Spinner detection is ignored until this time — set briefly on app-initiated
    /// repaints (window/PTY resize) whose rapid redraws would otherwise look animated.
    suppress_until_ms: AtomicU64,
    /// A text menu/approval prompt is currently on the visible screen — level-
    /// triggered by the reader's grid scan, so it clears the instant the prompt
    /// leaves (the user escapes/answers). Covers AskUserQuestion / plan / proceed.
    menu_on_screen: AtomicBool,
    /// A permission/elicitation hook fired (edge-triggered) — cleared when Claude
    /// resumes (activity) or the turn ends (Stop). Covers tool-permission prompts
    /// that don't show a grid marker.
    hook_attention: AtomicBool,
    /// When the user last submitted a slash command here (0 = never). A chooser that
    /// appears just after one is theirs, not Claude's: no hook reports `/model`, and
    /// its footer is the one AskUserQuestion draws, so the screen alone cannot tell
    /// them apart. Cleared by activity, so a command that sets Claude working
    /// (`/init`) still reports the prompts that turn raises.
    slash_submit_ms: AtomicU64,
    /// The menu currently on screen is the one that slash command opened.
    menu_user_opened: AtomicBool,
    /// Claude's status row says it is working, read level-triggered from the grid.
    /// Unlike spinner frames this survives a frozen row and a stalled read, neither
    /// of which means the turn ended.
    working_row: AtomicBool,
    /// Bumped once per turn that is KNOWN to have ended: a Stop hook, or Claude's
    /// working row leaving the screen. The notification reads this counter rather than
    /// sampling the lifecycle, so a turn end can neither be invented by a timeout nor
    /// missed by sampling at the wrong moment.
    finish_seq: AtomicU64,
    /// An SSH/mosh client is running in this pane, so its foreground program lives on
    /// another machine. Set from the same busy-edge scan as `claude_running`; gates
    /// the on-screen probe below, which only remote panes need.
    remote: AtomicBool,
    /// Sticky version of `remote`: true once this pane has been remote, never cleared.
    was_remote: AtomicBool,
    /// The last command the user submitted in this pane (memory only), and the one
    /// accepted as its startup command (persisted). See `latch_startup_cmd`.
    last_command: Mutex<Option<String>>,
    startup_cmd: Mutex<Option<String>>,
    /// This pane's private command-history file, consulted when the typed line was
    /// abandoned (an up-arrow recall). See `histfile_last_remote_cmd`.
    histfile: Option<std::path::PathBuf>,
    /// Claude's own UI chrome is on this pane's screen (`VtTerm::claude_chrome`).
    /// This is how a REMOTE Claude is recognised, where the local process scan sees
    /// only `ssh` and no statusLine capture is ever written. Latched rather than
    /// timed: an idle Claude produces no output, so nothing would refresh a TTL, but
    /// the moment it exits the shell prints a prompt and that scan clears this.
    on_screen: AtomicBool,
    /// Whether this pane's connection asks for a credential. `None` until observed,
    /// `Some(true)` once ssh has prompted, `Some(false)` once a login completed with no
    /// prompt. Persisted, so the startup dialog only asks for connections that will ask.
    prompts_for_credential: Mutex<Option<bool>>,
    /// Which secret it asked for, and for what (a key's file name, or `user@host`), read
    /// from ssh's prompt. Persisted, so the dialog can say what it wants.
    credential_prompt: Mutex<Option<(crate::persist::CredentialKind, String)>>,
    /// A credential prompt appeared on the CURRENT connection. Reset per connection, so
    /// the login evidence below can tell "asked" from "never asked" this time.
    prompted_this_connection: AtomicBool,
    /// The current connection has shown signs of a completed login: the far shell
    /// reported a directory, Claude's chrome appeared, or enough time passed. Ends the
    /// window in which a credential may be typed and settles `prompts_for_credential`.
    login_seen: AtomicBool,
    /// Arbiter typed a credential on the current connection (auto or from the re-ask
    /// dialog), so a further prompt or a denial means it was rejected. Reset per prompt
    /// handled and per connection.
    credential_typed: AtomicBool,
    /// When the current connection was detected, ms since epoch; 0 when not remote.
    remote_since_ms: AtomicU64,
    /// The one automatic reconnect a connection gets has been used. Reset when the next
    /// connection is detected, so every established session earns one more.
    retry_spent: AtomicBool,
    /// Requests from the reader to the UI, taken on the next redraw: rerun this pane's
    /// connection (its ssh exited abnormally), or ask again for its credential (the one
    /// typed was rejected).
    retry_wanted: AtomicBool,
    reask_wanted: AtomicBool,
    /// ssh said it never reached the host on the current attempt (refused, timed out,
    /// unresolvable), so however long the attempt took, nothing was up to drop.
    connect_failed: AtomicBool,
    /// The far host's working directory: from an OSC-7 the REMOTE shell emitted (the
    /// opt-in snippet) where there is one, else followed from the `cd` commands typed
    /// there. Kept when the connection ends so Reconnect and restore can go back to it.
    remote_cwd: Mutex<Option<String>>,
    /// The value before the last change, so a `cd` the far shell then rejected can be
    /// undone.
    remote_cwd_prev: Mutex<Option<String>>,
    /// A remote OSC-7 arrived on the current connection: the snippet is active there.
    remote_cwd_reported: AtomicBool,
    /// Claude was on the far side when the connection ended (or, seeded on restore, at
    /// save time), so the next connection should resume it. Taken by whoever queues
    /// that connection.
    remote_claude_pending: AtomicBool,
    /// `claude` was typed at the far shell's prompt and no other command has been typed
    /// at a shell prompt since. A second witness to a far Claude, for when its screen
    /// chrome is not recognised.
    remote_claude_typed: AtomicBool,
    /// The far Claude's conversation, when Arbiter knows it: the id it completed onto a
    /// typed `claude` (`--session-id`), or the one typed with `--resume`. Kept across a
    /// drop so the next connection resumes that very conversation; cleared once Claude is
    /// seen to have exited, or says the conversation is gone.
    remote_session: Mutex<Option<String>>,
    /// The user declined to bring this pane's connection back (Skip in the sign-in
    /// dialog). The command is kept so Reconnect still can, and the amber button stays,
    /// until a command is run here; a save meanwhile writes the pane as a plain local one.
    dismissed: AtomicBool,
}

/// Working reverts to ready after this long without a detected spinner frame.
/// Must comfortably exceed the gap BETWEEN detected frames: Claude's ✻ bloom
/// passes through `·` frames (not in our star range) and animates slower while
/// "thinking", so a too-short TTL makes working flicker on/off between frames.
/// The web used 2s; matching it. The turn-end stays instant regardless — the Stop
/// hook (+ post-stop guard) clears working immediately; this only bounds the
/// no-hook fallback.
const WORKING_TTL_MS: u64 = 2000;
/// After a Stop hook, treat the turn as over: ignore a trailing spinner frame (the
/// final redraw) and force ready for this long, so the turn-end can't flicker
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
/// Comfortably longer than Claude takes to draw it, far shorter than a turn.
const SLASH_MENU_WINDOW_MS: u64 = 3000;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl ClaudeHandle {
    pub fn new(
        pane_id: u64,
        shell_pid: Option<u32>,
        cwd: Arc<Mutex<Option<String>>>,
        claude_running: Arc<AtomicBool>,
        histfile: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            pane_id,
            shell_pid,
            cwd,
            claude_running,
            stats: Mutex::new(ClaudeStatus::default()),
            session_id: Mutex::new(None),
            last_nonce: Mutex::new(0),
            activity_ms: AtomicU64::new(0),
            stop_ms: AtomicU64::new(0),
            last_spinner_ms: AtomicU64::new(0),
            last_spinner_glyphs: AtomicU64::new(0),
            last_star_ms: AtomicU64::new(0),
            scrolled: AtomicBool::new(false),
            scroll_holds_working: AtomicBool::new(false),
            waiting_agents: AtomicBool::new(false),
            suppress_until_ms: AtomicU64::new(0),
            menu_on_screen: AtomicBool::new(false),
            hook_attention: AtomicBool::new(false),
            slash_submit_ms: AtomicU64::new(0),
            menu_user_opened: AtomicBool::new(false),
            working_row: AtomicBool::new(false),
            finish_seq: AtomicU64::new(0),
            remote: AtomicBool::new(false),
            was_remote: AtomicBool::new(false),
            last_command: Mutex::new(None),
            startup_cmd: Mutex::new(None),
            histfile,
            on_screen: AtomicBool::new(false),
            prompts_for_credential: Mutex::new(None),
            credential_prompt: Mutex::new(None),
            prompted_this_connection: AtomicBool::new(false),
            login_seen: AtomicBool::new(false),
            credential_typed: AtomicBool::new(false),
            remote_since_ms: AtomicU64::new(0),
            retry_spent: AtomicBool::new(false),
            retry_wanted: AtomicBool::new(false),
            reask_wanted: AtomicBool::new(false),
            connect_failed: AtomicBool::new(false),
            remote_cwd: Mutex::new(None),
            remote_cwd_prev: Mutex::new(None),
            remote_cwd_reported: AtomicBool::new(false),
            remote_claude_pending: AtomicBool::new(false),
            remote_claude_typed: AtomicBool::new(false),
            remote_session: Mutex::new(None),
            dismissed: AtomicBool::new(false),
        })
    }

    /// Busy-edge scan: an SSH/mosh client is (or is no longer) running in this pane.
    /// Turning remote off also drops the on-screen latch, so a pane that leaves a
    /// remote session can't keep reporting the far host's Claude.
    pub fn set_remote(&self, on: bool) {
        self.remote.store(on, Ordering::Relaxed);
        if on {
            self.was_remote.store(true, Ordering::Relaxed);
            self.remote_since_ms.store(now_ms(), Ordering::Relaxed);
            self.retry_spent.store(false, Ordering::Relaxed);
            self.login_seen.store(false, Ordering::Relaxed);
            self.prompted_this_connection.store(false, Ordering::Relaxed);
            self.credential_typed.store(false, Ordering::Relaxed);
            self.connect_failed.store(false, Ordering::Relaxed);
            self.remote_claude_typed.store(false, Ordering::Relaxed);
            // A new connection is the new truth; whoever queued it took the flag first.
            self.remote_claude_pending.store(false, Ordering::Relaxed);
            self.latch_startup_cmd();
        } else {
            // Claude was still on the far side when the session ended, so the next
            // connection should bring it back. Both witnesses count.
            let on_screen = self.on_screen.swap(false, Ordering::Relaxed);
            let typed = self.remote_claude_typed.swap(false, Ordering::Relaxed);
            if on_screen || typed {
                self.remote_claude_pending.store(true, Ordering::Relaxed);
                SAVE_DIRTY.store(true, Ordering::Relaxed);
            }
            self.remote_since_ms.store(0, Ordering::Relaxed);
            self.remote_cwd_reported.store(false, Ordering::Relaxed);
        }
    }

    /// Reader: the user ended the remote session themselves (the far shell exited, so ssh
    /// returned that shell's status rather than its own 255). The pane is a plain local
    /// shell again: nothing to reconnect, nothing to replay or sign in to on the next
    /// launch, and a new `ssh` typed here latches afresh.
    pub fn forget_connection(&self) {
        *self.startup_cmd.lock().unwrap() = None;
        *self.last_command.lock().unwrap() = None;
        *self.prompts_for_credential.lock().unwrap() = None;
        *self.credential_prompt.lock().unwrap() = None;
        *self.remote_cwd.lock().unwrap() = None;
        *self.remote_cwd_prev.lock().unwrap() = None;
        *self.remote_session.lock().unwrap() = None;
        self.was_remote.store(false, Ordering::Relaxed);
        self.remote_claude_pending.store(false, Ordering::Relaxed);
        self.remote_claude_typed.store(false, Ordering::Relaxed);
        self.dismissed.store(false, Ordering::Relaxed);
        SAVE_DIRTY.store(true, Ordering::Relaxed);
    }

    /// UI: the user declined to bring this connection back for now (see `dismissed`).
    pub fn dismiss_connection(&self) {
        self.dismissed.store(true, Ordering::Relaxed);
        SAVE_DIRTY.store(true, Ordering::Relaxed);
    }

    /// UI: the connection is being brought back after all.
    pub fn undismiss(&self) {
        self.dismissed.store(false, Ordering::Relaxed);
    }

    pub fn dismissed(&self) -> bool {
        self.dismissed.load(Ordering::Relaxed)
    }

    /// Reader: ssh printed a credential prompt on this pane while its connection was
    /// being made, asking for `prompt` (see `session::describe_credential_prompt`).
    /// Ignored once the login is known to have completed, so a prompt from a nested
    /// `ssh` or `git push` on the far host does not describe this connection.
    pub fn note_credential_prompt(&self, prompt: Option<(crate::persist::CredentialKind, String)>) {
        if self.login_seen.load(Ordering::Relaxed) {
            return;
        }
        self.prompted_this_connection.store(true, Ordering::Relaxed);
        let mut prompts = self.prompts_for_credential.lock().unwrap();
        if *prompts != Some(true) {
            *prompts = Some(true);
            SAVE_DIRTY.store(true, Ordering::Relaxed);
        }
        if prompt.is_some() {
            let mut cur = self.credential_prompt.lock().unwrap();
            if *cur != prompt {
                *cur = prompt;
                SAVE_DIRTY.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Which secret the connection asked for, and for what, if it has been seen.
    pub fn credential_prompt(&self) -> Option<(crate::persist::CredentialKind, String)> {
        self.credential_prompt.lock().unwrap().clone()
    }

    /// Seed from a saved layout.
    pub fn seed_credential_prompt(&self, kind: Option<crate::persist::CredentialKind>, detail: Option<&str>) {
        *self.credential_prompt.lock().unwrap() =
            kind.map(|k| (k, detail.unwrap_or_default().to_string()));
    }

    /// Evidence that the current connection's login has completed. The first call per
    /// connection settles whether this connection prompts (it did not, if no prompt was
    /// seen) and returns true, so the caller can stop holding a credential.
    pub fn note_login_evidence(&self) -> bool {
        if !self.is_remote() || self.login_seen.swap(true, Ordering::Relaxed) {
            return false;
        }
        // Whatever was typed got the pane in, so nothing after this can reject it.
        self.credential_typed.store(false, Ordering::Relaxed);
        if !self.prompted_this_connection.load(Ordering::Relaxed) {
            let mut prompts = self.prompts_for_credential.lock().unwrap();
            if *prompts != Some(false) {
                *prompts = Some(false);
                SAVE_DIRTY.store(true, Ordering::Relaxed);
            }
        }
        true
    }

    /// Whether the current connection's login is known to have completed.
    pub fn login_seen(&self) -> bool {
        self.login_seen.load(Ordering::Relaxed)
    }

    /// Whether a credential prompt has appeared on the current connection.
    pub fn prompted_this_connection(&self) -> bool {
        self.prompted_this_connection.load(Ordering::Relaxed)
    }

    /// Reader: ssh reported that it could not reach the host on this attempt.
    pub fn note_connect_failure(&self) {
        self.connect_failed.store(true, Ordering::Relaxed);
    }

    /// What is known about whether this connection asks for a credential.
    pub fn prompts_for_credential(&self) -> Option<bool> {
        *self.prompts_for_credential.lock().unwrap()
    }

    /// Seed from a saved layout.
    pub fn set_prompts_for_credential(&self, prompts: Option<bool>) {
        *self.prompts_for_credential.lock().unwrap() = prompts;
    }

    /// A credential was just typed into this pane's connection (by Arbiter).
    pub fn note_credential_typed(&self) {
        self.credential_typed.store(true, Ordering::Relaxed);
    }

    /// Reader: ssh asked again, or denied access, after a credential was typed. True
    /// once per typed credential, so the UI is asked to re-ask exactly once for it.
    pub fn note_credential_rejected(&self) -> bool {
        if !self.credential_typed.swap(false, Ordering::Relaxed) {
            return false;
        }
        self.reask_wanted.store(true, Ordering::Relaxed);
        true
    }

    /// Reader: the connection ended abnormally and this pane should be reconnected.
    /// Honoured once per ESTABLISHED connection (see `retry_spent`): one whose login
    /// was seen, or that prompted, or that ran at least `min_connection` without ssh
    /// saying the host was unreachable. That last clause matters: a connect that times
    /// out runs for as long as ssh waits, and must not count as a session that dropped,
    /// or an unreachable host would be retried for as long as it stays down.
    pub fn request_retry(&self, min_connection: Duration) -> bool {
        let since = self.remote_since_ms.load(Ordering::Relaxed);
        let lasted = since != 0 && now_ms().saturating_sub(since) >= min_connection.as_millis() as u64;
        let established = self.login_seen.load(Ordering::Relaxed)
            || self.prompted_this_connection.load(Ordering::Relaxed)
            || (lasted && !self.connect_failed.load(Ordering::Relaxed));
        if !established || self.retry_spent.swap(true, Ordering::Relaxed) {
            return false;
        }
        self.retry_wanted.store(true, Ordering::Relaxed);
        true
    }

    /// When the current connection was detected (ms since epoch), 0 if not remote.
    /// Identifies a connection, so a timer armed for one cannot act on the next.
    pub fn remote_since_ms(&self) -> u64 {
        self.remote_since_ms.load(Ordering::Relaxed)
    }

    /// UI: take a pending reconnect request.
    pub fn take_retry_wanted(&self) -> bool {
        self.retry_wanted.swap(false, Ordering::Relaxed)
    }

    /// UI: take a pending re-ask request.
    pub fn take_reask_wanted(&self) -> bool {
        self.reask_wanted.swap(false, Ordering::Relaxed)
    }

    /// Reader: the REMOTE shell reported its working directory.
    pub fn note_remote_cwd(&self, path: String) {
        self.set_remote_cwd(Some(path));
        self.remote_cwd_reported.store(true, Ordering::Relaxed);
    }

    /// Where the far shell now is, as inferred (a `cd` typed there, a fresh login's home,
    /// a follow-up's `cd`). `None` when a `cd` made it unknowable.
    pub fn set_remote_cwd(&self, path: Option<String>) {
        let mut cur = self.remote_cwd.lock().unwrap();
        *self.remote_cwd_prev.lock().unwrap() = cur.clone();
        *cur = path;
    }

    /// The far shell rejected the `cd` that produced the current value: go back.
    pub fn revert_remote_cwd(&self) {
        if let Some(prev) = self.remote_cwd_prev.lock().unwrap().take() {
            *self.remote_cwd.lock().unwrap() = Some(prev);
        }
    }

    /// The far host's last reported working directory, if its shell ever reported one.
    pub fn remote_cwd(&self) -> Option<String> {
        self.remote_cwd.lock().unwrap().clone()
    }

    /// Seed from a saved layout.
    pub fn seed_remote_cwd(&self, path: Option<&str>) {
        *self.remote_cwd.lock().unwrap() = path.map(str::to_string);
    }

    /// Whether the far host has reported a directory on the current connection, which
    /// is how the user can tell the snippet took.
    pub fn remote_cwd_reported(&self) -> bool {
        self.remote_cwd_reported.load(Ordering::Relaxed)
    }

    /// Whether Claude is (or, for an ended connection, was) running on the far side, by
    /// either witness. This is what the saved layout carries for a remote pane.
    pub fn remote_claude(&self) -> bool {
        self.on_screen()
            || self.remote_claude_typed.load(Ordering::Relaxed)
            || self.remote_claude_pending.load(Ordering::Relaxed)
    }

    /// Take the "resume Claude on the next connection" flag.
    pub fn take_remote_claude_pending(&self) -> bool {
        self.remote_claude_pending.swap(false, Ordering::Relaxed)
    }

    /// Seed from a saved layout.
    pub fn seed_remote_claude(&self, pending: bool) {
        self.remote_claude_pending.store(pending, Ordering::Relaxed);
    }

    /// Record a command the user submitted in this pane, if it invokes a remote client.
    ///
    /// Filtered HERE, at the point of capture, rather than only when the value is used.
    /// Keystrokes reach this from the input path, which includes what is typed at ssh's
    /// own prompts, so a key passphrase or a login password would otherwise sit in
    /// memory until the next command displaced it. Nothing but a recognised client
    /// invocation is worth keeping, so nothing else is kept.
    ///
    /// It also makes the capture more robust: a passphrase entered after `ssh mini` no
    /// longer overwrites it, so the connection command is still there to be latched.
    pub fn note_command(&self, cmd: String) {
        if crate::claude::looks_like_remote_cmd(&cmd) {
            *self.last_command.lock().unwrap() = Some(cmd);
        }
    }

    /// Promote the last submitted command to this pane's startup command, if it invokes
    /// a remote client.
    ///
    /// Called the moment an ssh client is detected, NOT lazily when something reads the
    /// value. The distinction is the whole point: a save only happens on a layout
    /// change, so a lazy latch would run at quit time and capture whatever had been
    /// typed since: `claude` at the remote prompt, or a key passphrase. The whitelist
    /// in `looks_like_remote_cmd` then makes the guarantee independent of timing
    /// altogether, which is what keeps a passphrase out of `session.json`.
    fn latch_startup_cmd(&self) {
        let mut startup = self.startup_cmd.lock().unwrap();
        if startup.is_some() {
            return;
        }
        // Prefer the line we watched being typed; otherwise ask the shell's own history.
        let typed = self.last_command.lock().unwrap().clone();
        *startup = typed
            .filter(|c| crate::claude::looks_like_remote_cmd(c))
            .or_else(|| self.histfile_last_remote_cmd());
    }

    /// The most recent line in this pane's private history file, if it invokes a remote
    /// client.
    ///
    /// This is what makes the feature usable in practice. Keystroke tracking refuses to
    /// guess after an up-arrow recall or a tab completion, and reconnecting by pressing
    /// up is the common case, so without this a retried `ssh` would never be remembered.
    /// PowerShell's PSReadLine appends each command as it is accepted, so the line is
    /// there immediately. bash only appends at its *next* prompt, which for a
    /// long-running ssh has not happened yet, so there the typed line is what counts.
    fn histfile_last_remote_cmd(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.histfile.as_ref()?).ok()?;
        text.lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .filter(|l| crate::claude::looks_like_remote_cmd(l))
            .map(str::to_string)
    }

    /// Seed the startup command from a saved layout on restore, where the command was
    /// replayed rather than typed. Returns whether it was accepted.
    ///
    /// Validated by the same whitelist as the live latch, which matters because a save
    /// written by an earlier build could hold anything that had been typed last. Without
    /// this check such a value would be replayed AND written straight back out on the
    /// next save, so it would outlive the fix instead of healing itself.
    pub fn set_startup_cmd(&self, cmd: &str) -> bool {
        if !crate::claude::looks_like_remote_cmd(cmd) {
            return false;
        }
        *self.startup_cmd.lock().unwrap() = Some(cmd.to_string());
        // A pane with a remote startup command was remote, whether or not its ssh has
        // been seen running yet. This is what lets a connection that fails before the
        // process scan catches it still offer Reconnect.
        self.was_remote.store(true, Ordering::Relaxed);
        true
    }

    /// The command that rebuilds this pane, if one was accepted.
    pub fn startup_cmd(&self) -> Option<String> {
        self.startup_cmd.lock().unwrap().clone()
    }

    pub fn is_remote(&self) -> bool {
        self.remote.load(Ordering::Relaxed)
    }

    /// Whether this pane has EVER been remote. Never cleared, so a pane whose ssh
    /// session has ended can still be told apart from one that was always local. That
    /// is the difference between "your connection dropped, reconnect?" and a perfectly
    /// ordinary local shell sitting at its prompt.
    pub fn was_remote(&self) -> bool {
        self.was_remote.load(Ordering::Relaxed)
    }

    /// Reader: the result of scanning this pane's screen for Claude's chrome.
    ///
    /// Present latches on. Absent only latches OFF when no spinner frame is fresh,
    /// because during a turn Claude replaces its idle hint line with the interrupt
    /// hint: clearing on that would drop the pane's Claude state mid-answer and take
    /// the status dot with it. A real exit has no spinner either, so it still clears
    /// on the very next chunk (the returning shell prompt).
    pub fn note_screen(&self, chrome: bool) {
        if chrome {
            if !self.on_screen.swap(true, Ordering::Relaxed) {
                SAVE_DIRTY.store(true, Ordering::Relaxed);
                crate::claude_shim::debug_log("remote claude: chrome on screen");
            }
            // Claude's own UI on screen means the login is long done.
            self.note_login_evidence();
        } else if !self.activity_fresh() && self.on_screen.swap(false, Ordering::Relaxed) {
            // Its screen went away with no turn in flight: Claude exited, whatever was typed.
            self.waiting_agents.store(false, Ordering::Relaxed);
            self.remote_claude_typed.store(false, Ordering::Relaxed);
            *self.remote_session.lock().unwrap() = None;
            SAVE_DIRTY.store(true, Ordering::Relaxed);
            crate::claude_shim::debug_log("remote claude: chrome left the screen");
        }
    }

    /// Keyboard: `claude` was typed at the far prompt (`true`), or another command was
    /// typed at a shell prompt (`false`), which is only possible once Claude has exited.
    pub fn set_remote_claude_typed(&self, typed: bool) {
        if self.remote_claude_typed.swap(typed, Ordering::Relaxed) != typed {
            SAVE_DIRTY.store(true, Ordering::Relaxed);
        }
    }

    /// The far Claude's conversation as far as Arbiter can know it (see the field). `None`
    /// when it cannot: a `claude -c` typed by hand, Claude gone, or its id refused.
    pub fn set_remote_session(&self, id: Option<String>) {
        let mut cur = self.remote_session.lock().unwrap();
        if *cur != id {
            *cur = id;
            SAVE_DIRTY.store(true, Ordering::Relaxed);
        }
    }

    /// The far Claude's session id, if known.
    pub fn remote_session(&self) -> Option<String> {
        self.remote_session.lock().unwrap().clone()
    }

    /// Seed from a saved layout.
    pub fn seed_remote_session(&self, id: Option<&str>) {
        *self.remote_session.lock().unwrap() = id.map(str::to_string);
    }

    /// Claude is running in this pane as far as the SCREEN is concerned (remote panes).
    pub fn on_screen(&self) -> bool {
        self.on_screen.load(Ordering::Relaxed)
    }

    /// Whether a spinner frame landed recently enough to count as an in-flight turn.
    fn activity_fresh(&self) -> bool {
        let act = self.activity_ms.load(Ordering::Relaxed);
        act != 0 && now_ms().saturating_sub(act) < WORKING_TTL_MS
    }

    /// Reader: a chunk carrying spinner glyphs arrived, `glyphs` being the fingerprint
    /// of the distinct ones in it (`session::chunk_spinner_key`). Also resolves any
    /// pending permission attention: Claude has resumed, so it's working, not waiting.
    pub fn note_activity(&self, glyphs: u64) {
        let now = now_ms();
        self.last_star_ms.store(now, Ordering::Relaxed);
        // Claude is doing something, so the last slash command is spent: a chooser
        // raised later in this turn is Claude's, not the user's (`/init` and friends).
        self.slash_submit_ms.store(0, Ordering::Relaxed);
        let stop = self.stop_ms.load(Ordering::Relaxed);
        // A spinner frame inside the post-Stop window is the turn's FINAL redraw —
        // ignore it so it can't revive "working" after Stop already ended the turn.
        // (A genuinely new turn's frames land well after the window.)
        if now.saturating_sub(stop) < STOP_SUPPRESS_MS {
            return;
        }
        let act = self.activity_ms.load(Ordering::Relaxed);
        let working_now = act > stop && now.saturating_sub(act) < WORKING_TTL_MS;
        if working_now {
            // Already working: any frame sustains it (bridging the slow `·` frames that
            // aren't in the star range), the established behaviour. Sustaining is
            // deliberately NOT suppressed: a resize or a scroll during a real turn must
            // not drop the working state Claude is genuinely in.
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
        if prev != 0
            && glyphs != prev_glyphs
            && (MIN_FRAME_GAP_MS..=MAX_FRAME_GAP_MS).contains(&gap)
        {
            self.activity_ms.store(now, Ordering::Relaxed);
            self.hook_attention.store(false, Ordering::Relaxed);
        }
    }

    /// Don't let spinner frames START working for `dur_ms`. Called when an action that
    /// doesn't start Claude working causes a repaint: a window/PTY resize, an edit key
    /// (Shift+Enter newline, Shift+Tab mode-cycle) on Windows where ConPTY repaints the
    /// region, or a wheel notch handed to a mouse-reporting Claude, which redraws its
    /// whole screen. Each re-emits on-screen stars that would otherwise pair into a
    /// false "working". An already-working turn keeps being sustained (see
    /// `note_activity`), so this can never blank a genuine working state.
    pub fn suppress_activity(&self, dur_ms: u64) {
        // Extend, never shorten, an existing window — a burst of edit keys each pushes it
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

    /// Resume spinner detection immediately — called on a SUBMIT (Enter). Real working is
    /// imminent after a submit, so an edit-key/resize suppression window must not delay it.
    pub fn clear_suppression(&self) {
        self.suppress_until_ms.store(0, Ordering::Relaxed);
    }

    /// Reader: whether a menu/approval prompt is on the visible screen, and whether
    /// the input box currently holds a slash command.
    ///
    /// Two things make a menu the user's. Claude lists its slash commands as soon as
    /// `/` is typed, so a box reading `/mod` is already showing one — that is what
    /// `slash_input` catches, before Enter is pressed at all. Then Enter clears the
    /// box and the chooser proper opens, which `slash_submit_ms` covers. Once either
    /// has claimed the menu it stays claimed until the menu leaves, so the handover
    /// between the two cannot show a gap.
    pub fn set_menu(&self, on: bool, slash_input: bool) {
        let was = self.menu_on_screen.swap(on, Ordering::Relaxed);
        if on {
            let submitted = self.slash_submit_ms.load(Ordering::Relaxed);
            let just_submitted =
                submitted != 0 && now_ms().saturating_sub(submitted) < SLASH_MENU_WINDOW_MS;
            if slash_input || just_submitted {
                self.menu_user_opened.store(true, Ordering::Relaxed);
            }
        } else if was {
            self.menu_user_opened.store(false, Ordering::Relaxed);
            // The command is spent on the chooser it opened. Anything Claude puts up
            // afterwards is Claude's, even within the window.
            self.slash_submit_ms.store(0, Ordering::Relaxed);
        }
    }

    /// UI: the user submitted a slash command in this pane (see `slash_submit_ms`).
    pub fn note_slash_command(&self) {
        self.slash_submit_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Reader: whether Claude's working status row is on screen, and whether its idle
    /// input box is. The row going away *and the box coming back* is a turn end we can
    /// see, which is what panes with no hooks (every remote one) rely on instead of a
    /// silence in the spinner stream.
    ///
    /// Requiring the box is what makes it safe: a resize or a reflow can catch Claude
    /// mid-redraw, with neither the row nor the box on screen, and that must not read
    /// as a finish. Claude swaps one hint for the other, so exactly one is present
    /// whenever it is drawing at all.
    pub fn set_working_row(&self, on: bool, idle_box: bool) {
        let was = self.working_row.swap(on, Ordering::Relaxed);
        let ended = was
            && !on
            && idle_box
            && !self.waiting_agents.load(Ordering::Relaxed)
            && now_ms() >= self.suppress_until_ms.load(Ordering::Relaxed);
        if ended {
            self.finish_seq.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// How many turns are known to have ended here (see `finish_seq`).
    pub fn finish_seq(&self) -> u64 {
        self.finish_seq.load(Ordering::Relaxed)
    }

    /// Reader: whether Claude's transcript is scrolled away from its live bottom. While it
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
        // Waiting on background agents counts as live even though the Stop hook has
        // already fired: Claude resumes by itself, so scrolling away mid-wait must
        // hold the turn exactly as scrolling away mid-spinner does.
        let live = (act > stop && now.saturating_sub(act) < WORKING_TTL_MS)
            || self.waiting_agents.load(Ordering::Relaxed)
            || self.working_row.load(Ordering::Relaxed);
        if on {
            self.scroll_holds_working.store(live, Ordering::Relaxed);
        } else if self.scroll_holds_working.swap(false, Ordering::Relaxed) {
            self.activity_ms.store(now, Ordering::Relaxed);
        }
    }

    /// Whether Claude's transcript is currently scrolled away from its live bottom.
    pub fn scrolled(&self) -> bool {
        self.scrolled.load(Ordering::Relaxed)
    }

    /// Reader: whether Claude's status row says it is waiting for background agents.
    /// While it does the pane is working (see `waiting_agents`); when the row goes, the
    /// turn Claude resumes with gets a fresh TTL, so the moment before its first frames
    /// pair up is not read as a turn end. A wait the user broke off with Escape ends the
    /// same way, one TTL later.
    pub fn set_waiting_agents(&self, on: bool) {
        let was = self.waiting_agents.swap(on, Ordering::Relaxed);
        if was && !on {
            self.activity_ms.store(now_ms(), Ordering::Relaxed);
        }
    }

    /// Reader: a menu/prompt just LEFT the screen (answered or escaped) → resolve
    /// any hook-set attention. AskUserQuestion fires a permission/elicitation hook
    /// but escaping it produces no spinner/Stop to clear that hook, so it would
    /// hang amber. Called only on the on→off edge, so a markerless prompt (which
    /// never sets a menu) is never cleared prematurely.
    pub fn clear_hook_attention(&self) {
        self.hook_attention.store(false, Ordering::Relaxed);
    }

    /// Delete this pane's statusLine capture file. Called when its command ends (the
    /// OSC-133 idle edge) so a capture left on disk after Claude exits can't keep
    /// re-marking the pane "running" on the next watcher pass. No-op if there's none.
    pub fn clear_capture(&self) {
        if let Some(dir) = crate::shell::app_data_dir() {
            let path = dir
                .join(crate::claude_shim::CAPTURE_SUBDIR)
                .join(format!("{}.json", self.pane_id));
            let _ = std::fs::remove_file(path);
        }
    }

    /// The bound Claude session id (set once a capture matches this pane), for
    /// `claude --resume` on restore.
    pub fn session_id(&self) -> Option<String> {
        self.session_id.lock().unwrap().clone()
    }

    /// The session id ONLY if `claude --resume` will actually find it — i.e. its
    /// transcript exists on disk. A freshly-launched Claude has a session id (shown
    /// in the statusline) but no conversation, so no transcript, so resuming it
    /// errors ("no conversation found"); we return `None` and let restore launch a
    /// clean `claude`. This is the webview's check: the transcript lives at
    /// `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`.
    pub fn resumable_session(&self) -> Option<String> {
        let sid = self.session_id.lock().unwrap().clone()?;
        let cwd = self.cwd.lock().unwrap().clone()?;
        let transcript = dirs::home_dir()?
            .join(".claude")
            .join("projects")
            .join(encode_project_dir(&cwd))
            .join(format!("{sid}.jsonl"));
        transcript.is_file().then_some(sid)
    }

    /// Derived lifecycle: the most recent signal wins; activity counts as
    /// "working" only while fresh, then reverts to ready.
    fn lifecycle(&self) -> Lifecycle {
        // Attention is level-based: a prompt on screen, or an unresolved hook. A menu
        // the user opened themselves is not a prompt (see `slash_submit_ms`).
        let menu = self.menu_on_screen.load(Ordering::Relaxed)
            && !self.menu_user_opened.load(Ordering::Relaxed);
        if menu || self.hook_attention.load(Ordering::Relaxed) {
            return Lifecycle::Attention;
        }
        // Waiting on its own agents: the turn is over on paper (Stop hook, still
        // spinner) but Claude carries on by itself when they report.
        if self.waiting_agents.load(Ordering::Relaxed) {
            return Lifecycle::Working;
        }
        // Claude's own status row says it is working. Level-triggered, so it holds
        // through a frozen row and a stalled read, where the spinner stream below
        // would decay into "ready" and read as a turn end.
        if self.working_row.load(Ordering::Relaxed) {
            return Lifecycle::Working;
        }
        // A turn held across a scroll, where Claude draws no status row at all.
        let held = self.scrolled.load(Ordering::Relaxed)
            && self.scroll_holds_working.load(Ordering::Relaxed);
        if held {
            return Lifecycle::Working;
        }
        let act = self.activity_ms.load(Ordering::Relaxed);
        let stop = self.stop_ms.load(Ordering::Relaxed);
        let now = now_ms();
        // Just stopped: clean turn-end — ignore a trailing spinner frame so it
        // doesn't flicker working→ready→working.
        if stop != 0 && now.saturating_sub(stop) < STOP_SUPPRESS_MS {
            return Lifecycle::Ready;
        }
        // Fallback for a pane whose Claude draws no row we recognise: activity fresh
        // and more recent than the last turn-end. This decays on a timeout, so it
        // drives the dot only — `finish_seq` is what raises a card.
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

    /// Snapshot for the view: stats + the currently-derived lifecycle.
    pub fn snapshot(&self) -> ClaudeStatus {
        let mut s = self.stats.lock().unwrap().clone();
        s.lifecycle = self.lifecycle();
        s
    }
}

/// Claude stores each session's transcript under `~/.claude/projects/<dir>/`,
/// where `<dir>` is the cwd with every non-alphanumeric (and non-`-`) char
/// replaced by `-`. (Mirrors the webview's `encode_project_dir`.)
fn encode_project_dir(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect()
}

static REGISTRY: Mutex<Vec<Weak<ClaudeHandle>>> = Mutex::new(Vec::new());

/// Set by the watcher when a Claude session newly binds to a pane, so the UI knows
/// to persist the layout (capturing "Claude is running here, resume id X") without
/// the watcher needing access to the window state. The UI clears it on save.
pub static SAVE_DIRTY: AtomicBool = AtomicBool::new(false);

/// Register a session handle for the watcher to update.
pub fn register(handle: &Arc<ClaudeHandle>) {
    let mut reg = REGISTRY.lock().unwrap();
    reg.retain(|w| w.strong_count() > 0); // prune closed panes
    reg.push(Arc::downgrade(handle));
}

fn live_handles() -> Vec<Arc<ClaudeHandle>> {
    REGISTRY.lock().unwrap().iter().filter_map(Weak::upgrade).collect()
}

/// Start the capture + hook watcher. Returns the debouncer (keep it alive for
/// the app's lifetime).
pub fn start_watcher() -> Option<Watcher> {
    let data_dir = crate::shell::app_data_dir()?;
    let capture_dir = data_dir.join(crate::claude_shim::CAPTURE_SUBDIR);
    let hooks_dir = data_dir.join(crate::claude_shim::HOOKS_SUBDIR);
    let _ = std::fs::create_dir_all(&capture_dir);
    let _ = std::fs::create_dir_all(&hooks_dir);
    // Drop leftovers from a previous run before we start binding. Capture/hook files
    // are keyed by pane id, and pane ids reuse across runs, so a stale file could
    // falsely bind to a new pane (and now mark Claude "running"). They're transient —
    // the durable resume id lives in session.json — so clearing them loses nothing.
    for d in [&capture_dir, &hooks_dir] {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    let (cap, hk) = (capture_dir.clone(), hooks_dir.clone());
    let mut deb = new_debouncer(Duration::from_millis(80), move |res: DebounceEventResult| {
        if res.is_ok() {
            process_captures(&cap);
            process_hooks(&hk);
        }
    })
    .ok()?;
    deb.watcher().watch(&capture_dir, RecursiveMode::NonRecursive).ok()?;
    deb.watcher().watch(&hooks_dir, RecursiveMode::NonRecursive).ok()?;
    // Process once up front so existing captures bind immediately.
    process_captures(&capture_dir);
    process_hooks(&hooks_dir);
    Some(deb)
}

/// Bind each pane to ITS capture by pane id: the capture file is named
/// `<pane_id>.json` (the `PANE_ID_ENV` we set on the shell, which rides through
/// to Claude's statusLine subcommand exactly like `CAPTURE_DIR` does). So a
/// capture binds to the EXACT pane that launched Claude — correct even when
/// several panes share a cwd or many launch at once. No cwd matching: cwd can't
/// disambiguate same-folder panes, and a cwd fallback could briefly cross-bind a
/// sibling's session during a launch race.
fn process_captures(dir: &Path) {
    let handles = live_handles();
    let caps = crate::claude_shim::read_captures(dir);
    crate::claude_shim::debug_log(&format!("process_captures: {} capture(s)", caps.len()));
    for h in &handles {
        let pid = h.pane_id.to_string();
        let Some(c) = caps.iter().find(|c| c.key == pid) else {
            continue;
        };
        // A capture for this pane means Claude wrote its statusLine here → it's
        // running. This is the reliable, event-driven launch signal: Claude (via our
        // injected --settings) writes the capture itself, so we no longer depend on the
        // process scan, which missed slow cold launches. Stale files can't false-trigger
        // — the dir is cleared on startup and a pane's capture is removed when its
        // command ends (ClaudeHandle::clear_capture), so a present capture == live.
        if !h.claude_running.swap(true, Ordering::Relaxed) {
            SAVE_DIRTY.store(true, Ordering::Relaxed); // newly running → persist for restore
        }
        crate::claude_shim::debug_log(&format!(
            "process_captures: pane {pid} -> capture key={} session={}",
            c.key, c.session_id
        ));
        {
            // Bind the session id; flag a save only when it's NEWLY bound (not on
            // every statusline refresh), so the restored layout knows to resume it.
            let mut sid = h.session_id.lock().unwrap();
            if sid.as_deref() != Some(c.session_id.as_str()) {
                *sid = Some(c.session_id.clone());
                SAVE_DIRTY.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// Re-read hook signals and apply them to the pane whose bound session id matches.
fn process_hooks(dir: &Path) {
    let handles = live_handles();
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let sid = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let signal = v.get("signal").and_then(|s| s.as_str()).unwrap_or("");
        let nonce = v.get("nonce").and_then(|n| n.as_u64()).unwrap_or(0) as u128;
        let Some(h) = handles
            .iter()
            .find(|h| h.session_id.lock().unwrap().as_deref() == Some(sid.as_str()))
        else {
            continue;
        };
        let mut last = h.last_nonce.lock().unwrap();
        if nonce <= *last {
            continue;
        }
        *last = nonce;
        match signal {
            "attention" => h.hook_attention.store(true, Ordering::Relaxed),
            "stop" => {
                h.stop_ms.store(now_ms(), Ordering::Relaxed);
                h.hook_attention.store(false, Ordering::Relaxed);
                // The authoritative turn end. Not counted while Claude is waiting on
                // its own agents: it fires then too, and Claude resumes by itself.
                if !h.waiting_agents.load(Ordering::Relaxed) {
                    h.finish_seq.fetch_add(1, Ordering::Relaxed);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{encode_project_dir, ClaudeHandle, Lifecycle};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// One animation-gap between frames: over MIN_FRAME_GAP_MS, well under MAX.
    const FRAME: Duration = Duration::from_millis(40);

    fn handle() -> Arc<ClaudeHandle> {
        ClaudeHandle::new(1, None, Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(true)), None)
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
        h.set_menu(false, false);
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
        h.set_menu(false, false);
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
        // Claude's working row going, with its input box back: that is a turn end.
        h.set_working_row(true, false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.set_working_row(false, true);
        assert_eq!(h.finish_seq(), 1);
    }

    // A resize catches Claude mid-redraw with neither the row nor the box on screen.
    #[test]
    fn a_redraw_without_the_input_box_is_not_a_turn_end() {
        let h = handle();
        h.set_working_row(true, false);
        h.set_working_row(false, false);
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
        h.set_waiting_agents(true);
        h.set_scrolled(true);
        h.set_waiting_agents(false); // the row is off screen now
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

    // Chrome on screen latches the pane as running Claude; chrome gone clears it. This
    // is what makes a remote pane light up and, when Claude exits on the far host, go
    // dark again on the very next chunk (the returning remote prompt).
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
        assert!(h.scroll_holds_working.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.set_scrolled(false);
        assert!(!h.scroll_holds_working.load(std::sync::atomic::Ordering::Relaxed));
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
        h.set_waiting_agents(true);
        h.stop_ms.store(super::now_ms(), std::sync::atomic::Ordering::Relaxed);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        std::thread::sleep(Duration::from_millis(super::STOP_SUPPRESS_MS + 50));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);
        h.set_waiting_agents(false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);

        // A prompt on screen still outranks the wait.
        h.set_waiting_agents(true);
        h.set_menu(true, false);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
    }

    #[test]
    fn chrome_latches_claude_on_and_off() {
        let h = handle();
        assert!(!h.on_screen());
        h.note_screen(true);
        assert!(h.on_screen());
        h.note_screen(false);
        assert!(!h.on_screen());
    }

    // The mid-turn case. While Claude works it swaps its hint line for the interrupt
    // hint, so the chrome scan comes back empty even though Claude is very much there.
    // Clearing on that would drop the pane's Claude state (and its dot) mid-answer.
    #[test]
    fn chrome_gone_during_a_live_turn_does_not_clear() {
        let h = handle();
        h.note_screen(true);
        // Two differing spinner frames an animation gap apart = a turn is under way.
        h.note_activity(0b1000);
        std::thread::sleep(FRAME);
        h.note_activity(0b0100);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Working);

        h.note_screen(false);
        assert!(h.on_screen(), "a working turn must keep the pane marked as Claude");
    }

    // The startup command is latched the moment ssh is detected, not when something
    // reads it. A save only happens on a layout change, so a lazy latch ran at quit time
    // and captured whatever had been typed since: `claude` at the remote prompt.
    #[test]
    fn latches_the_ssh_command_not_what_was_typed_afterwards() {
        let h = handle();
        h.note_command("ssh mini".into());
        h.set_remote(true); // busy-edge scan finds the ssh client
        h.note_command("claude".into()); // typed at the remote prompt
        assert_eq!(h.startup_cmd().as_deref(), Some("ssh mini"));
    }

    // Everything typed while connecting goes through the same tracking, passphrases
    // included, so only a recognised client invocation may ever be latched.
    #[test]
    fn refuses_to_latch_anything_that_is_not_a_remote_client() {
        for typed in ["correct horse battery staple", "claude", "cd ~/src"] {
            let h = handle();
            h.note_command(typed.into());
            h.set_remote(true);
            assert!(h.startup_cmd().is_none(), "{typed:?} must not be latched");
        }
    }

    // A key passphrase or login password is typed at ssh's own prompt and submitted
    // with Enter, so it arrives here exactly like a command would. It must not be
    // retained at all, and it must not displace the connection command that preceded
    // it, which is still needed for the latch.
    #[test]
    fn a_passphrase_entered_after_the_ssh_command_is_not_retained() {
        let h = handle();
        h.note_command("ssh tre@10.0.0.16".into());
        h.note_command("correct horse battery staple".into()); // key passphrase
        h.note_command("hunter2".into()); // login password
        h.set_remote(true);
        assert_eq!(h.startup_cmd().as_deref(), Some("ssh tre@10.0.0.16"));
    }

    // A save written before the whitelist existed can hold anything. Seeding it on
    // restore has to reject it, or it would be replayed and written straight back out,
    // outliving the fix.
    #[test]
    fn a_poisoned_saved_command_is_rejected_on_restore() {
        let h = handle();
        assert!(!h.set_startup_cmd("claude"), "must not be accepted");
        assert!(h.startup_cmd().is_none());
        // A legitimate one still seeds, and then wins over any later typing.
        assert!(h.set_startup_cmd("ssh mini"));
        assert_eq!(h.startup_cmd().as_deref(), Some("ssh mini"));
        h.note_command("ssh other-host".into());
        h.set_remote(true);
        assert_eq!(h.startup_cmd().as_deref(), Some("ssh mini"), "first command wins");
    }

    // Leaving the ssh session drops both, so the far host's Claude can't linger on a
    // pane that is back at a local prompt.
    #[test]
    fn clearing_remote_also_drops_the_chrome_latch() {
        let h = handle();
        h.set_remote(true);
        h.note_screen(true);
        assert!(h.is_remote() && h.on_screen());
        h.set_remote(false);
        assert!(!h.is_remote());
        assert!(!h.on_screen());
    }

    // Leaving a session with `exit` makes the pane local again: no Reconnect, nothing to
    // replay on the next launch, and the next `ssh` typed defines a new connection.
    #[test]
    fn leaving_a_session_forgets_the_connection() {
        let h = handle();
        h.note_command("ssh mini".into());
        h.set_remote(true);
        h.note_credential_prompt(None);
        h.note_remote_cwd("/home/tre/src".into());
        h.note_screen(true);
        h.set_remote(false);
        h.forget_connection();
        assert!(h.startup_cmd().is_none());
        assert!(!h.was_remote());
        assert_eq!(h.prompts_for_credential(), None);
        assert!(h.remote_cwd().is_none());
        assert!(!h.remote_claude());
        // The next connection latches afresh.
        h.note_command("ssh other".into());
        h.set_remote(true);
        assert_eq!(h.startup_cmd().as_deref(), Some("ssh other"));
        assert!(h.was_remote());
    }

    // A restored pane whose ssh fails before the process scan sees it still has to
    // offer Reconnect, so the saved command alone marks the pane as having been remote.
    #[test]
    fn a_seeded_startup_command_marks_the_pane_remote() {
        let h = handle();
        assert!(!h.was_remote());
        assert!(h.set_startup_cmd("ssh mini"));
        assert!(h.was_remote());
        assert!(!h.is_remote(), "seeding does not pretend the connection is up");
    }

    // One automatic retry per established connection, none for a refusal that never got
    // established, and a fresh one once the next connection is up.
    #[test]
    fn a_dropped_connection_earns_one_retry() {
        let h = handle();
        assert!(!h.request_retry(Duration::ZERO), "never connected: nothing to retry");
        h.set_remote(true);
        assert!(!h.request_retry(Duration::from_secs(3600)), "too short to count as established");
        assert!(h.request_retry(Duration::ZERO));
        assert!(h.take_retry_wanted());
        assert!(!h.take_retry_wanted(), "taken once");
        assert!(!h.request_retry(Duration::ZERO), "the one retry is spent");
        h.set_remote(true);
        assert!(h.request_retry(Duration::ZERO), "a new connection earns another");
    }

    // A connect that times out keeps ssh running for as long as it waits, which is not
    // a session that dropped. Without this rule an unreachable host was retried for as
    // long as it stayed down.
    #[test]
    fn an_attempt_that_never_reached_the_host_earns_no_retry() {
        let h = handle();
        h.set_remote(true);
        h.note_connect_failure();
        assert!(!h.request_retry(Duration::ZERO), "long, but never up");
        // Evidence of a real session outranks the duration rule either way.
        let h = handle();
        h.set_remote(true);
        h.note_login_evidence();
        assert!(h.request_retry(Duration::from_secs(3600)), "logged in: a drop, however quick");
        let h = handle();
        h.set_remote(true);
        h.note_credential_prompt(None);
        assert!(h.request_retry(Duration::from_secs(3600)), "reached the prompt: the host was up");
    }

    // Inferred directories can be undone when the far shell rejects the cd, but a
    // reported one (the snippet) is never second-guessed by the caller.
    #[test]
    fn a_rejected_cd_restores_the_previous_directory() {
        let h = handle();
        h.set_remote_cwd(Some("~".into()));
        h.set_remote_cwd(Some("~/Do".into()));
        h.revert_remote_cwd();
        assert_eq!(h.remote_cwd().as_deref(), Some("~"));
        h.revert_remote_cwd();
        assert_eq!(h.remote_cwd().as_deref(), Some("~"), "nothing further to undo");
    }

    // The directory outlives the connection (Reconnect and restore need it); the
    // "reported on this connection" fact does not.
    #[test]
    fn the_remote_directory_survives_the_connection_ending() {
        let h = handle();
        h.set_remote(true);
        h.note_remote_cwd("/home/tre/src".into());
        assert!(h.remote_cwd_reported());
        h.set_remote(false);
        assert_eq!(h.remote_cwd().as_deref(), Some("/home/tre/src"));
        assert!(!h.remote_cwd_reported());
    }

    // Claude still on screen when the connection ends is what the next connection
    // resumes; a Claude that had already exited is not.
    #[test]
    fn a_far_claude_alive_at_the_drop_is_resumed_next_time() {
        let h = handle();
        h.set_remote(true);
        h.note_screen(true);
        assert!(h.remote_claude());
        h.set_remote(false);
        assert!(h.remote_claude(), "pending for the next connection");
        assert!(h.take_remote_claude_pending());
        assert!(!h.take_remote_claude_pending(), "taken once");

        let h = handle();
        h.set_remote(true);
        h.note_screen(true);
        h.note_screen(false); // Claude exited at the remote prompt
        h.set_remote(false);
        assert!(!h.remote_claude());

        // A stale pending flag does not survive a new connection being detected.
        let h = handle();
        h.seed_remote_claude(true);
        h.set_remote(true);
        assert!(!h.take_remote_claude_pending());
    }

    // The keyboard is a second witness: `claude` typed at the far prompt counts as
    // running until another command is typed at a shell prompt, or its chrome leaves.
    #[test]
    fn claude_typed_at_the_far_prompt_counts_as_running() {
        let h = handle();
        h.set_remote(true);
        h.set_remote_claude_typed(true);
        assert!(h.remote_claude());
        h.set_remote(false);
        assert!(h.take_remote_claude_pending(), "resumed on the next connection");

        let h = handle();
        h.set_remote(true);
        h.set_remote_claude_typed(true);
        h.set_remote_claude_typed(false); // `ls` at the shell prompt: Claude has exited
        h.set_remote(false);
        assert!(!h.remote_claude());

        let h = handle();
        h.set_remote(true);
        h.set_remote_claude_typed(true);
        h.note_screen(true);
        h.note_screen(false); // chrome gone with no turn in flight
        assert!(!h.remote_claude(), "the screen outranks what was typed");
    }

    // What is known about a connection's prompting: unknown until seen; a prompt during
    // login says yes; a login completing with no prompt says no; a prompt after the login
    // (a nested ssh on the far host) says nothing about this connection.
    #[test]
    fn learns_whether_a_connection_prompts() {
        let h = handle();
        assert_eq!(h.prompts_for_credential(), None);
        h.set_remote(true);
        h.note_credential_prompt(None);
        assert_eq!(h.prompts_for_credential(), Some(true));
        assert!(h.note_login_evidence(), "first evidence on this connection");
        assert!(!h.note_login_evidence(), "only the first counts");
        assert_eq!(h.prompts_for_credential(), Some(true), "it did prompt");

        let h = handle();
        h.set_remote(true);
        assert!(h.note_login_evidence());
        assert_eq!(h.prompts_for_credential(), Some(false));
        h.note_credential_prompt(None); // a nested ssh, after login
        assert_eq!(h.prompts_for_credential(), Some(false));

        let h = handle();
        assert!(!h.note_login_evidence(), "not remote: no connection to speak of");
        assert_eq!(h.prompts_for_credential(), None);
    }

    // A typed credential followed by another prompt (or a denial) is a rejection, and
    // is reported once per typed credential.
    #[test]
    fn a_repeated_prompt_after_typing_is_a_rejection() {
        let h = handle();
        assert!(!h.note_credential_rejected(), "nothing was typed");
        h.note_credential_typed();
        assert!(h.note_credential_rejected());
        assert!(h.take_reask_wanted());
        assert!(!h.note_credential_rejected(), "already reported for that credential");
        assert!(!h.take_reask_wanted());
    }

    #[test]
    fn encodes_cwd_like_claude() {
        // Matches the real on-disk dir under ~/.claude/projects/.
        assert_eq!(
            encode_project_dir("/Users/tor/Private/Source/arbiter-app"),
            "-Users-tor-Private-Source-arbiter-app"
        );
        assert_eq!(encode_project_dir("/a/b-c.d_e"), "-a-b-c-d-e");
    }
}
