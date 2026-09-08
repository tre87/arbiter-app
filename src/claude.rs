//! Claude process detection — is a `claude` process running as a descendant of
//! a pane's shell right now. Ported from `src-tauri/src/claude.rs` (the
//! process-tree scan), Tauri-free. Called on OSC-133 busy edges (not polled);
//! a shared sysinfo snapshot with a 250ms refresh gate keeps repeated scans
//! across panes cheap.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

static SHARED_SYSTEM: OnceLock<SharedSystem> = OnceLock::new();

fn shared_system() -> &'static SharedSystem {
    SHARED_SYSTEM.get_or_init(SharedSystem::new)
}

struct SharedSystem {
    sys: Mutex<(System, Instant)>,
}

impl SharedSystem {
    fn new() -> Self {
        Self {
            sys: Mutex::new((System::new(), Instant::now() - Duration::from_secs(60))),
        }
    }

    fn with<T>(&self, max_age: Duration, f: impl FnOnce(&System) -> T) -> T {
        let mut guard = self.sys.lock().unwrap();
        let (sys, last) = &mut *guard;
        if last.elapsed() >= max_age {
            sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::new().with_cmd(UpdateKind::Always),
            );
            *last = Instant::now();
        }
        f(sys)
    }
}

/// Walk up the parent chain of `pid` to see if `ancestor` is found.
fn is_descendant(sys: &System, pid: u32, ancestor: u32) -> bool {
    let mut cur = pid;
    loop {
        if cur == ancestor {
            return true;
        }
        if cur == 0 {
            break;
        }
        match sys.process(sysinfo::Pid::from_u32(cur)).and_then(|p| p.parent()) {
            Some(p) => cur = p.as_u32(),
            None => break,
        }
    }
    false
}

fn is_claude_process(proc: &sysinfo::Process) -> bool {
    let cmd: Vec<String> = proc.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect();
    is_claude_cmdline(&proc.name().to_string_lossy(), &cmd)
}

/// Whether a process (its `name` + argv `cmd`) is the Claude Code CLI. Split out so
/// it's unit-testable without a real `sysinfo::Process`.
fn is_claude_cmdline(name: &str, cmd: &[String]) -> bool {
    let name = name.to_lowercase();
    if name.starts_with("claude") {
        return true;
    }
    // Otherwise it must be `node` actually running the claude-code CLI. Matching ANY
    // arg that merely *contains* "claude" was too loose: `npm run dev` in a project
    // whose path contains "claude" (e.g. ~/.../claude-app/node_modules/vite/…) put
    // that dir in argv and false-detected Claude — the pane bound to Claude and
    // showed an idle dot. Require the claude-code package, or a bin/script whose
    // basename starts with "claude" (the `claude` launcher, however it's installed).
    if !name.contains("node") {
        return false;
    }
    cmd.iter()
        // Drop leaked environment variables. macOS reports a process-title-rewriting
        // program's argv from a region that spills into its environ, so e.g. npm
        // (`process.title = "npm run dev"`) surfaces Arbiter's OWN injected env here —
        // ARBITER_CAPTURE_DIR=…/claude-sessions, ARBITER_CLAUDE_SETTINGS=…/claude-shim/…,
        // ARBITER_REAL_CLAUDE=…/claude — every value of which contains "claude" and
        // falsely matched. Only genuine argv should count.
        .filter(|s| !looks_like_env_assignment(s))
        .any(|s| {
            let lower = s.to_lowercase();
            // The package DIR (followed by a separator, so `claude-code-tutorial/` etc.
            // doesn't match), or a bin/script whose basename starts with "claude".
            lower.contains("claude-code/")
                || lower.contains("claude-code\\")
                || lower
                    .rsplit(|c| c == '/' || c == '\\')
                    .next()
                    .map_or(false, |base| base.starts_with("claude"))
        })
}

/// Whether `s` is a `KEY=VALUE` environment assignment (KEY a valid env-var name) —
/// not a real argv element. Used to drop env vars that macOS leaks into `cmd()` for
/// processes that rewrite their title (npm, and node tools that call setproctitle).
fn looks_like_env_assignment(s: &str) -> bool {
    match s.split_once('=') {
        Some((key, _)) => {
            !key.is_empty()
                && !key.as_bytes()[0].is_ascii_digit()
                && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        }
        None => false,
    }
}

/// True if a `claude` process is a descendant of `shell_pid`.
pub fn running_under(shell_pid: u32) -> bool {
    shared_system().with(Duration::from_millis(250), |sys| {
        for (pid, proc) in sys.processes() {
            if is_claude_process(proc) && is_descendant(sys, pid.as_u32(), shell_pid) {
                // Diagnostic (ARBITER_CLAUDE_DEBUG): exactly which process matched, and
                // its parent chain — so a false positive names its culprit. Guarded so
                // the ancestry walk only runs when the debug flag is on.
                if std::env::var_os("ARBITER_CLAUDE_DEBUG").is_some() {
                    crate::claude_shim::debug_log(&format!(
                        "running_under: MATCH shell_pid={shell_pid} pid={} name={:?} cmd={:?} chain={}",
                        pid.as_u32(),
                        proc.name().to_string_lossy(),
                        proc.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect::<Vec<_>>(),
                        ancestry_chain(sys, pid.as_u32()),
                    ));
                }
                return true;
            }
        }
        false
    })
}

/// True if an SSH (or mosh) client is a descendant of `shell_pid`, i.e. this pane
/// is driving a shell on another machine. Scanned on the same OSC-133 busy edge as
/// `running_under`, off the same 250ms-gated snapshot, so it costs nothing extra.
///
/// A remote pane needs different treatment in two places: Claude runs on the far
/// host, so the process scan and statusLine capture can never see it (the pane is
/// recognised from Claude's on-screen chrome instead), and the local shell stays
/// "busy" for the whole session, so its busy dot would otherwise be stuck on.
pub fn ssh_under(shell_pid: u32) -> bool {
    shared_system().with(Duration::from_millis(250), |sys| {
        for (pid, proc) in sys.processes() {
            if is_ssh_name(&proc.name().to_string_lossy()) && is_descendant(sys, pid.as_u32(), shell_pid)
            {
                return true;
            }
        }
        false
    })
}

/// Whether a process name is an SSH/mosh *client*. Matched on the whole basename so
/// the daemon and helpers (`sshd`, `ssh-agent`, `ssh-add`) can't count, since those
/// run under the user's session all the time and would mark every pane remote.
fn is_ssh_name(name: &str) -> bool {
    let base = name.to_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    matches!(base, "ssh" | "mosh" | "mosh-client" | "plink")
}

/// Whether a typed command line invokes a remote client, judged from its first word.
///
/// This is the gate on what may be persisted as a pane's startup command, and it is
/// deliberately a whitelist rather than "whatever was typed last". Everything else
/// entered while connecting goes through the same keystroke tracking, including **key
/// passphrases**, so anything but a recognised client must be refused outright: a
/// timing rule alone could not prevent a secret reaching `session.json`.
///
/// The cost is that connecting through a wrapper script is not remembered, and such a
/// pane restores to a plain local shell.
pub fn looks_like_remote_cmd(cmd: &str) -> bool {
    let Some(word) = cmd.split_whitespace().next() else { return false };
    // Accept a path-qualified client (`/usr/bin/ssh`, `C:\...\ssh.exe`) by its basename.
    let base = word.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(word);
    is_ssh_name(base)
}

/// A readable `pid(name)→parent(name)→…` chain for diagnostics.
fn ancestry_chain(sys: &System, mut pid: u32) -> String {
    let mut out = Vec::new();
    for _ in 0..32 {
        let p = sys.process(sysinfo::Pid::from_u32(pid));
        let name = p.map(|p| p.name().to_string_lossy().into_owned()).unwrap_or_default();
        out.push(format!("{pid}({name})"));
        match p.and_then(|p| p.parent()) {
            Some(par) if par.as_u32() != pid => pid = par.as_u32(),
            _ => break,
        }
    }
    out.join("→")
}

#[cfg(test)]
mod tests {
    use super::is_claude_cmdline;

    fn cmd(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detects_real_claude() {
        // A process literally named claude (compiled launcher / set title).
        assert!(is_claude_cmdline("claude", &cmd(&[])));
        // node running the `claude` bin (basename), however it's installed.
        assert!(is_claude_cmdline("node", &cmd(&["node", "/Users/x/.local/bin/claude", "--resume"])));
        assert!(is_claude_cmdline("node", &cmd(&["node", "/x/node_modules/.bin/claude"])));
        // node running the package's cli.js directly (basename is cli.js, but the
        // claude-code package dir is in the path).
        assert!(is_claude_cmdline(
            "node",
            &cmd(&["node", "/x/node_modules/@anthropic-ai/claude-code/cli.js"]),
        ));
        // Windows-style separators.
        assert!(is_claude_cmdline(
            "node.exe",
            &cmd(&["node.exe", "C:\\x\\node_modules\\@anthropic-ai\\claude-code\\cli.js"]),
        ));
    }

    #[test]
    fn ignores_dev_server_in_claude_named_dir() {
        // The reported bug: `npm run dev` in a project whose path contains "claude".
        assert!(!is_claude_cmdline(
            "node",
            &cmd(&["node", "/Users/x/claude-app/node_modules/vite/bin/vite.js"]),
        ));
        assert!(!is_claude_cmdline(
            "node",
            &cmd(&["node", "/Users/x/my-claude-ui/node_modules/.bin/next", "dev"]),
        ));
        // npm itself, launched from a claude-named dir.
        assert!(!is_claude_cmdline(
            "node",
            &cmd(&["node", "/Users/x/claude-app/node_modules/npm/bin/npm-cli.js", "run", "dev"]),
        ));
        // A dir merely PREFIXED with "claude-code" must not match the package check.
        assert!(!is_claude_cmdline(
            "node",
            &cmd(&["node", "/Users/x/claude-code-tutorial/node_modules/vite/bin/vite.js"]),
        ));
    }

    #[test]
    fn ignores_leaked_arbiter_env_vars() {
        // The real-world repro: macOS leaks Arbiter's injected env into cmd() for a
        // title-rewriting process (npm). None of these ARBITER_* assignments — whose
        // values point at claude-shim / claude-sessions / the real claude bin — may
        // count as the Claude CLI.
        assert!(!is_claude_cmdline(
            "node",
            &cmd(&[
                "npm run dev",
                "ARBITER_CAPTURE_DIR=/Users/x/Library/Application Support/arbiter-native-debug/claude-sessions",
                "ARBITER_CLAUDE_DEBUG=1",
                "ARBITER_CLAUDE_SETTINGS=/Users/x/Library/Application Support/arbiter-native-debug/claude-shim/settings.json",
                "ARBITER_REAL_CLAUDE=/Users/x/.local/bin/claude",
            ]),
        ));
    }

    #[test]
    fn ignores_non_node_non_claude() {
        assert!(!is_claude_cmdline("bash", &cmd(&["bash", "-lc", "echo claude"])));
        assert!(!is_claude_cmdline("vite", &cmd(&["vite"])));
    }

    // The gate on what may be written to session.json. Everything typed while
    // connecting passes through the same keystroke tracking, so this is what keeps a
    // key passphrase, or whatever was typed at the remote prompt, off disk.
    #[test]
    fn only_a_remote_client_invocation_is_accepted_as_a_startup_command() {
        use super::looks_like_remote_cmd;
        assert!(looks_like_remote_cmd("ssh mini"));
        assert!(looks_like_remote_cmd("ssh tre@10.0.0.16"));
        assert!(looks_like_remote_cmd("ssh -A -J jump host"));
        assert!(looks_like_remote_cmd("mosh mini"));
        assert!(looks_like_remote_cmd("  ssh mini  "));
        // Path-qualified clients still count, by basename.
        assert!(looks_like_remote_cmd("/usr/bin/ssh mini"));
        assert!(looks_like_remote_cmd(r"C:\Windows\System32\OpenSSH\ssh.exe mini"));

        // A key passphrase typed at ssh's prompt. This is the case that matters: it
        // must never be mistaken for the command that built the pane.
        assert!(!looks_like_remote_cmd("correct horse battery staple"));
        assert!(!looks_like_remote_cmd("hunter2"));
        // Whatever was typed at the remote prompt afterwards.
        assert!(!looks_like_remote_cmd("claude"));
        assert!(!looks_like_remote_cmd("cd ~/src && claude --resume abc"));
        // ssh only as a later word, or as an unrelated command.
        assert!(!looks_like_remote_cmd("echo ssh mini"));
        assert!(!looks_like_remote_cmd("sshd -t"));
        assert!(!looks_like_remote_cmd("ssh-add ~/.ssh/id_ed25519"));
        assert!(!looks_like_remote_cmd(""));
        assert!(!looks_like_remote_cmd("   "));
    }

    #[test]
    fn detects_ssh_clients_but_not_the_daemon_or_helpers() {
        use super::is_ssh_name;
        for n in ["ssh", "ssh.exe", "SSH.EXE", "mosh", "mosh-client", "plink.exe"] {
            assert!(is_ssh_name(n), "{n} should count as a remote client");
        }
        // These run in a normal desktop session all the time; matching any of them
        // would mark every pane remote.
        for n in ["sshd", "ssh-agent", "ssh-add.exe", "sshfs", "node", "pwsh.exe"] {
            assert!(!is_ssh_name(n), "{n} must not count as a remote client");
        }
    }
}
