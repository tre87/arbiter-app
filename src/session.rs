//! Core terminal session — the seed of the Tauri-free backend the native app
//! drives directly (no IPC, no events bus). A PTY + headless VT term + a reader
//! thread that feeds the grid and parses OSC-7 (cwd) / OSC-133 (shell busy/idle)
//! / BEL into shared state. Ported from `src-tauri/src/pty.rs`, minus the
//! webview/xterm streaming, flow control and Claude monitoring (those follow as
//! features land). cwd/shell-idle are tracked here and read by the UI; later
//! they drive the per-pane status + the overview, and `core` grows claude/git/shim.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::term::VtTerm;

/// Bumped on each OSC-133 idle→busy edge; the Claude monitor waits on it.
type CmdEpoch = Arc<(Mutex<u64>, Condvar)>;
/// Native FS watcher for the current repo (refreshes git on external edits).
type GitWatcher = Debouncer<RecommendedWatcher>;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Last-resort delay before a queued startup command is sent even though the pane never
/// reported itself ready. Only reachable when shell integration is broken (no OSC-133 at
/// all), where the alternative is the command silently never running.
const STARTUP_CMD_FALLBACK: Duration = Duration::from_secs(5);

/// Holds a pane's startup command until the pane can safely be typed into, then sends it.
///
/// Two things have to be true, and they arrive as unrelated events on different threads:
///
///   * the shell has reached a prompt, so something is actually reading the PTY;
///   * the PTY has its real size, because panes are created at 80x24 and are resized on
///     their first rendered frame.
///
/// The second one is the subtle one. A resize lands as SIGWINCH on whatever is running,
/// and Git's MSYS build of ssh does not survive one during its echo-off passphrase read:
/// the read is abandoned, it reports "incorrect passphrase", and ssh falls back to
/// password auth. So a command injected before the startup resize can have its program
/// destroyed by a resize Arbiter itself caused a moment later. (Native Windows OpenSSH
/// shrugs it off, which is why this only appeared when Arbiter was launched from Git
/// Bash, whose PATH puts Git's ssh first.)
///
/// Both notifications simply record their fact and then try to release, so whichever
/// arrives last performs the send. No polling and no settle window: `take` makes it
/// once-only, and ordering does not matter.
pub struct StartupGate {
    cmd: Mutex<Option<String>>,
    prompt_seen: AtomicBool,
    resized: AtomicBool,
    tx: std::sync::mpsc::Sender<Vec<u8>>,
}

impl StartupGate {
    fn new(tx: std::sync::mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            cmd: Mutex::new(None),
            prompt_seen: AtomicBool::new(false),
            resized: AtomicBool::new(false),
            tx,
        }
    }

    /// Hold `cmd` until the pane is ready. Replaces anything already queued.
    fn queue(&self, cmd: &str) {
        *self.cmd.lock().unwrap() = Some(cmd.to_string());
        self.release_if_ready();
    }

    /// The shell reached a prompt (an OSC-133 idle edge).
    pub fn note_prompt(&self) {
        self.prompt_seen.store(true, Ordering::Relaxed);
        self.release_if_ready();
    }

    /// The PTY was resized, so it now has a real size rather than the initial 80x24.
    pub fn note_resized(&self) {
        self.resized.store(true, Ordering::Relaxed);
        self.release_if_ready();
    }

    fn release_if_ready(&self) {
        if !(self.prompt_seen.load(Ordering::Relaxed) && self.resized.load(Ordering::Relaxed)) {
            return;
        }
        self.send();
    }

    /// Send the queued command, if there still is one.
    fn send(&self) {
        if let Some(cmd) = self.cmd.lock().unwrap().take() {
            let _ = self.tx.send(format!("{cmd}\r").into_bytes());
        }
    }
}

/// A credential a restored pane will type at ssh's prompt, held in memory only.
///
/// Deliberately not a `String`: it carries a redacting `Debug` so it cannot be printed
/// into a log or a panic message by accident, and it overwrites its bytes on drop. That
/// last part is best-effort rather than a guarantee, since the plain `String` the dialog
/// collected still exists in the UI's own state until that is cleared.
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(s: &str) -> Self {
        Self(s.as_bytes().to_vec())
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The secret plus the Return that submits it.
    fn line(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.0.len() + 1);
        out.extend_from_slice(&self.0);
        out.push(b'\r');
        out
    }

    /// A second copy, for arming another pane from the same vault entry. Deliberately
    /// not `Clone`: every copy of a secret should be a visible decision.
    pub fn duplicate(&self) -> Secret {
        Secret(self.0.clone())
    }
}

/// An armed credential and when it was armed, so an earlier arm's expiry cannot
/// disarm a later one.
type Armed = Arc<Mutex<Option<(Secret, Instant)>>>;

/// How long an armed credential is held. ssh either prompts within seconds or never
/// (the key is in an agent), and after this nothing on the pane can be assumed to be
/// this connection's login any more.
const ARM_WINDOW: Duration = Duration::from_secs(90);

/// ssh's exit status for every connection-level failure (refused, reset, timed out,
/// host key changed), as distinct from whatever the remote shell exited with.
const SSH_CONNECTION_FAILED: i32 = 255;

/// A connection that lasted at least this long was established; one that died sooner
/// was refused, and re-running it at once would only fail again.
const RETRY_MIN_CONNECTION: Duration = Duration::from_secs(4);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(<redacted>, {} bytes)", self.0.len())
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Whether `text` contains one of ssh's credential prompts.
///
/// The markers are the literal prompt forms both installed OpenSSH builds emit, captured
/// by running them: the MSYS build names the key (`Enter passphrase for key '...'`), the
/// native Windows build does not (`Enter passphrase: `), password auth is
/// `<user>@<host>'s password:`, and keyboard-interactive is `(<user>@<host>) Password:`.
///
/// The password forms deliberately require their punctuation. Matching a bare "password"
/// would fire on `cat /etc/passwd` or any output mentioning the word, and the consequence
/// of a false match is typing a secret somewhere it does not belong.
fn is_credential_prompt(text: &str) -> bool {
    const PROMPTS: &[&str] = &["Enter passphrase", "'s password:", ") Password:"];
    PROMPTS.iter().any(|p| text.contains(p))
}

/// Whether `text` carries ssh's final refusal. After a credential Arbiter typed, this
/// means it was wrong (or the key was not accepted), and only the user can fix that.
fn permission_denied(text: &str) -> bool {
    text.contains("Permission denied")
}

/// Decode the body of an OSC-133 report: the idle state its letter implies (`A`/`D` =
/// at a prompt, `B`/`C` = running a command) and the exit code a `D;<code>` carries.
fn osc133(rest: &str) -> (Option<bool>, Option<i32>) {
    let mut parts = rest.splitn(2, ';');
    let idle = match parts.next().and_then(|s| s.chars().next()) {
        Some('A') | Some('D') => Some(true),
        Some('B') | Some('C') => Some(false),
        _ => None,
    };
    let code = parts.next().and_then(|c| c.trim().parse::<i32>().ok());
    (idle, code)
}

/// One OSC-7 report: the `file://` authority (empty for Arbiter's own emitters) and the
/// percent-decoded path, before any platform fixup.
struct Osc7 {
    host: String,
    path: String,
}

fn parse_osc7(payload: &str) -> Option<Osc7> {
    let uri = payload.strip_prefix("7;")?.strip_prefix("file://")?;
    let slash = uri.find('/')?;
    Some(Osc7 { host: uri[..slash].to_string(), path: url_decode(&uri[slash..]) })
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Osc7Origin {
    Local,
    Remote,
}

/// Which machine an OSC-7 describes. Arbiter's own emitters leave the authority empty,
/// so an empty host is local by construction; a named host is remote unless it is this
/// machine (a third-party local emitter filling in `$HOST`). Only when the local name is
/// unknown does the ssh flag decide. Timing alone would get it wrong: when ssh exits,
/// the local prompt's OSC-7 arrives while that flag is still on.
fn osc7_origin(host: &str, local_host: Option<&str>, ssh_active: bool) -> Osc7Origin {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return Osc7Origin::Local;
    }
    match local_host {
        Some(local) if same_host(host, local) => Osc7Origin::Local,
        Some(_) => Osc7Origin::Remote,
        None if ssh_active => Osc7Origin::Remote,
        None => Osc7Origin::Local,
    }
}

/// First DNS label, case-insensitive: `Mini.local` and `mini` are the same box.
fn same_host(a: &str, b: &str) -> bool {
    let label = |h: &str| h.split('.').next().unwrap_or(h).to_ascii_lowercase();
    label(a) == label(b)
}

/// This machine's hostname, looked up once. `None` if the OS cannot say.
fn local_host_name() -> Option<&'static str> {
    static HOST: OnceLock<Option<String>> = OnceLock::new();
    HOST.get_or_init(sysinfo::System::host_name).as_deref()
}

/// The local form of a path the LOCAL shell reported. Windows shells report
/// `/C:/Users/x`, which becomes `C:\Users\x`. A remote POSIX path must never come
/// through here: stripping its leading slash is exactly what mangled it before.
fn local_path(decoded: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        let trimmed = decoded.strip_prefix('/').unwrap_or(decoded);
        if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
            return trimmed.replace('/', "\\");
        }
        trimmed.to_string()
    }
    #[cfg(not(target_os = "windows"))]
    {
        decoded.to_string()
    }
}

/// What a restored or reconnected remote pane still has to do once its far shell is at
/// a prompt: go back to the saved directory, then optionally bring Claude back there.
///
/// Typed only once the far shell is known to be reading input: on its OSC-7 report (the
/// opt-in snippet) or, failing that, when its prompt is recognised on screen. Nothing is
/// typed before either, because until then ssh itself may be the one reading, at a
/// credential prompt.
struct Followup {
    dir: String,
    resume_claude: bool,
    /// The conversation to resume, when Arbiter knows the far Claude's id.
    session: Option<String>,
    /// The connection command itself carries the `cd` (see `remote::remote_launch_line`),
    /// so the far shell starts in `dir` and only Claude is left to type.
    cd_in_command: bool,
}

/// What to type at the far prompt for a follow-up, and the directory it changes into,
/// if it does. `reported` is the directory the far shell itself announced, if it did:
/// already being there, by report, by the command line, or because the directory is
/// home, saves the `cd`. Claude comes back as `remote::claude_command` says.
fn followup_line(f: Followup, reported: Option<&str>) -> Option<(Vec<u8>, Option<String>)> {
    let there = f.cd_in_command
        || reported == Some(f.dir.as_str())
        || (f.dir == "~" && reported.is_none());
    let claude = format!("({})", crate::remote::claude_command(f.session.as_deref()));
    let line = match (there, f.resume_claude) {
        (true, false) => return None,
        (true, true) => format!("{claude}\r"),
        (false, false) => format!("{}\r", cd_cmd(&f.dir)),
        (false, true) => format!("{} && {claude}\r", cd_cmd(&f.dir)),
    };
    Some((line.into_bytes(), (!there).then_some(f.dir)))
}

/// `cd` into `dir` for the remote shell. Single quotes make every character literal (a
/// quote itself is spliced in as `'\''`); a leading `~` stays outside them so the far
/// shell expands it.
fn cd_cmd(dir: &str) -> String {
    let (tilde, rest) = match dir {
        "~" => ("~", ""),
        d => d.strip_prefix("~/").map_or(("", d), |r| ("~/", r)),
    };
    if rest.is_empty() {
        return format!("cd {tilde}");
    }
    format!("cd {tilde}'{}'", rest.replace('\'', "'\\''"))
}

/// What a command typed at the far prompt means for the pane's Claude conversation.
#[derive(Debug, PartialEq, Eq)]
enum ClaudeLaunch {
    NotClaude,
    /// A plain interactive launch naming no conversation: Arbiter may name one.
    Bare,
    /// Launched into a named conversation (`--resume <id>` or `--session-id <id>`).
    Named(String),
    /// Something Arbiter cannot follow: `-c`, a subcommand, a prompt, `--print`, the
    /// resume picker.
    Other,
}

/// Claude's subcommands, which take no `--session-id`. A first bare word that is one of
/// these is a subcommand; any other bare word is an initial prompt, which an interactive
/// launch happily takes alongside a session id.
const CLAUDE_SUBCOMMANDS: &[&str] = &[
    "mcp", "update", "doctor", "install", "auth", "config", "plugin", "agents", "setup-token",
    "migrate-installer",
];

fn classify_claude_launch(cmd: &str) -> ClaudeLaunch {
    let mut words = cmd.split_whitespace();
    let Some(first) = words.next() else { return ClaudeLaunch::NotClaude };
    if first.rsplit('/').next().unwrap_or(first) != "claude" {
        return ClaudeLaunch::NotClaude;
    }
    let mut named = None;
    let mut after_flag = false;
    while let Some(word) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((f, v)) => (f, Some(v)),
            None => (word, None),
        };
        match flag {
            "--resume" | "-r" | "--session-id" => {
                let value = inline.or_else(|| words.next());
                match value.filter(|v| crate::remote::plausible_session_id(v)) {
                    Some(id) => named = Some(id.to_string()),
                    None => return ClaudeLaunch::Other,
                }
                after_flag = false;
            }
            "-c" | "--continue" | "-p" | "--print" | "-h" | "--help" | "-v" | "--version" => {
                return ClaudeLaunch::Other;
            }
            f if f.starts_with('-') => after_flag = inline.is_none(),
            // A bare word: the preceding flag's value, a subcommand, or an initial prompt.
            w if !after_flag && CLAUDE_SUBCOMMANDS.contains(&w) => return ClaudeLaunch::Other,
            _ => after_flag = false,
        }
    }
    match named {
        Some(id) => ClaudeLaunch::Named(id),
        None => ClaudeLaunch::Bare,
    }
}

/// A fresh conversation id for a far Claude, in the UUID form `claude --session-id`
/// accepts. Randomness comes from the standard library's per-process hash seeds mixed
/// with the clock, plenty for uniqueness: this is an identifier, not a secret.
fn new_session_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(nanos);
        h.write_usize(i);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut out = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Claude's own words for a `--resume` that named a conversation it no longer has.
fn conversation_gone(text: &str) -> bool {
    text.contains("No conversation found with session ID")
}

/// Characters a shell prompt ends with: sh/bash, zsh, root, PowerShell and fish, and the
/// arrows the popular prompt themes use.
const PROMPT_ENDINGS: &[char] = &['$', '%', '#', '>', '\u{276f}', '\u{bb}', '\u{203a}'];

/// Whether a screen row reads as a shell prompt waiting for input: its last visible
/// character is one a prompt ends with. Used only for a REMOTE shell without
/// integration, once ssh is known to be running and its credential prompt is behind it,
/// so a wrong guess costs a line typed a moment early, which the far tty buffers until
/// its shell reads it.
fn looks_like_prompt(row: &str) -> bool {
    row.trim_end().chars().next_back().is_some_and(|c| PROMPT_ENDINGS.contains(&c))
}

/// The prompt ending and the command on a screen row: what follows the last prompt
/// ending that has a space after it. `None` when the row shows no prompt at all. The
/// ending tells a shell (`$`, `%`, `#`) from Claude's own input box (`>`, `❯`).
fn command_on_row(row: &str) -> Option<(char, &str)> {
    let mut best = None;
    let mut chars = row.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if PROMPT_ENDINGS.contains(&c) && matches!(chars.peek(), Some((_, ' '))) {
            best = Some((c, row[i + c.len_utf8() + 1..].trim()));
        }
    }
    best
}

/// The `cd` a REMOTE command line performs, read from the screen row as the shell showed
/// it when Enter was pressed (so completed and recalled text counts), against the
/// directory the shell was in. `None`: not a `cd`. `Some(None)`: a `cd` whose target
/// the text cannot tell (a variable, a glob, `cd -`). `Some(Some(dir))`: the new
/// directory, `~`-relative or absolute.
fn remote_cd(row: &str, base: &str) -> Option<Option<String>> {
    let (_, cmd) = command_on_row(row)?;
    let rest = cmd.strip_prefix("cd")?;
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    // Only the cd itself: `cd x && make` changes into x all the same.
    let arg = rest.split([';', '&', '|']).next().unwrap_or("").trim();
    if arg.is_empty() {
        return Some(Some("~".to_string()));
    }
    let unquoted = arg
        .strip_prefix('\'')
        .and_then(|a| a.strip_suffix('\''))
        .or_else(|| arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')));
    let arg = unquoted.unwrap_or(arg);
    let opaque = arg.is_empty()
        || arg == "-"
        || arg.chars().any(|c| matches!(c, '$' | '`' | '*' | '?' | '[' | '{' | '\\' | '"' | '\'' | '(' | ')' | '<' | '>'))
        || (arg.starts_with('~') && arg != "~" && !arg.starts_with("~/"));
    if opaque {
        return Some(None);
    }
    let joined = if arg.starts_with('/') || arg.starts_with('~') {
        arg.to_string()
    } else {
        format!("{base}/{arg}")
    };
    Some(normalize_remote_path(&joined))
}

/// Resolve `.` and `..` in a `~`-relative or absolute POSIX path. `None` if `..` would
/// climb above `~`, whose parent cannot be known from here.
fn normalize_remote_path(path: &str) -> Option<String> {
    let (root, rest) = match path.strip_prefix('~') {
        Some(r) => ("~", r),
        None => ("", path),
    };
    let mut out: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.pop().is_none() && root == "~" {
                    return None;
                }
            }
            s => out.push(s),
        }
    }
    if out.is_empty() {
        return Some(if root.is_empty() { "/".to_string() } else { root.to_string() });
    }
    let mut s = String::from(root);
    for seg in out {
        s.push('/');
        s.push_str(seg);
    }
    Some(s)
}

/// The far shell reported a failed `cd`, in bash's or zsh's words, so the directory
/// inferred from that `cd` is wrong.
fn cd_failed(text: &str) -> bool {
    text.contains("cd: ")
        && ["o such file or directory", "ot a directory", "ermission denied"]
            .iter()
            .any(|m| text.contains(m))
}

/// ssh never reached the host, in its own words. An exit after one of these is a
/// refusal, not a drop, however long the attempt took to time out.
fn connect_failed(text: &str) -> bool {
    [
        "ssh: connect to host",
        "ssh: Could not resolve hostname",
        "kex_exchange_identification:",
        "Connection closed by ",
    ]
    .iter()
    .any(|m| text.contains(m))
}

/// The far shell is at a prompt, so the login is complete. Settles what the connection
/// taught (see `ClaudeHandle::note_login_evidence`), releases a credential still held
/// for it, records where the shell is (its home, unless it said otherwise), and types
/// the pane's follow-up, if one is pending.
fn remote_ready(
    claude: &crate::claude_status::ClaudeHandle,
    credential: &Armed,
    followup: &Mutex<Option<Followup>>,
    writer_tx: &std::sync::mpsc::Sender<Vec<u8>>,
    reported: Option<&str>,
) {
    let pending = followup.lock().unwrap().take();
    if claude.note_login_evidence() {
        *credential.lock().unwrap() = None;
        if reported.is_none() && !claude.remote_cwd_reported() {
            // Where the far shell is: where its command line put it, else home.
            let now = pending.as_ref().filter(|f| f.cd_in_command).map_or("~", |f| f.dir.as_str());
            claude.set_remote_cwd(Some(now.to_string()));
        }
    }
    // Claude already has the keyboard (its own screen is up), so nothing may be typed:
    // it would land in the conversation. Whatever was pending is simply dropped.
    if claude.on_screen() {
        return;
    }
    if let Some((line, dir)) = pending.and_then(|f| followup_line(f, reported)) {
        let _ = writer_tx.send(line);
        if dir.is_some() {
            claude.set_remote_cwd(dir);
        }
    }
}

/// UI redraw hook: a PTY reader calls this after feeding new output so the UI can
/// redraw *on output* instead of polling the grid every frame. The iced shell wires
/// it to a redraw message at startup; it's a no-op until then (early output is covered
/// by the startup fast tick). Keeps this lib iced-agnostic.
static UI_WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Register the UI redraw hook (called once by the shell).
pub fn set_ui_waker(f: Box<dyn Fn() + Send + Sync>) {
    let _ = UI_WAKER.set(f);
}

/// Wake the UI to redraw, if a waker is registered.
fn wake_ui() {
    if let Some(f) = UI_WAKER.get() {
        f();
    }
}

pub type SharedTerm = Arc<Mutex<VtTerm>>;
pub type SharedMaster = Arc<Mutex<Box<dyn MasterPty + Send>>>;

/// portable-pty returns its own error type; map any Display error to io::Error.
fn io_err<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

pub struct Session {
    /// Unique, stable id — used to key this session's per-pane GPU renderer.
    id: u64,
    // All PTY input is funnelled through a dedicated writer thread via this
    // channel: the UI thread's keystrokes AND the reader thread's query replies
    // (cursor-position / device-attribute responses). Sending never blocks, so a
    // slow/blocking PTY write can't wedge the reader (which must keep draining
    // output) or freeze the UI.
    writer_tx: std::sync::mpsc::Sender<Vec<u8>>,
    master: SharedMaster,
    term: SharedTerm,
    cwd: Arc<Mutex<Option<String>>>,
    shell_idle: Arc<Mutex<Option<bool>>>,
    claude_running: Arc<AtomicBool>,
    git: Arc<Mutex<Option<crate::git::GitInfo>>>,
    claude: Arc<crate::claude_status::ClaudeHandle>,
    /// The shell exited (the PTY reader hit EOF). Nothing can be written to this
    /// session any more; the pane shows it as disconnected and offers Reconnect,
    /// which respawns rather than trying to revive it.
    exited: Arc<AtomicBool>,
    /// Holds a queued startup command until this pane can safely be typed into.
    startup: Arc<StartupGate>,
    /// A credential to type once at this pane's next ssh prompt, if the user supplied
    /// one for its restored connection. Memory only, taken on first use.
    credential: Armed,
    /// What to type once the far shell is at a prompt (see `Followup`).
    followup: Arc<Mutex<Option<Followup>>>,
    /// The input line being typed. Fed only from real keystrokes (see `note_typed`),
    /// so PTY query replies and program output can never pollute it. The submitted
    /// commands it yields live on the `ClaudeHandle`, which the busy-edge monitor also
    /// holds and so can latch one the instant it sees an ssh client.
    typed_line: Arc<Mutex<TypedLine>>,
    _watcher: Arc<Mutex<Option<GitWatcher>>>,
    _child: Box<dyn Child + Send + Sync>,
}

impl Session {
    pub fn spawn(cols: u16, rows: u16, mut cmd: CommandBuilder) -> std::io::Result<Self> {
        // Unique pane id, tagged onto the shell env so the statusLine/hook
        // subcommand (claude → our shim) keys its capture to THIS pane — robust
        // when many Claudes launch at once or several share a cwd.
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        cmd.env(crate::claude_shim::PANE_ID_ENV, id.to_string());
        // Read before `cmd` is consumed by the spawn. The pane's private history file
        // (set on the command by the shell layer) is a second source for its startup
        // command when keystroke tracking gives up on an up-arrow recall.
        let histfile = cmd.get_env("ARBITER_HISTFILE").map(std::path::PathBuf::from);
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(io_err)?;
        let child = pair.slave.spawn_command(cmd).map_err(io_err)?;
        let shell_pid = child.process_id();
        drop(pair.slave); // child keeps its own handle
        let raw_writer = pair.master.take_writer().map_err(io_err)?;
        let reader = pair.master.try_clone_reader().map_err(io_err)?;
        // Dedicated writer thread: serializes all PTY input (keystrokes + query
        // replies) off the reader/UI threads. A blocking write here can't deadlock
        // the reader, which must keep draining output for the slave to make progress.
        let (writer_tx, writer_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut w = raw_writer;
            while let Ok(bytes) = writer_rx.recv() {
                if w.write_all(&bytes).is_err() {
                    break;
                }
                let _ = w.flush();
            }
        });

        let term: SharedTerm = Arc::new(Mutex::new(VtTerm::new(cols as usize, rows as usize)));
        let cwd = Arc::new(Mutex::new(None));
        let shell_idle = Arc::new(Mutex::new(None));
        let claude_running = Arc::new(AtomicBool::new(false));
        let git = Arc::new(Mutex::new(None));
        let watcher: Arc<Mutex<Option<GitWatcher>>> = Arc::new(Mutex::new(None));
        let cmd_epoch: CmdEpoch = Arc::new((Mutex::new(0), Condvar::new()));
        let exited = Arc::new(AtomicBool::new(false));
        let startup = Arc::new(StartupGate::new(writer_tx.clone()));
        let credential: Armed = Arc::new(Mutex::new(None));
        // The exit code of the last command the LOCAL shell ran (`OSC 133;D;<code>`),
        // read only by the reader, which is what decides whether a drop earns a retry.
        let last_exit: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let followup: Arc<Mutex<Option<Followup>>> = Arc::new(Mutex::new(None));

        // Shared Claude status, updated by the capture/hook watcher (registered
        // here so it routes by cwd / session id) + the reader (spinner/menu →
        // activity/attention). Created before the reader so it gets a clone.
        let claude = crate::claude_status::ClaudeHandle::new(
            id,
            shell_pid,
            cwd.clone(),
            claude_running.clone(),
            histfile,
        );
        crate::claude_status::register(&claude);

        {
            let term = term.clone();
            let cwd = cwd.clone();
            let shell_idle = shell_idle.clone();
            let claude = claude.clone();
            let git = git.clone();
            let watcher = watcher.clone();
            let cmd_epoch = cmd_epoch.clone();
            let writer_tx = writer_tx.clone();
            let exited = exited.clone();
            let startup = startup.clone();
            let credential = credential.clone();
            let followup = followup.clone();
            std::thread::spawn(move || {
                reader_loop(
                    reader, writer_tx, term, cwd, shell_idle, claude, git, watcher, cmd_epoch,
                    exited, startup, credential, last_exit, followup,
                )
            });
        }

        // Event-driven Claude monitor: on each busy edge, scan the shell's descendants
        // for a `claude` process (it execs shortly after the edge), or for an ssh/mosh
        // client, which means the foreground program is on another machine.
        if let Some(pid) = shell_pid {
            let claude_running = claude_running.clone();
            let shell_idle = shell_idle.clone();
            let claude = claude.clone();
            std::thread::spawn(move || {
                claude_monitor(pid, cmd_epoch, claude_running, shell_idle, claude)
            });
        }

        Ok(Self {
            id,
            writer_tx,
            master: Arc::new(Mutex::new(pair.master)),
            term,
            cwd,
            shell_idle,
            claude_running,
            git,
            claude,
            exited,
            startup,
            credential,
            followup,
            typed_line: Arc::new(Mutex::new(TypedLine::default())),
            _watcher: watcher,
            _child: child,
        })
    }

    /// True once this pane's shell has exited. The screen keeps its last contents but
    /// nothing can be written to it any more, so the pane offers Reconnect (which
    /// respawns the session) rather than pretending to still be live.
    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    /// Record keystrokes on their way to the PTY, so the pane can remember the command
    /// that is currently running in it. Enter promotes the line, backspace edits it,
    /// and anything else non-printable clears it.
    ///
    /// Deliberately crude: this tracks a plain typed line and gives up on arrow keys,
    /// history recall, tab completion or a multi-line entry. Giving up means the pane
    /// remembers nothing and restores to a plain shell, which is the safe direction. It
    /// must never end up holding a line the user did not actually run.
    pub fn note_typed(&self, bytes: &[u8]) {
        let mut line = self.typed_line.lock().unwrap();
        if let Some(cmd) = line.fold(bytes) {
            self.claude.note_command(cmd);
        }
    }

    /// Seed the startup command on restore, so a replayed ssh line is still known to
    /// this session (nobody typed it) and survives the next save.
    pub fn set_startup_cmd(&self, cmd: &str) -> bool {
        self.claude.set_startup_cmd(cmd)
    }

    /// Remember `cmd` as this pane's startup command AND schedule it to run once the
    /// shell reaches its first prompt. Returns whether it was accepted (see
    /// `ClaudeHandle::set_startup_cmd` for the whitelist).
    ///
    /// The command is queued rather than written immediately, because writing into a
    /// PTY that nothing is reading yet leaves a stray newline behind on ConPTY, which
    /// ssh then swallows as an empty key passphrase.
    pub fn queue_startup_cmd(&self, cmd: &str) -> bool {
        if !self.set_startup_cmd(cmd) {
            return false;
        }
        // What is remembered is the line as saved. What is typed may carry the saved
        // remote directory, and Claude, in the connection itself, which then has nothing
        // to type after connecting; where that is not possible the follow-up types them.
        let mut line = cmd.to_string();
        if let Some(f) = self.followup.lock().unwrap().as_mut() {
            let chained = crate::remote::remote_launch_line(
                cmd,
                Some(&f.dir),
                f.resume_claude,
                f.session.as_deref(),
            );
            if let Some(chained) = chained {
                line = chained;
                f.cd_in_command = true;
                f.resume_claude = false;
            }
        }
        self.startup.queue(&line);

        // Last resort for a pane that never reports itself ready, which in practice means
        // shell integration is broken (no OSC-133 at all). One sleep, not a poll: the
        // release is event-driven, and by this point either it has already happened and
        // this is a no-op, or nothing was ever going to trigger it.
        let gate = self.startup.clone();
        std::thread::spawn(move || {
            std::thread::sleep(STARTUP_CMD_FALLBACK);
            gate.send();
        });
        true
    }

    /// This pane's startup gate, for the renderer to notify on resize (it owns the cell
    /// metrics, so it is where the real size is applied).
    pub fn startup_gate(&self) -> Arc<StartupGate> {
        self.startup.clone()
    }

    /// Arm this pane to answer ONE ssh credential prompt with `secret`.
    ///
    /// Only meaningful alongside a queued startup command: the pane is armed just before
    /// its connection is replayed, and disarms the moment it answers a prompt or the
    /// command ends, so the window in which anything could be typed is the connection
    /// itself and nothing more. An empty secret arms nothing, which is how "I will type
    /// this one myself" is expressed.
    pub fn arm_credential(&self, secret: Secret) {
        if secret.is_empty() {
            return;
        }
        let armed_at = Instant::now();
        *self.credential.lock().unwrap() = Some((secret, armed_at));
        // Expiry. One sleep, not a poll: by the time it fires the credential has almost
        // always been consumed or dropped already, and this is a no-op. A later arm is
        // left alone, which is what the timestamp is for.
        let credential = self.credential.clone();
        std::thread::spawn(move || {
            std::thread::sleep(ARM_WINDOW);
            let mut slot = credential.lock().unwrap();
            if slot.as_ref().is_some_and(|(_, at)| *at == armed_at) {
                *slot = None;
            }
        });
    }

    /// Type `secret` into this pane's connection right now, for a re-asked credential
    /// whose prompt is already on screen. Refused unless the local shell is running a
    /// remote client: had ssh given up in the meantime, the bytes would land on the
    /// local prompt, echoed and written to history.
    pub fn send_credential_now(&self, secret: Secret) -> bool {
        if secret.is_empty() || self.shell_idle() != Some(false) || !self.is_remote() {
            return false;
        }
        let _ = self.writer_tx.send(secret.line());
        self.claude.note_credential_typed();
        true
    }

    /// Queue what to type once the far shell is at its prompt after the next connection:
    /// `cd` into `dir`, then `claude -c` if `resume_claude`. Set right before
    /// `queue_startup_cmd`. Nothing happens unless the far shell reports its directory.
    pub fn set_followup(&self, dir: &str, resume_claude: bool, session: Option<&str>) {
        *self.followup.lock().unwrap() = Some(Followup {
            dir: dir.to_string(),
            resume_claude,
            session: session.map(str::to_string),
            cd_in_command: false,
        });
    }

    /// Enter was pressed with `row` on screen in this pane. Returns what to type BEFORE
    /// the Enter, if anything: with `name_sessions` (a setting, off by default), a bare
    /// `claude` at the far shell's prompt gets ` --session-id <uuid>`, so the conversation
    /// is known from its first moment and a restore can resume exactly it (the local shim
    /// learns the id from Claude's status line; nothing on the far host can tell Arbiter,
    /// so Arbiter names it). Only at a prompt ending a shell uses (`$`, `%`, `#`), which
    /// Claude's own `>` box never shows.
    ///
    /// The row also keeps the pane's remote facts current: `claude` typed there is Claude
    /// running (with the id it was launched into, when the line names one), any other
    /// command at a shell prompt means it is not, and a `cd` moves the inferred directory
    /// (see `remote_cd`) unless the far shell reports it itself. Ignored when Claude has
    /// the keyboard.
    pub fn on_remote_enter(&self, row: &str, name_sessions: bool) -> Option<Vec<u8>> {
        if !self.is_remote() || self.claude_running() {
            return None;
        }
        let (ending, cmd) = command_on_row(row)?;
        let shell_prompt = matches!(ending, '$' | '%' | '#');
        match classify_claude_launch(cmd) {
            ClaudeLaunch::Bare => {
                self.claude.set_remote_claude_typed(true);
                if !name_sessions || !shell_prompt {
                    self.claude.set_remote_session(None);
                    return None;
                }
                let id = new_session_id();
                self.claude.set_remote_session(Some(id.clone()));
                Some(format!(" --session-id {id}").into_bytes())
            }
            ClaudeLaunch::Named(id) => {
                self.claude.set_remote_claude_typed(true);
                self.claude.set_remote_session(Some(id));
                None
            }
            ClaudeLaunch::Other => {
                self.claude.set_remote_claude_typed(true);
                self.claude.set_remote_session(None);
                None
            }
            ClaudeLaunch::NotClaude => {
                if shell_prompt {
                    self.claude.set_remote_claude_typed(false);
                    self.claude.set_remote_session(None);
                }
                if !self.claude.remote_cwd_reported() {
                    let base = self.remote_cwd().unwrap_or_else(|| "~".to_string());
                    if let Some(target) = remote_cd(row, &base) {
                        self.claude.set_remote_cwd(target);
                    }
                }
                None
            }
        }
    }

    /// The far Claude's session id, if Arbiter knows it.
    pub fn remote_session(&self) -> Option<String> {
        self.claude.remote_session()
    }

    /// Seed from a saved layout.
    pub fn seed_remote_session(&self, id: Option<&str>) {
        self.claude.seed_remote_session(id);
    }

    /// Whether the header should offer Reconnect: the shell has exited, or this was a
    /// remote pane whose connection is not up AND whose shell is not busy making one.
    /// The busy half keeps the button from flashing while a restored pane connects.
    pub fn show_reconnect(&self) -> bool {
        self.exited()
            || (self.claude.was_remote() && !self.is_remote() && self.shell_idle() != Some(false))
    }

    /// What is known about whether this pane's connection asks for a credential:
    /// `None` never observed, `Some(true)` it prompted, `Some(false)` it logged in
    /// without prompting.
    pub fn prompts_for_credential(&self) -> Option<bool> {
        self.claude.prompts_for_credential()
    }

    /// Seed from a saved layout.
    pub fn set_prompts_for_credential(&self, prompts: Option<bool>) {
        self.claude.set_prompts_for_credential(prompts);
    }

    /// The far host's working directory, if its shell has ever reported one.
    pub fn remote_cwd(&self) -> Option<String> {
        self.claude.remote_cwd()
    }

    /// Seed from a saved layout.
    pub fn seed_remote_cwd(&self, path: Option<&str>) {
        self.claude.seed_remote_cwd(path);
    }

    /// Whether the far shell has reported its directory on the current connection.
    pub fn remote_cwd_reported(&self) -> bool {
        self.claude.remote_cwd_reported()
    }

    /// Claude is, or when the connection ended was, running on the far side.
    pub fn remote_claude(&self) -> bool {
        self.claude.remote_claude()
    }

    /// Take the "resume Claude on the next connection" flag.
    pub fn take_remote_claude_pending(&self) -> bool {
        self.claude.take_remote_claude_pending()
    }

    /// Seed from a saved layout.
    pub fn seed_remote_claude(&self, pending: bool) {
        self.claude.seed_remote_claude(pending);
    }

    /// UI: this pane's connection dropped and should be re-run once (taken).
    pub fn take_retry_wanted(&self) -> bool {
        self.claude.take_retry_wanted()
    }

    /// UI: the credential typed into this pane was rejected; ask again (taken).
    pub fn take_reask_wanted(&self) -> bool {
        self.claude.take_reask_wanted()
    }

    /// The command to replay to rebuild this pane, if there is one.
    ///
    /// Only a recognised remote-client invocation is ever accepted, and only at the
    /// moment an ssh client is detected (see `ClaudeHandle::latch_startup_cmd`). A local
    /// pane is fully described by its shell and cwd, which the saved layout already
    /// carries, and replaying its last command could re-run something with side effects
    /// on every launch.
    pub fn startup_cmd(&self) -> Option<String> {
        self.claude.startup_cmd()
    }

    /// Whether this pane should be offering to reconnect: its shell has exited, or it
    /// was a remote pane whose session has ended (ssh is gone but the command that
    /// built it is still known). The `was_remote` half is what keeps a freshly restored
    /// pane from flashing the affordance before its ssh has started.
    pub fn needs_reconnect(&self) -> bool {
        self.exited() || (self.claude.was_remote() && !self.is_remote())
    }

    /// Current Claude status for this pane (stats + derived lifecycle). Cheap;
    /// read it from the view.
    pub fn claude_status(&self) -> crate::claude_status::ClaudeStatus {
        self.claude.snapshot()
    }

    /// True if Claude is running in this pane right now, whether that is a local
    /// `claude` process or one on the far side of an ssh session (recognised from its
    /// on-screen chrome). Every Claude-gated affordance reads this, so the remote case
    /// lights the status dot, lists in the overview and picks the right Shift+Enter
    /// encoding without any of them knowing the difference.
    pub fn claude_running(&self) -> bool {
        self.claude_running.load(Ordering::Relaxed) || self.claude.on_screen()
    }

    /// True if this pane is driving a shell on another machine (an ssh/mosh client is
    /// its foreground program).
    pub fn is_remote(&self) -> bool {
        self.claude.is_remote()
    }

    /// Whether a LOCAL `claude` process is running here. This is the flag the saved
    /// layout must use: restore relaunches Claude by typing `claude` into a freshly
    /// spawned LOCAL shell, and a remote pane's Claude lives on another machine, so
    /// persisting the broader `claude_running()` would have a restored ssh pane start
    /// a local Claude instead.
    pub fn claude_running_local(&self) -> bool {
        self.claude_running.load(Ordering::Relaxed)
    }

    /// Ignore Claude spinner detection for `dur_ms` — called for a repaint that doesn't
    /// start work (window/PTY resize, or a newline/mode edit key on Windows ConPTY) so the
    /// rapid redraws don't read as "working".
    pub fn suppress_claude_activity(&self, dur_ms: u64) {
        self.claude.suppress_activity(dur_ms);
    }

    /// Resume Claude spinner detection immediately — called on Enter/submit, where real
    /// working is imminent and must not be delayed by a suppression window.
    pub fn clear_claude_suppression(&self) {
        self.claude.clear_suppression();
    }

    /// The Claude session id to resume on restore IF Claude is running here AND a
    /// real conversation has happened (else `None`, so restore launches a clean
    /// `claude` rather than `--resume`ing a non-existent empty session).
    pub fn claude_session_id(&self) -> Option<String> {
        self.claude_running_local().then(|| self.claude.resumable_session()).flatten()
    }

    /// Basename of the current working directory, if known.
    pub fn folder(&self) -> Option<String> {
        self.cwd().map(|p| {
            p.trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&p)
                .to_string()
        })
    }

    /// Cached git info for the cwd (branch + status counts), refreshed on cd.
    pub fn git(&self) -> Option<crate::git::GitInfo> {
        self.git.lock().unwrap().clone()
    }

    /// Stable unique id for keying per-session GPU state.
    pub fn id(&self) -> u64 { self.id }

    /// Shared grid handle for the renderer.
    pub fn term(&self) -> SharedTerm { self.term.clone() }
    /// Shared master handle (for resizing the PTY from the render path).
    pub fn master(&self) -> SharedMaster { self.master.clone() }
    /// Latest cwd from OSC-7, if the shell reported one.
    pub fn cwd(&self) -> Option<String> { self.cwd.lock().unwrap().clone() }
    /// Latest OSC-133 idle state (Some(true)=at prompt, Some(false)=running).
    pub fn shell_idle(&self) -> Option<bool> { *self.shell_idle.lock().unwrap() }

    /// Whether a command is running in this pane, for the green "running" dot.
    ///
    /// Always false for a remote pane. The shell integration that reports this is the
    /// LOCAL shell's, and from its point of view the single command `ssh` runs from
    /// connect to disconnect, so the dot would be stuck on for the whole session while
    /// saying nothing about what the far host is doing. A remote pane's useful state
    /// comes from Claude's lifecycle instead, which is read from the screen.
    pub fn shell_busy(&self) -> bool {
        self.shell_idle() == Some(false) && !self.is_remote()
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer_tx.send(bytes.to_vec());
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Ok(m) = self.master.lock() {
            let _ = m.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
        self.startup.note_resized();
        self.term.lock().unwrap().resize(cols as usize, rows as usize);
    }
}

const MAX_UTF8_REMAINDER: usize = 8;

/// The input line being typed into a pane, tracked well enough to recognise a plain
/// command and to know when it can no longer be trusted.
///
/// Printable ASCII accumulates, backspace deletes, Enter submits. Any control byte
/// POISONS the line: it submits nothing until the next Enter starts a fresh one. That
/// is what makes this safe rather than clever. An arrow key, history recall, tab
/// completion or Ctrl+C means the line on screen is no longer the line seen here, so
/// the pane must remember nothing rather than something the user never ran. Poisoning
/// (not merely clearing) matters because the tail of an escape sequence is itself
/// printable: without it, an up-arrow leaves `[A` behind and submits *that*.
#[derive(Default)]
struct TypedLine {
    line: String,
    poisoned: bool,
}

impl TypedLine {
    /// Fold typed bytes in, returning the last command submitted with Enter, if any.
    /// Pure state machine, so it is tested without a PTY.
    fn fold(&mut self, bytes: &[u8]) -> Option<String> {
        let mut submitted = None;
        for &b in bytes {
            match b {
                b'\r' | b'\n' => {
                    let cmd = std::mem::take(&mut self.line).trim().to_string();
                    let trusted = !std::mem::take(&mut self.poisoned);
                    if trusted && !cmd.is_empty() {
                        submitted = Some(cmd);
                    }
                }
                0x08 | 0x7f => {
                    self.line.pop();
                }
                0x20..=0x7e if !self.poisoned => self.line.push(b as char),
                0x20..=0x7e => {}
                _ => {
                    self.line.clear();
                    self.poisoned = true;
                }
            }
        }
        submitted
    }
}

/// Bit index for one spinner glyph, or `None` if the char isn't one. The animated
/// bloom frames are the star/asterisk dingbats U+2722..U+273F (verified by capturing
/// the CLI); tool spinners and Claude's window-title spinner use Braille
/// U+2800..U+28FF. Deliberately NOT the whole U+2700..U+27BF range: that includes the
/// input prompt arrow (U+276F), which would make typing read as working.
///
/// Each star gets its own bit; the 256 Braille frames fold into the upper 34, so two
/// of those can collide. Harmless: a collision costs one animation frame before the
/// next differing pair confirms working.
fn spinner_bit(c: char) -> Option<u32> {
    match c as u32 {
        u @ 0x2722..=0x273F => Some(u - 0x2722),
        u @ 0x2800..=0x28FF => Some(30 + (u - 0x2800) % 34),
        _ => None,
    }
}

/// Fingerprint of the DISTINCT spinner glyphs in a chunk, or `None` if it has none.
/// Two chunks fingerprint alike iff they drew the same *set* of spinner glyphs, which
/// is what separates a real animation frame from a plain repaint: the bloom draws a
/// different frame every time, while a repaint re-emits whatever static stars are on
/// screen. Chief among those are the thinking summaries Claude leaves in the
/// transcript ("* Brewed for 7s"), which use the very glyph the animation cycles
/// through.
fn chunk_spinner_key(bytes: &[u8]) -> Option<u64> {
    let text = unsafe { std::str::from_utf8_unchecked(bytes) };
    let key = text.chars().filter_map(spinner_bit).fold(0u64, |k, bit| k | 1 << bit);
    (key != 0).then_some(key)
}

fn reader_loop(
    mut reader: Box<dyn Read + Send>,
    writer_tx: std::sync::mpsc::Sender<Vec<u8>>,
    term: SharedTerm,
    cwd: Arc<Mutex<Option<String>>>,
    shell_idle: Arc<Mutex<Option<bool>>>,
    claude: Arc<crate::claude_status::ClaudeHandle>,
    git: Arc<Mutex<Option<crate::git::GitInfo>>>,
    watcher: Arc<Mutex<Option<GitWatcher>>>,
    cmd_epoch: CmdEpoch,
    exited: Arc<AtomicBool>,
    startup: Arc<StartupGate>,
    credential: Armed,
    last_exit: Arc<Mutex<Option<i32>>>,
    followup: Arc<Mutex<Option<Followup>>>,
) {
    let claude_running = claude.claude_running.clone();
    let mut buf = [0u8; 8192];
    let mut remainder: Vec<u8> = Vec::new();
    let mut osc = String::new();
    let mut in_osc = false;
    let mut prev_cwd: Option<String> = None;
    let mut prev_idle: Option<bool> = None;
    let mut prev_menu = false;

    loop {
        let n = match reader.read(&mut buf) {
            // EOF (or a dead PTY): the shell is gone. Publish that and wake the UI, so
            // a pane whose shell exited can render as such and offer to reconnect
            // instead of sitting on a frozen screen forever. Writes to this PTY are
            // silently dropped from here on, so recovery has to respawn the session.
            Ok(0) | Err(_) => {
                exited.store(true, Ordering::Relaxed);
                claude_running.store(false, Ordering::Relaxed);
                claude.set_remote(false);
                wake_ui();
                break;
            }
            Ok(n) => n,
        };
        // Stitch any partial UTF-8 from last read onto this chunk.
        let mut chunk = Vec::with_capacity(remainder.len() + n);
        chunk.extend_from_slice(&remainder);
        chunk.extend_from_slice(&buf[..n]);
        remainder.clear();
        let valid_up_to = match std::str::from_utf8(&chunk) {
            Ok(_) => chunk.len(),
            Err(e) => e.valid_up_to(),
        };
        if valid_up_to < chunk.len() {
            remainder = chunk[valid_up_to..].to_vec();
            if remainder.len() > MAX_UTF8_REMAINDER {
                remainder.clear();
            }
        }
        let valid = &chunk[..valid_up_to];
        if valid.is_empty() {
            continue;
        }

        // Feed the full byte stream to the grid (alacritty parses VT incl. OSC),
        // then write back any replies it produced — responses to queries the
        // running program sent (cursor position, device attributes, status). Apps
        // like vim or .NET/Spectre console UIs wait on these; dropping them made
        // their input handling misbehave.
        let responses = {
            let mut t = term.lock().unwrap();
            t.feed(valid);
            t.take_responses()
        };
        if !responses.is_empty() {
            // Hand off to the writer thread — never write from here, or a blocking
            // PTY write would stop us draining output and could deadlock the slave.
            let _ = writer_tx.send(responses);
        }
        // The grid changed — wake the UI to redraw (event-driven; the UI no longer
        // polls the grid every frame). Coalesced on the UI side, so a burst of output
        // is one redraw.
        wake_ui();

        // Tier-3b: while Claude runs here, reflect the live turn from the *rendered
        // screen* (level-triggered, so attention clears the instant a menu leaves —
        // e.g. the user escapes/answers). A menu/approval prompt → attention (no
        // hook covers AskUserQuestion/plan); Claude's "(esc to interrupt)" status
        // line → working. Plain output (typing, redraws) is neither.
        // A REMOTE pane (an ssh/mosh client in the foreground) can't be detected the
        // local way: the process scan sees only `ssh` and the far host writes no
        // statusLine capture. So probe the rendered screen for Claude's own chrome
        // instead. Deliberately gated on `remote`, so a local pane's detection path is
        // byte-for-byte the one that already works and can't regress here.
        if claude.is_remote() {
            let chrome = term.lock().unwrap().claude_chrome();
            claude.note_screen(chrome);
        } else if std::env::var_os("ARBITER_CLAUDE_DEBUG").is_some() {
            // Diagnostic (ARBITER_CLAUDE_DEBUG): run the probe on LOCAL panes too, where
            // the process scan is ground truth, and log both. It changes nothing, but it
            // is how the chrome markers get verified against a real Claude, and how a
            // future Claude UI change that breaks them gets caught: a pane with
            // local_claude=true and chrome=false at its prompt means the markers or the
            // cursor geometry in `VtTerm::claude_chrome` have drifted.
            let chrome = term.lock().unwrap().claude_chrome();
            crate::claude_shim::debug_log(&format!(
                "chrome probe: chrome={chrome} local_claude={}",
                claude_running.load(Ordering::Relaxed)
            ));
        }
        // The login is known to be over (see `ClaudeHandle::note_login_evidence`), so a
        // credential still held for it cannot be meant for anything on this pane.
        if claude.login_seen() {
            *credential.lock().unwrap() = None;
        }
        if claude_running.load(Ordering::Relaxed) || claude.on_screen() {
            // Attention: a menu/approval prompt on the rendered screen (level-based,
            // so amber clears the instant the prompt leaves). Working: the ✻ spinner
            // glyph in the *new* bytes (chunk-based like the web — instant, and a
            // stale star left on screen can't pin it to "working").
            let menu = term.lock().unwrap().visible_menu();
            claude.set_menu(menu);
            if prev_menu && !menu {
                // A menu just LEFT the screen (answered or escaped). AskUserQuestion
                // fires a permission/elicitation hook, but escaping it produces no
                // spinner/Stop to clear that sticky hook attention — so clear it on
                // this on→off edge (markerless prompts never set a menu, so they're
                // unaffected and still clear via activity/Stop).
                claude.clear_hook_attention();
            }
            prev_menu = menu;
            if !menu {
                if let Some(glyphs) = chunk_spinner_key(valid) {
                    claude.note_activity(glyphs);
                }
            }
        }

        // Separately scan for OSC-7 (cwd) + OSC-133 (busy/idle), which the grid
        // doesn't surface.
        let text = unsafe { std::str::from_utf8_unchecked(valid) };

        // ssh's credential prompt. An ARMED pane (a connection whose credential the user
        // supplied) answers it once, then disarms; a prompt after that answer, or a
        // denial, means the answer was wrong and the UI is asked to re-ask.
        //
        // The `shell_idle == Some(false)` guard is the load-bearing safety rule, not a
        // nicety: it means a command is running, so the shell is not reading a command
        // line. Were a prompt pattern ever to match while the shell sat at its prompt,
        // the secret would be echoed on screen AND written into this pane's private
        // command-history file. `shell_idle` is read from BEFORE this chunk's own OSC
        // scan below, which is the state the output was produced under.
        if *shell_idle.lock().unwrap() == Some(false) {
            // Once the login is known complete, a further prompt or denial belongs to
            // something else on the far host (a nested ssh, a `cd` into a forbidden
            // directory) and says nothing about this connection's credential.
            if is_credential_prompt(text) && claude.startup_cmd().is_some() {
                claude.note_credential_prompt();
                match credential.lock().unwrap().take() {
                    Some((secret, _)) => {
                        let _ = writer_tx.send(secret.line());
                        claude.note_credential_typed();
                    }
                    None if !claude.login_seen() => {
                        claude.note_credential_rejected();
                    }
                    None => {}
                }
            } else if !claude.login_seen() && permission_denied(text) {
                claude.note_credential_rejected();
            }
            if connect_failed(text) {
                claude.note_connect_failure();
            }
            if claude.is_remote() && !claude.remote_cwd_reported() && cd_failed(text) {
                claude.revert_remote_cwd();
            }
            // The conversation Arbiter would resume no longer exists on the far host, so
            // stop naming it: the fallbacks (`-c`, then a fresh `claude`) take over.
            if claude.is_remote() && conversation_gone(text) {
                claude.set_remote_session(None);
            }
        }
        for ch in text.chars() {
            if in_osc {
                if ch == '\x07' || (osc.ends_with('\x1b') && ch == '\\') {
                    let payload = if osc.ends_with('\x1b') { &osc[..osc.len() - 1] } else { &osc };
                    if let Some(rest) = payload.strip_prefix("133;") {
                        let (idle, code) = osc133(rest);
                        if code.is_some() {
                            *last_exit.lock().unwrap() = code;
                        }
                        if let Some(idle) = idle {
                            *shell_idle.lock().unwrap() = Some(idle);
                            if prev_idle != Some(idle) {
                                prev_idle = Some(idle);
                                if idle {
                                    // The shell has reached a prompt, so release any queued
                                    // startup command now. It is deliberately NOT written at
                                    // spawn time: on ConPTY a CR written into a PTY that
                                    // nothing is reading yet can arrive as CRLF, and the stray
                                    // LF is then consumed by whatever runs next. ssh read it as
                                    // an empty key passphrase and fell straight through to
                                    // password auth before the user could type anything.
                                    // `take` makes this once-only, so later prompts are unaffected.
                                    // The PTY is being read. If the pane has also been
                                    // resized, this releases any queued startup command.
                                    startup.note_prompt();
                                    // The command that needed a credential has ended, so
                                    // stop holding one. Bounds the armed window to the
                                    // connection itself.
                                    *credential.lock().unwrap() = None;
                                    // Prompt returned → the foreground command
                                    // (incl. Claude, or an ssh session) ended.
                                    let was = claude_running.swap(false, Ordering::Relaxed);
                                    if was {
                                        // Claude stopped → persist so a restore doesn't relaunch it,
                                        // and remove its statusLine capture so a lingering file can't
                                        // re-mark this pane as running on the next watcher pass.
                                        crate::claude_status::SAVE_DIRTY.store(true, Ordering::Relaxed);
                                        claude.clear_capture();
                                    }
                                    // The LOCAL prompt is back, so any ssh session is over and
                                    // the remote Claude with it (this also drops the on-screen
                                    // latch). The chrome probe would clear it on the next chunk
                                    // anyway; doing it on the edge means the dot goes out the
                                    // moment the session ends rather than on the next output.
                                    if claude.is_remote() {
                                        let code = *last_exit.lock().unwrap();
                                        // ssh's own failure code after a session that was
                                        // up is a drop: not a refusal, not an `exit`. Worth
                                        // one automatic re-run, which the UI performs.
                                        if code == Some(SSH_CONNECTION_FAILED) {
                                            claude.request_retry(RETRY_MIN_CONNECTION);
                                        }
                                        // Ended before the far shell ever showed a prompt,
                                        // with the command's own status rather than ssh's:
                                        // the `cd` the connection carried failed, so that
                                        // directory is gone. Forget it, or every reconnect
                                        // would fail the same way.
                                        let chained = followup
                                            .lock()
                                            .unwrap()
                                            .as_ref()
                                            .is_some_and(|f| f.cd_in_command);
                                        if chained
                                            && !claude.login_seen()
                                            && code.is_some_and(|c| c != 0 && c != SSH_CONNECTION_FAILED)
                                        {
                                            claude.set_remote_cwd(None);
                                        }
                                        // A session that was up and ended with the far
                                        // shell's own status was ended by the user (`exit`),
                                        // not lost. The pane is a local shell again.
                                        let left = claude.login_seen()
                                            && code.is_some_and(|c| c != SSH_CONNECTION_FAILED);
                                        claude.set_remote(false);
                                        if left {
                                            claude.forget_connection();
                                        }
                                        crate::claude_status::SAVE_DIRTY.store(true, Ordering::Relaxed);
                                    }
                                    // Whatever was to be typed at the far prompt has no far
                                    // prompt to go to any more.
                                    *followup.lock().unwrap() = None;
                                    // A command just finished — it may have changed
                                    // files, so refresh the git status.
                                    recompute_git(cwd.clone(), git.clone());
                                } else {
                                    // A command started → wake the monitor to scan.
                                    let (lock, cvar) = &*cmd_epoch;
                                    *lock.lock().unwrap() += 1;
                                    cvar.notify_all();
                                }
                            }
                        }
                    }
                    if let Some(report) = parse_osc7(payload) {
                        match osc7_origin(&report.host, local_host_name(), claude.is_remote()) {
                            Osc7Origin::Remote => {
                                claude.note_remote_cwd(report.path.clone());
                                remote_ready(
                                    &claude,
                                    &credential,
                                    &followup,
                                    &writer_tx,
                                    Some(&report.path),
                                );
                            }
                            Osc7Origin::Local => {
                                let path = local_path(&report.path);
                                let changed = prev_cwd.as_ref() != Some(&path);
                                *cwd.lock().unwrap() = Some(path.clone());
                                if changed {
                                    prev_cwd = Some(path.clone());
                                    recompute_git(cwd.clone(), git.clone());
                                    // Re-point the FS watcher at the new repo so external
                                    // edits (made outside the terminal) refresh git too.
                                    repoint_watcher(&watcher, &cwd, &git, path);
                                }
                            }
                        }
                    }
                    osc.clear();
                    in_osc = false;
                } else {
                    osc.push(ch);
                    if osc.len() > 1024 {
                        osc.clear();
                        in_osc = false;
                    }
                }
            } else if ch == '\x1b' {
                osc.clear();
                osc.push(ch);
            } else if osc == "\x1b" && ch == ']' {
                osc.clear();
                in_osc = true;
            } else {
                osc.clear();
            }
        }

        // A far shell without integration announces nothing, so its first prompt is
        // recognised on screen instead (see `looks_like_prompt`). Only while the login
        // is unconfirmed, never while a credential is still to be typed, and for a
        // connection known to prompt not before that prompt has been seen: a pre-login
        // banner line must not pass for the prompt.
        if claude.is_remote()
            && !claude.login_seen()
            && credential.lock().unwrap().is_none()
            && (claude.prompts_for_credential() != Some(true) || claude.prompted_this_connection())
        {
            let row = term.lock().unwrap().cursor_row_text();
            if looks_like_prompt(&row) {
                remote_ready(&claude, &credential, &followup, &writer_tx, None);
            }
        }
    }
}

/// Recompute git info for the current cwd off-thread. Only applies the result
/// if the cwd hasn't changed since (so a `cd`'s stale pre-`cd` scan can't
/// clobber the new dir's info — the idle edge fires before the OSC-7 cwd update).
fn recompute_git(cwd: Arc<Mutex<Option<String>>>, git: Arc<Mutex<Option<crate::git::GitInfo>>>) {
    let path = match cwd.lock().unwrap().clone() {
        Some(p) => p,
        None => {
            *git.lock().unwrap() = None;
            return;
        }
    };
    std::thread::spawn(move || {
        let info = crate::git::repo_info(&path);
        if cwd.lock().unwrap().as_deref() == Some(path.as_str()) {
            let mut guard = git.lock().unwrap();
            if *guard != info {
                *guard = info;
                drop(guard);
                // Redraw. Without this a watcher-driven refresh (a git
                // command in a SIBLING pane on the same repo) would update the cached
                // info but never repaint until the next unrelated redraw.
                wake_ui();
            }
        }
    });
}

/// Whether a debounced FS change (path relative to the repo root) should refresh
/// the git status. Watched recursively, so we filter here:
///   - skip gitignored high-churn dirs (`target/`, `node_modules/`, …) git ignores;
///   - inside `.git/`, take only metadata the status reflects (HEAD, index, refs,
///     packed-refs, MERGE_HEAD, …) — skip the object store + logs/reflog that churn
///     on commits/fetches/gc, and transient `*.lock` files. The `.lock` skip is
///     scoped to `.git/` so a working-tree `Cargo.lock` / `yarn.lock` still counts.
/// Our status reads use `--no-optional-locks`, so observing `.git/` can't self-loop.
fn git_relevant_change(rel: &std::path::Path) -> bool {
    use std::path::Component;
    let names: Vec<&str> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    if names
        .iter()
        .any(|n| matches!(*n, "target" | "node_modules" | "dist" | ".next" | ".venv" | "__pycache__"))
    {
        return false;
    }
    if names.first() == Some(&".git") {
        return !matches!(names.get(1).copied(), Some("objects") | Some("logs"))
            && !names.last().map_or(false, |f| f.ends_with(".lock"));
    }
    true
}

/// Point the session's FS watcher at the repo containing `cwd_path`, replacing
/// any previous watcher. On any debounced filesystem change under the repo root
/// we recompute git — so edits made *outside* the terminal (a text editor, a
/// branch switch in another tool) refresh the status without polling. This is
/// what VS Code does (FSEvents / ReadDirectoryChangesW / inotify via `notify`).
/// Runs off the reader thread: resolving the repo root spawns `git`, which we
/// don't want to block terminal output on.
fn repoint_watcher(
    watcher: &Arc<Mutex<Option<GitWatcher>>>,
    cwd: &Arc<Mutex<Option<String>>>,
    git: &Arc<Mutex<Option<crate::git::GitInfo>>>,
    cwd_path: String,
) {
    let watcher = watcher.clone();
    let cwd = cwd.clone();
    let git = git.clone();
    std::thread::spawn(move || {
        let new = crate::git::repo_root(&cwd_path).and_then(|root| {
            let cwd = cwd.clone();
            let git = git.clone();
            let root_path = std::path::PathBuf::from(&root);
            let mut deb = new_debouncer(Duration::from_millis(400), move |res: DebounceEventResult| {
                let Ok(events) = res else { return };
                // Refresh only on changes the status reflects (see git_relevant_change):
                // meaningful `.git/` metadata + working-tree files, NOT object/log churn or
                // gitignored build dirs. This lets a git command (staging/commit/branch) in
                // one pane refresh SIBLING panes on the same repo — terminal git commands
                // also still refresh their own pane via the OSC-133 prompt edge.
                let relevant = events.iter().any(|e| {
                    let rel = e.path.strip_prefix(&root_path).unwrap_or(e.path.as_path());
                    git_relevant_change(rel)
                });
                if relevant {
                    recompute_git(cwd.clone(), git.clone());
                }
            })
            .ok()?;
            deb.watcher()
                .watch(std::path::Path::new(&root), RecursiveMode::Recursive)
                .ok()?;
            Some(deb)
        });
        // Replacing the slot drops the previous watcher, stopping the old watch.
        *watcher.lock().unwrap() = new;
    });
}

/// Per-session Claude monitor: blocks until a busy edge (a command started),
/// then scans the shell's descendants for `claude` — with a short retry since
/// `claude` execs a moment after the edge. Bails early if the shell returns to
/// idle (a quick command that wasn't Claude). The reader clears `claude_running`
/// on the idle edge. (Currently leaks one blocked thread per closed session —
/// cleanup when sessions get a shutdown signal.)
fn claude_monitor(
    shell_pid: u32,
    cmd_epoch: CmdEpoch,
    claude_running: Arc<AtomicBool>,
    shell_idle: Arc<Mutex<Option<bool>>>,
    claude: Arc<crate::claude_status::ClaudeHandle>,
) {
    // How long to keep looking for Claude after a command starts. Enough to catch a
    // slow cold launch (Windows especially: $PROFILE + shim + node + MCP servers,
    // antivirus scanning node.exe) — well beyond the old 2s that missed them — yet
    // bounded so a long-running NON-Claude command (a build, a dev server) only gets a
    // short burst (~7 backed-off checks) and then nothing.
    const SCAN_WINDOW: Duration = Duration::from_secs(10);

    let (lock, cvar) = &*cmd_epoch;
    let mut last = *lock.lock().unwrap();
    loop {
        // Block until a command actually starts (a busy edge bumps the epoch). The 60s
        // timeout is ONLY a lost-wakeup safety net: on a pure timeout the epoch is
        // unchanged, so we loop back WITHOUT scanning — nothing runs while idle.
        let prev = last;
        {
            let guard = lock.lock().unwrap();
            let (guard, _to) =
                cvar.wait_timeout_while(guard, Duration::from_secs(60), |e| *e == prev).unwrap();
            last = *guard;
        }
        if last == prev {
            continue; // woke on the timeout with no new command → don't scan
        }
        // A command started. Look for Claude with BACKOFF until we find it, the shell
        // returns to idle (command ended → wasn't Claude), or the window elapses. The
        // loop stops the instant Claude appears, so a running Claude is never scanned,
        // and it never runs while idle (that's the blocking wait above).
        crate::claude_shim::debug_log(&format!(
            "claude_monitor: busy edge on shell_pid={shell_pid}, scanning"
        ));
        let started = Instant::now();
        let mut delay = Duration::from_millis(250);
        loop {
            if crate::claude::running_under(shell_pid) {
                claude_running.store(true, Ordering::Relaxed);
                // Persist that Claude is running here (even before a session binds) so
                // a restore relaunches it.
                crate::claude_status::SAVE_DIRTY.store(true, Ordering::Relaxed);
                crate::claude_shim::debug_log(&format!("claude_monitor: found on shell_pid={shell_pid}"));
                break;
            }
            // Not Claude locally. An ssh/mosh client means the foreground program is on
            // another machine, so hand the pane to the screen probe (see `reader_loop`)
            // and stop scanning: nothing local will ever appear for it. Uses the same
            // 250ms-gated snapshot the Claude scan just took, so it is effectively free.
            if crate::claude::ssh_under(shell_pid) {
                claude.set_remote(true);
                crate::claude_shim::debug_log(&format!("claude_monitor: ssh on shell_pid={shell_pid}"));
                // A connection still up when the arm window closes, having never
                // prompted, logged in without a credential. Learned here because a far
                // host without the snippet and without Claude offers no other evidence.
                // One sleep, not a poll; checks it is still the same connection.
                let since = claude.remote_since_ms();
                let claude = claude.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(ARM_WINDOW);
                    if claude.remote_since_ms() == since {
                        claude.note_login_evidence();
                    }
                });
                break;
            }
            if *shell_idle.lock().unwrap() == Some(true) {
                break; // command finished → it wasn't Claude
            }
            if started.elapsed() >= SCAN_WINDOW {
                break; // give up for this command — don't scan a long non-Claude one forever
            }
            std::thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_secs(2)); // 250ms → 500ms → 1s → 2s cap
        }
    }
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{chunk_spinner_key, git_relevant_change, TypedLine};
    use std::path::Path;

    fn key(s: &str) -> Option<u64> {
        chunk_spinner_key(s.as_bytes())
    }

    /// Type `s` a byte at a time (as real keystrokes arrive) and return the last
    /// command submitted.
    fn typed(s: &str) -> Option<String> {
        let mut t = TypedLine::default();
        let mut last = None;
        for b in s.bytes() {
            if let Some(cmd) = t.fold(&[b]) {
                last = Some(cmd);
            }
        }
        last
    }

    /// A gate plus the receiving end of its PTY channel.
    fn gate() -> (super::StartupGate, std::sync::mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (super::StartupGate::new(tx), rx)
    }

    fn sent(rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Option<String> {
        rx.try_recv().ok().map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    // Both facts are required, and they arrive from unrelated threads in either order,
    // so whichever is last has to be the one that releases.
    #[test]
    fn startup_command_waits_for_the_prompt_and_the_real_size() {
        // Resize last.
        let (g, rx) = gate();
        g.queue("ssh mini");
        assert_eq!(sent(&rx), None, "nothing is ready yet");
        g.note_prompt();
        assert_eq!(sent(&rx), None, "being read, but still 80x24");
        g.note_resized();
        assert_eq!(sent(&rx).as_deref(), Some("ssh mini\r"));

        // Prompt last.
        let (g, rx) = gate();
        g.queue("ssh mini");
        g.note_resized();
        assert_eq!(sent(&rx), None, "sized, but nothing is reading yet");
        g.note_prompt();
        assert_eq!(sent(&rx).as_deref(), Some("ssh mini\r"));
    }

    // A pane already prompted and sized before a command is queued (a Reconnect into a
    // warm pane) must still send, and exactly once however many more events arrive.
    #[test]
    fn startup_command_is_sent_once_whenever_it_is_queued() {
        let (g, rx) = gate();
        g.note_prompt();
        g.note_resized();
        g.queue("ssh mini");
        assert_eq!(sent(&rx).as_deref(), Some("ssh mini\r"));
        g.note_prompt();
        g.note_resized();
        g.send();
        assert_eq!(sent(&rx), None, "must not repeat");
    }

    // The five prompt forms, transcribed from running both installed OpenSSH builds.
    #[test]
    fn recognises_every_ssh_credential_prompt() {
        use super::is_credential_prompt;
        assert!(is_credential_prompt("Enter passphrase for key '/c/Users/TRE/.ssh/id_ed25519': "));
        assert!(is_credential_prompt("Enter passphrase for \"C:\\Users\\TRE\\.ssh\\id_ed25519\": "));
        assert!(is_credential_prompt("Enter passphrase: "));
        assert!(is_credential_prompt("tre@10.0.0.16's password: "));
        assert!(is_credential_prompt("(tre@10.0.0.16) Password: "));
        // Prompts arrive mid-chunk, after other output.
        assert!(is_credential_prompt("Last login: Tue\r\ntre@10.0.0.16's password: "));
    }

    // A false match means typing a secret somewhere it does not belong, so the near
    // misses matter more than the hits.
    #[test]
    fn ordinary_output_is_not_a_credential_prompt() {
        use super::is_credential_prompt;
        assert!(!is_credential_prompt("PS C:\\Users\\TRE> "));
        assert!(!is_credential_prompt("tre ~ $ "));
        // The bare word, which is why the markers carry their punctuation.
        assert!(!is_credential_prompt("cat /etc/passwd"));
        assert!(!is_credential_prompt("export DB_PASSWORD=hunter2"));
        assert!(!is_credential_prompt("Password rotation is due"));
        assert!(!is_credential_prompt("error: bad password"));
        // Talking about a passphrase is not being asked for one.
        assert!(!is_credential_prompt("ssh-add: no passphrase supplied"));
        assert!(!is_credential_prompt(""));
    }

    #[test]
    fn a_secret_never_prints_itself() {
        let s = super::Secret::new("hunter2");
        let shown = format!("{s:?}");
        assert!(!shown.contains("hunter2"), "Debug must not leak the secret: {shown}");
        assert!(shown.contains("redacted"));
        // What gets typed is the secret plus Return.
        assert_eq!(s.line(), b"hunter2\r".to_vec());
        assert!(super::Secret::new("").is_empty(), "an empty secret arms nothing");
    }

    #[test]
    fn remembers_the_submitted_command() {
        assert_eq!(typed("ssh mini\r").as_deref(), Some("ssh mini"));
        // Surrounding whitespace is trimmed, and the last submission wins.
        assert_eq!(typed("  ssh mini  \r").as_deref(), Some("ssh mini"));
        assert_eq!(typed("ls\rssh mini\r").as_deref(), Some("ssh mini"));
        // Backspace edits before submitting.
        assert_eq!(typed("ssh minx\u{7f}i\r").as_deref(), Some("ssh mini"));
    }

    #[test]
    fn nothing_is_remembered_without_a_submission() {
        assert_eq!(typed("ssh mini"), None); // still being typed
        assert_eq!(typed(""), None);
        assert_eq!(typed("   \r"), None); // a bare Enter submits nothing
    }

    // The safety property. Tracking a plain typed line cannot survive history recall,
    // completion or cursor movement, so those must abandon the line rather than leave a
    // half-formed one that could be replayed on restore as if the user had run it.
    #[test]
    fn control_sequences_abandon_the_line() {
        // Up-arrow (history recall): the real command never passed through here.
        assert_eq!(typed("ssh mini\u{1b}[A\r"), None);
        // Tab completion.
        assert_eq!(typed("ssh mi\t\r"), None);
        // Ctrl+C on a half-typed line.
        assert_eq!(typed("ssh mini\u{3}\r"), None);
        // A line abandoned mid-way still lets the NEXT clean line be remembered.
        assert_eq!(typed("ssh mi\u{1b}[A\rssh mini\r").as_deref(), Some("ssh mini"));
    }

    #[test]
    fn plain_output_carries_no_spinner() {
        assert_eq!(key("cargo build --bin arbiter\r\n"), None);
        // The input prompt arrow sits just outside the star range on purpose: typing
        // must never read as working.
        assert_eq!(key("\u{1b}[2K\u{276F} write the file"), None);
    }

    #[test]
    fn bloom_frames_fingerprint_differently() {
        // Consecutive frames of the animation Claude draws at its status line.
        assert_ne!(key("\u{1b}[23;1H\u{273B}"), key("\u{1b}[23;1H\u{273D}"));
        // The window-title spinner is Braille, and its frames differ from each other
        // and from the stars.
        assert_ne!(key("\u{1b}]0;\u{2802} Claude Code\u{7}"), key("\u{1b}]0;\u{2810} Claude Code\u{7}"));
        assert_ne!(key("\u{1b}]0;\u{2802} Claude Code\u{7}"), key("\u{1b}[23;1H\u{273B}"));
    }

    #[test]
    fn repainting_a_thinking_summary_fingerprints_the_same() {
        // Scrolling Claude's transcript redraws the screen, re-emitting whichever
        // "thinking summary" lines are on it. They all use the same star, so however
        // many land in a chunk the fingerprint is identical: no false animation.
        let a = key("\u{1b}[14;1H\u{273B} Brewed for 7s");
        let b = key("\u{1b}[9;1H\u{273B} Crunched for 2m 5s");
        let both = key("\u{1b}[9;1H\u{273B} Crunched for 2m 5s\u{1b}[14;1H\u{273B} Brewed for 7s");
        assert!(a.is_some());
        assert_eq!(a, b);
        assert_eq!(a, both);
    }

    fn rel(p: &str) -> bool {
        git_relevant_change(Path::new(p))
    }

    #[test]
    fn git_metadata_that_moves_the_status_is_relevant() {
        assert!(rel(".git/index")); // staging (git add)
        assert!(rel(".git/HEAD")); // branch switch
        assert!(rel(".git/refs/heads/main")); // commit / branch tip
        assert!(rel(".git/packed-refs"));
        assert!(rel(".git/MERGE_HEAD"));
    }

    #[test]
    fn git_churn_and_locks_are_ignored() {
        // Object store + reflog churn on commits/fetches/gc: status unaffected.
        assert!(!rel(".git/objects/ab/cdef0123"));
        assert!(!rel(".git/logs/HEAD"));
        // Transient lock files that flap on every git op (would self-fire otherwise).
        assert!(!rel(".git/index.lock"));
        assert!(!rel(".git/refs/heads/main.lock"));
    }

    #[test]
    fn working_tree_changes_are_relevant() {
        assert!(rel("src/main.rs"));
        // A working-tree *.lock is a real tracked file — must NOT be caught by the
        // `.git/`-scoped lock filter.
        assert!(rel("Cargo.lock"));
        assert!(rel("frontend/yarn.lock"));
    }

    #[test]
    fn gitignored_build_dirs_are_ignored() {
        assert!(!rel("target/debug/build/x"));
        assert!(!rel("node_modules/vite/dist/x.js"));
        assert!(!rel("frontend/.next/cache/x"));
    }

    #[test]
    fn a_duplicated_secret_types_the_same_line() {
        let s = super::Secret::new("hunter2");
        let d = s.duplicate();
        drop(s);
        assert_eq!(d.line(), b"hunter2\r".to_vec());
    }

    // `D;<code>` carries the exit status of the command that just ended; the older
    // bare letters still mean what they meant.
    #[test]
    fn osc133_reports_carry_the_exit_code() {
        use super::osc133;
        assert_eq!(osc133("D;255"), (Some(true), Some(255)));
        assert_eq!(osc133("D;0"), (Some(true), Some(0)));
        assert_eq!(osc133("D"), (Some(true), None));
        assert_eq!(osc133("A"), (Some(true), None));
        assert_eq!(osc133("C"), (Some(false), None));
        assert_eq!(osc133("B"), (Some(false), None));
        assert_eq!(osc133("D;"), (Some(true), None));
        assert_eq!(osc133("D;abc"), (Some(true), None));
        assert_eq!(osc133("Z;1"), (None, Some(1)));
        assert_eq!(osc133(""), (None, None));
    }

    #[test]
    fn a_denial_is_recognised_but_talk_of_permissions_is_not() {
        use super::permission_denied;
        assert!(permission_denied("tre@10.0.0.16: Permission denied (publickey,password)."));
        assert!(!permission_denied("chmod: changing permissions of 'x': Operation not permitted"));
        assert!(!permission_denied(""));
    }

    // Arbiter's own emitters leave the host empty, so that alone says local; a named
    // host is remote unless it is this machine. The flag is only a last resort because
    // at ssh exit the local prompt reports while the flag is still on.
    #[test]
    fn osc7_reports_are_told_apart_by_their_host() {
        use super::{osc7_origin, Osc7Origin::*};
        assert_eq!(osc7_origin("", Some("mac"), true), Local);
        assert_eq!(osc7_origin("", Some("mac"), false), Local);
        assert_eq!(osc7_origin("localhost", Some("mac"), true), Local);
        assert_eq!(osc7_origin("mini", Some("mac"), false), Remote);
        assert_eq!(osc7_origin("mini", Some("mac"), true), Remote);
        assert_eq!(osc7_origin("Mac.local", Some("mac"), true), Local);
        assert_eq!(osc7_origin("mac", Some("Mac.fritz.box"), true), Local);
        assert_eq!(osc7_origin("mini", None, true), Remote);
        assert_eq!(osc7_origin("mini", None, false), Local);
    }

    #[test]
    fn osc7_parsing_keeps_the_host_and_decodes_the_path() {
        use super::parse_osc7;
        let r = parse_osc7("7;file://mini/home/tre/my%20dir").unwrap();
        assert_eq!(r.host, "mini");
        assert_eq!(r.path, "/home/tre/my dir");
        let r = parse_osc7("7;file:///Users/tor").unwrap();
        assert_eq!(r.host, "");
        assert_eq!(r.path, "/Users/tor");
        let r = parse_osc7("7;file:///C:/Users/TRE").unwrap();
        assert_eq!(r.host, "");
        assert_eq!(r.path, "/C:/Users/TRE");
        assert!(parse_osc7("7;http://x/y").is_none());
        assert!(parse_osc7("133;A").is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_local_windows_path_gets_its_drive_form() {
        assert_eq!(super::local_path("/C:/Users/TRE"), "C:\\Users\\TRE");
    }

    // Quotes make the path literal; a leading `~` stays outside them so the far shell
    // expands it, which is what a `~`-relative inferred path needs.
    #[test]
    fn cd_cmd_quotes_for_the_remote_shell() {
        use super::cd_cmd;
        assert_eq!(cd_cmd("/home/tre/src"), "cd '/home/tre/src'");
        assert_eq!(cd_cmd("/home/tre/my dir"), "cd '/home/tre/my dir'");
        assert_eq!(cd_cmd("/it's"), "cd '/it'\\''s'");
        assert_eq!(cd_cmd("/h\u{e9}"), "cd '/h\u{e9}'");
        assert_eq!(cd_cmd("~"), "cd ~");
        assert_eq!(cd_cmd("~/Source/dev-webapp"), "cd ~/'Source/dev-webapp'");
    }

    fn followup(dir: &str, claude: bool) -> super::Followup {
        super::Followup { dir: dir.into(), resume_claude: claude, session: None, cd_in_command: false }
    }

    #[test]
    fn followup_resumes_the_known_conversation() {
        let mut f = followup("~/src", true);
        f.session = Some("abc-123".into());
        f.cd_in_command = true;
        assert_eq!(
            line(super::followup_line(f, None)),
            ("(claude --resume abc-123 || claude -c || claude)\r".into(), None)
        );
    }

    // What a typed `claude` line tells Arbiter about the conversation it starts.
    #[test]
    fn classifies_typed_claude_launches() {
        use super::{classify_claude_launch as c, ClaudeLaunch::*};
        assert_eq!(c("claude"), Bare);
        assert_eq!(c("claude --dangerously-skip-permissions"), Bare);
        assert_eq!(c("claude --model opus"), Bare, "a value after a flag is still a flag's");
        assert_eq!(c("claude --model=opus mcp"), Other, "an inline value does not swallow the next word");
        assert_eq!(c("claude fix the tests"), Bare, "an initial prompt is still interactive");
        assert_eq!(c("/opt/homebrew/bin/claude"), Bare);
        assert_eq!(c("claude --resume abc-123"), Named("abc-123".into()));
        assert_eq!(c("claude -r abc-123"), Named("abc-123".into()));
        assert_eq!(c("claude --resume=abc-123"), Named("abc-123".into()));
        assert_eq!(c("claude --session-id abc-123"), Named("abc-123".into()));
        assert_eq!(c("claude -r"), Other, "the picker: whatever is chosen is unknown");
        assert_eq!(c("claude -c"), Other);
        assert_eq!(c("claude --continue"), Other);
        assert_eq!(c("claude -p hello"), Other, "non-interactive");
        assert_eq!(c("claude mcp list"), Other, "a subcommand");
        assert_eq!(c("claude update"), Other);
        assert_eq!(c("claude --version"), Other);
        assert_eq!(c("claudette"), NotClaude);
        assert_eq!(c("ls"), NotClaude);
        assert_eq!(c(""), NotClaude);
    }

    #[test]
    fn fresh_session_ids_are_uuids_claude_accepts() {
        let id = super::new_session_id();
        assert_eq!(id.len(), 36);
        assert!(crate::remote::plausible_session_id(&id));
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
        assert!(parts[2].starts_with('4'), "version 4: {id}");
        assert!(matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'), "variant: {id}");
        assert_ne!(id, super::new_session_id());
    }

    #[test]
    fn recognises_claude_saying_a_conversation_is_gone() {
        assert!(super::conversation_gone("No conversation found with session ID: abc-123"));
        assert!(!super::conversation_gone("tre ~/src $ "));
    }

    fn line(out: Option<(Vec<u8>, Option<String>)>) -> (String, Option<String>) {
        let (bytes, dir) = out.expect("something to type");
        (String::from_utf8(bytes).unwrap(), dir)
    }

    // The typed fallback (mosh, plink, an ssh line that cannot be rewritten): one line
    // does it all, the cd guards Claude, and Claude continues or starts afresh.
    #[test]
    fn followup_changes_directory_and_relaunches_claude_in_one_line() {
        assert_eq!(
            line(super::followup_line(followup("/home/tre/src", true), None)),
            ("cd '/home/tre/src' && (claude -c || claude)\r".into(), Some("/home/tre/src".into()))
        );
        assert_eq!(
            line(super::followup_line(followup("~/Source/dev-webapp", false), None)),
            ("cd ~/'Source/dev-webapp'\r".into(), Some("~/Source/dev-webapp".into()))
        );
    }

    // Already there, by the far shell's report, by the connection's own `cd`, or because
    // the directory is home: only Claude is left to type, or nothing at all.
    #[test]
    fn followup_skips_the_cd_when_already_there() {
        assert_eq!(
            line(super::followup_line(followup("/home/tre", true), Some("/home/tre"))),
            ("(claude -c || claude)\r".into(), None)
        );
        assert!(super::followup_line(followup("/home/tre", false), Some("/home/tre")).is_none());
        let mut chained = followup("~/Source/x", true);
        chained.cd_in_command = true;
        assert_eq!(
            line(super::followup_line(chained, None)),
            ("(claude -c || claude)\r".into(), None)
        );
        assert!(super::followup_line(followup("~", false), None).is_none());
        assert_eq!(
            line(super::followup_line(followup("~", true), None)),
            ("(claude -c || claude)\r".into(), None)
        );
    }

    #[test]
    fn recognises_a_prompt_row() {
        use super::looks_like_prompt;
        assert!(looks_like_prompt("tre ~ $ "));
        assert!(looks_like_prompt("tre@ubuntu:~$"));
        assert!(looks_like_prompt("root@box:~# "));
        assert!(looks_like_prompt("tre@mini ~ %"));
        assert!(looks_like_prompt("PS C:\\Users\\tre> "));
        assert!(looks_like_prompt("~/src on main \u{276f} "));
        assert!(!looks_like_prompt("Enter passphrase for key '/c/Users/TRE/.ssh/id_ed25519': "));
        assert!(!looks_like_prompt("tre@10.0.0.16's password: "));
        assert!(!looks_like_prompt("Are you sure you want to continue connecting (yes/no/[fingerprint])? "));
        assert!(!looks_like_prompt("Last login: Sat Sep 12 17:40:39 2026 from 10.0.0.8"));
        assert!(!looks_like_prompt(""));
    }

    // The command is whatever follows the last prompt ending on the row, whatever the
    // prompt looks like.
    #[test]
    fn finds_the_command_after_the_prompt() {
        use super::command_on_row;
        assert_eq!(command_on_row("tre ~ $ cd Source"), Some(('$', "cd Source")));
        assert_eq!(command_on_row("tre ~/Source/dev-webapp [main] $ ls -la"), Some(('$', "ls -la")));
        assert_eq!(command_on_row("tre@ubuntu:~$ cd x"), Some(('$', "cd x")));
        assert_eq!(command_on_row("PS C:\\Users\\tre> cd x"), Some(('>', "cd x")));
        assert_eq!(command_on_row("> hello claude"), Some(('>', "hello claude")), "Claude's own box");
        assert_eq!(command_on_row("cd Source"), None, "no prompt on the row");
        assert_eq!(command_on_row("tre ~ $ echo $ x"), Some(('$', "x")), "the last ending wins");
    }

    // Typed cds move the inferred directory; anything the text cannot resolve makes it
    // unknown rather than wrong.
    #[test]
    fn follows_typed_cd_commands() {
        use super::remote_cd;
        let cd = |row: &str, base: &str| remote_cd(row, base);
        assert_eq!(cd("tre ~ $ cd Source", "~"), Some(Some("~/Source".into())));
        assert_eq!(cd("tre ~/Source $ cd dev-webapp", "~/Source"), Some(Some("~/Source/dev-webapp".into())));
        assert_eq!(cd("tre ~/Source/dev-webapp $ cd ..", "~/Source/dev-webapp"), Some(Some("~/Source".into())));
        assert_eq!(cd("tre ~/Source $ cd", "~/Source"), Some(Some("~".into())));
        assert_eq!(cd("tre ~/Source $ cd ~", "~/Source"), Some(Some("~".into())));
        assert_eq!(cd("tre ~ $ cd ~/src/x", "~"), Some(Some("~/src/x".into())));
        assert_eq!(cd("tre ~ $ cd /var/log", "~"), Some(Some("/var/log".into())));
        assert_eq!(cd("tre ~ $ cd 'My Docs'", "~"), Some(Some("~/My Docs".into())));
        assert_eq!(cd("tre ~ $ cd \"a b\"", "~"), Some(Some("~/a b".into())));
        assert_eq!(cd("tre ~ $ cd ./x/./y", "~"), Some(Some("~/x/y".into())));
        assert_eq!(cd("tre ~ $ cd src && claude", "~"), Some(Some("~/src".into())));
        assert_eq!(cd("tre ~ $ cd src; ls", "~"), Some(Some("~/src".into())));
        // Unknowable targets.
        assert_eq!(cd("tre ~ $ cd -", "~/src"), Some(None));
        assert_eq!(cd("tre ~ $ cd $HOME/x", "~"), Some(None));
        assert_eq!(cd("tre ~ $ cd ~tre/x", "~"), Some(None));
        assert_eq!(cd("tre ~ $ cd ..", "~"), Some(None), "above home is unknowable");
        assert_eq!(cd("tre ~ $ cd a\\ b", "~"), Some(None));
        // Not a cd at all.
        assert_eq!(cd("tre ~ $ cdx", "~"), None);
        assert_eq!(cd("tre ~ $ ls", "~"), None);
        assert_eq!(cd("tre ~ $ echo cd foo", "~"), None);
        assert_eq!(cd("> cd foo", "~"), Some(Some("~/foo".into())), "gated by the caller, not here");
        // Absolute bases climb normally.
        assert_eq!(cd("root@box:/var/log# cd ../..", "/var/log"), Some(Some("/".into())));
        assert_eq!(cd("root@box:/# cd ..", "/"), Some(Some("/".into())));
    }

    #[test]
    fn recognises_failed_cds_and_unreachable_hosts() {
        use super::{cd_failed, connect_failed};
        assert!(cd_failed("cd: no such file or directory: Do"));
        assert!(cd_failed("bash: cd: Do: No such file or directory"));
        assert!(cd_failed("bash: cd: /root: Permission denied"));
        assert!(cd_failed("cd: not a directory: file.txt"));
        assert!(!cd_failed("ls: cannot access 'x': No such file or directory"));
        assert!(!cd_failed("tre ~/Downloads $ "));

        assert!(connect_failed("ssh: connect to host 10.0.0.16 port 22: Connection refused"));
        assert!(connect_failed("ssh: connect to host 10.0.0.16 port 22: Connection timed out"));
        assert!(connect_failed("ssh: Could not resolve hostname mini: Name or service not known"));
        assert!(connect_failed("kex_exchange_identification: read: Connection reset by peer"));
        assert!(connect_failed("Connection closed by 10.0.0.16 port 22"));
        // A session that WAS up and then went: these must still earn a retry.
        assert!(!connect_failed("Connection to 10.0.0.16 closed by remote host."));
        assert!(!connect_failed("client_loop: send disconnect: Connection reset by peer"));
        assert!(!connect_failed("Connection to 10.0.0.16 closed."));
    }
}
