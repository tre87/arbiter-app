//! Pure logic behind the built-in editor: loading and saving a text file
//! faithfully, the undo history iced 0.13's `text_editor` does not provide, and
//! the "Send to Agent" message. No iced types, so it is all unit-testable; the
//! widgets live in `src/bin/editor_ui.rs`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::SystemTime;

/// Largest file the editor opens. Above this the Shrink-height layout shapes
/// more text than a frame can afford, so the file goes to the OS handler instead.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Bytes inspected for a NUL before calling a file binary.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Keystrokes closer together than this join one undo entry.
pub const COALESCE_MS: u64 = 500;

/// Undo history caps. The stack holds whole-document snapshots, so both a count
/// and a byte budget are needed: 200 edits of a small file, or fewer of a big one.
pub const UNDO_MAX_ENTRIES: usize = 200;
pub const UNDO_MAX_BYTES: usize = 64 * 1024 * 1024;

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

/// Which line ending the file uses: the first one found wins, and a file with
/// none at all is written back as LF.
pub fn detect_eol(text: &str) -> Eol {
    match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => Eol::CrLf,
        _ => Eol::Lf,
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

pub fn hash_text(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
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

/// The syntax token for a path: syntect resolves most languages straight from
/// the extension. The aliases cover languages its bundled set lacks, so a
/// TypeScript file colours as JavaScript rather than as plain text.
pub fn lang_for_path(path: &Path) -> String {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "" => "txt".to_string(),
        "ts" | "tsx" | "mts" | "cts" | "jsx" | "mjs" | "cjs" => "js".to_string(),
        "yml" => "yaml".to_string(),
        "htm" => "html".to_string(),
        "zsh" | "bash" | "fish" => "sh".to_string(),
        // syntect has no INI grammar; Java Properties is the same key=value
        // shape with `#` comments, which is most of the colouring these get.
        "ini" | "conf" | "cfg" | "env" => "properties".to_string(),
        other => other.to_string(),
    }
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
        // A mixed file follows its first ending.
        assert_eq!(detect_eol("a\nb\r\nc"), Eol::Lf);
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
    fn language_tokens_and_fence_tags_come_from_the_extension() {
        assert_eq!(lang_for_path(Path::new("Foo.VUE")), "vue");
        assert_eq!(lang_for_path(Path::new("x.rs")), "rs");
        assert_eq!(lang_for_path(Path::new("x.ts")), "js");
        assert_eq!(lang_for_path(Path::new("x.yml")), "yaml");
        assert_eq!(lang_for_path(Path::new("Makefile")), "txt");
        assert_eq!(fence_tag(Path::new("a/b/App.vue")), "vue");
        assert_eq!(fence_tag(Path::new("Makefile")), "");
    }
}
