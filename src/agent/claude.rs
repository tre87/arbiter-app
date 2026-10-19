//! Claude Code. Everything Arbiter knows about Claude specifically lives here: how its
//! process is recognised, the hooks handed to it through `--settings`, what its hook
//! payloads mean, its screen (spinner glyphs, hint lines, choosers), and how one of its
//! conversations is resumed.

use std::path::PathBuf;

use super::{
    field, hook_exe, plausible_session_id, Agent, AgentEvent, AgentKind, GlyphBits, GlyphRange,
    HookOutcome, Lead, LaunchCtx, LaunchSpec, MenuRule, PromptShape, RowRule, ScreenProfile,
    TypedLaunch,
};

pub struct Claude;

/// The generated settings file the launcher passes as `--settings`.
pub const SETTINGS_ENV: &str = "ARBITER_CLAUDE_SETTINGS";

/// Claude's spinner frames: the star/asterisk dingbats of its ✻ bloom (verified by
/// capturing the CLI). Deliberately NOT the whole U+2700..U+27BF block: that includes
/// the input prompt arrow (U+276F), which would make typing read as working.
const STARS: GlyphRange = GlyphRange { lo: 0x2722, hi: 0x273F };
const BRAILLE: GlyphRange = GlyphRange { lo: 0x2800, hi: 0x28FF };
const BOX_DRAWING: GlyphRange = GlyphRange { lo: 0x2500, hi: 0x257F };

/// The footer hint Claude shows only while a turn is running. 2.1.28x draws it under the
/// input box, in the slot "? for shortcuts" holds while idle; earlier releases put it on
/// the spinner row ("✻ Thinking… (esc to interrupt)"), which is still accepted.
const INTERRUPT_HINT: &str = "esc to interrupt";

pub static PROFILE: ScreenProfile = ScreenProfile {
    // The hint line reads `? for shortcuts` in the default mode and shows the mode label
    // in each of the others (verified against the shipped CLI's mode table).
    chrome: &[
        "? for shortcuts",
        "manual mode on",
        "accept edits on",
        "plan mode on",
        "auto mode on",
        "bypass permissions on",
    ],
    // Measured against a real session: below the cursor come the box's bottom border, an
    // optional warning line, the user's statusLine, and the mode line, so the marker was
    // 4 rows down. Set well past that because everything between is optional; the window
    // only extends DOWNWARD, which is what keeps stale chrome above the cursor from
    // matching, so widening it costs no precision.
    chrome_rows_below: 8,
    // Codex draws "? for shortcuts" too, beside its context meter.
    chrome_veto: &["% context left"],
    working_footer: &[INTERRUPT_HINT],
    // A spinner frame (the ✢✳✶✻✽ range, or the bloom's `·` frame) then a space.
    status_glyphs: &[STARS, GlyphRange::one('·')],
    working_rows: &[RowRule { lead: Lead::StatusGlyph, starts: "", all: &[INTERRUPT_HINT], any: &[] }],
    live_rows: 40,
    // "Jump to bottom (ctrl+End) ↓" or "N new messages (ctrl+End) ↓".
    scrolled: &["(ctrl+End)"],
    menu: MenuRule {
        rows: 12,
        anywhere: &["Would you like to proceed"],
        // The tool-permission dialog's question on a row of its own. Its footer can carry
        // a single hint, which the two-hint rule does not accept, and a remote pane has no
        // permission hook to fall back on. Only the whole row counts: Claude's prose asks
        // the same thing mid-paragraph.
        whole_row: &["Do you want to proceed?"],
        edges: &['\u{2502}'],
        // "↑/↓ to navigate · Enter to select · Esc to cancel", under AskUserQuestion,
        // plan-mode approval and Claude's own slash-command menus.
        hints: &["to navigate", "to select", "Esc to cancel", "Enter to"],
        min_hints: 2,
    },
    // "✳ Waiting for N background agents to finish".
    waiting_rows: &[RowRule {
        lead: Lead::StatusGlyph,
        starts: "Waiting for ",
        all: &[" background agent", " to finish"],
        any: &[],
    }],
    // The mode line under the box, "⏸ manual mode on · 2 shells" (2.1.291, captured in a
    // ConPTY) or "⏵⏵ auto mode on · 1 shell · ← for agents". Unlike the turn's summary
    // line ("· 1 shell still running"), it stays up for as long as the shell does.
    shells: &["shell", "shells"],
    prompt: PromptShape {
        glyphs: &['\u{276f}'],
        // Claude's plain fallback draws `>`, which is also how the transcript echoes a
        // sent prompt (`> run the tests`).
        boxed: Some("> "),
        border: BOX_DRAWING,
        side: '\u{2502}',
        // Lines of typed text a box can hold before its bottom border and still be read.
        box_lines: 8,
    },
    // Both of Claude's glyph sets, the Unicode one and its plain fallback.
    task_marks: &[
        "\u{2610} ", "\u{2612} ", "\u{25fb} ", "\u{25fc} ", "\u{2714} ", "\u{25a1} ", "\u{25a0} ",
        "\u{221a} ", "[ ] ", "[\u{d7}] ",
    ],
    task_hang: '\u{23bf}',
    // The box draws `│ > /model        │`.
    slash_edges: &['\u{2502}', '|', '>', '\u{276f}'],
    // Each star its own bit; tool spinners and the window-title spinner are Braille, whose
    // 256 frames fold into the upper 34 bits. Two of those can collide, which costs one
    // animation frame before the next differing pair confirms working.
    activity_glyphs: &[
        GlyphBits { range: STARS, base: 0, fold: 0 },
        GlyphBits { range: BRAILLE, base: 30, fold: 34 },
    ],
};

/// The Claude starburst logo (matches the web's ClaudeIcon, fill #D97757).
const ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 248 248"><path fill="#D97757" d="M52.4285 162.873L98.7844 136.879L99.5485 134.602L98.7844 133.334H96.4921L88.7237 132.862L62.2346 132.153L39.3113 131.207L17.0249 130.026L11.4214 128.844L6.2 121.873L6.7094 118.447L11.4214 115.257L18.171 115.847L33.0711 116.911L55.485 118.447L71.6586 119.392L95.728 121.873H99.5485L100.058 120.337L98.7844 119.392L97.7656 118.447L74.5877 102.732L49.4995 86.1905L36.3823 76.62L29.3779 71.7757L25.8121 67.2858L24.2839 57.3608L30.6515 50.2716L39.3113 50.8623L41.4763 51.4531L50.2636 58.1879L68.9842 72.7209L93.4357 90.6804L97.0015 93.6343L98.4374 92.6652L98.6571 91.9801L97.0015 89.2625L83.757 65.2772L69.621 40.8192L63.2534 30.6579L61.5978 24.632C60.9565 22.1032 60.579 20.0111 60.579 17.4246L67.8381 7.49965L71.9133 6.19995L81.7193 7.49965L85.7946 11.0443L91.9074 24.9865L101.714 46.8451L116.996 76.62L121.453 85.4816L123.873 93.6343L124.764 96.1155H126.292V94.6976L127.566 77.9197L129.858 57.3608L132.15 30.8942L132.915 23.4505L136.608 14.4708L143.994 9.62643L149.725 12.344L154.437 19.0788L153.8 23.4505L150.998 41.6463L145.522 70.1215L141.957 89.2625H143.994L146.414 86.7813L156.093 74.0206L172.266 53.698L179.398 45.6635L187.803 36.802L193.152 32.5484H203.34L210.726 43.6549L207.415 55.1159L196.972 68.3492L188.312 79.5739L175.896 96.2095L168.191 109.585L168.882 110.689L170.738 110.53L198.755 104.504L213.91 101.787L231.994 98.7149L240.144 102.496L241.036 106.395L237.852 114.311L218.495 119.037L195.826 123.645L162.07 131.592L161.696 131.893L162.137 132.547L177.36 133.925L183.855 134.279H199.774L229.447 136.524L237.215 141.605L241.8 147.867L241.036 152.711L229.065 158.737L213.019 154.956L175.45 145.977L162.587 142.787H160.805V143.85L171.502 154.366L191.242 172.089L215.82 195.011L217.094 200.682L213.91 205.172L210.599 204.699L188.949 188.394L180.544 181.069L161.696 165.118H160.422V166.772L164.752 173.152L187.803 207.771L188.949 218.405L187.294 221.832L181.308 223.959L174.813 222.777L161.187 203.754L147.305 182.486L136.098 163.345L134.745 164.2L128.075 235.42L125.019 239.082L117.887 241.8L111.902 237.31L108.718 229.984L111.902 215.452L115.722 196.547L118.779 181.541L121.58 162.873L123.291 156.636L123.14 156.219L121.773 156.449L107.699 175.752L86.304 204.699L69.3663 222.777L65.291 224.431L58.2867 220.768L58.9235 214.27L62.8713 208.48L86.304 178.705L100.44 160.155L109.551 149.507L109.462 147.967L108.959 147.924L46.6977 188.512L35.6182 189.93L30.7788 185.44L31.4156 178.115L33.7079 175.752L52.4285 162.873Z"/></svg>"##;

/// Claude's subcommands, which take no `--session-id`. A first bare word that is one of
/// these is a subcommand; any other bare word is an initial prompt, which an interactive
/// launch happily takes alongside a session id.
const SUBCOMMANDS: &[&str] = &[
    "mcp", "update", "doctor", "install", "auth", "config", "plugin", "agents", "setup-token",
    "migrate-installer",
];

/// What brings Claude back on a far host with no conversation to name: the directory's
/// most recent one, else a fresh one.
const REMOTE_FALLBACK: &str = "claude -c || claude";

/// Claude's config dir: `$CLAUDE_CONFIG_DIR` if set, else `~/.claude`.
pub fn config_dir() -> Option<PathBuf> {
    super::config_dir_from("CLAUDE_CONFIG_DIR", ".claude")
}

/// Claude keeps each session's transcript under `<config>/projects/<dir>/`, where `<dir>`
/// is the cwd with every non-alphanumeric (and non-`-`) char replaced by `-`.
fn encode_project_dir(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect()
}

/// Where Claude keeps `session`'s transcript for a conversation started in `cwd`.
pub fn transcript_in(config_dir: &std::path::Path, cwd: &str, session: &str) -> PathBuf {
    config_dir.join("projects").join(encode_project_dir(cwd)).join(format!("{session}.jsonl"))
}

impl Agent for Claude {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn id(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude"
    }

    fn description(&self) -> &'static str {
        "Claude Code (`claude`). Arbiter shows when it is working, waiting on you or finished, \
         raises its cards and office desk, and resumes its conversation when a terminal is restored."
    }

    fn icon_svg(&self) -> &'static str {
        ICON
    }

    fn binaries(&self) -> &'static [&'static str] {
        &["claude"]
    }

    // A process literally named claude (the compiled launcher), or `node` running the
    // claude-code CLI. Matching ANY arg that merely *contains* "claude" was too loose:
    // `npm run dev` in a project whose path contains "claude" put that dir in argv and
    // false-detected Claude. So require the claude-code package dir, or a bin/script whose
    // basename starts with "claude".
    fn matches_process(&self, name: &str, argv: &[String]) -> bool {
        let name = name.to_lowercase();
        if name.starts_with("claude") {
            return true;
        }
        if !name.contains("node") {
            return false;
        }
        argv.iter().any(|s| {
            let lower = s.to_lowercase();
            // The package DIR, followed by a separator so `claude-code-tutorial/` etc.
            // doesn't match.
            lower.contains("claude-code/")
                || lower.contains("claude-code\\")
                || lower
                    .rsplit(|c| c == '/' || c == '\\')
                    .next()
                    .is_some_and(|base| base.starts_with("claude"))
        })
    }

    fn launch(&self, ctx: &LaunchCtx) -> Option<LaunchSpec> {
        let command = format!("\"{}\" agent-hook claude", hook_exe(ctx.arbiter_exe));
        let entry = serde_json::json!([{ "hooks": [{ "type": "command", "command": command }] }]);
        // No `statusLine`: the user's own status line runs as configured. The session id
        // comes from SessionStart, which fires on startup, resume, /clear and compaction.
        // Claude merges these hooks with the user's own.
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": entry.clone(),
                "Notification": entry.clone(),
                "PermissionRequest": entry.clone(),
                "Stop": entry,
            }
        });
        let path = ctx.dir.join("settings.json");
        Some(LaunchSpec {
            files: vec![(path.clone(), serde_json::to_string_pretty(&settings).ok()?)],
            env: vec![(SETTINGS_ENV.to_string(), path.display().to_string())],
            args: vec![("--settings", SETTINGS_ENV.to_string())],
        })
    }

    fn normalize_hook(&self, _args: &[String], stdin: &[u8]) -> HookOutcome {
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
            "Stop" => AgentEvent::TurnEnded,
            "SessionEnd" => AgentEvent::SessionEnded,
            "PermissionRequest" => AgentEvent::NeedsAttention,
            "Notification" => match field(&v, &["notification_type"]).unwrap_or("") {
                // `idle_prompt` is intentionally excluded: it fires after ~60s idle and
                // would turn a merely-idle pane amber (idle stays grey).
                //
                // `elicitation_dialog` (AskUserQuestion) is ALSO excluded: it is detected
                // level-based from the rendered menu (`ScreenProfile::menu`), which clears
                // the instant the user escapes or answers. The hook is edge-only and would
                // get STUCK on escape (no spinner or Stop follows to clear it).
                "permission_prompt" => AgentEvent::NeedsAttention,
                _ => return HookOutcome::default(),
            },
            _ => return HookOutcome::default(),
        };
        HookOutcome { session, events: vec![event] }
    }

    fn screen(&self) -> &'static ScreenProfile {
        &PROFILE
    }

    fn resume_local(&self, session: Option<&str>) -> String {
        match session.filter(|s| plausible_session_id(s)) {
            Some(id) => format!("claude --resume {id}"),
            None => "claude".to_string(),
        }
    }

    // The pane's own conversation when Arbiter knows its id, else the directory's most
    // recent one, else a fresh one; each step runs only if the previous could not.
    fn resume_remote(&self, session: Option<&str>) -> String {
        match session.filter(|s| plausible_session_id(s)) {
            Some(id) => format!("claude --resume {id} || {REMOTE_FALLBACK}"),
            None => REMOTE_FALLBACK.to_string(),
        }
    }

    fn classify_launch(&self, cmd: &str) -> TypedLaunch {
        let mut words = cmd.split_whitespace();
        let Some(first) = words.next() else { return TypedLaunch::NotThis };
        if first.rsplit('/').next().unwrap_or(first) != "claude" {
            return TypedLaunch::NotThis;
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
                    match value.filter(|v| plausible_session_id(v)) {
                        Some(id) => named = Some(id.to_string()),
                        None => return TypedLaunch::Other,
                    }
                    after_flag = false;
                }
                "-c" | "--continue" | "-p" | "--print" | "-h" | "--help" | "-v" | "--version" => {
                    return TypedLaunch::Other;
                }
                f if f.starts_with('-') => after_flag = inline.is_none(),
                // A bare word: the preceding flag's value, a subcommand, or an initial prompt.
                w if !after_flag && SUBCOMMANDS.contains(&w) => return TypedLaunch::Other,
                _ => after_flag = false,
            }
        }
        match named {
            Some(id) => TypedLaunch::Named(id),
            None => TypedLaunch::Bare,
        }
    }

    fn name_flag(&self) -> Option<&'static str> {
        Some("--session-id")
    }

    fn gone_markers(&self) -> &'static [&'static str] {
        &["No conversation found with session ID"]
    }

    fn derived_transcript(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        Some(transcript_in(&config_dir()?, cwd, session))
    }

    // The kitty keyboard protocol's Shift+Enter, which Claude reads as a newline in its box.
    fn shift_enter(&self) -> &'static [u8] {
        b"\x1b[13;2u"
    }

    fn wheel_nudge(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detects_real_claude() {
        // A process literally named claude (compiled launcher / set title).
        assert!(Claude.matches_process("claude", &cmd(&[])));
        // node running the `claude` bin (basename), however it's installed.
        assert!(Claude.matches_process("node", &cmd(&["node", "/Users/x/.local/bin/claude", "--resume"])));
        assert!(Claude.matches_process("node", &cmd(&["node", "/x/node_modules/.bin/claude"])));
        // node running the package's cli.js directly (basename is cli.js, but the
        // claude-code package dir is in the path).
        assert!(Claude.matches_process("node", &cmd(&["node", "/x/node_modules/@anthropic-ai/claude-code/cli.js"])));
        // Windows-style separators.
        assert!(Claude.matches_process(
            "node.exe",
            &cmd(&["node.exe", "C:\\x\\node_modules\\@anthropic-ai\\claude-code\\cli.js"]),
        ));
    }

    #[test]
    fn ignores_dev_server_in_claude_named_dir() {
        // The reported bug: `npm run dev` in a project whose path contains "claude".
        assert!(!Claude.matches_process("node", &cmd(&["node", "/Users/x/claude-app/node_modules/vite/bin/vite.js"])));
        assert!(!Claude.matches_process("node", &cmd(&["node", "/Users/x/my-claude-ui/node_modules/.bin/next", "dev"])));
        // npm itself, launched from a claude-named dir.
        assert!(!Claude.matches_process(
            "node",
            &cmd(&["node", "/Users/x/claude-app/node_modules/npm/bin/npm-cli.js", "run", "dev"]),
        ));
        // A dir merely PREFIXED with "claude-code" must not match the package check.
        assert!(!Claude.matches_process(
            "node",
            &cmd(&["node", "/Users/x/claude-code-tutorial/node_modules/vite/bin/vite.js"]),
        ));
    }

    #[test]
    fn ignores_non_node_non_claude() {
        assert!(!Claude.matches_process("bash", &cmd(&["bash", "-lc", "echo claude"])));
        assert!(!Claude.matches_process("vite", &cmd(&["vite"])));
    }

    // What a typed `claude` line tells Arbiter about the conversation it starts.
    #[test]
    fn classifies_typed_claude_launches() {
        use TypedLaunch::*;
        let c = |s: &str| Claude.classify_launch(s);
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
        assert_eq!(c("claudette"), NotThis);
        assert_eq!(c("codex"), NotThis);
        assert_eq!(c(""), NotThis);
    }

    // The pane's own conversation first, then the directory's most recent, then a fresh
    // one; an id that could not be Claude's is ignored rather than escaped.
    #[test]
    fn claude_resumes_its_own_conversation_when_the_id_is_known() {
        assert_eq!(Claude.resume_remote(None), "claude -c || claude");
        assert_eq!(
            Claude.resume_remote(Some("3f2a9c1e-7b4d-4e8a-9f01-2c5d6e7f8a9b")),
            "claude --resume 3f2a9c1e-7b4d-4e8a-9f01-2c5d6e7f8a9b || claude -c || claude"
        );
        assert_eq!(Claude.resume_remote(Some("")), "claude -c || claude");
        assert_eq!(Claude.resume_remote(Some("x; rm -rf ~")), "claude -c || claude");
        // Locally a pane never falls back to the directory's most recent conversation:
        // with two panes in one folder, it could be the other one's.
        assert_eq!(Claude.resume_local(Some("abc-123")), "claude --resume abc-123");
        assert_eq!(Claude.resume_local(None), "claude");
        assert_eq!(Claude.resume_local(Some("x; rm -rf ~")), "claude");
    }

    #[test]
    fn recognises_claude_saying_a_conversation_is_gone() {
        let gone = |t: &str| Claude.gone_markers().iter().any(|m| t.contains(m));
        assert!(gone("No conversation found with session ID: abc-123"));
        assert!(!gone("tre ~/src $ "));
    }

    #[test]
    fn encodes_cwd_like_claude() {
        // Matches the real on-disk dir under ~/.claude/projects/.
        assert_eq!(encode_project_dir("/Users/tor/Private/Source/arbiter-app"), "-Users-tor-Private-Source-arbiter-app");
        assert_eq!(encode_project_dir("/a/b-c.d_e"), "-a-b-c-d-e");
    }

    // `CLAUDE_CONFIG_DIR` moves the transcripts with the rest of Claude's state; the
    // resumable check used to look under ~/.claude regardless.
    #[test]
    fn the_transcript_is_found_under_the_config_dir_it_was_given() {
        let t = transcript_in(std::path::Path::new("/cfg"), "/Users/tor/src", "abc-123");
        assert_eq!(t, std::path::Path::new("/cfg/projects/-Users-tor-src/abc-123.jsonl"));
    }

    fn hook(json: &str) -> HookOutcome {
        Claude.normalize_hook(&[], json.as_bytes())
    }

    // Payloads as Claude sends them (fields trimmed to those that matter plus a few
    // that must be ignored).
    #[test]
    fn hook_payloads_normalize_to_agent_events() {
        let start = hook(
            r#"{"session_id":"abc-123","transcript_path":"/h/.claude/projects/-x/abc-123.jsonl","cwd":"/x","hook_event_name":"SessionStart","source":"clear"}"#,
        );
        assert_eq!(start.session.as_deref(), Some("abc-123"));
        assert_eq!(
            start.events,
            vec![AgentEvent::SessionBound {
                id: "abc-123".into(),
                transcript: Some("/h/.claude/projects/-x/abc-123.jsonl".into())
            }]
        );
        assert_eq!(hook(r#"{"session_id":"s","hook_event_name":"Stop","stop_hook_active":false}"#).events, vec![AgentEvent::TurnEnded]);
        assert_eq!(
            hook(r#"{"session_id":"s","hook_event_name":"PermissionRequest","tool_name":"Bash"}"#).events,
            vec![AgentEvent::NeedsAttention]
        );
        assert_eq!(
            hook(r#"{"session_id":"s","hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"}"#).events,
            vec![AgentEvent::NeedsAttention]
        );
        for ignored in ["idle_prompt", "elicitation_dialog", "auth_success"] {
            let json = format!(r#"{{"session_id":"s","hook_event_name":"Notification","notification_type":"{ignored}"}}"#);
            assert!(hook(&json).events.is_empty(), "{ignored}");
        }
        assert!(hook(r#"{"session_id":"s","hook_event_name":"PreToolUse"}"#).events.is_empty());
        assert!(hook("not json").events.is_empty());
        // A session id that could escape a file name is dropped with its event.
        assert!(hook(r#"{"session_id":"../x","hook_event_name":"SessionStart"}"#).events.is_empty());
    }

    // Recorded from Claude 2.1.281 on Windows through the live harness
    // (`agent_shim::tests::live_claude_reports_its_session_through_the_launcher`), paths
    // shortened. Fields Arbiter does not read are kept, so a rename among the ones it does
    // shows up here.
    #[test]
    fn recorded_payloads_normalize() {
        let start = hook(
            r#"{"session_id":"3e2c0e76-f20c-4916-a498-c6d24cd6091b","transcript_path":"C:\\Users\\TRE\\.claude\\projects\\C--tmp-live\\3e2c0e76-f20c-4916-a498-c6d24cd6091b.jsonl","cwd":"C:\\tmp\\live","hook_event_name":"SessionStart","source":"startup"}"#,
        );
        assert_eq!(
            start.events,
            vec![AgentEvent::SessionBound {
                id: "3e2c0e76-f20c-4916-a498-c6d24cd6091b".into(),
                transcript: Some("C:\\Users\\TRE\\.claude\\projects\\C--tmp-live\\3e2c0e76-f20c-4916-a498-c6d24cd6091b.jsonl".into()),
            }]
        );
        let stop = hook(
            r#"{"session_id":"3e2c0e76-f20c-4916-a498-c6d24cd6091b","transcript_path":"C:\\x.jsonl","cwd":"C:\\tmp\\live","prompt_id":"3e98fe5a-b779-40e9-984e-2bafd507ba12","permission_mode":"auto","effort":{"level":"xhigh"},"hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"ok","background_tasks":[],"session_crons":[]}"#,
        );
        assert_eq!(stop.session.as_deref(), Some("3e2c0e76-f20c-4916-a498-c6d24cd6091b"));
        assert_eq!(stop.events, vec![AgentEvent::TurnEnded]);
        // The transcript Claude named is where Arbiter would have looked for it.
        let derived = transcript_in(std::path::Path::new("C:\\Users\\TRE\\.claude"), "C:\\tmp\\live", "3e2c0e76-f20c-4916-a498-c6d24cd6091b");
        assert!(derived.ends_with(std::path::Path::new("C--tmp-live").join("3e2c0e76-f20c-4916-a498-c6d24cd6091b.jsonl")));
    }

    // The settings Claude is launched with: hooks only, never a statusLine, so the
    // user's own status line runs as they configured it.
    #[test]
    fn the_launch_settings_carry_hooks_and_no_status_line() {
        let dir = std::path::Path::new("/data/agent-shim/claude");
        let spec = Claude.launch(&LaunchCtx { arbiter_exe: std::path::Path::new("/opt/arbiter"), dir }).unwrap();
        let (path, body) = &spec.files[0];
        assert_eq!(path, &dir.join("settings.json"));
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(v.get("statusLine").is_none());
        for ev in ["SessionStart", "Notification", "PermissionRequest", "Stop"] {
            let cmd = v.pointer(&format!("/hooks/{ev}/0/hooks/0/command")).and_then(|c| c.as_str());
            assert_eq!(cmd, Some("\"/opt/arbiter\" agent-hook claude"), "{ev}");
        }
        assert_eq!(spec.args, vec![("--settings", SETTINGS_ENV.to_string())]);
    }
}
