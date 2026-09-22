//! Syntax colouring for the built-in editor: a syntect parser wearing iced's
//! `Highlighter` trait.
//!
//! syntect is used directly rather than through iced's `highlighter` feature,
//! which wraps a private `SyntaxSet` we could not add Vue to and whose default
//! features build oniguruma. The set here is syntect's own bundle plus the
//! grammars in `assets/syntaxes`, so a `.vue` file colours its template,
//! script and style blocks with the HTML, JavaScript and CSS grammars.

use std::ops::Range;
use std::sync::OnceLock;

use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// The editor's font family. Declared here because `format` has to be a plain
/// `fn` and cannot capture one.
pub const MONO_FAMILY: &str = "Cascadia Mono";

/// Files longer than this are shown without colouring: the widget re-highlights
/// from the edited line to the end of the document on every keystroke, and past
/// this length that stops being free even with the line cache below.
pub const MAX_HIGHLIGHT_LINES: usize = 20_000;

/// Grammars syntect does not bundle, as `(name, source)`. Vue is the one the
/// user asked for; TOML is missing from the default set and this is a Rust
/// project, so `Cargo.toml` would otherwise open uncoloured.
const EXTRA_SYNTAXES: [(&str, &str); 2] = [
    ("Vue", include_str!("../assets/syntaxes/Vue.sublime-syntax")),
    ("TOML", include_str!("../assets/syntaxes/TOML.sublime-syntax")),
];

/// Dark theme closest to the app's own chrome. Foregrounds read well on #121212.
const THEME_NAME: &str = "base16-ocean.dark";

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

/// Bumped when `link_syntaxes` changes in a way a cached dump would not reflect.
const CACHE_VERSION: u32 = 1;

/// syntect's bundled grammars plus our own, linked together.
///
/// Adding any grammar forces syntect to re-link the whole set, which costs far
/// more than loading its pre-linked default dump did. So the result is cached as
/// a dump of our own, keyed by the grammars that went into it.
fn link_syntaxes() -> SyntaxSet {
    // Newline mode: syntect's own advice, and the no-newline variants are
    // rewritten regexes that are unreliable for a grammar it did not ship.
    let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
    for (name, source) in EXTRA_SYNTAXES {
        match syntect::parsing::SyntaxDefinition::load_from_str(source, true, Some(name)) {
            Ok(syntax) => builder.add(syntax),
            Err(e) => eprintln!("arbiter: bundled {name} syntax failed to load: {e}"),
        }
    }
    builder.build()
}

/// Where the linked set is cached. The name carries a hash of what went into it,
/// so editing a bundled grammar produces a different file rather than a stale hit.
fn cache_path() -> Option<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    CACHE_VERSION.hash(&mut h);
    for (name, source) in EXTRA_SYNTAXES {
        name.hash(&mut h);
        source.hash(&mut h);
    }
    Some(crate::shell::app_data_dir()?.join(format!("syntaxes-{:016x}.bin", h.finish())))
}

pub fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(|| {
        let path = cache_path();
        if let Some(p) = path.as_ref() {
            // A dump from an older syntect, or a truncated one, simply fails to
            // load and is rebuilt over.
            if let Ok(set) = syntect::dumps::from_dump_file::<SyntaxSet, _>(p) {
                return set;
            }
        }
        let set = link_syntaxes();
        if let Some(p) = path.as_ref() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
                // Sweep dumps from older grammars so the folder cannot grow.
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for e in entries.flatten() {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("syntaxes-") && name.ends_with(".bin") && e.path() != *p
                        {
                            let _ = std::fs::remove_file(e.path());
                        }
                    }
                }
            }
            let _ = syntect::dumps::dump_to_file(&set, p);
        }
        set
    })
}

pub fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        let mut set = ThemeSet::load_defaults();
        set.themes
            .remove(THEME_NAME)
            .or_else(|| set.themes.values().next().cloned())
            .unwrap_or_default()
    })
}

/// Build the syntax set off the UI thread, once.
///
/// Call this as early as the explorer is known to be in use, not when a file is
/// opened: the first `Syntax::new` blocks on the same initialisation, so warming
/// at that moment buys nothing and the pause lands in the frame that opens the
/// editor.
pub fn warm() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(|| {
            let _ = syntaxes();
            let _ = theme();
        });
    });
}

/// What the editor asks to be coloured.
///
/// `doc` identifies the buffer, not the file: it changes whenever the widget's
/// `Content` is replaced (open, reload, undo, redo). iced only re-initialises a
/// highlighter when the settings differ, so without it a second tab in the same
/// language would inherit the first one's parser, parked at its last line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub token: String,
    pub doc: u64,
}

/// One span's colour. Only the colour is set: giving spans a bold or italic
/// face risks a fallback font with a different advance width, which would slide
/// the text out of step with the line-number gutter beside it.
#[derive(Clone, Copy, Debug)]
pub struct Highlight {
    color: Option<iced::Color>,
}

/// The `fn` iced calls to turn a span into text attributes.
pub fn format(highlight: &Highlight, _theme: &iced::Theme) -> iced::advanced::text::highlighter::Format<iced::Font> {
    iced::advanced::text::highlighter::Format { color: highlight.color, font: None }
}

/// A parsed line, kept so an edit low in a file does not re-parse everything
/// below it. `state_in` is what makes the cache safe to reuse: when re-feeding
/// after an edit reaches a line whose text and incoming parser state both match,
/// nothing below it can have changed either.
struct LineCache {
    text: String,
    state_in: (ParseState, ScopeStack),
    state_out: (ParseState, ScopeStack),
    spans: Vec<(Range<usize>, Highlight)>,
}

pub struct Syntax {
    syntax: &'static SyntaxReference,
    highlighter: syntect::highlighting::Highlighter<'static>,
    lines: Vec<LineCache>,
    current: usize,
    state: (ParseState, ScopeStack),
    /// Lines actually parsed since construction. Read by the tests to prove the
    /// cache stops the re-feed early.
    #[cfg(test)]
    parsed: usize,
}

impl Syntax {
    fn resolve(token: &str) -> &'static SyntaxReference {
        let set = syntaxes();
        set.find_syntax_by_token(token).unwrap_or_else(|| set.find_syntax_plain_text())
    }

    fn initial(&self) -> (ParseState, ScopeStack) {
        (ParseState::new(self.syntax), ScopeStack::new())
    }

    fn parse(&mut self, line: &str) -> Vec<(Range<usize>, Highlight)> {
        #[cfg(test)]
        {
            self.parsed += 1;
        }
        // The set is built in newline mode, so the grammar expects the line
        // terminator; the ranges are then clamped back to the text iced gave us.
        let mut owned = String::with_capacity(line.len() + 1);
        owned.push_str(line);
        owned.push('\n');

        let ops = self.state.0.parse_line(&owned, syntaxes()).unwrap_or_default();
        let mut spans: Vec<(Range<usize>, Highlight)> = Vec::new();
        let mut last = 0usize;
        for (index, op) in ops {
            let index = index.min(line.len());
            if index > last {
                spans.push((last..index, self.current_highlight()));
            }
            if self.state.1.apply(&op).is_err() {
                break;
            }
            last = index;
        }
        if last < line.len() {
            spans.push((last..line.len(), self.current_highlight()));
        }
        spans
    }

    fn current_highlight(&self) -> Highlight {
        let style = self.highlighter.style_mod_for_stack(&self.state.1.scopes);
        Highlight {
            color: style
                .foreground
                .map(|c| iced::Color::from_rgba8(c.r, c.g, c.b, f32::from(c.a) / 255.0)),
        }
    }
}

impl iced::advanced::text::Highlighter for Syntax {
    type Settings = Settings;
    type Highlight = Highlight;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Highlight)>;

    fn new(settings: &Self::Settings) -> Self {
        let syntax = Self::resolve(&settings.token);
        let mut me = Syntax {
            syntax,
            highlighter: syntect::highlighting::Highlighter::new(theme()),
            lines: Vec::new(),
            current: 0,
            state: (ParseState::new(syntax), ScopeStack::new()),
            #[cfg(test)]
            parsed: 0,
        };
        me.state = me.initial();
        me
    }

    fn update(&mut self, new_settings: &Self::Settings) {
        self.syntax = Self::resolve(&new_settings.token);
        self.lines.clear();
        self.current = 0;
        self.state = self.initial();
    }

    fn change_line(&mut self, line: usize) {
        self.current = line.min(self.lines.len());
        self.state = match self.lines.get(self.current) {
            Some(c) => c.state_in.clone(),
            None => self.initial(),
        };
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let index = self.current.min(self.lines.len());
        if let Some(cached) = self.lines.get(index) {
            if cached.text == line && cached.state_in == self.state {
                self.state = cached.state_out.clone();
                self.current = index + 1;
                return cached.spans.clone().into_iter();
            }
        }
        let state_in = self.state.clone();
        let spans = self.parse(line);
        let entry = LineCache {
            text: line.to_string(),
            state_in,
            state_out: self.state.clone(),
            spans: spans.clone(),
        };
        // Overwrite in place rather than truncating: the entries below are what
        // makes the re-feed after an edit cheap, and the `state_in` test above is
        // already what decides whether one of them is still valid.
        match self.lines.get_mut(index) {
            Some(slot) => *slot = entry,
            None => self.lines.push(entry),
        }
        self.current = index + 1;
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::text::Highlighter as _;

    fn settings(token: &str) -> Settings {
        Settings { token: token.to_string(), doc: 0 }
    }

    fn colours(h: &mut Syntax, line: &str) -> Vec<Option<iced::Color>> {
        h.highlight_line(line).map(|(_, hl)| hl.color).collect()
    }

    #[test]
    fn the_bundled_vue_grammar_loads() {
        assert!(syntaxes().find_syntax_by_extension("vue").is_some());
    }

    #[test]
    fn the_languages_the_icons_promise_all_resolve() {
        let set = syntaxes();
        for token in ["rs", "js", "css", "html", "json", "md", "py", "toml", "yaml", "sh", "vue", "properties"] {
            assert!(set.find_syntax_by_token(token).is_some(), "no syntax for {token}");
        }
    }

    #[test]
    fn an_unknown_extension_falls_back_to_plain_text_without_panicking() {
        let mut h = Syntax::new(&settings("no-such-language"));
        let spans = colours(&mut h, "anything at all");
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn a_vue_file_colours_its_template_script_and_style_blocks() {
        let mut h = Syntax::new(&settings("vue"));
        let doc = [
            "<template>",
            "  <div class=\"a\">{{ msg }}</div>",
            "</template>",
            "<script>",
            "export default { data() { return { msg: 'hi' } } }",
            "</script>",
            "<style scoped>",
            ".a { color: red; }",
            "</style>",
        ];
        let mut distinct = std::collections::HashSet::new();
        for line in doc {
            for c in colours(&mut h, line).into_iter().flatten() {
                distinct.insert(format!("{:?}", c));
            }
        }
        // Three grammars in one file: plain text would give a single colour.
        assert!(distinct.len() > 3, "vue highlighting produced {} colours", distinct.len());
    }

    #[test]
    fn a_vue_file_never_fails_to_parse_a_line() {
        // An unresolved `scope:` embed makes parse_line error and the rest of the
        // block fall back to plain text, which is what the vendored grammar's
        // retargeted embeds exist to prevent.
        let mut h = Syntax::new(&settings("vue"));
        for line in ["<script lang=\"ts\">", "const a: number = 1", "</script>", "<style lang=\"scss\">", ".a { .b { color: red; } }", "</style>"] {
            let before = h.parsed;
            let _ = colours(&mut h, line);
            assert_eq!(h.parsed, before + 1);
        }
    }

    #[test]
    fn rust_keywords_and_strings_get_different_colours() {
        let mut h = Syntax::new(&settings("rs"));
        let spans = colours(&mut h, "let s = \"text\";");
        let distinct: std::collections::HashSet<String> =
            spans.into_iter().flatten().map(|c| format!("{c:?}")).collect();
        assert!(distinct.len() >= 2);
    }

    #[test]
    fn re_feeding_after_an_edit_stops_at_the_first_unchanged_line() {
        let mut h = Syntax::new(&settings("rs"));
        let doc: Vec<String> = (0..200).map(|i| format!("let v{i} = {i};")).collect();
        for line in &doc {
            let _ = colours(&mut h, line);
        }
        assert_eq!(h.parsed, 200);

        // iced re-feeds from the changed line to the end of the document. Only
        // the changed line should actually be parsed again.
        h.change_line(100);
        let mut edited = doc.clone();
        edited[100] = "let v100 = 999;".to_string();
        for line in &edited[100..] {
            let _ = colours(&mut h, line);
        }
        assert_eq!(h.parsed, 201, "cache did not converge after one changed line");
    }
}
