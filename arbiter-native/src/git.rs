//! Minimal git info for the footer: branch + staged/unstaged/untracked counts
//! from a single `git status --porcelain=v1 --branch`. Tauri-free; computed off
//! the cwd the Session tracks (OSC-7), cached per session.
//!
//! Also the git backend for project workspaces (worktree list/add/remove, file
//! status, branch list, merge check) — ports of the web's `src-tauri/src/git.rs`
//! Tauri commands, run via the `git` CLI like the web does.

use std::collections::HashMap;
use std::process::Command;

/// Run `git <args>` in `cwd`, returning stdout on success (no console flash on
/// Windows). The single choke point for all git invocations here.
fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Like [`git`] but returns the stderr text on failure (for surfacing to the UI).
fn git_checked(cwd: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

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

// ── Project-workspace git backend (ports of the web's Tauri commands) ─────────

/// One entry from `git worktree list --porcelain`.
#[derive(Clone, Debug)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: Option<String>, // short name (refs/heads/<x> → <x>); None if detached
    pub head: Option<String>,   // HEAD sha
    pub is_main: bool,          // the first/primary worktree
    pub exists: bool,           // the folder is present on disk
}

/// All worktrees of the repo containing `repo_root`. First entry is the main one.
pub fn worktree_list(repo_root: &str) -> Vec<WorktreeInfo> {
    let Some(text) = git(repo_root, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cur: Option<WorktreeInfo> = None;
    let mut first = true;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            cur = Some(WorktreeInfo {
                exists: std::path::Path::new(p).is_dir(),
                path: p.to_string(),
                branch: None,
                head: None,
                is_main: std::mem::replace(&mut first, false),
            });
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            if let Some(w) = cur.as_mut() {
                w.head = Some(h.to_string());
            }
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(w) = cur.as_mut() {
                w.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            }
        }
    }
    if let Some(w) = cur.take() {
        out.push(w);
    }
    out
}

/// Create a worktree as a sibling dir `../<repo>-<branch>` on a new `branch`
/// (off `base`, or HEAD). Branch `/` → `-` in the folder name. Returns its info.
pub fn worktree_add(repo_root: &str, branch: &str, base: Option<&str>) -> Result<WorktreeInfo, String> {
    let parent = std::path::Path::new(repo_root)
        .parent()
        .ok_or("repo has no parent dir")?;
    let repo_name =
        std::path::Path::new(repo_root).file_name().and_then(|n| n.to_str()).unwrap_or("repo");
    let folder = format!("{repo_name}-{}", branch.replace('/', "-"));
    let dest = parent.join(folder);
    let dest_str = dest.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "add", "-b", branch, &dest_str];
    if let Some(b) = base {
        args.push(b);
    }
    git_checked(repo_root, &args)?;
    worktree_list(repo_root)
        .into_iter()
        .find(|w| w.path == dest_str || std::path::Path::new(&w.path) == dest)
        .ok_or_else(|| "worktree created but not found in list".to_string())
}

/// Remove a worktree (and its folder). `force` discards uncommitted changes.
pub fn worktree_remove(repo_root: &str, worktree_path: &str, force: bool) -> Result<(), String> {
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(worktree_path);
    git_checked(repo_root, &args).map(|_| ())
}

/// Clear stale `.git/worktrees/` entries for folders that no longer exist.
pub fn worktree_prune(repo_root: &str) -> Result<(), String> {
    git_checked(repo_root, &["worktree", "prune"]).map(|_| ())
}

/// Per-file git status for `worktree_path` (or `repo_root`): relative path →
/// one of modified/added/deleted/renamed/untracked/conflicted. From
/// `git status --porcelain=v1 -uall`.
pub fn file_status(worktree_path: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(text) = git(worktree_path, &["status", "--porcelain=v1", "-uall"]) else {
        return map;
    };
    for line in text.lines() {
        if line.len() < 4 {
            continue;
        }
        let xy = &line[..2];
        let path = line[3..].trim();
        // Renames are "R  old -> new"; key by the new path.
        let path = path.rsplit(" -> ").next().unwrap_or(path);
        let status = match xy {
            "??" => "untracked",
            "UU" | "AA" | "DD" => "conflicted",
            _ if xy.starts_with('R') => "renamed",
            _ if xy.contains('D') => "deleted",
            _ if xy.starts_with('A') => "added",
            _ if xy.contains('M') => "modified",
            _ => "modified",
        };
        map.insert(path.to_string(), status.to_string());
    }
    map
}

/// Local + remote branch names (locals short, remotes as `origin/<x>`), deduped.
pub fn list_branches(repo: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(text) = git(repo, &["branch", "--format=%(refname:short)"]) {
        out.extend(text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()));
    }
    if let Some(text) = git(repo, &["branch", "-r", "--format=%(refname:short)"]) {
        for l in text.lines().map(str::trim) {
            // Skip "origin/HEAD"; keep distinct remote-only branches.
            if l.is_empty() || l.ends_with("/HEAD") {
                continue;
            }
            let short = l.split_once('/').map(|(_, b)| b).unwrap_or(l);
            if !out.iter().any(|b| b == short) {
                out.push(l.to_string());
            }
        }
    }
    out
}

/// True if `branch`'s tip is an ancestor of `into` (merged) but not the same
/// commit (so an unmerged feature branch reads false, and `into` itself false).
pub fn is_branch_merged(repo: &str, branch: &str, into: &str) -> bool {
    let tip = match git(repo, &["rev-parse", branch]) {
        Some(t) => t.trim().to_string(),
        None => return false,
    };
    let into_tip = git(repo, &["rev-parse", into]).map(|t| t.trim().to_string());
    if into_tip.as_deref() == Some(tip.as_str()) {
        return false; // same commit → not "merged away"
    }
    // exit 0 ⇒ branch is an ancestor of into.
    let mut cmd = Command::new("git");
    cmd.args(["merge-base", "--is-ancestor", branch, into]).current_dir(repo);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.status().map(|s| s.success()).unwrap_or(false)
}
