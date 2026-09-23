//! Pure logic behind the built-in editor: loading and saving a text file
//! faithfully, the undo history iced 0.13's `text_editor` does not provide, and
//! the "Send to Agent" message. No iced types, so it is all unit-testable; the
//! widgets live in `src/bin/files_pane.rs` and `src/bin/gutter.rs`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::SystemTime;

/// Largest file the editor opens. Above this the undo history, which holds whole
/// document copies, is the binding cost; the file goes to the OS handler instead.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Bytes inspected for a NUL before calling a file binary.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Keystrokes closer together than this join one undo entry.
pub const COALESCE_MS: u64 = 500;

/// Undo history caps. The stack holds whole-document snapshots, so both a count
/// and a byte budget are needed: 200 edits of a small file, or fewer of a big one.
pub const UNDO_MAX_ENTRIES: usize = 200;
/// An entry is a copy of the whole document, so the byte budget is what really
/// bounds this: 200 snapshots of a 400 KB file would be 80 MB per tab, and the
/// redo stack can hold as many again. 8 MiB keeps the full 200 steps for an
/// ordinary source file and trims the history early on a large one.
pub const UNDO_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Line ending a file was read with, so saving writes back what was there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eol {
    Lf,
    CrLf,
}

impl Eol {
    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::CrLf => "CRLF",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
        }
    }
}

/// A file read for editing: text normalised to LF, plus what it takes to write
/// the original form back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub text: String,
    pub eol: Eol,
    pub trailing_newline: bool,
    pub bom: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    Binary,
    NotUtf8,
}

impl LoadError {
    pub fn message(self) -> &'static str {
        match self {
            LoadError::Binary => "This looks like a binary file, so the editor will not open it.",
            LoadError::NotUtf8 => "This file is not valid UTF-8, so the editor will not open it.",
        }
    }
}

/// True when the first few kilobytes hold a NUL, the usual cheap binary test.
pub fn is_probably_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SNIFF_BYTES).any(|b| *b == 0)
}

/// Which line ending the file uses. A mixed file takes the one most of its lines
/// have, so a save rewrites as few lines as it can, and the first one found breaks
/// a tie. A file with none at all is written back as LF.
pub fn detect_eol(text: &str) -> Eol {
    let bytes = text.as_bytes();
    let (mut crlf, mut lf, mut first) = (0usize, 0usize, None);
    for (i, _) in text.match_indices('\n') {
        let eol = if i > 0 && bytes[i - 1] == b'\r' { Eol::CrLf } else { Eol::Lf };
        match eol {
            Eol::CrLf => crlf += 1,
            Eol::Lf => lf += 1,
        }
        first.get_or_insert(eol);
    }
    match crlf.cmp(&lf) {
        std::cmp::Ordering::Greater => Eol::CrLf,
        std::cmp::Ordering::Less => Eol::Lf,
        std::cmp::Ordering::Equal => first.unwrap_or(Eol::Lf),
    }
}

/// Decode a file for editing. The buffer works in LF throughout; `eol`, `bom`
/// and `trailing_newline` carry the original form to `serialize`.
pub fn load_bytes(bytes: &[u8]) -> Result<Loaded, LoadError> {
    if is_probably_binary(bytes) {
        return Err(LoadError::Binary);
    }
    let (bom, rest) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    let raw = std::str::from_utf8(rest).map_err(|_| LoadError::NotUtf8)?;
    let eol = detect_eol(raw);
    let text = raw.replace("\r\n", "\n");
    let trailing_newline = text.ends_with('\n');
    Ok(Loaded { text, eol, trailing_newline, bom })
}

/// Write the buffer back in the file's own form. `lines` comes from the widget,
/// which knows nothing of CRLF or of a final newline, so both are restored here.
pub fn serialize(
    lines: impl IntoIterator<Item = String>,
    eol: Eol,
    trailing_newline: bool,
    bom: bool,
) -> Vec<u8> {
    let mut out = String::new();
    if bom {
        out.push('\u{feff}');
    }
    let mut first = true;
    for line in lines {
        if !first {
            out.push_str(eol.as_str());
        }
        first = false;
        out.push_str(line.trim_end_matches('\r'));
    }
    if trailing_newline {
        out.push_str(eol.as_str());
    }
    out.into_bytes()
}

/// The text to hand `Content::with_text` so the buffer comes back identical.
///
/// cosmic-text's line iterator never yields a trailing empty line, so `"a\n"`
/// reads back as the single line `"a"`. A buffer whose last line is empty
/// therefore needs one more newline than a plain join, or every undo would eat
/// the file's final blank line.
pub fn to_buffer_text(lines: &[String]) -> String {
    let mut out = lines.join("\n");
    if lines.last().map(String::is_empty).unwrap_or(false) {
        out.push('\n');
    }
    out
}

/// Split text the way cosmic-text does, for tests and for reasoning about
/// `to_buffer_text`: a trailing newline closes the last line rather than
/// starting an empty one.
pub fn buffer_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text.split('\n').map(str::to_string).collect();
    if out.len() > 1 && out.last().map(String::is_empty).unwrap_or(false) {
        out.pop();
    }
    out
}

/// A fingerprint of a buffer's lines, for telling an edited buffer from the saved
/// one. Streamed a line at a time, so asking after every keystroke copies nothing.
pub fn hash_lines<L: std::ops::Deref<Target = str>>(lines: impl Iterator<Item = L>) -> u64 {
    let mut h = DefaultHasher::new();
    for line in lines {
        h.write(line.as_bytes());
        h.write_u8(b'\n');
    }
    h.finish()
}

pub fn hash_text(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Save `bytes` as the file at `path` so that a failure part way leaves the old
/// contents whole: written to a temp file beside it, synced, then renamed over it.
///
/// The rename replaces the directory entry, so what an in-place write keeps has to
/// be kept by hand, or the save falls back to writing in place:
/// - a symlink is resolved and its target saved, so the link stays a link;
/// - the permissions are copied onto the temp file;
/// - a file with other hard links, or owned by another user, is written in place,
///   since a new entry would split it from its other names or change its owner;
/// - a rename the OS refuses (Windows, while another program holds the file open
///   without sharing delete) falls back to writing in place.
pub fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path)?,
        Ok(_) => path.to_path_buf(),
        Err(_) => return std::fs::write(path, bytes),
    };
    let meta = std::fs::metadata(&target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() > 1 {
            return std::fs::write(&target, bytes);
        }
    }
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return std::fs::write(&target, bytes);
    };
    let tmp = dir.join(format!(".{}.arbiter-save-{}", name.to_string_lossy(), std::process::id()));
    let written = (|| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if f.metadata()?.uid() != meta.uid() {
                return Err(std::io::Error::other("owner differs"));
            }
        }
        f.write_all(bytes)?;
        f.set_permissions(meta.permissions())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return std::fs::write(&target, bytes);
    }
    Ok(())
}

/// What the file looked like on disk when we last read or wrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskStamp {
    pub mtime: Option<SystemTime>,
    pub len: u64,
}

impl DiskStamp {
    pub fn read(path: &Path) -> Option<Self> {
        let m = std::fs::metadata(path).ok()?;
        Some(DiskStamp { mtime: m.modified().ok(), len: m.len() })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskChange {
    Unchanged,
    Changed,
    Missing,
}

/// Compare the file on disk with the stamp taken when the tab was loaded or saved.
pub fn check_disk(path: &Path, stamp: Option<DiskStamp>) -> DiskChange {
    match (DiskStamp::read(path), stamp) {
        (None, _) => DiskChange::Missing,
        (Some(now), Some(then)) if now == then => DiskChange::Unchanged,
        (Some(_), None) => DiskChange::Unchanged,
        _ => DiskChange::Changed,
    }
}

/// The syntax token for a path: the grammar set resolves most languages straight
/// from the extension, and these are the ones it cannot.
pub fn lang_for_path(path: &Path) -> String {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "" => "txt".to_string(),
        // .NET carries most of its project files as XML under names no grammar
        // claims. `.xaml` itself the XML grammar does claim.
        "axaml" | "csproj" | "vbproj" | "fsproj" | "props" | "targets" | "nuspec" | "resx"
        | "vsixmanifest" | "plist" | "xsl" => "xml".to_string(),
        "jsonc" | "json5" => "json".to_string(),
        "mts" | "cts" => "ts".to_string(),
        // No JSX grammar in the bundled set: plain JavaScript colours everything
        // but the tags, which is what it did before there was one.
        "jsx" | "mjs" | "cjs" => "js".to_string(),
        "mdx" => "md".to_string(),
        "htm" => "html".to_string(),
        "yml" => "yaml".to_string(),
        "zsh" => "sh".to_string(),
        other => other.to_string(),
    }
}

/// Which line numbers a gutter beside the editor has to draw, taken from the
/// editor's own scroll rather than tracked alongside it.
///
/// `scroll_line` and `vertical` are cosmic-text's scroll as the buffer holds it
/// RIGHT NOW, which is not where it will end up: `Editor::perform` applies a
/// wheel notch as a raw `vertical += lines * line_h` and leaves it there, and the
/// buffer only settles it during the next layout, after the frame this gutter is
/// built for. Mid-document the two agree by accident (line 0 at -80px draws the
/// same as line 4 at 0). At either end they do not, which showed as numbers that
/// scrolled in a file the text could not scroll, so the same clamp cosmic-text is
/// about to apply is applied here: never above the first line, never past the
/// last screenful. It is a clamp on a value read fresh every frame, not a copy of
/// the scroll kept alongside it, and it holds because every line is exactly
/// `line_h` tall (an absolute line height, and no wrapping).
///
/// Returns the first line (0-based), the y its top sits at relative to the top of
/// the text area (never positive), and how many lines to draw, including a last
/// one the viewport only half shows.
pub fn gutter_window(
    scroll_line: usize,
    vertical: f32,
    view_h: f32,
    line_h: f32,
    line_count: usize,
) -> (usize, f32, usize) {
    if line_h <= 0.0 || view_h <= 0.0 || line_count == 0 {
        return (0, 0.0, 0);
    }
    let furthest = (line_count as f32 * line_h - view_h).max(0.0);
    let scrolled = (scroll_line as f32 * line_h + vertical).clamp(0.0, furthest);
    let first = (scrolled / line_h) as usize;
    let offset = scrolled - first as f32 * line_h;
    let count = ((view_h + offset) / line_h).ceil() as usize;
    (first, -offset, count.min(line_count - first.min(line_count)))
}

/// The language tag on the fenced block sent to an agent: the extension as
/// typed, not the highlighting alias, so a `.vue` file is announced as Vue.
pub fn fence_tag(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// The message "Send to Agent" pastes into a terminal. `first` and `last` are
/// 1-based inclusive line numbers. It ends with a blank line so the agent's
/// input is left on a fresh line, ready to type a question under the code.
pub fn agent_message(path: &Path, first: usize, last: usize, tag: &str, code: &str) -> String {
    let lines = if last > first { format!("{first}-{last}") } else { first.to_string() };
    let body = code.strip_suffix('\n').unwrap_or(code);
    format!(
        "File: {}\n\nLine: {}\n\n```{}\n{}\n```\n\n",
        path.display(),
        lines,
        tag,
        body
    )
}

/// The 0-based inclusive line range a selection covers.
///
/// iced 0.13 reports the caret but not the selection's anchor, so the range is
/// recovered by testing the selection's own text against the lines around the
/// caret: the caret is at one end or the other, and the first and last segments
/// of the selection have to match that line's text there.
pub fn selection_lines(
    line: impl Fn(usize) -> Option<String>,
    caret: (usize, usize),
    selection: &str,
) -> (usize, usize) {
    let (caret_line, caret_byte) = caret;
    let n = selection.matches('\n').count();
    if n == 0 {
        return (caret_line, caret_line);
    }
    let first_seg = selection.split('\n').next().unwrap_or_default();
    let last_seg = selection.rsplit('\n').next().unwrap_or_default();

    // Caret at the end of the selection (dragged or shift-arrowed downwards).
    if caret_line >= n {
        let start = caret_line - n;
        let head_ok = line(caret_line)
            .map(|l| l.get(..caret_byte.min(l.len())).unwrap_or("").ends_with(last_seg))
            .unwrap_or(false);
        let tail_ok = line(start).map(|l| l.ends_with(first_seg)).unwrap_or(false);
        if head_ok && tail_ok {
            return (start, caret_line);
        }
    }
    // Caret at the start (selected upwards).
    let end = caret_line + n;
    let head_ok = line(caret_line)
        .map(|l| l.get(caret_byte.min(l.len())..).unwrap_or("") == first_seg)
        .unwrap_or(false);
    let tail_ok = line(end).map(|l| l.starts_with(last_seg)).unwrap_or(false);
    if head_ok && tail_ok {
        return (caret_line, end);
    }
    (caret_line, caret_line + n)
}

/// A selection that stops at column 0 of the next line covers one line fewer
/// than it spans. Trims that line off the range and the newline off the code.
pub fn trim_dangling_line(range: (usize, usize), code: &str) -> ((usize, usize), String) {
    if code.ends_with('\n') && range.1 > range.0 {
        return ((range.0, range.1 - 1), code[..code.len() - 1].to_string());
    }
    (range, code.to_string())
}

// ---------------------------------------------------------------------------
// Undo history
// ---------------------------------------------------------------------------

/// The whole document plus the caret, which is what it takes to undo through a
/// widget that only accepts a fresh buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub text: String,
    pub cursor: (usize, usize),
}

/// What kind of edit is about to happen, so a run of them can be judged one
/// undo step or several.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Insert(char),
    Backspace,
    Delete,
    Enter,
    Paste,
}

impl EditKind {
    /// Whether this edit continues `prev`'s run rather than starting a new entry.
    fn continues(self, prev: EditKind) -> bool {
        match (prev, self) {
            (EditKind::Insert(_), EditKind::Insert(c)) => !c.is_whitespace(),
            (EditKind::Backspace, EditKind::Backspace) => true,
            (EditKind::Delete, EditKind::Delete) => true,
            _ => false,
        }
    }
}

#[derive(Default)]
pub struct UndoStack {
    entries: Vec<Snapshot>,
    bytes: usize,
    run: Option<(EditKind, u64)>,
}

impl UndoStack {
    /// Called before an edit is performed. Returns true when it opened a new
    /// undo entry, which is also the signal to drop the redo stack.
    pub fn begin(
        &mut self,
        kind: EditKind,
        now_ms: u64,
        snapshot: impl FnOnce() -> Snapshot,
    ) -> bool {
        let joins = match self.run {
            Some((prev, at)) => kind.continues(prev) && now_ms.saturating_sub(at) < COALESCE_MS,
            None => false,
        };
        self.run = Some((kind, now_ms));
        if joins {
            return false;
        }
        self.push(snapshot());
        true
    }

    pub fn push(&mut self, s: Snapshot) {
        self.bytes += s.text.len();
        self.entries.push(s);
        while self.entries.len() > UNDO_MAX_ENTRIES
            || (self.bytes > UNDO_MAX_BYTES && self.entries.len() > 1)
        {
            let dropped = self.entries.remove(0);
            self.bytes = self.bytes.saturating_sub(dropped.text.len());
        }
    }

    pub fn pop(&mut self) -> Option<Snapshot> {
        let s = self.entries.pop()?;
        self.bytes = self.bytes.saturating_sub(s.text.len());
        self.run = None;
        Some(s)
    }

    /// Any caret move, click or selection ends the run, so the next keystroke
    /// starts a fresh undo entry.
    pub fn clear_run(&mut self) {
        self.run = None;
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
        self.run = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn win_path() -> PathBuf {
        PathBuf::from(r"C:\Full\File\Path\someFile.xyz")
    }

    #[test]
    fn agent_message_names_one_line_without_a_range() {
        let m = agent_message(&win_path(), 6, 6, "xyz", "let x = 1;");
        assert_eq!(
            m,
            "File: C:\\Full\\File\\Path\\someFile.xyz\n\nLine: 6\n\n```xyz\nlet x = 1;\n```\n\n"
        );
    }

    #[test]
    fn agent_message_spans_a_range_and_ends_ready_to_type() {
        let m = agent_message(&win_path(), 6, 18, "rs", "fn main() {}");
        assert!(m.starts_with("File: C:\\Full\\File\\Path\\someFile.xyz\n\nLine: 6-18\n\n"));
        assert!(m.ends_with("```rs\nfn main() {}\n```\n\n"));
    }

    #[test]
    fn agent_message_does_not_double_the_code_newline() {
        let m = agent_message(Path::new("/tmp/a.vue"), 1, 2, "vue", "<template>\n</template>\n");
        assert!(m.ends_with("```vue\n<template>\n</template>\n```\n\n"));
        assert!(!m.contains("\n\n```\n\n"));
    }

    fn lines_fn(lines: &'static [&'static str]) -> impl Fn(usize) -> Option<String> {
        move |i| lines.get(i).map(|s| s.to_string())
    }

    #[test]
    fn selection_on_one_line_is_one_line() {
        let f = lines_fn(&["let x = 1;", "let y = 2;"]);
        assert_eq!(selection_lines(f, (1, 8), "y = 2"), (1, 1));
    }

    #[test]
    fn selection_with_the_caret_at_the_end_reads_upwards() {
        let f = lines_fn(&["aaa", "bbb", "ccc"]);
        // Selected "a" on line 0 through "bb" on line 2, caret at line 2 col 2.
        assert_eq!(selection_lines(f, (2, 2), "aa\nbbb\ncc"), (0, 2));
    }

    #[test]
    fn selection_with_the_caret_at_the_start_reads_downwards() {
        let f = lines_fn(&["aaa", "bbb", "ccc"]);
        // Caret parked at line 0 col 1, selection running down to line 2.
        assert_eq!(selection_lines(f, (0, 1), "aa\nbbb\ncc"), (0, 2));
    }

    #[test]
    fn selection_ending_at_column_zero_drops_the_dangling_line() {
        let (range, code) = trim_dangling_line((5, 8), "one\ntwo\n");
        assert_eq!(range, (5, 7));
        assert_eq!(code, "one\ntwo");
    }

    #[test]
    fn eol_is_detected_and_written_back() {
        assert_eq!(detect_eol("a\r\nb"), Eol::CrLf);
        assert_eq!(detect_eol("a\nb"), Eol::Lf);
        assert_eq!(detect_eol("no newline"), Eol::Lf);
        // A mixed file follows most of its lines, and its first ending on a tie.
        assert_eq!(detect_eol("a\nb\r\nc"), Eol::Lf);
        assert_eq!(detect_eol("a\r\nb\nc\r\nd"), Eol::CrLf);
        assert_eq!(detect_eol("a\nb\r\nc\r\nd"), Eol::CrLf);
    }

    #[test]
    fn load_and_serialize_round_trip() {
        for original in [
            &b"a\r\nb"[..],
            &b"a\nb\n"[..],
            &b""[..],
            &b"\n"[..],
            &b"\xef\xbb\xbfx\r\ny\r\n"[..],
        ] {
            let loaded = load_bytes(original).expect("text");
            let lines = buffer_lines(&loaded.text);
            let back = serialize(lines, loaded.eol, loaded.trailing_newline, loaded.bom);
            assert_eq!(back, original, "round trip failed for {original:?}");
        }
    }

    fn save_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("arbiter-save-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_save_replaces_the_contents_and_leaves_no_temp_file() {
        let d = save_dir("plain");
        let f = d.join("a.txt");
        std::fs::write(&f, "old").unwrap();
        write_file(&f, b"new").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "no temp file left behind");
        // A file that does not exist yet is simply created.
        write_file(&d.join("b.txt"), b"b").unwrap();
        assert_eq!(std::fs::read(d.join("b.txt")).unwrap(), b"b");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_save_keeps_permissions_links_and_hard_links() {
        use std::os::unix::fs::PermissionsExt;
        let d = save_dir("unix");
        let f = d.join("script.sh");
        std::fs::write(&f, "old").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o750)).unwrap();
        write_file(&f, b"new").unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o750);

        let link = d.join("link.sh");
        std::os::unix::fs::symlink(&f, &link).unwrap();
        write_file(&link, b"via link").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&f).unwrap(), b"via link");

        let hard = d.join("hard.sh");
        std::fs::hard_link(&f, &hard).unwrap();
        write_file(&f, b"both names").unwrap();
        assert_eq!(std::fs::read(&hard).unwrap(), b"both names");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn binary_and_invalid_utf8_are_refused() {
        assert_eq!(load_bytes(b"ok\0bad"), Err(LoadError::Binary));
        assert_eq!(load_bytes(&[0xff, 0xfe, 0x41]), Err(LoadError::NotUtf8));
        assert!(load_bytes("héllo".as_bytes()).is_ok());
    }

    #[test]
    fn buffer_text_survives_the_widgets_line_splitting() {
        for lines in [
            vec!["a".to_string()],
            vec!["a".to_string(), String::new()],
            vec![String::new()],
            vec![String::new(), String::new()],
            vec!["a".to_string(), "b".to_string()],
        ] {
            let text = to_buffer_text(&lines);
            assert_eq!(buffer_lines(&text), lines, "round trip failed for {lines:?}");
        }
    }

    #[test]
    fn typing_coalesces_until_a_pause_or_a_space() {
        let snap = || Snapshot { text: String::new(), cursor: (0, 0) };
        let mut u = UndoStack::default();
        assert!(u.begin(EditKind::Insert('a'), 0, snap));
        assert!(!u.begin(EditKind::Insert('b'), 100, snap));
        assert!(!u.begin(EditKind::Insert('c'), 200, snap));
        assert_eq!(u.len(), 1);
        // A space breaks the word.
        assert!(u.begin(EditKind::Insert(' '), 250, snap));
        // So does a long pause.
        assert!(!u.begin(EditKind::Insert('d'), 300, snap));
        assert!(u.begin(EditKind::Insert('e'), 1500, snap));
        assert_eq!(u.len(), 3);
    }

    #[test]
    fn moving_the_caret_breaks_the_run() {
        let snap = || Snapshot { text: String::new(), cursor: (0, 0) };
        let mut u = UndoStack::default();
        assert!(u.begin(EditKind::Insert('a'), 0, snap));
        u.clear_run();
        assert!(u.begin(EditKind::Insert('b'), 10, snap));
        assert_eq!(u.len(), 2);
    }

    #[test]
    fn enter_and_paste_always_start_a_new_entry() {
        let snap = || Snapshot { text: String::new(), cursor: (0, 0) };
        let mut u = UndoStack::default();
        assert!(u.begin(EditKind::Enter, 0, snap));
        assert!(u.begin(EditKind::Enter, 10, snap));
        assert!(u.begin(EditKind::Paste, 20, snap));
        assert!(u.begin(EditKind::Paste, 30, snap));
        assert_eq!(u.len(), 4);
        // Backspace runs still coalesce.
        assert!(u.begin(EditKind::Backspace, 40, snap));
        assert!(!u.begin(EditKind::Backspace, 50, snap));
        assert_eq!(u.len(), 5);
    }

    #[test]
    fn the_stack_drops_the_oldest_past_its_cap() {
        let mut u = UndoStack::default();
        for i in 0..(UNDO_MAX_ENTRIES + 10) {
            u.push(Snapshot { text: format!("v{i}"), cursor: (0, 0) });
        }
        assert_eq!(u.len(), UNDO_MAX_ENTRIES);
        assert_eq!(u.pop().unwrap().text, format!("v{}", UNDO_MAX_ENTRIES + 9));
    }

    #[test]
    fn the_gutter_window_follows_the_editors_scroll() {
        // Whole-line scroll: 30 lines of a 20px grid fill a 600px viewport.
        assert_eq!(gutter_window(0, 0.0, 600.0, 20.0, 1000), (0, 0.0, 30));
        assert_eq!(gutter_window(100, 0.0, 600.0, 20.0, 1000), (100, 0.0, 30));

        // A viewport that is not a whole number of lines, scrolled part way into
        // its first line: both edges are half shown, so 32 numbers are needed.
        assert_eq!(gutter_window(100, 5.0, 605.0, 20.0, 1000), (100, -5.0, 31));

        // The end of the document never numbers past the last line: the furthest
        // the editor will settle at is the last screenful, line 970 of 1000.
        assert_eq!(gutter_window(970, 0.0, 600.0, 20.0, 1000), (970, 0.0, 30));
        assert_eq!(gutter_window(980, 0.0, 600.0, 20.0, 1000), (970, 0.0, 30));
        assert_eq!(gutter_window(1000, 0.0, 600.0, 20.0, 1000), (970, 0.0, 30));

        // A wheel notch past the end is refused, as the editor will refuse it.
        assert_eq!(gutter_window(970, 80.0, 600.0, 20.0, 1000), (970, 0.0, 30));

        // A file shorter than the viewport cannot scroll at all, however far the
        // raw scroll has been pushed. This is the bug the clamp exists for.
        assert_eq!(gutter_window(0, 80.0, 600.0, 20.0, 10), (0, 0.0, 10));
        assert_eq!(gutter_window(4, 0.0, 600.0, 20.0, 10), (0, 0.0, 10));

        // Scrolling up past the first line is refused the same way.
        assert_eq!(gutter_window(4, -80.0, 600.0, 20.0, 1000), (0, 0.0, 30));
        assert_eq!(gutter_window(0, -40.0, 600.0, 20.0, 1000), (0, 0.0, 30));

        // Degenerate sizes ask for nothing rather than panicking.
        assert_eq!(gutter_window(0, 0.0, 0.0, 20.0, 1000).2, 0);
        assert_eq!(gutter_window(0, 0.0, 600.0, 0.0, 1000).2, 0);
        assert_eq!(gutter_window(0, 0.0, 600.0, 20.0, 0).2, 0);
    }

    #[test]
    fn language_tokens_and_fence_tags_come_from_the_extension() {
        assert_eq!(lang_for_path(Path::new("Foo.VUE")), "vue");
        assert_eq!(lang_for_path(Path::new("x.rs")), "rs");
        assert_eq!(lang_for_path(Path::new("x.ps1")), "ps1");
        assert_eq!(lang_for_path(Path::new("x.ts")), "ts");
        assert_eq!(lang_for_path(Path::new("x.cts")), "ts");
        assert_eq!(lang_for_path(Path::new("Arbiter.csproj")), "xml");
        // `.xaml` needs no alias: the XML grammar claims the extension itself.
        assert_eq!(lang_for_path(Path::new("MainWindow.xaml")), "xaml");
        assert_eq!(lang_for_path(Path::new("tsconfig.jsonc")), "json");
        assert_eq!(lang_for_path(Path::new("x.yml")), "yaml");
        assert_eq!(lang_for_path(Path::new("Makefile")), "txt");
        assert_eq!(fence_tag(Path::new("a/b/App.vue")), "vue");
        assert_eq!(fence_tag(Path::new("Makefile")), "");
    }
}

/// Open-latency diagnostic, gated on `ARBITER_TIME_OPEN`. Off, every call costs
/// one relaxed atomic load. On, one line per opened file goes to stderr with the
/// phases we control and the wall time to the frame that has paid for the
/// layout and the first highlighting pass.
pub mod timing {
    use std::sync::Mutex;
    use std::sync::OnceLock;
    use std::time::Instant;

    pub fn enabled() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("ARBITER_TIME_OPEN").is_some())
    }

    struct Open {
        at: Instant,
        last: Instant,
        name: String,
        lines: usize,
        phases: Vec<(&'static str, f32)>,
        frames: u32,
        filled: bool,
    }

    static PENDING: Mutex<Option<Open>> = Mutex::new(None);

    pub fn open(name: &str) {
        if !enabled() {
            return;
        }
        let now = Instant::now();
        *PENDING.lock().unwrap() = Some(Open {
            at: now,
            last: now,
            name: name.to_string(),
            lines: 0,
            phases: Vec::new(),
            frames: 0,
            filled: false,
        });
    }

    pub fn phase(label: &'static str) {
        if !enabled() {
            return;
        }
        if let Some(o) = PENDING.lock().unwrap().as_mut() {
            let now = Instant::now();
            o.phases.push((label, (now - o.last).as_secs_f32() * 1000.0));
            o.last = now;
        }
    }

    pub fn lines(n: usize) {
        if !enabled() {
            return;
        }
        if let Some(o) = PENDING.lock().unwrap().as_mut() {
            o.lines = n;
            o.filled = true;
        }
    }

    /// Called once per editor frame. The first frame only builds the widget
    /// tree; the layout and the first highlight are paid for after `view`
    /// returns, so the second frame is the earliest one that has seen the file.
    pub fn frame() {
        if !enabled() {
            return;
        }
        let mut guard = PENDING.lock().unwrap();
        let Some(o) = guard.as_mut() else { return };
        o.frames += 1;
        if o.frames == 1 {
            let now = Instant::now();
            o.phases.push(("to-frame", (now - o.last).as_secs_f32() * 1000.0));
            o.last = now;
        }
        // The frame that has the text is the one worth reporting: with a buffer
        // carried over from the tab being left that is the first, and without one
        // it is the second.
        if !o.filled {
            return;
        }
        let total = (Instant::now() - o.at).as_secs_f32() * 1000.0;
        let phases: Vec<String> =
            o.phases.iter().map(|(l, ms)| format!("{l} {ms:.1}")).collect();
        eprintln!(
            "arbiter: open {} ({} lines) {} | visible {:.1} ms over {} frames",
            o.name,
            o.lines,
            phases.join(" "),
            total,
            o.frames
        );
        *guard = None;
    }
}
