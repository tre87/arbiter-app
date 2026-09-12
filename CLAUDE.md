# Arbiter — project notes for Claude

Arbiter runs many Claude Code sessions side by side. This is the **native** app: Rust +
iced 0.13 + wgpu, with a custom GPU terminal renderer (alacritty_terminal for VT parsing,
per-OS glyph rasterization). It replaced an earlier Tauri/Vue web app; the native app is at
the repo root. The user-facing binary is `arbiter` (source: `src/bin/iced_shell.rs`).

## Build / run / test

- Build / run: `cargo build --bin arbiter` · `cargo run --bin arbiter`
- Unit tests: `cargo test --lib`
- **Compile-check Windows code from macOS/Linux** (the DirectWrite path only builds on
  Windows): `cargo check --bin arbiter --target x86_64-pc-windows-gnu --features usage-helper`
- **Always isolate GUI launches** so they can't clobber the real saved session:
  `ARBITER_DATA_DIR=/tmp/arbiter-test cargo run --bin arbiter`

## Conventions

- **Keep `CHANGELOG.md` current.** Update `[Unreleased]` as you go; on a version bump,
  rename `[Unreleased]` → the new version. The user does all `git push` + tag pushes.
- **Don't touch the macOS glyph-rendering path** when fixing Windows rendering, and
  vice-versa — they're separate (CoreText vs DirectWrite) and easy to regress.
- **No polling.** Every live signal must be event-driven (file watchers / PTY reader
  callbacks); web parity depends on it.

## Remote Claude resume — deferred design

A restored SSH pane brings Claude back with `claude -c || claude` (`src/remote.rs`
`CLAUDE_COMMAND`): the directory's most recent conversation, else a fresh one. Local panes
are exact (`claude --resume <id>`) because Arbiter's shim runs as Claude's status line and
writes each pane's session id to a file; over SSH nothing on the far host says which
conversation belonged to which pane, so two panes in one remote directory cannot both be
resumed (only the first gets `-c`).

The exact version was built and then shelved (2026-09-12) because it needs a script copied
to every remote host, which the user did not want yet. If it comes back, the design was:

- A remote status-line command (`~/.claude/statusLine` in the far `~/.claude/settings.json`)
  that reads Claude's status JSON and prints an invisible control sequence,
  `ESC ] 7777 ; arbiter ; session=<id> ; cwd=<percent-encoded path> BEL`, chaining any
  status line the user already had. Claude's renderer strips recognised escape sequences
  when measuring, so it passes through unchanged; stick to the character set
  `ansi-regex` accepts.
- The reader (`session::reader_loop`, OSC scan) parses it on remote panes only: session id
  → a `remote_session` on `ClaudeHandle` (kept across a drop, cleared when Claude's chrome
  leaves or another command is typed at the shell prompt), cwd → `set_remote_cwd`, and
  both count as login evidence. Persist it as `remote_session` on `SavedNode::Leaf`.
- The relaunch becomes `claude --resume <id> || claude -c || claude`, and a pane with an
  id never contends for its directory in `connect_pane`'s `claimed` set.

## Terminal renderer — known limitation (intentional)

The GPU renderer draws **one opaque quad per cell**, so a glyph cannot overflow its cell
without painting over (erasing) the neighbour. Consequences, both intentional / accepted:

- **Single-width emoji-capable symbols render as TEXT on Windows.** A 1-cell text-default
  character like ⏸ (U+23F8, the plan-mode indicator), ⏺ or ✳ that the terminal font lacks
  is laid out in Segoe UI Symbol by name in the DirectWrite path (`raster.rs`
  `emoji_block` + `render`), giving the plain glyph in the text colour instead of Segoe UI
  Emoji's colour button squashed into the cell (Claude over SSH drew its bullets as blue
  squares that way; Claude on Windows avoids those characters, which is why local panes
  never showed it). Double-width emoji (👋, 2 cells) still take the colour path and render
  full-size. A colour glyph that does land in one cell is downscaled to it; proper
  **overflow rendering** (alpha-composited glyphs or variable-size atlas tiles) is a real
  renderer change, deliberately deferred.
- `fit_to_box` (`src/gpu.rs`) instead **center-clips** a mono fallback symbol that's only
  slightly wider than the cell (e.g. ✻), keeping full height, unless the clip would cut
  through solid ink (`clip_cuts_solid_ink`: a filled ⏺ came out as a square that way, so
  it scales down instead), and downscales anything
  larger. Don't re-add an "upscale undersized symbols" path — it enlarges glyphs like ⏵
  past their natural size (reverted once already).
