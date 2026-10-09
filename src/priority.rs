//! Keeping the terminals live while another program saturates every core (Windows).
//!
//! With a CPU-bound program on every core the window stayed responsive while the
//! terminals froze. Measured 2026-10-09 with 32 busy threads on 16 logical cores, typing
//! into a Claude pane through `Session`: the redraw wake, which passes through a timer
//! (the output settle, `session::WakeHold`), stalled for 3.5 s and longer, and keystroke
//! echo, which also passes through the pane's console host and agent, reached a p90 of
//! 278 ms. The hops that wake for I/O stayed under a second. The likely cause is that
//! Windows boosts a thread woken for input or I/O ahead of busy threads of its priority
//! more readily than one woken by a timer, and stalls of a few seconds fit the
//! scheduler's anti-starvation boost; the boost rules themselves were not measured.
//!
//! So Arbiter and each pane's own processes run one class above normal: with all of them
//! raised the same test gave 26 ms and 4.5 ms, and raising Arbiter alone fixed the redraws
//! but not the echo. A raised class is not inherited (only idle and below-normal are), so
//! whatever a pane runs, a build, or a sim its agent starts, stays at normal. That is what
//! keeps this from just moving the starvation onto the user's work.

use crate::agent::AgentKind;

/// Arbiter itself: the UI, the PTY readers and writers, the redraw timer, the executor.
pub fn raise_self() {
    #[cfg(windows)]
    imp::raise_self();
}

/// What a pane starts with: its shell and the console host behind it.
pub fn raise_pane(shell_pid: u32) {
    #[cfg(windows)]
    imp::raise_pane(shell_pid);
    #[cfg(not(windows))]
    let _ = shell_pid;
}

/// The shell a launcher started, called at the pane's first prompt, by when it is up: Git
/// Bash's `bin\bash.exe` runs the real `usr\bin\bash.exe` as a child, and a raised
/// launcher does not pass its class on.
pub fn raise_launched_shell(shell_pid: u32) {
    #[cfg(windows)]
    imp::raise_launched_shell(shell_pid);
    #[cfg(not(windows))]
    let _ = shell_pid;
}

/// The `kind` agent under a pane's shell, when its hook reports a session. The busy-edge
/// scan raises an agent it finds, but stops looking ten seconds into a command
/// (`SCAN_WINDOW` in `session::agent_monitor`), so one started later, or again within the
/// same command, is only seen here.
pub fn raise_agent_under(shell_pid: u32, kind: AgentKind) {
    #[cfg(windows)]
    if let Some((found, pid)) = crate::procscan::agent_under(shell_pid) {
        if found == kind {
            imp::raise(pid);
        }
    }
    #[cfg(not(windows))]
    let _ = (shell_pid, kind);
}

/// A program found running in a pane that the user types into: its agent, an ssh client.
pub fn raise(pid: u32) {
    #[cfg(windows)]
    imp::raise(pid);
    #[cfg(not(windows))]
    let _ = pid;
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS, PROCESS_SET_INFORMATION,
    };

    pub(super) struct Proc {
        pub pid: u32,
        pub parent: u32,
        pub name: String,
    }

    pub fn raise_self() {
        unsafe {
            let _ = SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS);
        }
    }

    pub fn raise(pid: u32) {
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_SET_INFORMATION, false, pid) else { return };
            let _ = SetPriorityClass(h, ABOVE_NORMAL_PRIORITY_CLASS);
            let _ = CloseHandle(h);
        }
    }

    pub fn raise_pane(shell_pid: u32) {
        raise(shell_pid);
        let me = std::process::id();
        // Every console host this process started, not just this pane's: none can be
        // told apart, and raising one twice is harmless.
        for p in snapshot().iter().filter(|p| p.parent == me && is_console_host(&p.name)) {
            raise(p.pid);
        }
    }

    /// A child of the shell running the shell's own executable is the shell it launched.
    pub fn raise_launched_shell(shell_pid: u32) {
        let procs = snapshot();
        let Some(shell) = procs.iter().find(|p| p.pid == shell_pid) else { return };
        for p in procs.iter().filter(|p| p.parent == shell_pid && p.name.eq_ignore_ascii_case(&shell.name)) {
            raise(p.pid);
        }
    }

    /// ConPTY's host: the system's, or the one a sideloaded `conpty.dll` brings.
    pub(super) fn is_console_host(name: &str) -> bool {
        name.eq_ignore_ascii_case("conhost.exe") || name.eq_ignore_ascii_case("OpenConsole.exe")
    }

    pub(super) fn snapshot() -> Vec<Proc> {
        let mut out = Vec::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
            let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut more = Process32FirstW(snap, &mut e).is_ok();
            while more {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                out.push(Proc {
                    pid: e.th32ProcessID,
                    parent: e.th32ParentProcessID,
                    name: String::from_utf16_lossy(&e.szExeFile[..len]),
                });
                more = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
        }
        out
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::io::{Read, Write};
    use std::time::Duration;

    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetPriorityClass, OpenProcess, ABOVE_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::imp::{is_console_host, snapshot};

    fn class(pid: u32) -> u32 {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).expect("open process");
            let c = GetPriorityClass(h);
            let _ = CloseHandle(h);
            c
        }
    }

    fn child_named(parent: u32, name: &str) -> super::imp::Proc {
        (0..100)
            .find_map(|_| {
                std::thread::sleep(Duration::from_millis(100));
                snapshot().into_iter().find(|p| p.parent == parent && p.name.eq_ignore_ascii_case(name))
            })
            .unwrap_or_else(|| panic!("{name} started under {parent}"))
    }

    // The pane's shell and console host move ahead of a CPU-bound program, and so does the
    // shell a launcher starts (a nested cmd.exe stands in for Git Bash's usr\bin\bash.exe);
    // a command run in that shell, which is where a build or a sim lives, does not.
    #[test]
    fn a_pane_runs_above_normal_and_what_it_starts_does_not() {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .unwrap();
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.args(["/q", "/k"]);
        let mut child = pair.slave.spawn_command(cmd).unwrap();
        let shell = child.process_id().unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
        });

        super::raise_pane(shell);
        assert_eq!(class(shell), ABOVE_NORMAL_PRIORITY_CLASS.0);
        let me = std::process::id();
        let host = snapshot().into_iter().find(|p| p.parent == me && is_console_host(&p.name)).expect("a console host");
        assert_eq!(class(host.pid), ABOVE_NORMAL_PRIORITY_CLASS.0);

        let mut w = pair.master.take_writer().unwrap();
        w.write_all(b"cmd.exe /q /k\r\n").unwrap();
        let launched = child_named(shell, "cmd.exe");
        super::raise_launched_shell(shell);
        assert_eq!(class(launched.pid), ABOVE_NORMAL_PRIORITY_CLASS.0);

        w.write_all(b"ping -n 5 127.0.0.1\r\n").unwrap();
        let ping = child_named(launched.pid, "PING.EXE");
        assert_eq!(class(ping.pid), NORMAL_PRIORITY_CLASS.0);
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &launched.pid.to_string(), "/T", "/F"])
            .output();
        let _ = child.kill();
    }

    // The stand-in agent of `an_agent_its_hook_binds_is_raised`, run only by that test from
    // a copy of this binary named like Claude.
    #[test]
    #[ignore]
    fn stand_in_agent() {
        std::thread::sleep(Duration::from_secs(15));
    }

    // An agent is raised when its hook reports a session, not only when the busy-edge
    // scan sees it start: found under the pane's shell, through the same event path.
    #[test]
    fn an_agent_its_hook_binds_is_raised() {
        use crate::agent::{AgentEvent, AgentKind};
        use crate::agent_state::AgentHandle;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};

        let exe = std::env::temp_dir().join(format!("claude-probe-{}.exe", std::process::id()));
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .unwrap();
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.args(["/q", "/k"]);
        let mut child = pair.slave.spawn_command(cmd).unwrap();
        let shell = child.process_id().unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
        });
        let mut w = pair.master.take_writer().unwrap();
        let line = format!("\"{}\" priority::tests::stand_in_agent --exact --ignored\r\n", exe.display());
        w.write_all(line.as_bytes()).unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().into_owned();
        let agent = child_named(shell, &name);
        assert_eq!(class(agent.pid), NORMAL_PRIORITY_CLASS.0);

        let h = AgentHandle::new(1, Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
        let _ = h.shell_pid.set(shell);
        let bound = AgentEvent::SessionBound { id: "s1".into(), transcript: None };
        crate::agent_watch::apply_lines(&h, &crate::agent_shim::encode_event(AgentKind::Claude, Some("s1"), &bound, 1));
        assert_eq!(class(agent.pid), ABOVE_NORMAL_PRIORITY_CLASS.0);

        let _ = std::process::Command::new("taskkill").args(["/PID", &agent.pid.to_string(), "/T", "/F"]).output();
        let _ = child.kill();
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::fs::remove_file(&exe);
    }
}
