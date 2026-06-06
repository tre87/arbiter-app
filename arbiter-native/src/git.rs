//! Minimal git info for the footer: branch + staged/unstaged/untracked counts
//! from a single `git status --porcelain=v1 --branch`. Tauri-free; computed off
//! the cwd the Session tracks (OSC-7), cached per session.

use std::process::Command;

#[derive(Clone, Debug, Default)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
}

/// The repository top-level for `cwd` (to know what directory tree to watch).
pub fn repo_root(cwd: &str) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(["rev-parse", "--show-toplevel"]).current_dir(cwd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(root)
    }
}

/// Run git in `cwd`. Returns None if not a repo / git missing.
pub fn repo_info(cwd: &str) -> Option<GitInfo> {
    let mut cmd = Command::new("git");
    cmd.args(["status", "--porcelain=v1", "--branch"]).current_dir(cwd);
    // Don't flash a console window on Windows.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut info = GitInfo::default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            // "## branch...upstream [ahead N, behind M]" — branch is before "...".
            let branch = rest.split("...").next().unwrap_or(rest);
            let branch = branch.split_whitespace().next().unwrap_or(branch);
            if !branch.is_empty() && !branch.starts_with("No commits") {
                info.branch = Some(branch.to_string());
            }
        } else {
            let b = line.as_bytes();
            if b.len() < 2 {
                continue;
            }
            let (x, y) = (b[0], b[1]);
            if x == b'?' && y == b'?' {
                info.untracked += 1;
            } else {
                if x != b' ' {
                    info.staged += 1;
                }
                if y == b'M' || y == b'D' {
                    info.unstaged += 1;
                }
            }
        }
    }
    Some(info)
}
