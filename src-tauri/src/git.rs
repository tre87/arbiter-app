use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize, Clone)]
pub struct GitInfo {
    pub is_repo: bool,
    pub branch: Option<String>,
}

/// Standalone git info lookup (usable from both Tauri commands and background threads)
pub fn get_git_info(cwd: &str) -> GitInfo {
    let path = std::path::Path::new(cwd);
    if !path.is_dir() {
        return GitInfo { is_repo: false, branch: None };
    }
    let is_repo = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !is_repo {
        return GitInfo { is_repo: false, branch: None };
    }
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty());
    GitInfo { is_repo, branch }
}

#[tauri::command]
pub fn get_session_git_info(cwd: String) -> GitInfo {
    get_git_info(&cwd)
}

#[derive(Serialize, Clone)]
pub struct WorktreeInfo {
    path: String,
    branch: Option<String>,
    head: Option<String>,
    is_main: bool,
    // Git keeps entries under .git/worktrees/ even after a user deletes the
    // worktree folder manually. We surface this so the frontend can skip
    // adopting stale entries (and, later, offer a prune action).
    exists: bool,
}

#[tauri::command]
pub fn git_worktree_list(repo_root: String) -> Result<Vec<WorktreeInfo>, String> {
    let output = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("Failed to run git worktree list: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut worktrees = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_head: Option<String> = None;
    let mut current_branch: Option<String> = None;
    let mut is_bare = false;

    for line in stdout.lines() {
        if line.starts_with("worktree ") {
            // Flush previous entry
            if let Some(path) = current_path.take() {
                if !is_bare {
                    let exists = std::path::Path::new(&path).is_dir();
                    worktrees.push(WorktreeInfo {
                        path: path.clone(),
                        branch: current_branch.take(),
                        head: current_head.take(),
                        is_main: false, // set below
                        exists,
                    });
                }
            }
            current_path = Some(line[9..].to_string());
            current_head = None;
            current_branch = None;
            is_bare = false;
        } else if line.starts_with("HEAD ") {
            current_head = Some(line[5..].to_string());
        } else if line.starts_with("branch ") {
            // "branch refs/heads/main" → "main"
            let branch = line[7..].to_string();
            current_branch = Some(branch.strip_prefix("refs/heads/").unwrap_or(&branch).to_string());
        } else if line == "bare" {
            is_bare = true;
        }
    }
    // Flush last entry
    if let Some(path) = current_path {
        if !is_bare {
            let exists = std::path::Path::new(&path).is_dir();
            worktrees.push(WorktreeInfo {
                path,
                branch: current_branch,
                head: current_head,
                is_main: false,
                exists,
            });
        }
    }

    // The first worktree (at repo root) is the main one
    if let Some(first) = worktrees.first_mut() {
        first.is_main = true;
    }

    Ok(worktrees)
}

#[tauri::command]
pub fn git_worktree_add(repo_root: String, branch_name: String, base_branch: Option<String>) -> Result<WorktreeInfo, String> {
    // Place worktree as sibling directory: ../reponame-branchname
    let repo_path = std::path::Path::new(&repo_root);
    let repo_name = repo_path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());

    let parent = repo_path.parent()
        .ok_or_else(|| "Cannot determine parent directory of repo".to_string())?;
    // Replace directory separators in the branch name so branches like
    // `feature/foo` produce `repo-feature-foo` instead of creating a nested
    // `repo-feature/foo` directory next to the repo.
    let safe_branch = branch_name.replace(['/', '\\'], "-");
    let worktree_dir = parent.join(format!("{}-{}", repo_name, safe_branch));
    let worktree_path = worktree_dir.to_string_lossy().to_string();

    let mut args = vec![
        "worktree".to_string(),
        "add".to_string(),
        "-b".to_string(),
        branch_name.clone(),
        worktree_path.clone(),
    ];
    if let Some(base) = &base_branch {
        args.push(base.clone());
    }

    let output = std::process::Command::new("git")
        .args(&args)
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("Failed to run git worktree add: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    // Get HEAD of the new worktree
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&worktree_path)
        .output()
        .ok()
        .and_then(|o| if o.status.success() {
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else { None });

    Ok(WorktreeInfo {
        path: worktree_path,
        branch: Some(branch_name),
        head,
        is_main: false,
        exists: true,
    })
}

// Clear `.git/worktrees/<name>/` entries whose folders no longer exist.
// Non-destructive to branches — only touches stale bookkeeping.
#[tauri::command]
pub fn git_worktree_prune(repo_root: String) -> Result<(), String> {
    let output = std::process::Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("Failed to run git worktree prune: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

// Re-check out a worktree whose folder was deleted manually. `-f` is required
// because the stale `.git/worktrees/<name>/` entry from the previous checkout
// still registers the path; without --force, git refuses the add.
#[tauri::command]
pub fn git_worktree_restore(repo_root: String, worktree_path: String, branch_name: String) -> Result<WorktreeInfo, String> {
    let output = std::process::Command::new("git")
        .args(["worktree", "add", "-f", &worktree_path, &branch_name])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("Failed to run git worktree add: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&worktree_path)
        .output()
        .ok()
        .and_then(|o| if o.status.success() {
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else { None });

    Ok(WorktreeInfo {
        path: worktree_path,
        branch: Some(branch_name),
        head,
        is_main: false,
        exists: true,
    })
}

#[tauri::command]
pub fn git_worktree_remove(repo_root: String, worktree_path: String, force: bool) -> Result<(), String> {
    let mut args: Vec<&str> = vec!["-C", &repo_root, "worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&worktree_path);

    let output = std::process::Command::new("git")
        .args(&args)
        .output()
        .map_err(|e| format!("Failed to run git worktree remove: {}", e))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Fallback: if git doesn't recognise the path as a worktree any more
    // (e.g. the .git gitlink is broken from a previous half-completed
    // removal, or the administrative entry in .git/worktrees was lost),
    // the directory still exists on disk. For force removals (discard),
    // delete the directory ourselves and run `git worktree prune` to
    // clean up any stale administrative state.
    let not_a_worktree = stderr.contains("is not a working tree")
        || stderr.contains("not a working tree");

    if force && not_a_worktree {
        let wt_path = std::path::Path::new(&worktree_path);
        if wt_path.exists() {
            std::fs::remove_dir_all(wt_path)
                .map_err(|e| format!("Filesystem removal failed: {}", e))?;
        }
        // Clean up stale administrative entries in <repo>/.git/worktrees.
        let _ = std::process::Command::new("git")
            .args(["-C", &repo_root, "worktree", "prune"])
            .output();
        return Ok(());
    }

    Err(stderr.trim().to_string())
}

#[tauri::command]
pub fn git_merge_branch(repo_root: String, source_branch: String, target_branch: String) -> Result<String, String> {
    // Find the worktree that has the target branch checked out
    let worktrees = git_worktree_list(repo_root.clone())?;
    let target_wt = worktrees.iter().find(|wt| wt.branch.as_deref() == Some(&target_branch));
    let merge_dir = target_wt.map(|wt| wt.path.clone()).unwrap_or(repo_root);

    let output = std::process::Command::new("git")
        .args(["merge", &source_branch])
        .current_dir(&merge_dir)
        .output()
        .map_err(|e| format!("Failed to run git merge: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(format!("{}\n{}", stdout, stderr).trim().to_string());
    }
    Ok(stdout.trim().to_string())
}

#[tauri::command]
pub fn git_push_and_create_pr(worktree_path: String) -> Result<String, String> {
    // Push branch
    let push_output = std::process::Command::new("git")
        .args(["push", "-u", "origin", "HEAD"])
        .current_dir(&worktree_path)
        .output()
        .map_err(|e| format!("Failed to push: {}", e))?;

    if !push_output.status.success() {
        return Err(String::from_utf8_lossy(&push_output.stderr).trim().to_string());
    }

    // Create PR using gh CLI
    let pr_output = std::process::Command::new("gh")
        .args(["pr", "create", "--fill"])
        .current_dir(&worktree_path)
        .output()
        .map_err(|e| format!("Failed to create PR (is gh CLI installed?): {}", e))?;

    if !pr_output.status.success() {
        return Err(String::from_utf8_lossy(&pr_output.stderr).trim().to_string());
    }

    Ok(String::from_utf8_lossy(&pr_output.stdout).trim().to_string())
}

#[tauri::command]
pub fn git_list_branches(repo_path: String) -> Result<Vec<String>, String> {
    // List local branches and remote branches that don't have a local counterpart.
    // Local branches are returned by their short name; remote-only branches keep the
    // remote prefix (e.g. "origin/foo") so the value is directly usable as a git ref.
    let output = std::process::Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"])
        .current_dir(&repo_path)
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut local: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut remote: Vec<String> = Vec::new();

    for line in stdout.lines() {
        let full = line.trim();
        if let Some(rest) = full.strip_prefix("refs/heads/") {
            if !rest.is_empty() { local.insert(rest.to_string()); }
        } else if let Some(rest) = full.strip_prefix("refs/remotes/") {
            if rest.ends_with("/HEAD") { continue; }
            remote.push(rest.to_string());
        }
    }

    let mut result: Vec<String> = local.iter().cloned().collect();
    for r in remote {
        // Strip the remote name to compare against local branches
        let short = match r.find('/') {
            Some(idx) => &r[idx + 1..],
            None => continue,
        };
        if !local.contains(short) {
            result.push(r);
        }
    }
    result.sort();
    Ok(result)
}

#[tauri::command]
pub fn git_repo_root(path: String) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&path)
        .output()
        .ok()?;

    if output.status.success() {
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !root.is_empty() { Some(root) } else { None }
    } else {
        None
    }
}

#[tauri::command]
pub fn git_file_status(repo_root: String, worktree_path: Option<String>) -> Result<HashMap<String, String>, String> {
    let cwd = worktree_path.as_deref().unwrap_or(&repo_root);
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain=v1", "-uall"])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("Failed to run git status: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut statuses = HashMap::new();

    for line in stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let xy = &line[0..2];
        let file_path = &line[3..];

        // Determine status from XY codes
        let status = match xy.trim() {
            "M" | "MM" | "AM" => "modified",
            "A" => "added",
            "D" => "deleted",
            "R" => "renamed",
            "??" => "untracked",
            "UU" | "AA" | "DD" => "conflicted",
            _ if xy.contains('M') => "modified",
            _ if xy.contains('A') => "added",
            _ if xy.contains('D') => "deleted",
            _ => "modified",
        };

        // Handle renamed files: "R  old -> new"
        let actual_path = if file_path.contains(" -> ") {
            file_path.split(" -> ").last().unwrap_or(file_path)
        } else {
            file_path
        };

        statuses.insert(actual_path.to_string(), status.to_string());
    }

    Ok(statuses)
}

#[tauri::command]
pub fn git_is_branch_merged(repo_root: String, branch: String, into_branch: String) -> Result<bool, String> {
    // A branch is "merged" into its parent when:
    //   1. branch's tip is reachable from into_branch (the ancestor check), AND
    //   2. the two tips are NOT the same commit.
    //
    // Without (2), a freshly-created branch (which shares a commit with its
    // parent) would be marked merged immediately, because every commit is its
    // own ancestor. Erring toward "not merged" when tips are equal also means
    // we won't falsely mark a just-fast-forwarded branch as merged, which is
    // acceptable — that case self-corrects as soon as the parent advances.

    let rev = |refname: &str| -> Result<String, String> {
        let out = std::process::Command::new("git")
            .args(["rev-parse", refname])
            .current_dir(&repo_root)
            .output()
            .map_err(|e| format!("Failed to run git rev-parse: {}", e))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };

    let branch_sha = rev(&branch)?;
    let parent_sha = rev(&into_branch)?;
    if branch_sha == parent_sha {
        return Ok(false);
    }

    // `git merge-base --is-ancestor <branch> <into_branch>`
    // exit 0 → branch's tip is reachable from into_branch (fully merged)
    // exit 1 → not merged
    // any other → real error
    let output = std::process::Command::new("git")
        .args(["merge-base", "--is-ancestor", &branch, &into_branch])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("Failed to run git merge-base: {}", e))?;

    if let Some(code) = output.status.code() {
        match code {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        }
    } else {
        Err("git merge-base terminated by signal".to_string())
    }
}
