//! Rewriting a saved `ssh` line so the connection itself lands in the saved remote
//! directory: `ssh -t <args as saved> 'cd <dir> && exec $SHELL -l'`. Nothing has to be
//! typed after connecting, so nothing can stray into a credential prompt, and the pane
//! shows the one line that did it.
//!
//! ssh has no option for a starting directory: a login shell always starts in the home
//! directory, and the only handle on it is a remote command. `-t` keeps a terminal, the
//! `cd` runs under the login shell sshd hands the command to, and `exec $SHELL -l` then
//! replaces that with the interactive login shell the user would have had anyway. A
//! `cd` that fails ends the connection with its own exit status, which the session layer
//! reads as "that directory is gone" (see `session::reader_loop`).

/// ssh options that take a value, so the word after them is not the host (`ssh -h`).
const OPTS_WITH_VALUE: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// Characters a saved line may contain for it to be rewritten at all. Anything else
/// (quotes, `$`, `;`, `&`, `|`, parentheses, globs) is shell syntax whose meaning depends
/// on the local shell, so such a line is replayed exactly as saved instead.
fn is_plain_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            ' ' | '@' | '.' | ':' | '/' | '\\' | '-' | '_' | '=' | ',' | '+' | '~' | '[' | ']' | '%'
        )
}

/// A plain `ssh [options] host`: its words, and whether a tty was already requested.
struct SshLine<'a> {
    words: Vec<&'a str>,
    has_tty: bool,
}

/// `None` for anything that must not be rewritten: another client, quoting or shell
/// syntax, a remote command already present, an explicit `-T`, or no host at all.
fn parse_simple_ssh(cmd: &str) -> Option<SshLine<'_>> {
    if !cmd.chars().all(is_plain_char) {
        return None;
    }
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let (first, args) = words.split_first()?;
    let base = first.rsplit(['/', '\\']).next().unwrap_or(first).to_ascii_lowercase();
    if base.strip_suffix(".exe").unwrap_or(&base) != "ssh" {
        return None;
    }
    let mut has_tty = false;
    let mut host_seen = false;
    let mut i = 0;
    while i < args.len() {
        if host_seen {
            return None;
        }
        let word = args[i];
        i += 1;
        let Some(flags) = word.strip_prefix('-').filter(|f| !f.is_empty()) else {
            host_seen = true;
            continue;
        };
        let mut letters = flags.chars();
        while let Some(c) = letters.next() {
            match c {
                't' => has_tty = true,
                'T' => return None,
                c if OPTS_WITH_VALUE.contains(c) => {
                    // The value is the rest of this word (`-p2222`) or the next word.
                    if letters.as_str().is_empty() {
                        i += 1;
                    }
                    break;
                }
                _ => {}
            }
        }
    }
    host_seen.then_some(SshLine { words, has_tty })
}

/// `text` as one word for the far host's `sh`, escaped with backslashes. Quotes are not
/// available: a single quote would end the string the LOCAL shell is given, and
/// PowerShell 5.1 mangles embedded double quotes on their way to a native `ssh.exe`. A
/// `~` stays bare so the far shell expands a leading one. `None` if it cannot be
/// expressed.
fn sh_escape_word(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if c.is_control() || c == '\'' {
            return None;
        }
        let plain = c.is_ascii_alphanumeric()
            || !c.is_ascii()
            || matches!(c, '/' | '.' | '_' | '-' | '~' | ':' | '@' | '+' | ',' | '=' | '%');
        if !plain {
            out.push('\\');
        }
        out.push(c);
    }
    Some(out)
}

/// A remote directory as one `sh` word: absolute or `~`-relative, since anything else
/// would be resolved against a directory we cannot know.
fn sh_escape_dir(dir: &str) -> Option<String> {
    if !(dir.starts_with('/') || dir == "~" || dir.starts_with("~/")) {
        return None;
    }
    sh_escape_word(dir)
}

/// Whether `id` is a session id Claude could have issued or accepted: letters, digits and
/// hyphens, which also means it needs no escaping anywhere it is used.
pub fn plausible_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The command that brings Claude back on the far host with no conversation to name: the
/// directory's most recent one, else a fresh one.
pub const CLAUDE_COMMAND: &str = "claude -c || claude";

/// The command that brings Claude back on the far host: the pane's own conversation when
/// Arbiter knows its id (see `ClaudeHandle::remote_session`), else the directory's most
/// recent one, else a fresh one. The remote counterpart of the local
/// `claude --resume <id>` / `claude` rule; each step runs only if the previous could not.
pub fn claude_command(session: Option<&str>) -> String {
    match session.filter(|s| plausible_session_id(s)) {
        Some(id) => format!("claude --resume {id} || {CLAUDE_COMMAND}"),
        None => CLAUDE_COMMAND.to_string(),
    }
}

/// The line to type into the LOCAL shell to rebuild a remote pane: in `dir` when known,
/// with Claude relaunched there when `claude`, into `session` when known. `None` when the
/// saved line is not a plain `ssh` invocation, when `dir` cannot be expressed, or when
/// there is nothing to add (no directory but home, no Claude); the caller then replays
/// the line as saved and types what is needed at the far prompt instead.
///
/// Claude runs through `$SHELL -lic`, an interactive login shell, because the shell sshd
/// hands a remote command to reads no rc file and so typically has no `claude` on its
/// PATH; the rc files are where the native installer and Homebrew put it. Its command,
/// followed by the user's login shell in the same place, is passed as one
/// backslash-escaped word (see `sh_escape_word`), which is why it looks the way it does.
pub fn remote_launch_line(
    startup_cmd: &str,
    dir: Option<&str>,
    claude: bool,
    session: Option<&str>,
) -> Option<String> {
    let dir = dir.filter(|d| *d != "~");
    if dir.is_none() && !claude {
        return None;
    }
    let ssh = parse_simple_ssh(startup_cmd.trim())?;
    let cd = match dir {
        Some(d) => Some(sh_escape_dir(d)?),
        None => None,
    };
    let mut out = String::from(ssh.words[0]);
    if !ssh.has_tty {
        out.push_str(" -t");
    }
    for w in &ssh.words[1..] {
        out.push(' ');
        out.push_str(w);
    }
    out.push_str(" '");
    if let Some(cd) = cd {
        out.push_str("cd ");
        out.push_str(&cd);
        out.push_str(" && ");
    }
    if claude {
        let then_shell = format!("{}; exec $SHELL -l", claude_command(session));
        out.push_str("exec $SHELL -lic ");
        out.push_str(&sh_escape_word(&then_shell).expect("built from checked parts"));
    } else {
        out.push_str("exec $SHELL -l");
    }
    out.push('\'');
    Some(out)
}

#[cfg(test)]
mod tests {
    fn line(cmd: &str, dir: &str) -> Option<String> {
        super::remote_launch_line(cmd, Some(dir), false, None)
    }

    fn full(cmd: &str, dir: Option<&str>, claude: bool) -> Option<String> {
        super::remote_launch_line(cmd, dir, claude, None)
    }

    // The pane's own conversation first, then the directory's most recent, then a fresh
    // one; an id that could not be Claude's is ignored rather than escaped.
    #[test]
    fn claude_resumes_its_own_conversation_when_the_id_is_known() {
        use super::claude_command;
        assert_eq!(claude_command(None), "claude -c || claude");
        assert_eq!(
            claude_command(Some("3f2a9c1e-7b4d-4e8a-9f01-2c5d6e7f8a9b")),
            "claude --resume 3f2a9c1e-7b4d-4e8a-9f01-2c5d6e7f8a9b || claude -c || claude"
        );
        assert_eq!(claude_command(Some("")), "claude -c || claude");
        assert_eq!(claude_command(Some("x; rm -rf ~")), "claude -c || claude");
        assert_eq!(
            super::remote_launch_line("ssh mini", Some("~/src"), true, Some("abc-123")).as_deref(),
            Some("ssh -t mini 'cd ~/src && exec $SHELL -lic claude\\ --resume\\ abc-123\\ \\|\\|\\ claude\\ -c\\ \\|\\|\\ claude\\;\\ exec\\ \\$SHELL\\ -l'")
        );
    }

    #[test]
    fn a_plain_ssh_line_carries_the_directory() {
        assert_eq!(
            line("ssh mini", "/home/tre/src").as_deref(),
            Some("ssh -t mini 'cd /home/tre/src && exec $SHELL -l'")
        );
        assert_eq!(
            line("ssh tre@10.0.0.16", "~/Source/dev-webapp").as_deref(),
            Some("ssh -t tre@10.0.0.16 'cd ~/Source/dev-webapp && exec $SHELL -l'")
        );
        assert_eq!(line("  ssh mini  ", "/x").as_deref(), Some("ssh -t mini 'cd /x && exec $SHELL -l'"));
    }

    // Claude rides in the line too, inside an interactive login shell so it is on PATH,
    // as one backslash-escaped word. With no directory to go to, Claude alone; with
    // neither, nothing to add.
    #[test]
    fn claude_rides_in_the_line() {
        assert_eq!(
            full("ssh mini", Some("~/src"), true).as_deref(),
            Some("ssh -t mini 'cd ~/src && exec $SHELL -lic claude\\ -c\\ \\|\\|\\ claude\\;\\ exec\\ \\$SHELL\\ -l'")
        );
        assert_eq!(
            full("ssh mini", Some("~"), true).as_deref(),
            Some("ssh -t mini 'exec $SHELL -lic claude\\ -c\\ \\|\\|\\ claude\\;\\ exec\\ \\$SHELL\\ -l'")
        );
        assert_eq!(
            full("ssh mini", None, true).as_deref(),
            Some("ssh -t mini 'exec $SHELL -lic claude\\ -c\\ \\|\\|\\ claude\\;\\ exec\\ \\$SHELL\\ -l'")
        );
        assert!(full("ssh mini", Some("~"), false).is_none(), "home is where a login lands anyway");
        assert!(full("ssh mini", None, false).is_none());
        assert!(full("mosh mini", Some("/x"), true).is_none());
    }

    // Options with values are told from the host, and `-t` lands right after the client.
    #[test]
    fn options_are_kept_and_the_host_is_found_past_them() {
        for args in ["-p 2222 mini", "-p2222 mini", "-i ~/.ssh/key mini", "-J jump mini", "-l tre mini", "-o StrictHostKeyChecking=no mini", "-4 -C mini"] {
            let got = line(&format!("ssh {args}"), "/x").unwrap();
            assert_eq!(got, format!("ssh -t {args} 'cd /x && exec $SHELL -l'"), "{args}");
        }
    }

    #[test]
    fn a_tty_already_requested_is_not_requested_twice() {
        assert_eq!(line("ssh -t mini", "/x").as_deref(), Some("ssh -t mini 'cd /x && exec $SHELL -l'"));
        assert_eq!(line("ssh -tt mini", "/x").as_deref(), Some("ssh -tt mini 'cd /x && exec $SHELL -l'"));
        assert_eq!(line("ssh -At mini", "/x").as_deref(), Some("ssh -At mini 'cd /x && exec $SHELL -l'"));
    }

    // Lines that mean more than "connect to this host" are left exactly as saved.
    #[test]
    fn anything_but_a_plain_connection_is_left_alone() {
        assert!(line("ssh -T mini", "/x").is_none(), "no tty wanted");
        assert!(line("ssh mini uptime", "/x").is_none(), "has its own remote command");
        assert!(line("ssh mini 'cd /y'", "/x").is_none(), "quoting");
        assert!(line("ssh mini && echo hi", "/x").is_none(), "shell syntax");
        assert!(line("ssh $HOST", "/x").is_none());
        assert!(line("ssh", "/x").is_none(), "no host");
        assert!(line("mosh mini", "/x").is_none());
        assert!(line("plink mini", "/x").is_none());
        assert!(line("", "/x").is_none());
    }

    #[test]
    fn a_path_qualified_client_is_rewritten_by_its_name() {
        assert_eq!(line("/usr/bin/ssh mini", "/x").as_deref(), Some("/usr/bin/ssh -t mini 'cd /x && exec $SHELL -l'"));
        assert_eq!(
            line("C:\\Windows\\System32\\OpenSSH\\ssh.exe mini", "/x").as_deref(),
            Some("C:\\Windows\\System32\\OpenSSH\\ssh.exe -t mini 'cd /x && exec $SHELL -l'")
        );
    }

    // The directory travels inside single quotes for the local shell, so it is escaped
    // with backslashes for the far one, and a quote in it cannot be expressed at all.
    #[test]
    fn directories_are_escaped_or_refused() {
        assert_eq!(line("ssh mini", "/home/tre/my dir").as_deref(), Some("ssh -t mini 'cd /home/tre/my\\ dir && exec $SHELL -l'"));
        assert_eq!(line("ssh mini", "/a$b*c").as_deref(), Some("ssh -t mini 'cd /a\\$b\\*c && exec $SHELL -l'"));
        assert_eq!(line("ssh mini", "/h\u{e9}").as_deref(), Some("ssh -t mini 'cd /h\u{e9} && exec $SHELL -l'"));
        assert!(line("ssh mini", "/it's").is_none());
        assert!(line("ssh mini", "/a\nb").is_none());
        assert!(line("ssh mini", "relative/x").is_none());
        assert!(line("ssh mini", "~tre/x").is_none(), "another user's home is not expanded reliably");
        assert!(line("ssh mini", "~").is_none(), "home is where a login lands anyway");
    }
}
