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

## Remote Claude resume — how the session id is known

Local panes resume exactly (`claude --resume <id>`) because Arbiter's shim runs as
Claude's status line and writes each pane's session id to a file. Over SSH nothing on the
far host can tell Arbiter that, so **Arbiter names the conversation** when the setting
`name_remote_claude_sessions` is on (Settings, General; off by default because it visibly
edits typed input): on Enter in a remote pane, a bare `claude` at a shell prompt is
completed with ` --session-id <uuid>` before the CR goes through (`Session::on_remote_enter`,
`classify_claude_launch` in `src/session.rs`). A typed `--resume <id>` is read as is
regardless of the setting. The id lives in `ClaudeHandle::remote_session`,
is persisted as `remote_session` on `SavedNode::Leaf`, and the relaunch is
`claude --resume <id> || claude -c || claude` (`remote::claude_command`). A pane with an id
never contends for its directory in `connect_pane`'s `claimed` set. It relies on Claude
keeping the id across `--resume`, which current versions do; if that changed, panes would
degrade to the `-c` fallback. A hand-typed `claude -c` is unknowable and stays `-c`.

A rejected alternative, for the record: a status-line script on the far host printing
`ESC ] 7777 ; arbiter ; session=<id> ; cwd=<path> BEL` for the reader to parse. It works
but needs a file copied to every host, which the user did not want (2026-09-12).

## Remote attach — how a local file reaches the far host

Ctrl+Shift+S, Ctrl+Shift+A and a file drop all end in `attach_paths` (`iced_shell.rs`). A
remote pane cannot read this machine's disk, so `src/attach.rs` copies the file first over a
second connection: `sftp`, chosen because its batch commands are protocol operations and
the far host's shell (cmd.exe included) never enters into it. The invocation comes from
the pane's saved ssh line (`remote::sftp_invocation`; `-p`→`-P`, `-l`→`user@`, the rest
dropped or kept by name). It runs inside a PTY so a password/passphrase prompt can be
answered from the `Vault`, with `-o BatchMode=no` placed BEFORE `-b` because ssh keeps the
first value it sees for an option. Copies land in `~/.arbiter/attach/<UTC stamp>-<name>`;
the pasted path is absolute, learnt from `pwd` in the batch. Cleanup is Arbiter's own daily
sweep (`attach::sweep`, `State::attach_swept`), globbing away stamps older than yesterday.
No secret was known → the same credential dialog opens (`ConnectKind::Attach`) and Submit
re-runs the copy. **ConPTY gotcha learnt here:** a PTY reader gets no EOF when the child
exits; the run watches the exit on its own thread, lets output settle, then drops the
master to close the console. Live harness: `cargo test --lib attach::tests::live -- --ignored`
(Windows, Git for Windows' sftp-server via `-D`).

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
