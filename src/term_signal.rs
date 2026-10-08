//! Terminal sequences a program uses to say something about itself: its window title,
//! the bell, progress (ConEmu's OSC 9;4, which Windows Terminal adopted) and desktop
//! notifications (OSC 9, OSC 777). Agents use these for working / needs-you states, and
//! unlike hooks they cross SSH, so they are the remote channel.
//!
//! Titles and the bell reach `VtTerm` as alacritty events; alacritty drops OSC 9 and 777,
//! so the reader's own OSC scan hands those payloads to `parse_osc`.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermSignal {
    Title(String),
    Bell,
    /// ConEmu's states: 0 cleared, 1 a percentage, 2 error, 3 indeterminate, 4 paused.
    Progress { state: u8, percent: Option<u8> },
    Notify { title: Option<String>, body: String },
}

impl TermSignal {
    /// Progress that says the program is busy: a percentage, or indeterminate.
    pub fn is_busy_progress(&self) -> bool {
        matches!(self, TermSignal::Progress { state: 1 | 3, .. })
    }
}

/// An OSC payload (between `ESC ]` and its terminator) that alacritty does not act on.
/// `None` for anything else, including ConEmu's other numbered `9;N` commands (9;9 is a
/// working directory some Windows prompts emit), which are not notifications.
pub fn parse_osc(payload: &str) -> Option<TermSignal> {
    if let Some(rest) = payload.strip_prefix("9;") {
        let head = rest.split(';').next().unwrap_or("");
        if !head.is_empty() && head.bytes().all(|b| b.is_ascii_digit()) {
            if head != "4" {
                return None;
            }
            let mut parts = rest.split(';').skip(1);
            let state = parts.next()?.trim().parse::<u8>().ok().filter(|s| *s <= 4)?;
            let percent = parts.next().and_then(|p| p.trim().parse::<u8>().ok()).map(|p| p.min(100));
            return Some(TermSignal::Progress { state, percent });
        }
        return Some(TermSignal::Notify { title: None, body: rest.to_string() });
    }
    if let Some(rest) = payload.strip_prefix("777;notify;") {
        let (title, body) = rest.split_once(';').unwrap_or(("", rest));
        return Some(TermSignal::Notify {
            title: (!title.is_empty()).then(|| title.to_string()),
            body: body.to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{parse_osc, TermSignal};

    #[test]
    fn notifications_and_progress_are_told_apart() {
        assert_eq!(parse_osc("9;Approval requested: cargo test"), Some(TermSignal::Notify { title: None, body: "Approval requested: cargo test".into() }));
        assert_eq!(parse_osc("9;4;1;40"), Some(TermSignal::Progress { state: 1, percent: Some(40) }));
        assert_eq!(parse_osc("9;4;3"), Some(TermSignal::Progress { state: 3, percent: None }));
        assert_eq!(parse_osc("9;4;0;0"), Some(TermSignal::Progress { state: 0, percent: Some(0) }));
        assert_eq!(parse_osc("9;4;1;250"), Some(TermSignal::Progress { state: 1, percent: Some(100) }));
        assert_eq!(parse_osc("9;4;7"), None, "not a ConEmu state");
        assert_eq!(parse_osc("9;4"), None);
        assert_eq!(parse_osc("777;notify;Codex;Turn complete"), Some(TermSignal::Notify { title: Some("Codex".into()), body: "Turn complete".into() }));
        assert_eq!(parse_osc("777;notify;;body"), Some(TermSignal::Notify { title: None, body: "body".into() }));
    }

    // Windows prompts emit ConEmu's 9;9 with the working directory on every prompt; read
    // as a notification, it would end an agent's turn each time.
    #[test]
    fn conemu_commands_other_than_progress_are_not_notifications() {
        assert_eq!(parse_osc("9;9;\"C:\\Users\\TRE\""), None);
        assert_eq!(parse_osc("9;1;500"), None);
        assert_eq!(parse_osc("9;12"), None);
        assert_eq!(parse_osc("7;file:///x"), None);
        assert_eq!(parse_osc("133;A"), None);
        assert_eq!(parse_osc("0;title"), None);
    }

    // Live probe: whether the platform's PTY hands these sequences through at all
    // (ConPTY rewrites much of what passes it). Needs `bash` (Git Bash on Windows).
    // `cargo test --lib term_signal::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_the_pty_forwards_notifications_and_progress() {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        use std::io::Read;
        let bash = crate::shell::detect_git_bash().unwrap_or_else(|| "bash".to_string());
        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .unwrap();
        let mut cmd = CommandBuilder::new(bash);
        cmd.args(["-c", r"printf '\033]9;4;1;40\007\033]9;hello\007\033]777;notify;T;B\007\033]0;title\007\007'; sleep 1"]);
        let _child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        // ConPTY gives no EOF when the child exits, so read on a thread and stop looking.
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        std::thread::sleep(std::time::Duration::from_secs(3));
        let out = String::from_utf8_lossy(&seen.lock().unwrap()).to_string();
        println!("{out:?}");
        for (name, seq) in [
            ("progress", "\x1b]9;4;1;40\x07"),
            ("osc 9", "\x1b]9;hello\x07"),
            ("osc 777", "\x1b]777;notify;T;B\x07"),
            ("title", "\x1b]0;title\x07"),
        ] {
            println!("{name}: {}", if out.contains(seq) { "forwarded" } else { "NOT forwarded" });
        }
        drop(pair.master);
    }

    #[test]
    fn busy_progress_is_percentage_or_indeterminate() {
        assert!(TermSignal::Progress { state: 1, percent: Some(3) }.is_busy_progress());
        assert!(TermSignal::Progress { state: 3, percent: None }.is_busy_progress());
        assert!(!TermSignal::Progress { state: 0, percent: None }.is_busy_progress());
        assert!(!TermSignal::Progress { state: 2, percent: None }.is_busy_progress());
        assert!(!TermSignal::Bell.is_busy_progress());
    }
}
