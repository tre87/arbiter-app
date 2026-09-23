//! File-explorer model: a lazily loaded directory tree, git colouring, the
//! filesystem operations behind its context menu, and a watcher that keeps it
//! current. Iced-free on purpose, so the whole thing is unit-testable; the bin
//! (`src/bin/iced_shell.rs`) owns the widgets and the per-workspace state.
//!
//! Ported from the explorer retired with project workspaces in `21079b9`, with
//! paths kept as `PathBuf` rather than `String`: `git::file_status` reports
//! forward-slash relative paths, so on Windows `root.join(rel)` and a `read_dir`
//! path disagree as strings while comparing equal component by component.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};

/// How deep the flattened tree may go. A guard against a symlink cycle, not a
/// real limit: 40 levels is far past any hand-navigated project.
const MAX_DEPTH: usize = 40;

/// Debounce for the filesystem watcher. The web app used the same 500 ms, which
/// is long enough that a `git checkout` touching a thousand files reloads once.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(500);

/// One row of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// A file's git state, strongest first. A directory takes the strongest state of
/// anything beneath it (the web's `getFolderStatus`), so the ordering here is the
/// contract `propagate_dir_status` relies on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Deleted,
    Untracked,
    Renamed,
    Added,
    Modified,
    Conflicted,
}

impl Status {
    /// Parses the strings `git::file_status` produces. Unknown states are dropped.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "modified" => Status::Modified,
            "added" => Status::Added,
            "untracked" => Status::Untracked,
            "renamed" => Status::Renamed,
            "deleted" => Status::Deleted,
            "conflicted" => Status::Conflicted,
            _ => return None,
        })
    }
}

/// The visible tree: the root's children plus the children of every expanded
/// directory. Nothing else is read, so opening a folder never walks the project.
#[derive(Default)]
pub struct Tree {
    pub expanded: HashSet<PathBuf>,
    pub entries: HashMap<PathBuf, Vec<DirEntry>>,
    /// Files from `git status`, plus directories carrying the strongest state beneath them.
    pub status: HashMap<PathBuf, Status>,
}

impl Tree {
    /// Re-read the root and every expanded directory, forgetting expanded
    /// directories that have gone away.
    pub fn reload(&mut self, root: &Path) {
        self.expanded.retain(|d| d.is_dir());
        self.entries.clear();
        self.entries.insert(normalize(root), read_dir_entries(root));
        let dirs: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        for d in dirs {
            self.entries.insert(normalize(&d), read_dir_entries(&d));
        }
    }

    /// Re-read one directory, if it is the root or currently expanded.
    pub fn reload_dir(&mut self, dir: &Path) {
        let key = normalize(dir);
        let Some(slot) = self.entries.get_mut(&key) else { return };
        if key.is_dir() {
            *slot = read_dir_entries(dir);
        } else {
            self.entries.remove(&key);
            self.expanded.remove(&key);
        }
    }

    pub fn is_expanded(&self, dir: &Path) -> bool {
        self.expanded.contains(&normalize(dir))
    }

    /// Expand or collapse a directory, reading its children on the way in.
    pub fn toggle_expand(&mut self, dir: &Path) {
        let key = normalize(dir);
        if self.expanded.remove(&key) {
            return;
        }
        if key.is_dir() {
            self.entries.insert(key.clone(), read_dir_entries(dir));
            self.expanded.insert(key);
        }
    }

    /// Ensure a directory is expanded and its children are loaded.
    pub fn expand(&mut self, dir: &Path) {
        let key = normalize(dir);
        if key.is_dir() {
            self.entries.insert(key.clone(), read_dir_entries(dir));
            self.expanded.insert(key);
        }
    }

    /// Replace the git colouring from `git::file_status` output (paths relative
    /// to `root`, forward slashes), propagating each file's state up to `root`.
    pub fn set_status(&mut self, raw: &HashMap<String, String>, root: &Path) {
        let mut files: HashMap<PathBuf, Status> = HashMap::new();
        for (rel, state) in raw {
            let Some(status) = Status::parse(state) else { continue };
            files.insert(normalize(&root.join(rel)), status);
        }
        self.status = propagate_dir_status(&files, root);
    }

    /// A directory rename moves every expanded path beneath it.
    pub fn remap_expanded(&mut self, old: &Path, new: &Path) {
        let (old, new) = (normalize(old), normalize(new));
        self.expanded = self
            .expanded
            .drain()
            .map(|p| match p.strip_prefix(&old) {
                Ok(rest) => new.join(rest),
                Err(_) => p,
            })
            .collect();
    }

    /// The flattened visible rows, as `(entry, depth)`.
    pub fn rows(&self, root: &Path) -> Vec<(DirEntry, usize)> {
        let mut out = Vec::new();
        flatten_tree(self, root, 0, &mut out);
        out
    }
}

/// Read one directory the way the web app's `read_directory` did: directories
/// first, then files, each case-insensitively alphabetical, dotfiles (and so
/// `.git`) skipped. Unreadable directories read as empty.
pub fn read_dir_entries(dir: &Path) -> Vec<DirEntry> {
    let mut out: Vec<DirEntry> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // A link to a folder (a symlink, or a junction on Windows) opens like one.
        // Following it here is safe: `MAX_DEPTH` bounds a link that loops back.
        let is_dir = match e.file_type() {
            Ok(t) if t.is_symlink() => e.path().is_dir(),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        out.push(DirEntry { name, path: normalize(&e.path()), is_dir });
    }
    out.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// Walk the loaded tree into render order, descending only into expanded dirs.
pub fn flatten_tree(tree: &Tree, dir: &Path, depth: usize, out: &mut Vec<(DirEntry, usize)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Some(children) = tree.entries.get(&normalize(dir)) else { return };
    for e in children {
        out.push((e.clone(), depth));
        if e.is_dir && tree.expanded.contains(&e.path) {
            flatten_tree(tree, &e.path, depth + 1, out);
        }
    }
}

/// Give every ancestor of a changed file the strongest state beneath it, so a
/// collapsed folder still shows that something inside it changed.
pub fn propagate_dir_status(
    files: &HashMap<PathBuf, Status>,
    root: &Path,
) -> HashMap<PathBuf, Status> {
    let root = normalize(root);
    let mut out = files.clone();
    for (path, status) in files {
        let mut cur = path.parent().map(Path::to_path_buf);
        while let Some(dir) = cur {
            if dir == root || !dir.starts_with(&root) {
                break;
            }
            let entry = out.entry(dir.clone()).or_insert(*status);
            if *status > *entry {
                *entry = *status;
            }
            cur = dir.parent().map(Path::to_path_buf);
        }
    }
    out
}

/// `p` written relative to `root`, in this OS's separators. None when `p` is
/// outside `root`.
pub fn relative_path(root: &Path, p: &Path) -> Option<String> {
    normalize(p)
        .strip_prefix(normalize(root))
        .ok()
        .map(|r| r.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

/// Reject names that cannot be created, before touching the disk. Windows rules
/// are applied everywhere: a project is often shared with a Windows machine, and
/// a name legal only on one of them is a trap either way.
pub fn valid_entry_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("Enter a name.");
    }
    if name == "." || name == ".." {
        return Err("That name is reserved.");
    }
    if name.contains('/') || name.contains('\\') {
        return Err("A name cannot contain a path separator.");
    }
    if name.contains('\0') {
        return Err("That name contains an illegal character.");
    }
    if name.contains(['<', '>', ':', '"', '|', '?', '*']) {
        return Err("A name cannot contain < > : \" | ? or *.");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err("A name cannot end with a dot or a space.");
    }
    Ok(())
}

/// Characters before the extension, so renaming `main.rs` can put the caret
/// after `main`. A dotfile has no stem to speak of, so the whole name counts.
pub fn stem_len(name: &str) -> usize {
    match name.rfind('.') {
        Some(0) | None => name.chars().count(),
        Some(i) => name[..i].chars().count(),
    }
}

/// Collapse `.` / `..` and re-emit the path in this OS's separators, so paths
/// built from git output and paths from `read_dir` hash and compare the same.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        p.to_path_buf()
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// Watcher
// ---------------------------------------------------------------------------

/// Live watch on one explorer root. Dropping it stops the watch.
pub struct Watcher(#[allow(dead_code)] Debouncer<RecommendedWatcher>);

type ChangeSink = Box<dyn Fn(PathBuf, Vec<PathBuf>) + Send + Sync>;

static CHANGE_SINK: OnceLock<Mutex<Option<ChangeSink>>> = OnceLock::new();

fn sink_slot() -> &'static Mutex<Option<ChangeSink>> {
    CHANGE_SINK.get_or_init(|| Mutex::new(None))
}

/// Register where debounced changes go, the way `session::set_ui_waker` does.
/// The UI wires this to a channel its subscription drains, so nothing polls.
pub fn set_change_sink(f: ChangeSink) {
    *sink_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

fn emit(root: PathBuf, paths: Vec<PathBuf>) {
    if let Some(f) = sink_slot().lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        f(root, paths);
    }
}

/// Whether a change under the root is worth a reload. Build output and the
/// churn inside `.git` would otherwise reload the tree continuously; the `.git`
/// rule is `session::git_relevant_change`, which decides the same question for
/// the footer's git status.
///
/// Anything inside a dot-directory other than the root's own `.git` is dropped too:
/// the tree never shows one, and with a folder of repositories as the root every
/// fetch or commit in one of them (`sub/.git/...`) reloaded the tree and ran a
/// status. A dotfile itself still counts; a `.gitignore` edit changes the colours.
pub fn watch_relevant(rel: &Path) -> bool {
    use std::path::Component;
    let names: Vec<&std::ffi::OsStr> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    let in_dot_dir = names
        .iter()
        .enumerate()
        .take(names.len().saturating_sub(1))
        .any(|(i, n)| n.to_string_lossy().starts_with('.') && !(i == 0 && *n == ".git"));
    !in_dot_dir && crate::session::git_relevant_change(rel)
}

/// Watch `root` recursively. `None` when the OS refuses (Linux inotify limits,
/// a vanished directory): the tree still works, it just will not self-refresh.
pub fn watch(root: &Path) -> Option<Watcher> {
    let root = normalize(root);
    let for_events = root.clone();
    // FSEvents watches the resolved path and reports resolved paths, so under a root
    // reached through a symlink (`/tmp` is `/private/tmp`) no event matched the tree
    // or an open tab. Events are mapped back onto the root as the tree knows it.
    let real = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let mut deb = new_debouncer(WATCH_DEBOUNCE, move |res: DebounceEventResult| {
        let Ok(events) = res else { return };
        let mut paths: Vec<PathBuf> = Vec::new();
        for e in events {
            let rel = e
                .path
                .strip_prefix(&real)
                .or_else(|_| e.path.strip_prefix(&for_events))
                .unwrap_or(e.path.as_path());
            if watch_relevant(rel) {
                let p = normalize(&for_events.join(rel));
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
        if !paths.is_empty() {
            emit(for_events.clone(), paths);
        }
    })
    .ok()?;
    deb.watcher().watch(&root, RecursiveMode::Recursive).ok()?;
    Some(Watcher(deb))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arbiter-explorer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn read_dir_puts_dirs_first_alphabetically_and_skips_dotfiles() {
        let d = tmp("read");
        std::fs::create_dir(d.join("src")).unwrap();
        std::fs::create_dir(d.join("Assets")).unwrap();
        std::fs::write(d.join("zebra.rs"), "").unwrap();
        std::fs::write(d.join("Alpha.rs"), "").unwrap();
        std::fs::write(d.join(".hidden"), "").unwrap();
        std::fs::create_dir(d.join(".git")).unwrap();

        let names: Vec<String> = read_dir_entries(&d).into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["Assets", "src", "Alpha.rs", "zebra.rs"]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn flatten_follows_only_expanded_dirs() {
        let d = tmp("flatten");
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/inner.rs"), "").unwrap();
        std::fs::write(d.join("top.rs"), "").unwrap();

        let mut t = Tree::default();
        t.reload(&d);
        let rows = t.rows(&d);
        assert_eq!(rows.iter().map(|(e, _)| e.name.as_str()).collect::<Vec<_>>(), vec!["a", "top.rs"]);

        t.toggle_expand(&d.join("a"));
        let rows = t.rows(&d);
        let shown: Vec<(&str, usize)> = rows.iter().map(|(e, dep)| (e.name.as_str(), *dep)).collect();
        assert_eq!(shown, vec![("a", 0), ("b", 1), ("inner.rs", 1), ("top.rs", 0)]);

        t.toggle_expand(&d.join("a"));
        assert_eq!(t.rows(&d).len(), 2);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn flatten_stops_at_the_depth_cap() {
        // A directory that claims to contain itself stands in for a symlink cycle.
        let root = PathBuf::from(if cfg!(windows) { r"C:\loop" } else { "/loop" });
        let child = root.join("self");
        let entry = DirEntry { name: "self".into(), path: child.clone(), is_dir: true };
        let mut t = Tree::default();
        t.entries.insert(normalize(&root), vec![entry.clone()]);
        t.entries.insert(normalize(&child), vec![entry]);
        t.expanded.insert(normalize(&child));
        assert_eq!(t.rows(&root).len(), MAX_DEPTH + 1);
    }

    #[test]
    fn a_dir_takes_the_strongest_status_beneath_it() {
        let root = PathBuf::from(if cfg!(windows) { r"C:\repo" } else { "/repo" });
        let mut raw = HashMap::new();
        raw.insert("src/deep/a.rs".to_string(), "untracked".to_string());
        raw.insert("src/deep/b.rs".to_string(), "conflicted".to_string());
        raw.insert("docs/c.md".to_string(), "modified".to_string());
        raw.insert("clean/.keep".to_string(), "unknown-state".to_string());

        let mut t = Tree::default();
        t.set_status(&raw, &root);

        assert_eq!(t.status.get(&root.join("src").join("deep")), Some(&Status::Conflicted));
        assert_eq!(t.status.get(&root.join("src")), Some(&Status::Conflicted));
        assert_eq!(t.status.get(&root.join("docs")), Some(&Status::Modified));
        assert_eq!(t.status.get(&root.join("clean")), None);
        assert_eq!(t.status.get(&root), None);
    }

    #[test]
    fn git_relative_paths_match_read_dir_paths() {
        // git reports "src/main.rs" with forward slashes on every OS; a row's
        // path comes from read_dir with the OS separator. They must agree.
        let root = PathBuf::from(if cfg!(windows) { r"C:\repo" } else { "/repo" });
        let mut raw = HashMap::new();
        raw.insert("src/main.rs".to_string(), "modified".to_string());
        let mut t = Tree::default();
        t.set_status(&raw, &root);

        let from_read_dir = normalize(&root.join("src").join("main.rs"));
        assert_eq!(t.status.get(&from_read_dir), Some(&Status::Modified));
    }

    #[test]
    fn relative_path_inside_and_outside_the_root() {
        let root = PathBuf::from(if cfg!(windows) { r"C:\repo" } else { "/repo" });
        let inside = root.join("src").join("main.rs");
        let want = if cfg!(windows) { r"src\main.rs" } else { "src/main.rs" };
        assert_eq!(relative_path(&root, &inside).as_deref(), Some(want));
        assert_eq!(relative_path(&root, &root), None);
        let outside = PathBuf::from(if cfg!(windows) { r"D:\other\x" } else { "/other/x" });
        assert_eq!(relative_path(&root, &outside), None);
    }

    #[test]
    fn entry_names_are_checked_before_touching_the_disk() {
        assert!(valid_entry_name("main.rs").is_ok());
        assert!(valid_entry_name(".gitignore").is_ok());
        assert!(valid_entry_name("").is_err());
        assert!(valid_entry_name(".").is_err());
        assert!(valid_entry_name("..").is_err());
        assert!(valid_entry_name("a/b").is_err());
        assert!(valid_entry_name("a\\b").is_err());
        assert!(valid_entry_name("a:b").is_err());
        assert!(valid_entry_name("a?").is_err());
        assert!(valid_entry_name("trailing.").is_err());
        assert!(valid_entry_name("trailing ").is_err());
    }

    #[test]
    fn stem_len_stops_before_the_extension() {
        assert_eq!(stem_len("main.rs"), 4);
        assert_eq!(stem_len("archive.tar.gz"), 11);
        assert_eq!(stem_len("Makefile"), 8);
        assert_eq!(stem_len(".gitignore"), 10);
    }

    #[test]
    fn the_watch_filter_drops_build_and_object_churn() {
        assert!(watch_relevant(Path::new("src/main.rs")));
        assert!(!watch_relevant(Path::new("target/debug/app.exe")));
        assert!(!watch_relevant(Path::new("node_modules/x/index.js")));
        assert!(!watch_relevant(Path::new(".git/objects/ab/cdef")));
        assert!(watch_relevant(Path::new(".git/HEAD")));
        // Nothing the tree shows lives in a dot-directory, a nested repo's included.
        assert!(!watch_relevant(Path::new("sub/.git/HEAD")));
        assert!(!watch_relevant(Path::new(".cache/x/y")));
        assert!(watch_relevant(Path::new(".gitignore")));
        assert!(watch_relevant(Path::new("sub/.env")));
    }

    #[test]
    fn renaming_a_dir_moves_the_expanded_paths_under_it() {
        let root = PathBuf::from(if cfg!(windows) { r"C:\repo" } else { "/repo" });
        let mut t = Tree::default();
        t.expanded.insert(normalize(&root.join("old")));
        t.expanded.insert(normalize(&root.join("old").join("deep")));
        t.expanded.insert(normalize(&root.join("other")));

        t.remap_expanded(&root.join("old"), &root.join("new"));

        assert!(t.expanded.contains(&normalize(&root.join("new"))));
        assert!(t.expanded.contains(&normalize(&root.join("new").join("deep"))));
        assert!(t.expanded.contains(&normalize(&root.join("other"))));
        assert!(!t.expanded.contains(&normalize(&root.join("old"))));
    }
}
