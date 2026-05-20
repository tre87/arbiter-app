use std::ffi::OsStr;
use std::process::Command;

/// Returns a `Command` configured to never allocate a console window on
/// Windows. In a GUI release build the parent has no console, so child
/// console apps (`git`, `powershell`, etc.) flash a new window each spawn
/// without this flag — visibly slow and ugly. Use this instead of
/// `Command::new` for every direct child-process spawn.
#[cfg(windows)]
pub fn hidden_command<S: AsRef<OsStr>>(program: S) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(not(windows))]
pub fn hidden_command<S: AsRef<OsStr>>(program: S) -> Command {
    Command::new(program)
}
