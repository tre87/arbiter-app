//! OpenAI Codex CLI (`codex`). Built from its source (rust-v0.156.1) and NOT yet
//! verified against a running copy (see the checklist in CLAUDE.md).
//!
//! Codex has hooks, but it runs one only after the user has trusted that exact hook
//! definition, and the trust record is a hash of a normalized TOML identity that cannot
//! be reproduced reliably from outside. A hook whose hash is off is skipped, or put in
//! front of the user as "needs review". So Arbiter uses the channels that need no trust:
//!
//!   * `notify`, which Codex runs after every turn with the thread id, injected with `-c`
//!     (the user's own `notify`, if any, is called through, see `after_hook`);
//!   * its OSC 9 desktop notifications, forced on for every terminal with `-c`, which
//!     say "Approval requested…" and friends;
//!   * its window title: a Braille spinner while it works, `[ ! ] Action Required` while
//!     it waits on the user. Both of these also cross SSH.
//!
//! Hook payloads are still understood, for a user who wires Arbiter's hook in themselves.

use std::path::PathBuf;

use super::{
    field, plausible_session_id, Agent, AgentEvent, AgentKind, GlyphBits, GlyphRange, HookOutcome, Lead,
    LaunchCtx, LaunchSpec, MenuRule, PromptShape, RowRule, ScreenProfile, SignalProfile, TypedLaunch,
};

pub struct Codex;

const NOTIFY_ARG_ENV: &str = "ARBITER_CODEX_NOTIFY_ARG";
const METHOD_ARG_ENV: &str = "ARBITER_CODEX_METHOD_ARG";
const CONDITION_ARG_ENV: &str = "ARBITER_CODEX_CONDITION_ARG";
/// The user's own `notify` argv, as JSON, for `after_hook` to call through to.
const USER_NOTIFY_ENV: &str = "ARBITER_CODEX_USER_NOTIFY";

const BRAILLE: GlyphRange = GlyphRange { lo: 0x2800, hi: 0x28FF };

pub static PROFILE: ScreenProfile = ScreenProfile {
    // The footer's context meter, "100% context left".
    chrome: &["% context left"],
    chrome_rows_below: 8,
    chrome_veto: &[],
    working_footer: &[],
    status_glyphs: &[],
    // "Working (12s • esc to interrupt)", after an optional activity glyph. The closing
    // parenthesis keeps the question view's footer ("esc to interrupt" alone) out.
    working_rows: &[RowRule { lead: Lead::Anywhere, starts: "", all: &["esc to interrupt)"], any: &[] }],
    live_rows: 40,
    scrolled: &[],
    menu: MenuRule {
        rows: 12,
        anywhere: &[
            "Would you like to run the following command?",
            "Would you like to make the following edits?",
            "Would you like to grant these permissions?",
            "Would you like to send input to",
        ],
        whole_row: &[],
        edges: &['\u{2502}'],
        // The question view (request_user_input) footer.
        hints: &["enter to submit answer", "enter to submit all"],
        min_hints: 1,
    },
    waiting_rows: &[],
    shells: &[],
    prompt: PromptShape {
        glyphs: &['\u{203a}'],
        boxed: None,
        border: GlyphRange { lo: 0x2500, hi: 0x257F },
        side: '\u{2502}',
        box_lines: 8,
    },
    task_marks: &[],
    task_hang: '\u{23bf}',
    slash_edges: &['\u{2502}', '>', '\u{203a}'],
    // The title spinner rides the raw output, 100 ms a frame.
    activity_glyphs: &[GlyphBits { range: BRAILLE, base: 0, fold: 60 }],
};

static SIGNALS: SignalProfile = SignalProfile {
    // `TERMINAL_TITLE_SPINNER_FRAMES` in codex-rs/tui/src/chatwidget/status_surfaces.rs.
    title_working: &[BRAILLE],
    // `[ ! ] Action Required`, blinking with `[ . ] Action Required`.
    title_attention: &["Action Required"],
    // `Notification::display` in codex-rs/tui/src/chatwidget/notifications.rs. Anything
    // else is the turn-complete preview.
    notify_attention: &["Approval requested", "Codex wants to edit", "Plan mode prompt: ", "Question: "],
    notify_is_turn_end: true,
    progress_is_working: false,
    bell_is_attention: false,
};

/// A neutral monogram, not OpenAI's mark.
const ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="6" fill="#5b6470"/><path d="M7 8.2l3.8 3.8L7 15.8M12.8 16h4.4" fill="none" stroke="#ffffff" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

const SUBCOMMANDS: &[&str] = &[
    "exec", "e", "login", "logout", "mcp", "mcp-server", "app-server", "completion", "sandbox", "debug",
    "apply", "a", "cloud", "features", "help", "review", "fork", "proto",
];

/// Root flags that take a value, so the word after them is not a prompt or subcommand.
const FLAGS_WITH_VALUE: &[&str] = &[
    "-c", "--config", "-m", "--model", "-p", "--profile", "-C", "--cd", "-i", "--image", "-s", "--sandbox",
    "-a", "--ask-for-approval", "--add-dir", "--enable", "--disable", "--local-provider", "--remote",
];

/// Codex's home: `$CODEX_HOME` if set, else `~/.codex`.
pub fn config_dir() -> Option<PathBuf> {
    super::config_dir_from("CODEX_HOME", ".codex")
}

/// The user's own top-level `notify = [...]` from `config.toml`, which Arbiter's replaces
/// for the session and so has to call through to. Only what that key can hold, an array
/// of strings before the first table, is understood; anything else reads as none.
fn user_notify(toml: &str) -> Option<Vec<String>> {
    let mut value: Option<String> = None;
    for line in toml.lines() {
        match value.as_mut() {
            Some(v) => {
                v.push('\n');
                v.push_str(line);
            }
            None => {
                let t = line.trim_start();
                if t.starts_with('[') {
                    return None;
                }
                let Some(rest) = t.strip_prefix("notify") else { continue };
                let Some(rest) = rest.trim_start().strip_prefix('=') else { continue };
                value = Some(rest.to_string());
            }
        }
        match parse_string_array(value.as_deref().unwrap_or("")) {
            Parsed::Done(argv) => return (!argv.is_empty()).then_some(argv),
            Parsed::Invalid => return None,
            Parsed::Incomplete => {}
        }
    }
    None
}

enum Parsed {
    Done(Vec<String>),
    Incomplete,
    Invalid,
}

/// A TOML array of basic (`"…"`) and literal (`'…'`) strings.
fn parse_string_array(s: &str) -> Parsed {
    let mut chars = s.chars().peekable();
    let skip_blank = |chars: &mut std::iter::Peekable<std::str::Chars>| {
        while let Some(&c) = chars.peek() {
            if c == '#' {
                while chars.peek().is_some_and(|&c| c != '\n') {
                    chars.next();
                }
            } else if c.is_whitespace() || c == ',' {
                chars.next();
            } else {
                break;
            }
        }
    };
    skip_blank(&mut chars);
    if chars.next() != Some('[') {
        return if s.trim().is_empty() { Parsed::Incomplete } else { Parsed::Invalid };
    }
    let mut out = Vec::new();
    loop {
        skip_blank(&mut chars);
        match chars.next() {
            None => return Parsed::Incomplete,
            Some(']') => return Parsed::Done(out),
            Some('\'') => {
                let mut v = String::new();
                loop {
                    match chars.next() {
                        None | Some('\n') => return Parsed::Invalid,
                        Some('\'') => break,
                        Some(c) => v.push(c),
                    }
                }
                out.push(v);
            }
            Some('"') => {
                let mut v = String::new();
                loop {
                    match chars.next() {
                        None | Some('\n') => return Parsed::Invalid,
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => v.push('\n'),
                            Some('t') => v.push('\t'),
                            Some('"') => v.push('"'),
                            Some('\\') => v.push('\\'),
                            Some('u') => {
                                let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                                    Some(c) => v.push(c),
                                    None => return Parsed::Invalid,
                                }
                            }
                            _ => return Parsed::Invalid,
                        },
                        Some(c) => v.push(c),
                    }
                }
                out.push(v);
            }
            Some(_) => return Parsed::Invalid,
        }
    }
}

impl Agent for Codex {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn description(&self) -> &'static str {
        "OpenAI Codex CLI (`codex`), followed the way Claude is. Experimental: built from Codex's \
         source and not yet checked against the real CLI, so expect rough edges."
    }

    fn experimental(&self) -> bool {
        true
    }

    fn icon_svg(&self) -> &'static str {
        ICON
    }

    fn binaries(&self) -> &'static [&'static str] {
        &["codex"]
    }

    // The native binary, or the npm package's node wrapper (`@openai/codex/bin/codex.js`).
    fn matches_process(&self, name: &str, argv: &[String]) -> bool {
        let name = name.to_lowercase();
        if name.strip_suffix(".exe").unwrap_or(&name) == "codex" {
            return true;
        }
        name.contains("node")
            && argv.iter().any(|s| {
                let lower = s.to_lowercase();
                lower.contains("@openai/codex/") || lower.contains("@openai\\codex\\")
            })
    }

    fn launch(&self, ctx: &LaunchCtx) -> Option<LaunchSpec> {
        let mut spec = LaunchSpec::default();
        // A TOML literal string: no escapes, so a Windows path needs none, but it cannot
        // hold a quote. Every value avoids double quotes, which the `.cmd` launcher's
        // own quoting could not carry.
        let exe = ctx.arbiter_exe.display().to_string();
        if !exe.contains(['\'', '\n']) {
            spec.env.push((NOTIFY_ARG_ENV.into(), format!("notify=['{exe}','agent-hook','codex','--notify']")));
            spec.args.push(("-c", NOTIFY_ARG_ENV.into()));
        }
        spec.env.push((METHOD_ARG_ENV.into(), "tui.notification_method='osc9'".into()));
        spec.args.push(("-c", METHOD_ARG_ENV.into()));
        // The default only notifies an unfocused terminal, and a pane never reports focus.
        spec.env.push((CONDITION_ARG_ENV.into(), "tui.notification_condition='always'".into()));
        spec.args.push(("-c", CONDITION_ARG_ENV.into()));
        let user = config_dir()
            .and_then(|d| std::fs::read_to_string(d.join("config.toml")).ok())
            .and_then(|t| user_notify(&t));
        if let Some(argv) = user {
            spec.env.push((USER_NOTIFY_ENV.into(), serde_json::to_string(&argv).ok()?));
        }
        Some(spec)
    }

    fn normalize_hook(&self, args: &[String], stdin: &[u8]) -> HookOutcome {
        // `notify` appends its JSON as the last argument, with stdin closed.
        if args.first().map(String::as_str) == Some("--notify") {
            let Some(v) = args.last().and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok()) else {
                return HookOutcome::default();
            };
            if field(&v, &["type"]) != Some("agent-turn-complete") {
                return HookOutcome::default();
            }
            let session = field(&v, &["thread-id", "thread_id"]).filter(|s| plausible_session_id(s)).map(str::to_string);
            let events = session
                .clone()
                .map(|id| AgentEvent::SessionBound { id, transcript: None })
                .into_iter()
                .chain([AgentEvent::TurnEnded])
                .collect();
            return HookOutcome { session, events };
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(stdin) else {
            return HookOutcome::default();
        };
        let session = field(&v, &["session_id"]).filter(|s| plausible_session_id(s)).map(str::to_string);
        let event = match field(&v, &["hook_event_name"]).unwrap_or("") {
            "SessionStart" => match &session {
                Some(id) => AgentEvent::SessionBound {
                    id: id.clone(),
                    transcript: field(&v, &["transcript_path"]).map(str::to_string),
                },
                None => return HookOutcome::default(),
            },
            "UserPromptSubmit" => AgentEvent::TurnStarted,
            "PermissionRequest" => AgentEvent::NeedsAttention,
            "Stop" => AgentEvent::TurnEnded,
            "Interrupt" => AgentEvent::Interrupted,
            "SessionEnd" => AgentEvent::SessionEnded,
            _ => return HookOutcome::default(),
        };
        HookOutcome { session, events: vec![event] }
    }

    // Arbiter's `notify` replaced the user's for this session: run theirs with the same
    // payload, detached, so a slow one cannot hold Codex's turn up.
    fn after_hook(&self, args: &[String], _stdin: &[u8]) {
        if args.first().map(String::as_str) != Some("--notify") {
            return;
        }
        let Some(payload) = args.last() else { return };
        let Some(argv) = std::env::var(USER_NOTIFY_ENV)
            .ok()
            .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
        else {
            return;
        };
        let Some((program, rest)) = argv.split_first() else { return };
        let mut cmd = std::process::Command::new(program);
        cmd.args(rest)
            .arg(payload)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let _ = cmd.spawn();
    }

    fn screen(&self) -> &'static ScreenProfile {
        &PROFILE
    }

    fn signals(&self) -> &'static SignalProfile {
        &SIGNALS
    }

    fn bound_after_a_turn(&self) -> bool {
        true
    }

    fn resume_local(&self, session: Option<&str>) -> String {
        match session.filter(|s| plausible_session_id(s)) {
            Some(id) => format!("codex resume {id}"),
            None => "codex".to_string(),
        }
    }

    fn resume_remote(&self, session: Option<&str>) -> String {
        match session.filter(|s| plausible_session_id(s)) {
            Some(id) => format!("codex resume {id} || codex resume --last || codex"),
            None => "codex resume --last || codex".to_string(),
        }
    }

    fn classify_launch(&self, cmd: &str) -> TypedLaunch {
        let mut words = cmd.split_whitespace();
        let Some(first) = words.next() else { return TypedLaunch::NotThis };
        if first.rsplit('/').next().unwrap_or(first) != "codex" {
            return TypedLaunch::NotThis;
        }
        while let Some(word) = words.next() {
            if FLAGS_WITH_VALUE.contains(&word) {
                words.next();
                continue;
            }
            if word.starts_with('-') {
                if matches!(word, "-h" | "--help" | "-V" | "--version") {
                    return TypedLaunch::Other;
                }
                continue;
            }
            if word == "resume" {
                return match words.find(|w| !w.starts_with('-') || *w == "--last") {
                    Some(id) if id != "--last" && plausible_session_id(id) => TypedLaunch::Named(id.to_string()),
                    _ => TypedLaunch::Other,
                };
            }
            if SUBCOMMANDS.contains(&word) {
                return TypedLaunch::Other;
            }
            // An initial prompt: still an interactive session, whose id Codex picks.
            return TypedLaunch::Bare;
        }
        TypedLaunch::Bare
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn detects_the_native_binary_and_the_npm_wrapper() {
        assert!(Codex.matches_process("codex", &[]));
        assert!(Codex.matches_process("codex.exe", &[]));
        assert!(Codex.matches_process("node", &argv(&["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js"])));
        assert!(!Codex.matches_process("node", &argv(&["node", "/x/codex-notes/index.js"])));
        assert!(!Codex.matches_process("codex-acp", &[]));
    }

    // The payload shape `legacy_notify.rs` writes (UNVERIFIED against a real Codex run).
    #[test]
    fn a_completed_turn_binds_the_thread_and_ends_the_turn() {
        let json = r#"{"type":"agent-turn-complete","thread-id":"019a2b3c-4d5e-7f80-9a1b-2c3d4e5f6a7b","turn-id":"t1","cwd":"/x","client":"codex-tui","input-messages":["hi"],"last-assistant-message":"done"}"#;
        let out = Codex.normalize_hook(&argv(&["--notify", json]), b"");
        assert_eq!(out.session.as_deref(), Some("019a2b3c-4d5e-7f80-9a1b-2c3d4e5f6a7b"));
        assert_eq!(
            out.events,
            vec![
                AgentEvent::SessionBound { id: "019a2b3c-4d5e-7f80-9a1b-2c3d4e5f6a7b".into(), transcript: None },
                AgentEvent::TurnEnded
            ]
        );
        assert!(Codex.normalize_hook(&argv(&["--notify", r#"{"type":"other"}"#]), b"").events.is_empty());
        assert!(Codex.normalize_hook(&argv(&["--notify", "garbage"]), b"").events.is_empty());
    }

    #[test]
    fn hook_payloads_are_understood_too() {
        let h = |json: &str| Codex.normalize_hook(&[], json.as_bytes()).events;
        assert_eq!(
            h(r#"{"session_id":"abc","hook_event_name":"SessionStart","transcript_path":"/h/.codex/sessions/r.jsonl","source":"startup"}"#),
            vec![AgentEvent::SessionBound { id: "abc".into(), transcript: Some("/h/.codex/sessions/r.jsonl".into()) }]
        );
        assert_eq!(h(r#"{"session_id":"abc","hook_event_name":"Interrupt"}"#), vec![AgentEvent::Interrupted]);
        assert_eq!(h(r#"{"session_id":"abc","hook_event_name":"PermissionRequest","tool_name":"shell"}"#), vec![AgentEvent::NeedsAttention]);
        assert!(h(r#"{"session_id":"abc","hook_event_name":"PreToolUse"}"#).is_empty());
    }

    #[test]
    fn the_users_own_notify_is_read_for_call_through() {
        assert_eq!(
            user_notify("model = \"o3\"\nnotify = [\"python3\", \"/Users/me/.codex/notify.py\"]\n[tui]\nx = 1\n"),
            Some(vec!["python3".into(), "/Users/me/.codex/notify.py".into()])
        );
        assert_eq!(
            user_notify("notify = [\n  'C:\\tools\\ping.exe', # a comment\n  \"--quiet\",\n]\n"),
            Some(vec!["C:\\tools\\ping.exe".into(), "--quiet".into()])
        );
        assert_eq!(user_notify("[profiles.x]\nnotify = [\"a\"]\n"), None, "inside a table, not top-level");
        assert_eq!(user_notify("notify_extra = [\"a\"]\n"), None);
        assert_eq!(user_notify("notify = \"not an array\"\n"), None);
        assert_eq!(user_notify("notify = []\n"), None);
        assert_eq!(user_notify(""), None);
    }

    #[test]
    fn the_launch_forces_osc9_and_injects_notify_without_double_quotes() {
        let spec = Codex
            .launch(&LaunchCtx {
                arbiter_exe: std::path::Path::new("C:\\Program Files\\Arbiter\\arbiter.exe"),
                dir: std::path::Path::new("/tmp"),
            })
            .unwrap();
        let env = |k: &str| spec.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(
            env(NOTIFY_ARG_ENV).as_deref(),
            Some("notify=['C:\\Program Files\\Arbiter\\arbiter.exe','agent-hook','codex','--notify']")
        );
        assert_eq!(env(METHOD_ARG_ENV).as_deref(), Some("tui.notification_method='osc9'"));
        assert!(spec.env.iter().all(|(_, v)| !v.contains('"') || v.starts_with('[')));
        assert!(spec.args.iter().all(|(flag, _)| *flag == "-c"));
    }

    #[test]
    fn classifies_typed_codex_launches() {
        use TypedLaunch::*;
        let c = |s: &str| Codex.classify_launch(s);
        assert_eq!(c("codex"), Bare);
        assert_eq!(c("codex -m o3 fix the build"), Bare);
        assert_eq!(c("codex -c model=o3"), Bare);
        assert_eq!(c("codex resume 019a2b3c-4d5e"), Named("019a2b3c-4d5e".into()));
        assert_eq!(c("codex resume --last"), Other);
        assert_eq!(c("codex resume"), Other, "the picker");
        assert_eq!(c("codex exec 'run tests'"), Other);
        assert_eq!(c("codex login"), Other);
        assert_eq!(c("claude"), NotThis);
    }

    #[test]
    fn resumes_by_id_and_never_by_recency_locally() {
        assert_eq!(Codex.resume_local(Some("019a")), "codex resume 019a");
        assert_eq!(Codex.resume_local(None), "codex");
        assert_eq!(Codex.resume_remote(Some("019a")), "codex resume 019a || codex resume --last || codex");
        assert_eq!(Codex.resume_remote(None), "codex resume --last || codex");
    }
}
