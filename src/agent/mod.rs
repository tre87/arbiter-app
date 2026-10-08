//! The seam between Arbiter and the coding agents it hosts. Each agent CLI is one file
//! here implementing `Agent`; everything else in the app (the shim, the event watcher,
//! the screen reader, the lifecycle state machine, persistence, the UI) talks to agents
//! only through this interface. Adding an agent is one new file plus one line in the
//! `agents!` registry below.
//!
//! What an agent supplies is mostly data: which process it is, how to hand it Arbiter's
//! hooks at launch, how its hook payloads map onto `AgentEvent`, which strings and glyphs
//! on its screen mean working, idle or waiting on the user (`ScreenProfile`), which
//! terminal sequences it emits for the same (`SignalProfile`), and how to resume one of
//! its conversations.

use std::path::{Path, PathBuf};

pub mod claude;
pub mod codex;
pub mod copilot;

/// Declares every agent once: the `AgentKind` variants, their serde names and the
/// registry. `ALL[kind as usize]` is that kind's agent; the conformance test pins it.
macro_rules! agents {
    ($($kind:ident => $ty:path),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum AgentKind {
            $($kind),+
        }

        static ALL: &[&'static dyn Agent] = &[$(&$ty),+];
    };
}

agents! {
    Claude => claude::Claude,
    Copilot => copilot::Copilot,
    Codex => codex::Codex,
}

/// Every registered agent, in `AgentKind` order.
pub fn all() -> &'static [&'static dyn Agent] {
    ALL
}

pub fn get(kind: AgentKind) -> &'static dyn Agent {
    ALL[kind as usize]
}

/// The agent whose `id()` is `id` (the word after `arbiter agent-hook`).
pub fn by_id(id: &str) -> Option<&'static dyn Agent> {
    ALL.iter().copied().find(|a| a.id() == id)
}

impl AgentKind {
    pub fn agent(self) -> &'static dyn Agent {
        get(self)
    }

    /// Inverse of `self as u8`, for the lock-free kind slot on the handle.
    pub fn from_index(i: u8) -> Option<Self> {
        ALL.get(i as usize).map(|a| a.kind())
    }
}

/// What a hook, a terminal sequence or the screen says happened, in Arbiter's terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEvent {
    /// A conversation is live in the pane: startup, a resume, `/clear`, a compaction.
    /// `transcript` is where the agent keeps it, when the agent says.
    SessionBound { id: String, transcript: Option<String> },
    /// The user submitted a prompt.
    TurnStarted,
    /// The agent is blocked on the user (a permission prompt, an approval).
    NeedsAttention,
    /// The turn is over.
    TurnEnded,
    /// The user stopped the turn.
    Interrupted,
    /// The conversation ended. Not the process: Claude fires this on `/clear` too.
    SessionEnded,
}

/// A hook invocation, normalized: the conversation it came from (for agents that host
/// several in one process) and what it means.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HookOutcome {
    pub session: Option<String>,
    pub events: Vec<AgentEvent>,
}

/// What a command line typed at a far shell's prompt means for the pane's agent.
#[derive(Debug, PartialEq, Eq)]
pub enum TypedLaunch {
    NotThis,
    /// A plain interactive launch naming no conversation: Arbiter may name one.
    Bare,
    /// Launched into a named conversation.
    Named(String),
    /// Something Arbiter cannot follow: continue-last, a subcommand, a one-shot prompt.
    Other,
}

/// What an agent's `launch` is given.
pub struct LaunchCtx<'a> {
    /// This Arbiter executable, which serves `agent-hook`.
    pub arbiter_exe: &'a Path,
    /// A directory the agent may write its launch files into, owned by Arbiter.
    pub dir: &'a Path,
}

/// How the launcher on the pane's PATH runs an agent: files to write first, environment
/// for the pane's shell, and arguments to put before the user's own. Every argument is
/// carried by an environment variable so the launcher scripts never quote a value.
#[derive(Default)]
pub struct LaunchSpec {
    pub files: Vec<(PathBuf, String)>,
    pub env: Vec<(String, String)>,
    /// `(flag, env var)`: the launcher passes `flag "$VAR"` when the variable is set.
    /// The first variable also gates the rest, as `ARBITER_CLAUDE_SETTINGS` always has.
    pub args: Vec<(&'static str, String)>,
}

/// One agent CLI. Implemented by a unit struct per file in this module.
pub trait Agent: Sync {
    fn kind(&self) -> AgentKind;
    /// Stable lowercase name: the hook subcommand's argument and the env-var infix.
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn icon_svg(&self) -> &'static str;
    /// The command names its launcher shadows on the pane's PATH.
    fn binaries(&self) -> &'static [&'static str];

    /// Whether a process (`name` plus genuine argv, leaked `KEY=VALUE` entries already
    /// removed) is this agent.
    fn matches_process(&self, name: &str, argv: &[String]) -> bool;

    /// How the launcher hands Arbiter's hooks to this agent. `None` runs it untouched.
    fn launch(&self, ctx: &LaunchCtx) -> Option<LaunchSpec>;

    /// Translate one hook invocation (`args` after `agent-hook <id>`, and stdin).
    fn normalize_hook(&self, args: &[String], stdin: &[u8]) -> HookOutcome;

    /// Runs after the hook's events are recorded, for an agent whose hook took the place
    /// of something of the user's that must still run.
    fn after_hook(&self, _args: &[String], _stdin: &[u8]) {}

    /// Whether one process can host several conversations (tabs).
    fn multi_session(&self) -> bool {
        false
    }

    fn screen(&self) -> &'static ScreenProfile;

    fn signals(&self) -> &'static SignalProfile {
        &SignalProfile::NONE
    }

    /// The line that relaunches this agent in a local shell: into `session` when known,
    /// else fresh. Never "the most recent conversation here", which could be another
    /// pane's.
    fn resume_local(&self, session: Option<&str>) -> String;

    /// The command that brings the agent back on a far host, as an `a || b || c` chain.
    fn resume_remote(&self, session: Option<&str>) -> String;

    fn classify_launch(&self, cmdline: &str) -> TypedLaunch;

    /// The flag that starts a new conversation under an id Arbiter chooses, if the agent
    /// has one. Remote session naming is only offered for such agents.
    fn name_flag(&self) -> Option<&'static str> {
        None
    }

    /// The agent's words for a resume naming a conversation it no longer has.
    fn gone_markers(&self) -> &'static [&'static str] {
        &[]
    }

    /// Where the agent keeps `session`'s transcript when the hook did not say.
    fn derived_transcript(&self, _session: &str, _cwd: &str) -> Option<PathBuf> {
        None
    }

    /// Whether a conversation id only ever arrives with a finished turn (Codex's
    /// `notify`), so it names a conversation that exists even with no transcript to check.
    fn bound_after_a_turn(&self) -> bool {
        false
    }

    /// Bytes for Shift+Enter: a newline in the agent's input box.
    fn shift_enter(&self) -> &'static [u8] {
        b"\n"
    }

    /// Whether the frozen-status-row nudge applies (anthropics/claude-code#94443).
    fn wheel_nudge(&self) -> bool {
        false
    }

    fn version_args(&self) -> &'static [&'static str] {
        &["--version"]
    }
}

/// A run of code points, inclusive.
#[derive(Clone, Copy, Debug)]
pub struct GlyphRange {
    pub lo: u32,
    pub hi: u32,
}

impl GlyphRange {
    pub const fn one(c: char) -> Self {
        Self { lo: c as u32, hi: c as u32 }
    }

    pub fn contains(&self, c: char) -> bool {
        (self.lo..=self.hi).contains(&(c as u32))
    }
}

/// Spinner glyphs and the fingerprint bit each maps to (see `agent_screen::activity_key`).
/// The bit is `base + (c - lo)`, folded modulo `fold` when it is non-zero; every bit must
/// be below 64.
#[derive(Clone, Copy, Debug)]
pub struct GlyphBits {
    pub range: GlyphRange,
    pub base: u32,
    pub fold: u32,
}

/// Where a row rule looks for its words.
#[derive(Clone, Copy, Debug)]
pub enum Lead {
    /// Only on a status row: a status glyph and a space open it, and the words are read
    /// from what follows (see `ScreenProfile::status_glyphs`).
    StatusGlyph,
    /// Anywhere in the row.
    Anywhere,
}

/// A row that means something: its text (per `lead`) starts with `starts`, contains
/// every one of `all`, and at least one of `any` unless `any` is empty.
#[derive(Clone, Copy, Debug)]
pub struct RowRule {
    pub lead: Lead,
    pub starts: &'static str,
    pub all: &'static [&'static str],
    pub any: &'static [&'static str],
}

/// A live chooser's footer, looked for among the bottom `rows` rows.
#[derive(Clone, Copy, Debug)]
pub struct MenuRule {
    pub rows: usize,
    /// A row containing one of these is a chooser.
    pub anywhere: &'static [&'static str],
    /// A row that is exactly one of these once `edges` and blanks are trimmed.
    pub whole_row: &'static [&'static str],
    pub edges: &'static [char],
    /// A row holding at least `min_hints` of these is a chooser's footer.
    pub hints: &'static [&'static str],
    pub min_hints: usize,
}

/// The input box's prompt line.
#[derive(Clone, Copy, Debug)]
pub struct PromptShape {
    /// A row starting with one of these (after blanks and `side`) is the prompt.
    pub glyphs: &'static [char],
    /// A plainer prompt that only counts inside the box: a border directly above, and
    /// another within `box_lines` below.
    pub boxed: Option<&'static str>,
    pub border: GlyphRange,
    pub side: char,
    pub box_lines: usize,
}

/// Everything the reader looks for on an agent's screen. Pure data, so a UI change in a
/// new release of the CLI is an edit here, not new code.
#[derive(Debug)]
pub struct ScreenProfile {
    /// In the cursor's row or `chrome_rows_below` under it: the idle input box's hint
    /// line. How a remote agent is recognised, and half of a visible turn end.
    pub chrome: &'static [&'static str],
    pub chrome_rows_below: usize,
    /// On the same row as a chrome marker, the row is NOT this agent's chrome. Only
    /// consulted when the kind is being discovered (`VtTerm::detect`), never once bound.
    pub chrome_veto: &'static [&'static str],
    /// In the same cursor-anchored window: the hint shown only while a turn runs.
    pub working_footer: &'static [&'static str],
    /// The first glyph of a status row, before a space.
    pub status_glyphs: &'static [GlyphRange],
    /// Among the bottom `live_rows`: a row saying a turn runs.
    pub working_rows: &'static [RowRule],
    pub live_rows: usize,
    /// Among the bottom `live_rows`: the bar shown while scrolled away from the bottom.
    pub scrolled: &'static [&'static str],
    pub menu: MenuRule,
    /// The last row above the input box saying background work is outstanding.
    pub waiting_rows: &'static [RowRule],
    pub prompt: PromptShape,
    /// Task-list items, which do not count as "the last row above the box".
    pub task_marks: &'static [&'static str],
    pub task_hang: char,
    /// What the input row's text is framed by, for telling a slash command.
    pub slash_edges: &'static [char],
    /// Spinner glyphs in the raw output (titles included) that fingerprint a frame.
    pub activity_glyphs: &'static [GlyphBits],
}

/// Terminal sequences an agent emits about its own state (see `term_signal`).
#[derive(Debug)]
pub struct SignalProfile {
    /// A window title starting with one of these glyphs means working.
    pub title_working: &'static [GlyphRange],
    /// A window title containing one of these means blocked on the user.
    pub title_attention: &'static [&'static str],
    /// An OSC 9 / 777 notification starting with one of these means blocked on the user.
    pub notify_attention: &'static [&'static str],
    /// Any other notification means the turn ended.
    pub notify_is_turn_end: bool,
    /// OSC 9;4 progress (busy or a percentage) means working.
    pub progress_is_working: bool,
    /// A bell means blocked on the user.
    pub bell_is_attention: bool,
}

impl SignalProfile {
    pub const NONE: SignalProfile = SignalProfile {
        title_working: &[],
        title_attention: &[],
        notify_attention: &[],
        notify_is_turn_end: false,
        progress_is_working: false,
        bell_is_attention: false,
    };
}

/// JSON field lookup that tolerates the two spellings agents use for the same field.
pub(crate) fn field<'a>(v: &'a serde_json::Value, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|n| v.get(*n).and_then(|s| s.as_str()))
}

/// Whether `id` is safe to use as a session id anywhere: a file name, a command line.
pub fn plausible_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The user's home, as the agents' own `os.homedir()` resolves it: the profile folder on
/// Windows, whatever `HOME` says there, and `$HOME` elsewhere.
pub(crate) fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// A config dir: `$env` when set and not blank, else `~/<default>`.
pub(crate) fn config_dir_from(env: &str, default: &str) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(env).filter(|d| !d.to_string_lossy().trim().is_empty()) {
        return Some(PathBuf::from(dir));
    }
    Some(home_dir()?.join(default))
}

/// The arbiter executable as hook commands spell it: forward slashes on Windows, where
/// the command may run under Git Bash, for which backslashes are escapes.
pub(crate) fn hook_exe(exe: &Path) -> String {
    let s = exe.display().to_string();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_indexed_by_kind_and_names_are_unique() {
        for (i, a) in all().iter().enumerate() {
            assert_eq!(a.kind() as usize, i, "{} is out of order", a.id());
            assert_eq!(get(a.kind()).id(), a.id());
            assert_eq!(by_id(a.id()).map(|b| b.kind()), Some(a.kind()));
            assert_eq!(AgentKind::from_index(i as u8), Some(a.kind()));
        }
        let mut ids: Vec<_> = all().iter().map(|a| a.id()).collect();
        let mut bins: Vec<_> = all().iter().flat_map(|a| a.binaries().iter().copied()).collect();
        ids.sort();
        bins.sort();
        assert!(ids.windows(2).all(|w| w[0] != w[1]), "duplicate agent id");
        assert!(bins.windows(2).all(|w| w[0] != w[1]), "two agents shadow one binary");
    }

    // An empty marker matches every row, which would light every pane.
    #[test]
    fn no_profile_has_an_empty_marker() {
        for a in all() {
            let s = a.screen();
            let lists: [&[&str]; 5] = [s.chrome, s.chrome_veto, s.working_footer, s.scrolled, s.task_marks];
            for list in lists {
                assert!(list.iter().all(|m| !m.is_empty()), "{}: empty marker", a.id());
            }
            for r in s.working_rows.iter().chain(s.waiting_rows) {
                assert!(!(r.starts.is_empty() && r.all.is_empty() && r.any.is_empty()), "{}: rule matches everything", a.id());
            }
            for b in s.activity_glyphs {
                let top = b.base + if b.fold > 0 { b.fold - 1 } else { b.range.hi - b.range.lo };
                assert!(top < 64, "{}: activity bit out of range", a.id());
            }
            let sig = a.signals();
            assert!(sig.title_attention.iter().chain(sig.notify_attention).all(|m| !m.is_empty()));
        }
    }

    // A plain shell must never read as any agent's chrome.
    #[test]
    fn plain_shell_output_is_nobody_s_chrome() {
        let corpus = [
            "$ cargo build --bin arbiter",
            "   Compiling arbiter-native v1.6.1",
            "total 48",
            "drwxr-xr-x  12 tre  staff   384 Sep  8 16:48 src",
            "PS C:\\Users\\TRE\\source\\arbiter-app> ",
            "tre@mini ~/src % ",
            "\u{276f} ",
            "error: could not compile `arbiter-native`",
        ];
        for a in all() {
            for row in corpus {
                assert!(!a.screen().chrome.iter().any(|m| row.contains(m)), "{} claims {row:?}", a.id());
            }
        }
    }

    #[test]
    fn session_ids_are_checked_before_use() {
        assert!(plausible_session_id("3f2a9c1e-7b4d-4e8a-9f01-2c5d6e7f8a9b"));
        assert!(!plausible_session_id(""));
        assert!(!plausible_session_id("x; rm -rf ~"));
        assert!(!plausible_session_id("../../etc"));
        assert!(!plausible_session_id(&"a".repeat(65)));
    }
}
