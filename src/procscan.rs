//! Which programs are running under a pane's shell: an agent CLI (any registered one),
//! or an ssh/mosh client. Called on OSC-133 busy edges (not polled); a shared sysinfo
//! snapshot with a 250ms refresh gate keeps repeated scans across panes cheap.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::agent::AgentKind;

static SHARED_SYSTEM: OnceLock<SharedSystem> = OnceLock::new();

fn shared_system() -> &'static SharedSystem {
    SHARED_SYSTEM.get_or_init(SharedSystem::new)
}

struct SharedSystem {
    /// The snapshot and when it was last refreshed (None: never). Not "now minus a
    /// minute": an `Instant` counts from boot, and that subtraction panics in the first
    /// minute after it.
    sys: Mutex<(System, Option<Instant>)>,
}

impl SharedSystem {
    fn new() -> Self {
        Self { sys: Mutex::new((System::new(), None)) }
    }

    fn with<T>(&self, max_age: Duration, f: impl FnOnce(&System) -> T) -> T {
        let mut guard = self.sys.lock().unwrap();
        let (sys, last) = &mut *guard;
        if last.map_or(true, |at| at.elapsed() >= max_age) {
            sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::new().with_cmd(UpdateKind::Always),
            );
            *last = Some(Instant::now());
        }
        f(sys)
    }
}

/// Walk up the parent chain of `pid` to see if `ancestor` is found.
fn is_descendant(sys: &System, pid: u32, ancestor: u32) -> bool {
    depth_below(sys, pid, ancestor).is_some()
}

/// How many generations `pid` is below `ancestor`, if it is below it at all.
fn depth_below(sys: &System, pid: u32, ancestor: u32) -> Option<usize> {
    let mut cur = pid;
    for depth in 0..64 {
        if cur == ancestor {
            return Some(depth);
        }
        if cur == 0 {
            break;
        }
        match sys.process(sysinfo::Pid::from_u32(cur)).and_then(|p| p.parent()) {
            Some(p) => cur = p.as_u32(),
            None => break,
        }
    }
    None
}

/// Which agent a process is, from its name and argv.
fn agent_of(name: &str, cmd: &[String]) -> Option<AgentKind> {
    // Drop leaked environment variables. macOS reports a process-title-rewriting
    // program's argv from a region that spills into its environ, so e.g. npm
    // (`process.title = "npm run dev"`) surfaces Arbiter's OWN injected env here, whose
    // values name the agents' shims and settings files and falsely matched. Only genuine
    // argv should count.
    let argv: Vec<String> = cmd.iter().filter(|s| !looks_like_env_assignment(s)).cloned().collect();
    crate::agent::all().iter().find(|a| a.matches_process(name, &argv)).map(|a| a.kind())
}

/// Whether `s` is a `KEY=VALUE` environment assignment (KEY a valid env-var name) and
/// not a real argv element.
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

/// The agent running as a descendant of `shell_pid`, and its pid, if any: the one nearest
/// the shell, since an agent's own children can be agents too (Claude running
/// `codex mcp-server`). Agents switched off are not looked for.
pub fn agent_under(shell_pid: u32) -> Option<(AgentKind, u32)> {
    shared_system().with(Duration::from_millis(250), |sys| {
        let mut best: Option<(usize, AgentKind, u32)> = None;
        for (pid, proc) in sys.processes() {
            let cmd: Vec<String> = proc.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect();
            let Some(kind) = agent_of(&proc.name().to_string_lossy(), &cmd).filter(|k| crate::agent::enabled(*k))
            else {
                continue;
            };
            let Some(depth) = depth_below(sys, pid.as_u32(), shell_pid) else { continue };
            if best.map_or(true, |(d, _, _)| depth < d) {
                best = Some((depth, kind, pid.as_u32()));
            }
        }
        let (_, kind, pid) = best?;
        // Which process matched, and its parent chain, so a false positive names its
        // culprit. Guarded so the ancestry walk only runs when debugging.
        if crate::debug::debug_enabled() {
            crate::debug::debug_log(&format!(
                "agent_under: MATCH {kind:?} shell_pid={shell_pid} pid={pid} chain={}",
                ancestry_chain(sys, pid),
            ));
        }
        Some((kind, pid))
    })
}

/// The pid of an SSH (or mosh) client descended from `shell_pid`, if there is one, i.e.
/// this pane is driving a shell on another machine. Scanned on the same OSC-133 busy edge as
/// `agent_under`, off the same 250ms-gated snapshot, so it costs nothing extra.
///
/// A remote pane needs different treatment in two places: the agent runs on the far
/// host, so the process scan and the hooks can never see it (the pane is recognised from
/// the agent's on-screen chrome instead), and the local shell stays "busy" for the whole
/// session, so its busy dot would otherwise be stuck on.
pub fn ssh_under(shell_pid: u32) -> Option<u32> {
    shared_system().with(Duration::from_millis(250), |sys| {
        sys.processes()
            .iter()
            .find(|(pid, proc)| {
                is_ssh_name(&proc.name().to_string_lossy()) && is_descendant(sys, pid.as_u32(), shell_pid)
            })
            .map(|(pid, _)| pid.as_u32())
    })
}

/// Whether a process name is an SSH/mosh *client*. Matched on the whole basename so the
/// daemon and helpers (`sshd`, `ssh-agent`, `ssh-add`) can't count, since those run under
/// the user's session all the time and would mark every pane remote.
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
    use super::{agent_of, AgentKind};

    fn cmd(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn each_agent_is_told_from_the_others() {
        assert_eq!(agent_of("claude", &[]), Some(AgentKind::Claude));
        assert_eq!(agent_of("copilot.exe", &[]), Some(AgentKind::Copilot));
        assert_eq!(agent_of("codex", &[]), Some(AgentKind::Codex));
        assert_eq!(agent_of("node", &cmd(&["node", "/x/node_modules/@anthropic-ai/claude-code/cli.js"])), Some(AgentKind::Claude));
        assert_eq!(agent_of("pwsh.exe", &cmd(&["pwsh"])), None);
    }

    #[test]
    fn ignores_leaked_arbiter_env_vars() {
        // The real-world repro: macOS leaks Arbiter's injected env into cmd() for a
        // title-rewriting process (npm). None of these ARBITER_* assignments, whose
        // values point at the shims and the real binaries, may count as an agent.
        assert_eq!(
            agent_of(
                "node",
                &cmd(&[
                    "npm run dev",
                    "ARBITER_EVENTS_DIR=/Users/x/Library/Application Support/arbiter-native-debug/agent-events",
                    "ARBITER_CLAUDE_DEBUG=1",
                    "ARBITER_CLAUDE_SETTINGS=/Users/x/Library/Application Support/arbiter-native-debug/agent-shim/claude/settings.json",
                    "ARBITER_REAL_CLAUDE=/Users/x/.local/bin/claude",
                    "ARBITER_REAL_CODEX=/opt/homebrew/bin/codex",
                    "ARBITER_COPILOT_PLUGIN=/Users/x/agent-shim/copilot/plugin/@github/copilot/",
                ]),
            ),
            None
        );
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
