//! GitHub Copilot CLI (`copilot`, npm `@github/copilot`). Built from its documentation
//! (1.0.88) and NOT yet verified against a running copy: the hook wiring, the payload
//! fields and the screen are all to be confirmed (see the checklist in CLAUDE.md).
//!
//! Its hooks mirror Claude's, and events registered under Claude's PascalCase names
//! receive Claude-shaped payloads. There is no per-process settings flag, so the hooks
//! travel in a plugin directory passed with `--plugin-dir`, which leaves the user's own
//! plugins and `~/.copilot` untouched. The screen profile is deliberately almost empty:
//! a wrong chrome marker would light the wrong pane, and none has been recorded yet.

use std::path::PathBuf;

use super::{
    field, hook_exe, plausible_session_id, Agent, AgentEvent, AgentKind, GlyphBits, GlyphRange,
    HookOutcome, LaunchCtx, LaunchSpec, MenuRule, PromptShape, ScreenProfile, SignalProfile,
    TypedLaunch,
};

pub struct Copilot;

/// The plugin directory the launcher passes as `--plugin-dir`.
pub const PLUGIN_ENV: &str = "ARBITER_COPILOT_PLUGIN";

const BRAILLE: GlyphRange = GlyphRange { lo: 0x2800, hi: 0x28FF };

pub static PROFILE: ScreenProfile = ScreenProfile {
    chrome: &[],
    chrome_rows_below: 8,
    chrome_veto: &[],
    working_footer: &[],
    status_glyphs: &[],
    working_rows: &[],
    live_rows: 40,
    scrolled: &[],
    menu: MenuRule { rows: 12, anywhere: &[], whole_row: &[], edges: &['\u{2502}'], hints: &[], min_hints: 2 },
    waiting_rows: &[],
    shells: &[],
    prompt: PromptShape {
        glyphs: &['\u{276f}'],
        boxed: None,
        border: GlyphRange { lo: 0x2500, hi: 0x257F },
        side: '\u{2502}',
        box_lines: 8,
    },
    task_marks: &[],
    task_hang: '\u{23bf}',
    slash_edges: &['\u{2502}', '>', '\u{276f}'],
    activity_glyphs: &[GlyphBits { range: BRAILLE, base: 0, fold: 60 }],
};

// `terminalProgress` (on by default) emits OSC 9;4 while the agent works. Its bell rings
// for attention, but rang on every turn end before 1.0.55, so it is not read as one.
static SIGNALS: SignalProfile = SignalProfile {
    title_working: &[],
    title_attention: &[],
    notify_attention: &[],
    notify_is_turn_end: false,
    progress_is_working: true,
    bell_is_attention: false,
};

/// A neutral monogram, not GitHub's mark.
const ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="6" fill="#6e7fd6"/><path d="M15.6 8.4a5.1 5.1 0 1 0 0 7.2" fill="none" stroke="#ffffff" stroke-width="2.2" stroke-linecap="round"/></svg>"##;

/// The hook events registered, each told its own name as an argument: payloads of the
/// camelCase events do not carry it.
const HOOK_EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd", "notification"];

const SUBCOMMANDS: &[&str] = &["plugin", "login", "logout", "mcp", "update", "help", "version", "config"];

/// Copilot's state dir: `$COPILOT_HOME` if set, else `~/.copilot`.
pub fn config_dir() -> Option<PathBuf> {
    super::config_dir_from("COPILOT_HOME", ".copilot")
}

impl Agent for Copilot {
    fn kind(&self) -> AgentKind {
        AgentKind::Copilot
    }

    fn id(&self) -> &'static str {
        "copilot"
    }

    fn display_name(&self) -> &'static str {
        "Copilot"
    }

    fn icon_svg(&self) -> &'static str {
        ICON
    }

    fn binaries(&self) -> &'static [&'static str] {
        &["copilot"]
    }

    // The npm loader (`node .../@github/copilot/npm-loader.js`) spawns a native binary
    // named `copilot`; a standalone install is that binary alone. Named exactly, so
    // VS Code's `copilot-language-server` cannot count.
    fn matches_process(&self, name: &str, argv: &[String]) -> bool {
        let name = name.to_lowercase();
        if name.strip_suffix(".exe").unwrap_or(&name) == "copilot" {
            return true;
        }
        name.contains("node")
            && argv.iter().any(|s| {
                let lower = s.to_lowercase();
                lower.contains("@github/copilot/") || lower.contains("@github\\copilot\\")
            })
    }

    fn launch(&self, ctx: &LaunchCtx) -> Option<LaunchSpec> {
        let plugin = ctx.dir.join("plugin");
        let exe = hook_exe(ctx.arbiter_exe);
        let mut hooks = serde_json::Map::new();
        for ev in HOOK_EVENTS {
            hooks.insert(
                ev.to_string(),
                serde_json::json!([{
                    "type": "command",
                    "exec": exe,
                    "args": ["agent-hook", "copilot", ev],
                    "timeoutSec": 10,
                }]),
            );
        }
        let hooks = serde_json::json!({ "version": 1, "hooks": hooks });
        let manifest = serde_json::json!({
            "name": "arbiter",
            "description": "Lets Arbiter follow this session's state. Written by Arbiter at every launch.",
            "version": env!("CARGO_PKG_VERSION"),
        });
        let manifest = serde_json::to_string_pretty(&manifest).ok()?;
        Some(LaunchSpec {
            files: vec![
                // The root manifest is the documented location; `.claude-plugin/` is read
                // too and covers releases that looked only there (copilot-cli#2010).
                (plugin.join("plugin.json"), manifest.clone()),
                (plugin.join(".claude-plugin").join("plugin.json"), manifest),
                (plugin.join("hooks.json"), serde_json::to_string_pretty(&hooks).ok()?),
            ],
            env: vec![(PLUGIN_ENV.to_string(), plugin.display().to_string())],
            args: vec![("--plugin-dir", PLUGIN_ENV.to_string())],
        })
    }

    fn normalize_hook(&self, args: &[String], stdin: &[u8]) -> HookOutcome {
        let v: serde_json::Value = serde_json::from_slice(stdin).unwrap_or(serde_json::Value::Null);
        let event = args
            .first()
            .map(String::as_str)
            .or_else(|| field(&v, &["hook_event_name", "hookEventName"]))
            .unwrap_or("");
        let session = field(&v, &["session_id", "sessionId"])
            .filter(|s| plausible_session_id(s))
            .map(str::to_string);
        let transcript = field(&v, &["transcript_path", "transcriptPath"]).map(str::to_string);
        let bound = |session: &Option<String>| {
            session.clone().map(|id| AgentEvent::SessionBound { id, transcript: transcript.clone() })
        };
        let events = match event {
            "SessionStart" | "sessionStart" => bound(&session).into_iter().collect(),
            "UserPromptSubmit" | "userPromptSubmitted" => vec![AgentEvent::TurnStarted],
            // Stop is where the transcript path is reported, so it rebinds with it.
            "Stop" | "agentStop" => bound(&session).into_iter().chain([AgentEvent::TurnEnded]).collect(),
            "SessionEnd" | "sessionEnd" => vec![AgentEvent::SessionEnded],
            "notification" | "Notification" => match field(&v, &["notification_type", "notificationType"]) {
                // Only when a prompt is actually shown (1.0.26); the others describe
                // background work or idleness, which the screen and Stop cover.
                Some("permission_prompt") => vec![AgentEvent::NeedsAttention],
                _ => vec![],
            },
            _ => vec![],
        };
        HookOutcome { session, events }
    }

    fn multi_session(&self) -> bool {
        true
    }

    fn screen(&self) -> &'static ScreenProfile {
        &PROFILE
    }

    fn signals(&self) -> &'static SignalProfile {
        &SIGNALS
    }

    fn resume_local(&self, session: Option<&str>) -> String {
        match session.filter(|s| plausible_session_id(s)) {
            Some(id) => format!("copilot --resume={id}"),
            None => "copilot".to_string(),
        }
    }

    fn resume_remote(&self, session: Option<&str>) -> String {
        match session.filter(|s| plausible_session_id(s)) {
            Some(id) => format!("copilot --resume={id} || copilot --continue || copilot"),
            None => "copilot --continue || copilot".to_string(),
        }
    }

    fn classify_launch(&self, cmd: &str) -> TypedLaunch {
        let mut words = cmd.split_whitespace();
        let Some(first) = words.next() else { return TypedLaunch::NotThis };
        if first.rsplit('/').next().unwrap_or(first) != "copilot" {
            return TypedLaunch::NotThis;
        }
        let mut named = None;
        let mut first_word = true;
        while let Some(word) = words.next() {
            let (flag, inline) = match word.split_once('=') {
                Some((f, v)) => (f, Some(v)),
                None => (word, None),
            };
            match flag {
                // `--resume` takes its value only inline; bare, it opens the picker.
                "--resume" | "-r" => match inline.filter(|v| plausible_session_id(v)) {
                    Some(id) => named = Some(id.to_string()),
                    None => return TypedLaunch::Other,
                },
                "--session-id" => match inline.or_else(|| words.next()).filter(|v| plausible_session_id(v)) {
                    Some(id) => named = Some(id.to_string()),
                    None => return TypedLaunch::Other,
                },
                "--continue" | "-p" | "--prompt" | "-h" | "--help" | "-v" | "--version" => return TypedLaunch::Other,
                w if first_word && SUBCOMMANDS.contains(&w) => return TypedLaunch::Other,
                _ => {}
            }
            first_word = false;
        }
        match named {
            Some(id) => TypedLaunch::Named(id),
            None => TypedLaunch::Bare,
        }
    }

    // Creates a session under that UUID, or resumes it if it exists (1.0.51).
    fn name_flag(&self) -> Option<&'static str> {
        Some("--session-id")
    }

    fn derived_transcript(&self, session: &str, _cwd: &str) -> Option<PathBuf> {
        Some(config_dir()?.join("session-state").join(session))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn detects_the_native_binary_and_the_npm_loader_only() {
        assert!(Copilot.matches_process("copilot", &[]));
        assert!(Copilot.matches_process("copilot.exe", &[]));
        assert!(Copilot.matches_process(
            "node.exe",
            &argv(&["node", "C:\\npm\\node_modules\\@github\\copilot\\npm-loader.js"])
        ));
        assert!(!Copilot.matches_process("copilot-language-server", &[]));
        assert!(!Copilot.matches_process("node", &argv(&["node", "/x/copilot-app/vite.js"])));
        assert!(!Copilot.matches_process("gh", &argv(&["gh", "copilot", "suggest"])));
    }

    // Payloads written from the documented field lists (UNVERIFIED against a real
    // Copilot): PascalCase registrations get Claude's snake_case, camelCase ones do not.
    #[test]
    fn hook_payloads_normalize_to_agent_events() {
        let h = |ev: &str, json: &str| Copilot.normalize_hook(&argv(&[ev]), json.as_bytes());
        let start = h("SessionStart", r#"{"hook_event_name":"SessionStart","session_id":"9d1c","source":"startup","cwd":"/x"}"#);
        assert_eq!(start.events, vec![AgentEvent::SessionBound { id: "9d1c".into(), transcript: None }]);
        let stop = h("Stop", r#"{"hook_event_name":"Stop","session_id":"9d1c","transcript_path":"/h/.copilot/session-state/9d1c/events.jsonl","stopReason":"end_turn"}"#);
        assert_eq!(
            stop.events,
            vec![
                AgentEvent::SessionBound { id: "9d1c".into(), transcript: Some("/h/.copilot/session-state/9d1c/events.jsonl".into()) },
                AgentEvent::TurnEnded
            ]
        );
        assert_eq!(h("UserPromptSubmit", r#"{"session_id":"9d1c","prompt":"hi"}"#).events, vec![AgentEvent::TurnStarted]);
        assert_eq!(
            h("notification", r#"{"sessionId":"9d1c","notification_type":"permission_prompt"}"#).events,
            vec![AgentEvent::NeedsAttention]
        );
        assert_eq!(h("notification", r#"{"sessionId":"9d1c"}"#).session.as_deref(), Some("9d1c"));
        for quiet in ["agent_idle", "shell_completed", "agent_completed", "elicitation_dialog"] {
            let json = format!(r#"{{"sessionId":"9d1c","notification_type":"{quiet}"}}"#);
            assert!(h("notification", &json).events.is_empty(), "{quiet}");
        }
        assert_eq!(h("SessionEnd", r#"{"session_id":"9d1c","reason":"user_exit"}"#).events, vec![AgentEvent::SessionEnded]);
    }

    #[test]
    fn classifies_typed_copilot_launches() {
        use TypedLaunch::*;
        let c = |s: &str| Copilot.classify_launch(s);
        assert_eq!(c("copilot"), Bare);
        assert_eq!(c("copilot --model gpt-5"), Bare);
        assert_eq!(c("copilot --resume=9d1c-44"), Named("9d1c-44".into()));
        assert_eq!(c("copilot --session-id 9d1c-44"), Named("9d1c-44".into()));
        assert_eq!(c("copilot --resume"), Other, "the picker");
        assert_eq!(c("copilot --continue"), Other);
        assert_eq!(c("copilot -p hello"), Other);
        assert_eq!(c("copilot plugin list"), Other);
        assert_eq!(c("claude"), NotThis);
    }

    #[test]
    fn resumes_by_id_and_never_by_recency_locally() {
        assert_eq!(Copilot.resume_local(Some("9d1c")), "copilot --resume=9d1c");
        assert_eq!(Copilot.resume_local(None), "copilot");
        assert_eq!(Copilot.resume_remote(Some("9d1c")), "copilot --resume=9d1c || copilot --continue || copilot");
        assert_eq!(Copilot.resume_remote(None), "copilot --continue || copilot");
    }

    #[test]
    fn the_plugin_registers_every_hook_with_its_name() {
        let dir = std::path::Path::new("/data/agent-shim/copilot");
        let spec = Copilot.launch(&LaunchCtx { arbiter_exe: std::path::Path::new("/opt/arbiter"), dir }).unwrap();
        let hooks = spec.files.iter().find(|(p, _)| p.ends_with("hooks.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&hooks.1).unwrap();
        assert_eq!(v["version"], 1);
        for ev in HOOK_EVENTS {
            assert_eq!(v["hooks"][ev][0]["exec"], "/opt/arbiter");
            assert_eq!(v["hooks"][ev][0]["args"], serde_json::json!(["agent-hook", "copilot", ev]));
        }
        assert_eq!(spec.args, vec![("--plugin-dir", PLUGIN_ENV.to_string())]);
    }
}
