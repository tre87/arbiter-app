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
    let name = proc.name().to_string_lossy().to_lowercase();
    if name.starts_with("claude") {
        return true;
    }
    if !name.contains("node") {
        return false;
    }
    proc.cmd().iter().any(|s| s.to_string_lossy().to_lowercase().contains("claude"))
}

/// True if a `claude` process is a descendant of `shell_pid`.
pub fn running_under(shell_pid: u32) -> bool {
    shared_system().with(Duration::from_millis(250), |sys| {
        sys.processes().iter().any(|(pid, proc)| {
            is_claude_process(proc) && is_descendant(sys, pid.as_u32(), shell_pid)
        })
    })
}
