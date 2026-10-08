//! Reading an agent's state off its rendered screen, for any agent: the matchers here
//! are driven by the agent's `ScreenProfile`, so they hold no agent's strings
//! themselves. `VtTerm` feeds them the visible rows (see `term.rs`).
//!
//! Every rule reads the bottom of the screen, because agents anchor their input box and
//! choosers there: the same words further up are transcript (an old approval box
//! scrolled back into view, prose quoting the words, Arbiter's own source in a diff).

use crate::agent::{Lead, RowRule, ScreenProfile};

/// What the reader reads from the screen for an agent's state, one field per question.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenState {
    pub menu: bool,
    pub scrolled: bool,
    pub waiting: bool,
    pub working: bool,
    pub idle_box: bool,
    pub slash_input: bool,
}

/// The last `n` of `rows`.
fn bottom(rows: &[String], n: usize) -> &[String] {
    &rows[rows.len().saturating_sub(n)..]
}

/// The text after the status glyph opening `row`, if the row is one of the agent's status
/// rows: a spinner frame and a space, which no transcript line begins with.
pub fn status_row_text<'a>(p: &ScreenProfile, row: &'a str) -> Option<&'a str> {
    let trimmed = row.trim_start();
    let mut chars = trimmed.chars();
    let glyph = chars.next()?;
    let is_frame = p.status_glyphs.iter().any(|g| g.contains(glyph));
    if !is_frame || chars.next() != Some(' ') {
        return None;
    }
    Some(chars.as_str())
}

pub fn row_matches(p: &ScreenProfile, rule: &RowRule, row: &str) -> bool {
    let text = match rule.lead {
        Lead::StatusGlyph => match status_row_text(p, row) {
            Some(t) => t,
            None => return false,
        },
        Lead::Anywhere => row,
    };
    text.starts_with(rule.starts)
        && rule.all.iter().all(|w| text.contains(w))
        && (rule.any.is_empty() || rule.any.iter().any(|w| text.contains(w)))
}

pub fn is_working_row(p: &ScreenProfile, row: &str) -> bool {
    p.working_rows.iter().any(|r| row_matches(p, r, row))
}

pub fn working_row_in(p: &ScreenProfile, rows: &[String]) -> bool {
    bottom(rows, p.live_rows).iter().any(|r| is_working_row(p, r))
}

pub fn scrolled_in(p: &ScreenProfile, rows: &[String]) -> bool {
    !p.scrolled.is_empty() && bottom(rows, p.live_rows).iter().any(|r| p.scrolled.iter().any(|m| r.contains(m)))
}

/// Whether a row is the footer of a live chooser, or its question on a row of its own.
/// Any ONE footer hint also turns up in ordinary prose ("explains how to navigate the
/// tree"), which is how scrolling a transcript used to raise a card, so a footer has to
/// carry `min_hints` of them.
pub fn is_menu_row(p: &ScreenProfile, row: &str) -> bool {
    let m = &p.menu;
    if m.anywhere.iter().any(|w| row.contains(w)) {
        return true;
    }
    let edge = |c: char| c.is_whitespace() || m.edges.contains(&c);
    let whole = row.trim_matches(edge);
    if m.whole_row.iter().any(|w| whole == *w) {
        return true;
    }
    !m.hints.is_empty() && m.hints.iter().filter(|h| row.contains(**h)).count() >= m.min_hints
}

pub fn menu_in(p: &ScreenProfile, rows: &[String]) -> bool {
    bottom(rows, p.menu.rows).iter().any(|r| is_menu_row(p, r))
}

/// Outstanding background work: with the input box's prompt among the live rows, only the
/// last row drawn above it counts, the first that is neither blank, nor the box's border,
/// nor a task-list item. Without the box any live row counts.
pub fn waiting_in(p: &ScreenProfile, rows: &[String]) -> bool {
    if p.waiting_rows.is_empty() {
        return false;
    }
    let waiting = |row: &str| p.waiting_rows.iter().any(|r| row_matches(p, r, row));
    let start = rows.len().saturating_sub(p.live_rows);
    match (start..rows.len()).rev().find(|&i| is_prompt_at(p, rows, i)) {
        Some(prompt) => rows[..prompt]
            .iter()
            .rev()
            .find(|r| !is_blank_or_border(p, r) && !is_task_list_row(p, r))
            .is_some_and(|r| waiting(r)),
        None => bottom(rows, p.live_rows).iter().any(|r| waiting(r)),
    }
}

/// Whether a row is the prompt line of the input box, possibly inside its side border.
pub fn is_input_prompt_row(p: &ScreenProfile, row: &str) -> bool {
    let side = p.prompt.side;
    let body = row.trim_start_matches(|c: char| c.is_whitespace() || c == side);
    body.chars().next().is_some_and(|c| p.prompt.glyphs.contains(&c))
}

/// Whether `rows[i]` is the prompt line of the input box. A prompt glyph is enough on its
/// own. The plainer boxed form (Claude's `>` fallback, which is also how the transcript
/// echoes what the user sent) counts only inside the box: a border directly above it,
/// and another within the few rows below that typed text can take.
pub fn is_prompt_at(p: &ScreenProfile, rows: &[String], i: usize) -> bool {
    let row = &rows[i];
    if is_input_prompt_row(p, row) {
        return true;
    }
    let Some(boxed) = p.prompt.boxed else { return false };
    let side = p.prompt.side;
    let body = row.trim_start_matches(|c: char| c.is_whitespace() || c == side);
    body.starts_with(boxed)
        && i > 0
        && is_box_rule(p, &rows[i - 1])
        && rows[i + 1..].iter().take(p.prompt.box_lines).any(|r| is_box_rule(p, r))
}

/// A border of the input box: border characters and blanks, and not blank alone.
pub fn is_box_rule(p: &ScreenProfile, row: &str) -> bool {
    is_blank_or_border(p, row) && row.chars().any(|c| p.prompt.border.contains(c))
}

/// Whether a row holds nothing but blanks and border characters.
pub fn is_blank_or_border(p: &ScreenProfile, row: &str) -> bool {
    row.chars().all(|c| c.is_whitespace() || p.prompt.border.contains(c))
}

/// Whether a row is an item of the agent's task list, the first hung from the row above
/// by `task_hang`.
pub fn is_task_list_row(p: &ScreenProfile, row: &str) -> bool {
    let hang = p.task_hang;
    let body = row.trim_start_matches(|c: char| c.is_whitespace() || c == hang);
    p.task_marks.iter().any(|m| body.starts_with(m))
}

/// Whether a rendered input row holds a slash command. The box's border and prompt marker
/// are stripped from both ends first; everything after the slash must still look like a
/// command, not prose that happens to start with one.
pub fn row_is_slash_command(p: &ScreenProfile, row: &str) -> bool {
    if p.slash_edges.is_empty() {
        return false;
    }
    let edge = |c: char| c.is_whitespace() || p.slash_edges.contains(&c);
    let body = row.trim_start_matches(edge).trim_end_matches(edge);
    let Some(rest) = body.strip_prefix('/') else { return false };
    // A bare `/` counts: the command list appears on that first keystroke.
    let Some(first) = rest.chars().next() else { return true };
    // `/usr/bin/x` or `//` is a path or a comment, not a command.
    first.is_ascii_alphabetic()
        && rest
            .chars()
            .take_while(|c| !c.is_whitespace())
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

/// Bit index for one spinner glyph, or `None` if the char is not one of the profile's.
pub fn activity_bit(p: &ScreenProfile, c: char) -> Option<u32> {
    p.activity_glyphs.iter().find(|b| b.range.contains(c)).map(|b| {
        let off = c as u32 - b.range.lo;
        b.base + if b.fold > 0 { off % b.fold } else { off }
    })
}

/// Fingerprint of the DISTINCT spinner glyphs in a chunk, or `None` if it has none. Two
/// chunks fingerprint alike iff they drew the same *set* of spinner glyphs, which is what
/// separates a real animation frame from a plain repaint: a spinner draws a different
/// frame every time, while a repaint re-emits whatever static glyphs are on screen, chief
/// among them the thinking summaries Claude leaves in its transcript ("✻ Brewed for 7s").
pub fn activity_key(p: &ScreenProfile, bytes: &[u8]) -> Option<u64> {
    let text = String::from_utf8_lossy(bytes);
    let key = text.chars().filter_map(|c| activity_bit(p, c)).fold(0u64, |k, bit| k | 1 << bit);
    (key != 0).then_some(key)
}

#[cfg(test)]
mod tests {
    use crate::agent::claude::PROFILE as CLAUDE;

    /// The Claude matchers as they were before the screen was made agent data, kept
    /// verbatim as the oracle the profile-driven code is held to.
    mod legacy {
        const LIVE_ROWS: usize = 40;
        const MENU_ROWS: usize = 12;

        fn bottom(rows: &[String], n: usize) -> &[String] {
            &rows[rows.len().saturating_sub(n)..]
        }
        pub fn menu_in(rows: &[String]) -> bool {
            bottom(rows, MENU_ROWS).iter().any(|r| is_menu_row(r))
        }
        pub fn scrolled_in(rows: &[String]) -> bool {
            bottom(rows, LIVE_ROWS).iter().any(|r| r.contains("(ctrl+End)"))
        }
        pub fn working_row_in(rows: &[String]) -> bool {
            bottom(rows, LIVE_ROWS).iter().any(|r| is_working_row(r))
        }
        pub fn waiting_in(rows: &[String]) -> bool {
            let waiting = |row: &str| is_waiting_agents_row(row) || is_background_shell_row(row);
            let start = rows.len().saturating_sub(LIVE_ROWS);
            match (start..rows.len()).rev().find(|&i| is_prompt_at(rows, i)) {
                Some(prompt) => rows[..prompt]
                    .iter()
                    .rev()
                    .find(|r| !is_blank_or_border(r) && !is_task_list_row(r))
                    .is_some_and(|r| waiting(r)),
                None => bottom(rows, LIVE_ROWS).iter().any(|r| waiting(r)),
            }
        }
        fn status_row_text(row: &str) -> Option<&str> {
            let trimmed = row.trim_start();
            let mut chars = trimmed.chars();
            let glyph = chars.next()?;
            let is_frame = matches!(glyph as u32, 0x2722..=0x273F) || glyph == '·';
            if !is_frame || chars.next() != Some(' ') {
                return None;
            }
            Some(chars.as_str())
        }
        fn is_input_prompt_row(row: &str) -> bool {
            row.trim_start_matches(|c: char| c.is_whitespace() || c == '│').starts_with('\u{276f}')
        }
        fn is_prompt_at(rows: &[String], i: usize) -> bool {
            const BOX_LINES: usize = 8;
            let row = &rows[i];
            if is_input_prompt_row(row) {
                return true;
            }
            let body = row.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{2502}');
            body.starts_with("> ")
                && i > 0
                && is_box_rule(&rows[i - 1])
                && rows[i + 1..].iter().take(BOX_LINES).any(|r| is_box_rule(r))
        }
        fn is_box_rule(row: &str) -> bool {
            is_blank_or_border(row) && row.chars().any(|c| matches!(c as u32, 0x2500..=0x257F))
        }
        fn is_blank_or_border(row: &str) -> bool {
            row.chars().all(|c| c.is_whitespace() || matches!(c as u32, 0x2500..=0x257F))
        }
        fn is_task_list_row(row: &str) -> bool {
            const MARKS: &[&str] = &[
                "\u{2610} ", "\u{2612} ", "\u{25fb} ", "\u{25fc} ", "\u{2714} ", "\u{25a1} ", "\u{25a0} ",
                "\u{221a} ", "[ ] ", "[\u{d7}] ",
            ];
            let body = row.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{23bf}');
            MARKS.iter().any(|m| body.starts_with(m))
        }
        pub fn is_waiting_agents_row(row: &str) -> bool {
            let Some(rest) = status_row_text(row) else { return false };
            rest.starts_with("Waiting for ") && rest.contains(" background agent") && rest.contains(" to finish")
        }
        pub fn is_background_shell_row(row: &str) -> bool {
            let Some(rest) = status_row_text(row) else { return false };
            rest.contains(" still running")
                && (rest.contains(" shell still running") || rest.contains(" shells still running"))
        }
        pub fn is_working_row(row: &str) -> bool {
            status_row_text(row).is_some_and(|rest| rest.contains("esc to interrupt"))
        }
        pub fn is_menu_row(row: &str) -> bool {
            if row.contains("Would you like to proceed") {
                return true;
            }
            let edge = |c: char| c.is_whitespace() || c == '\u{2502}';
            if row.trim_matches(edge) == "Do you want to proceed?" {
                return true;
            }
            const HINTS: &[&str] = &["to navigate", "to select", "Esc to cancel", "Enter to"];
            HINTS.iter().filter(|h| row.contains(**h)).count() >= 2
        }
        pub fn row_is_slash_command(row: &str) -> bool {
            let edge = |c: char| c.is_whitespace() || matches!(c, '│' | '|' | '>' | '\u{276f}');
            let body = row.trim_start_matches(edge).trim_end_matches(edge);
            let Some(rest) = body.strip_prefix('/') else { return false };
            let Some(first) = rest.chars().next() else { return true };
            first.is_ascii_alphabetic()
                && rest
                    .chars()
                    .take_while(|c| !c.is_whitespace())
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
        }
        pub fn spinner_bit(c: char) -> Option<u32> {
            match c as u32 {
                u @ 0x2722..=0x273F => Some(u - 0x2722),
                u @ 0x2800..=0x28FF => Some(30 + (u - 0x2800) % 34),
                _ => None,
            }
        }
    }

    /// Every row literal from the matcher tests, plus the edges of each rule: the glyphs
    /// either side of the star range, `·` with and without its space, borders, tabs,
    /// trailing blanks and wide characters.
    const CORPUS: &[&str] = &[
        "✻ Thinking… (esc to interrupt)",
        "· Brewing… (esc to interrupt · ctrl+t to hide todos)",
        "✳ Waiting for 2 background agents to finish · esc to interrupt   ",
        "  the hint reads (esc to interrupt) while it works",
        "● Ran tool, esc to interrupt was shown",
        "+    assert!(row(\"✻ Thinking… (esc to interrupt)\"));",
        "✻ Brewed for 7s",
        "",
        "  ↑/↓ to navigate · Enter to select · Esc to cancel",
        "Would you like to proceed?",
        " Do you want to proceed?",
        "\u{2502} Do you want to proceed?            \u{2502}",
        "\u{25cf} I can rename it everywhere. Do you want to proceed?",
        "  ↑/↓ to navigate · Esc to cancel",
        "  the plan explains how to navigate the tree",
        "  press Esc to cancel, it said, and I did",
        "> /model",
        "│ ❯ /config ",
        "  /agents",
        "> /statusline setup",
        "> /",
        "│ > /   │",
        "> /mod",
        "> fix the bug in /src/main.rs",
        "> /usr/bin/env",
        "> //",
        "> how do I use /model?",
        "> ",
        "✳ Waiting for 3 background agents to finish",
        "· Waiting for 1 background agent to finish",
        "✻ Waiting for 2 background agents to finish · esc to interrupt   ",
        "● Waiting for 3 background agents to finish.",
        "  Waiting for 3 background agents to finish",
        "✳ Waiting for API response",
        "✳ Brewed for 7s",
        "✻ Brewed for 14m 1s · done 11:40 PM · 1 shell still running",
        "✳ Cooked for 3s · done 9:02 AM · 2 shells still running   ",
        "· Baked for 1m 12s · 1 shell still running",
        "✻ Brewed for 14m 1s · done 11:40 PM",
        "● The build is done, 1 shell still running in the background.",
        "  1 shell still running",
        "⏵⏵ auto mode on · 1 shell · ← for agents",
        "● Launching two agents.",
        "● All background agents stopped",
        "────────────────────────────────────────",
        "❯ ",
        "❯ and then check the logs",
        "> run the tests",
        "⏵⏵ auto mode on (shift+tab to cycle) · ← 1 agent",
        "  \u{23bf}  \u{2714} Read the logs",
        "     \u{25fc} Fix the parser",
        "     \u{25fb} Run the tests",
        "  [ ] pending",
        "  [\u{d7}] done",
        "\u{2721} esc to interrupt",
        "\u{2722} esc to interrupt",
        "\u{273f} esc to interrupt",
        "\u{2740} esc to interrupt",
        "·esc to interrupt",
        "·",
        "\t✻ Waiting for 1 background agent to finish",
        "  ✻  esc to interrupt",
        "日本語 ✻ esc to interrupt",
        "│",
        "╭──────╮",
        "│ ❯ │",
        "   ",
        "Jump to bottom (ctrl+End) ↓",
        "3 new messages (ctrl+End) ↓",
        "│/model│",
        "| > /x-y_z:1 arg",
        "/9",
        "❯/",
    ];

    #[test]
    fn claude_rows_read_as_they_did_before_the_profile() {
        use super::*;
        for row in CORPUS {
            assert_eq!(is_working_row(&CLAUDE, row), legacy::is_working_row(row), "working {row:?}");
            assert_eq!(is_menu_row(&CLAUDE, row), legacy::is_menu_row(row), "menu {row:?}");
            assert_eq!(row_is_slash_command(&CLAUDE, row), legacy::row_is_slash_command(row), "slash {row:?}");
            assert_eq!(
                row_matches(&CLAUDE, &CLAUDE.waiting_rows[0], row),
                legacy::is_waiting_agents_row(row),
                "agents {row:?}"
            );
            assert_eq!(
                row_matches(&CLAUDE, &CLAUDE.waiting_rows[1], row),
                legacy::is_background_shell_row(row),
                "shell {row:?}"
            );
        }
    }

    // Every run of up to 8 consecutive corpus rows, alone and inside an input box, as a
    // screen: the box rules are what `waiting_in` measures against.
    #[test]
    fn claude_screens_read_as_they_did_before_the_profile() {
        use super::*;
        let rule = "────────────────────────────────────────".to_string();
        for len in 1..=8 {
            for start in 0..=CORPUS.len() - len {
                let body: Vec<String> = CORPUS[start..start + len].iter().map(|s| s.to_string()).collect();
                let mut boxed = body.clone();
                boxed.extend([rule.clone(), "❯ ".into(), rule.clone()]);
                let mut plain_box = body.clone();
                plain_box.extend([rule.clone(), "> ".into(), rule.clone()]);
                for rows in [&body, &boxed, &plain_box] {
                    assert_eq!(menu_in(&CLAUDE, rows), legacy::menu_in(rows), "menu {rows:?}");
                    assert_eq!(scrolled_in(&CLAUDE, rows), legacy::scrolled_in(rows), "scrolled {rows:?}");
                    assert_eq!(working_row_in(&CLAUDE, rows), legacy::working_row_in(rows), "working {rows:?}");
                    assert_eq!(waiting_in(&CLAUDE, rows), legacy::waiting_in(rows), "waiting {rows:?}");
                }
            }
        }
    }

    #[test]
    fn claude_spinner_bits_are_what_they_were() {
        for u in (0x20..0x80).chain(0x2600..=0x2900) {
            let Some(c) = char::from_u32(u) else { continue };
            assert_eq!(super::activity_bit(&CLAUDE, c), legacy::spinner_bit(c), "U+{u:04X}");
        }
    }

    fn key(s: &str) -> Option<u64> {
        super::activity_key(&CLAUDE, s.as_bytes())
    }

    #[test]
    fn plain_output_carries_no_spinner() {
        assert_eq!(key("cargo build --bin arbiter\r\n"), None);
        // The input prompt arrow sits just outside the star range on purpose: typing
        // must never read as working.
        assert_eq!(key("\u{1b}[2K\u{276F} write the file"), None);
    }

    #[test]
    fn bloom_frames_fingerprint_differently() {
        // Consecutive frames of the animation Claude draws at its status line.
        assert_ne!(key("\u{1b}[23;1H\u{273B}"), key("\u{1b}[23;1H\u{273D}"));
        // The window-title spinner is Braille, and its frames differ from each other
        // and from the stars.
        assert_ne!(key("\u{1b}]0;\u{2802} Claude Code\u{7}"), key("\u{1b}]0;\u{2810} Claude Code\u{7}"));
        assert_ne!(key("\u{1b}]0;\u{2802} Claude Code\u{7}"), key("\u{1b}[23;1H\u{273B}"));
    }

    #[test]
    fn repainting_a_thinking_summary_fingerprints_the_same() {
        // Scrolling Claude's transcript redraws the screen, re-emitting whichever
        // "thinking summary" lines are on it. They all use the same star, so however
        // many land in a chunk the fingerprint is identical: no false animation.
        let a = key("\u{1b}[14;1H\u{273B} Brewed for 7s");
        let b = key("\u{1b}[9;1H\u{273B} Crunched for 2m 5s");
        let both = key("\u{1b}[9;1H\u{273B} Crunched for 2m 5s\u{1b}[14;1H\u{273B} Brewed for 7s");
        assert!(a.is_some());
        assert_eq!(a, b);
        assert_eq!(a, both);
    }

    // Claude's working row, against the same words as they appear in a transcript,
    // including Arbiter's own source, which contains the phrase in these very tests.
    #[test]
    fn the_working_row_is_known_by_its_shape() {
        let row = |r: &str| super::is_working_row(&CLAUDE, r);
        assert!(row("✻ Thinking… (esc to interrupt)"));
        assert!(row("· Brewing… (esc to interrupt · ctrl+t to hide todos)"));
        assert!(row("✳ Waiting for 2 background agents to finish · esc to interrupt   "));
        // Prose, quoted text and source: same words, no status-row shape.
        assert!(!row("  the hint reads (esc to interrupt) while it works"));
        assert!(!row("● Ran tool, esc to interrupt was shown"));
        assert!(!row("+    assert!(row(\"✻ Thinking… (esc to interrupt)\"));"));
        assert!(!row("✻ Brewed for 7s"));
        assert!(!row(""));
    }

    // A live chooser's footer, against an approval box sitting in the transcript.
    #[test]
    fn a_menu_footer_is_matched_by_its_words() {
        let row = |r: &str| super::is_menu_row(&CLAUDE, r);
        assert!(row("  ↑/↓ to navigate · Enter to select · Esc to cancel"));
        assert!(row("Would you like to proceed?"));
        assert!(row(" Do you want to proceed?"));
        assert!(row("\u{2502} Do you want to proceed?            \u{2502}"));
        assert!(!row("\u{25cf} I can rename it everywhere. Do you want to proceed?"));
        assert!(row("  ↑/↓ to navigate · Esc to cancel"));
        // One stray fragment is prose, which is what used to raise a card on every
        // pass while scrolling back over a plan.
        assert!(!row("  the plan explains how to navigate the tree"));
        assert!(!row("  press Esc to cancel, it said, and I did"));
        assert!(!row(""));
    }

    // Telling a slash command in Claude's input box from an ordinary prompt.
    #[test]
    fn a_slash_command_is_told_from_a_prompt() {
        let slash = |r: &str| super::row_is_slash_command(&CLAUDE, r);
        assert!(slash("> /model"));
        assert!(slash("│ ❯ /config "));
        assert!(slash("  /agents"));
        assert!(slash("> /statusline setup"));
        // The first keystroke, which is when Claude puts its command list up.
        assert!(slash("> /"));
        assert!(slash("│ > /   │"));
        assert!(slash("> /mod"));
        assert!(!slash("> fix the bug in /src/main.rs"));
        assert!(!slash("> /usr/bin/env"));
        assert!(!slash("> //"));
        assert!(!slash("> how do I use /model?"));
        assert!(!slash("> "));
        assert!(!slash(""));
    }

    // The status row while Claude waits on agents it launched, against the same words
    // where Claude writes them in its transcript.
    #[test]
    fn the_waiting_for_agents_row_is_known_by_its_shape() {
        let row = |r: &str| super::row_matches(&CLAUDE, &CLAUDE.waiting_rows[0], r);
        assert!(row("✳ Waiting for 3 background agents to finish"));
        assert!(row("· Waiting for 1 background agent to finish"));
        assert!(row("✻ Waiting for 2 background agents to finish · esc to interrupt   "));
        assert!(!row("● Waiting for 3 background agents to finish."));
        assert!(!row("  Waiting for 3 background agents to finish"));
        assert!(!row("✳ Waiting for API response"));
        assert!(!row("✳ Brewed for 7s"));
        assert!(!row(""));
    }

    // A finished turn's line while a shell Claude started in the background runs on.
    // The first is captured from a real session; the verb and the clock both vary.
    #[test]
    fn the_background_shell_row_is_known_by_its_shape() {
        let row = |r: &str| super::row_matches(&CLAUDE, &CLAUDE.waiting_rows[1], r);
        assert!(row("✻ Brewed for 14m 1s · done 11:40 PM · 1 shell still running"));
        assert!(row("✳ Cooked for 3s · done 9:02 AM · 2 shells still running   "));
        assert!(row("· Baked for 1m 12s · 1 shell still running"));
        // The same turn with nothing left behind is an ordinary turn end.
        assert!(!row("✻ Brewed for 14m 1s · done 11:40 PM"));
        assert!(!row("✻ Brewed for 7s"));
        // The same words where Claude writes them in its transcript, or in a pane that
        // is not Claude's status row at all.
        assert!(!row("● The build is done, 1 shell still running in the background."));
        assert!(!row("  1 shell still running"));
        assert!(!row("⏵⏵ auto mode on · 1 shell · ← for agents"));
        assert!(!row(""));
    }
}
