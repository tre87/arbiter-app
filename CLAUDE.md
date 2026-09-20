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

## Pane environment — how a shell knows it is in Arbiter

Every pane's shell gets `ARBITER_PANE_ID` (the shim keys its captures by it) and
`LC_TERMINAL=Arbiter` (`Session::spawn`, `session::TERMINAL_ENV`). The `LC_` prefix is the
point, copied from iTerm2: sshd accepts `LANG` and `LC_*` by default (macOS, Debian family),
so once the client's `~/.ssh/config` has `SendEnv LC_TERMINAL` under `Host *` the far host's
shell can tell it is drawn by Arbiter; nothing else crosses ssh without server-side
configuration. The user's Claude status line (`~/.claude/scripts/statusline.sh`, a git repo
synced to the Mac) reads both to emit Nerd Font icons instead of its plain fallback set
(2026-09-14).

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

## Wake on LAN

`src/wol.rs` builds and broadcasts the magic packet (255.255.255.255, ports 9 and 7, default
route only; no directed broadcast). Machines live in `Settings::wol_hosts` (MAC stored
canonical, `wol::format_mac`), the titlebar button behind `Settings::show_wol_button` (off by
default, right of the overview button). The menu is `State::wol_menu`, opened by the button or
Ctrl+Shift+M (`handle_key`; the chord is free in the app and indistinguishable from Enter to
programs in a terminal). While it is open, `Message::Input` is routed to it (arrows, Enter,
Space) and nothing reaches the PTY; Escape closes via `dismiss_top_overlay`. A sent packet
starts the checkmark (fast tick while `sent` is set) and `Tick` closes the menu after
`WOL_SENT_SHOW_MS`. The dropdown's x is estimated from the titlebar button widths in
`wol_menu_view`, not measured.

## Wheel over Claude: the frozen status row and the nudge

Claude Code enables mouse reporting, so a wheel notch over a Claude pane is handed to it
as a mouse report (`Message::WheelReport`) and Claude scrolls its own transcript. Its
fullscreen UI (2.1.27x) then stops animating the status row once the row has scrolled out
of view and does not resume when it comes back; only a change of the row's own phrase, a
focus event or a resize re-renders it (anthropics/claude-code#94443). Without spinner
frames Arbiter's working detection lapses after `WORKING_TTL_MS` and the working bar goes
too. After a gesture settles, `Message::WheelSettled` checks the pane every
`WHEEL_CHECK_MS` for `WHEEL_CHECKS` rounds; while no spinner glyph has been drawn for
`WHEEL_FROZEN_MS` (`Session::spinner_age_ms`) it nudges: a focus pulse (`CSI O` `CSI I`,
only if the program asked for focus events) and, if that changed nothing, a resize
(`Session::nudge_resize`: the PTY one column narrower and back 120 ms later, grid
untouched). Either nudge makes Claude clear and repaint the whole screen, so the pane's
frame is frozen for `WHEEL_NUDGE_HOLD_MS` first (`WakeHold::freeze`, via
`Session::hold_frames`), and the first frame after the thaw waits for the output to settle;
without that the repaints showed as a full-screen flicker. Measured 2026-09-18 with Claude
2.1.276 in a ConPTY probe: Claude asks for
focus events (`?1004h`) and ConPTY forwards them, but node hands them to Claude as console
focus records it ignores, so on Windows the pulse does nothing and the resize is what
works. An earlier run that seemed to show the pulse working was Claude's own phrase change
("thinking" to "still thinking"). While scrolled away Claude shows "Jump to bottom
(ctrl+End) ↓" (`VtTerm::visible_scrolled`, `ClaudeHandle::set_scrolled`): a turn live at
that moment is held as working until the view returns (`scroll_holds_working`), else the
frameless two seconds read as a turn end and raised a false "Claude finished" card; and no
nudge is sent while scrolled, a repaint there could lose the reader's place. Remove once
Claude fixes the issue. Rejected: scrolling
Arbiter's own scrollback instead (the transcript is not in it under the fullscreen UI) and
a row resize (shifts the whole layout for a frame; a column does not).

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
- **Icons (Private Use Area) come from a bundled font, no setting.** `font::SYMBOLS` is
  "Symbols Nerd Font Mono" (the icons-only Nerd Font, `assets/`, MIT, as WezTerm ships it).
  Each rasteriser tries it for a glyph the terminal font lacks, before the OS fallback,
  which has nothing for the PUA (DirectWrite: after the Segoe UI Symbol rule above, so
  Claude's own symbols are untouched; CoreText: before the cascade; swash: before fontdb).
  No font is looked up by name; the user's terminal font stays what it is. Windows Terminal
  makes you set a Nerd Font as the face instead; this was chosen so nothing is manual.
  Test: `raster::tests::icons_render_with_nothing_installed_or_configured` (Windows, in
  the suite). The macOS side cannot be type-checked from Windows (`objc_exception` needs a
  C compiler and the SDK), so CoreText changes are verified on the Mac.
- `fit_to_box` (`src/gpu.rs`) instead **center-clips** a mono fallback symbol that's only
  slightly wider than the cell (e.g. ✻), keeping full height, unless the clip would cut
  through solid ink (`clip_cuts_solid_ink`: a filled ⏺ came out as a square that way, so
  it scales down instead), and downscales anything
  larger. Don't re-add an "upscale undersized symbols" path — it enlarges glyphs like ⏵
  past their natural size (reverted once already).
- **Powerline separators (U+E0B0..=U+E0D7) are never fitted.** The straight four are drawn
  programmatically at exact cell size like box drawing (`gpu::draw_powerline_glyph`), the
  rest are stretched to the cell on each axis (`gpu::stretch_to_box`): a segment edge with a
  gap above or below shows as a notch, and uniform fitting of the bundled font's glyphs left
  one. They never take a second cell either (`raster::is_icon` excludes them).

## Memory: what is bounded, what is pooled, and how to look (2026-09-20)

A two-day, 18-pane run was taken apart with a region census (VirtualQueryEx by allocation
base, then reading the contents of the largest blocks) and isolated instances driven from
crafted `session.json` files. Findings that shape the code:

- **Per-pane renderers are freed via a retire list.** `Renderers` lives in iced's shader
  storage, reachable only from `TermPrimitive::prepare`; a dropped `Session` pushes its id to
  `session::RETIRED` and the next frame removes the entry. Every pane ever drawn used to
  keep its 5 MB of atlas copies forever (30 renderers for 18 panes).
- **One deadline thread** (`session::schedule`) replaces spawn-per-event for the redraw
  hold, the frozen frame, the cursor grace and the resize nudge. It parks on a condvar with
  no timeout while nothing is armed, so it is still event-driven under the no-polling rule.
- **A frame is rebuilt only when `VtTerm::generation` (plus cursor, background, canvas)
  changed** (`gpu::FrameKey`). Every `&mut self` method on `VtTerm` that can alter the
  picture must bump the generation; `term::tests::generation_moves_with_every_visible_change`
  pins the list. Atlas uploads are per dirty rectangle, and a full atlas flushes.
- **Scrollback while Claude owns a pane is `term::CLAUDE_SCROLLBACK`** (the reader loop
  switches it on the `claude_running || on_screen` edge). Blank pre-allocated alacritty rows
  were the bulk of the Rust heap: rows are allocated 1000 at a time as history grows.
- **The 16 MiB zero-filled heap blocks** seen in the long run (13 to 14 of them, appearing
  under sustained 60 fps rendering, occasionally recycled) are not produced by any Rust
  allocation site in this tree or its dependencies, and `ARBITER_MEM_DIAG` saw none in
  isolated runs. The best-fitting owner is the NVIDIA in-game overlay's capture hook
  (`nvspcap64.dll` was loaded in the process). Disable the overlay to test.
- **Do not attach process-memory readers, ETW heap tracing or keystroke automation to a
  running Arbiter from a shell descended from it.** Defender's behaviour classifier
  attributed exactly that to `arbiter.exe` (Trojan:Win32/Bearfoos.A!ml) and killed the
  app. Use `ARBITER_MEM_DIAG`, `Get-Process` counters, or an elevated console you open
  yourself.
