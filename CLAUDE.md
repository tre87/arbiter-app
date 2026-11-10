# Arbiter — project notes for Claude

Arbiter runs many Claude Code sessions side by side (and, experimentally, GitHub Copilot
CLI and OpenAI Codex CLI; see "Agent adapters"). This is the **native** app: Rust +
iced 0.13 + wgpu, with a custom GPU terminal renderer (alacritty_terminal for VT parsing,
per-OS glyph rasterization). It replaced an earlier Tauri/Vue web app; the native app is at
the repo root. The user-facing binary is `arbiter` (source: `src/bin/iced_shell.rs`).

## Build / run / test

- Build / run: `cargo build --bin arbiter` · `cargo run --bin arbiter`
- Unit tests: `cargo test --lib`
- **Live agent check** (a signed-in Claude on PATH; spends one short turn): after
  `cargo build --bin arbiter`, `cargo test --lib agent_shim::tests::live -- --ignored --nocapture`
  runs a prompt through the generated launcher and asserts the pane's event log got the
  session and the turn end. `term_signal::tests::live` probes which terminal sequences the
  platform PTY forwards (ConPTY forwards OSC 0, 9, 9;4, 777 and BEL, 2026-09-24).
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
- **True idle: when nothing is moving, Arbiter draws no frames.** Not few: none. A
  window that keeps presenting, even once a second, makes the Windows GPU driver hold
  the system timer at 1 ms and keeps the display and GPU awake, so a focused Arbiter
  stopped the machine idling or sleeping (fixed in 1.0.11). The rule, concretely:
  - **One clock, and one gate on it.** The only repaint clock is the 16 ms `Tick`, and
    it exists only while `needs_fast_tick` says so (`subscription`). Everything else
    repaints on an event: PTY output (`term_wake`), input, a window event, a hook.
  - **What may turn the clock on is something moving on screen that will stop by
    itself:** an agent that is Working (its bar, its banner), a robot flying to its
    place, a scroll indicator fading, a refresh the user clicked (capped at
    `USAGE_SPIN_MAX_MS`), a file copy in flight, the first usage load (capped at
    `USAGE_PENDING_TIMEOUT_MS`), the Wake-on-LAN checkmark. Each entry is tied to
    work in progress or has a time limit. Adding one without either breaks the rule.
  - **Waiting is not moving.** An agent at a prompt (Attention), an idle agent, a
    finished turn, a shell left running: all are still pictures. That is why the
    attention dot is solid and not pulsing, the usage button has no countdown, and an
    idle banner is a still. An idea that needs motion while idle (a fish biting) can
    only be done as a different still.
  - **A deadline is one event, not a clock.** Something due later (a card closing, the
    banner's done flag coming down, a hold running out) is a single scheduled message
    or a `session::schedule` deadline. A cadence that must run while idle (the 120 s
    usage refresh) lives on a background thread and wakes the UI only when it has
    something to show.
  - **Check it:** leave Arbiter with idle agents and nothing on screen changing, and
    its CPU should sit at zero with no frames presented. Do this after touching
    `needs_fast_tick`, a subscription, or anything a watcher triggers.
- **Every `git` read reached from a watcher needs `--no-optional-locks`.** A plain
  `git status` refreshes `.git/index`, and a watcher over the working tree sees that as a
  change and runs the status again, forever. The explorer shipped that loop once
  (`git::file_status`, 4.7% of a core on an idle repo). Measure idle CPU after touching
  anything a watcher triggers.
- **Three iced crates are forked** under `vendor/` and applied via `[patch.crates-io]`, and so is
  `wgpu-hal` (the DX12 swapchain presents 1:1, see the graphics bullet under Memory):
  `iced_winit` (inactive notification windows, no present while occluded,
  `conversion::OCCLUSION_HOOK`, which tells the app a window was hidden or shown
  again: macOS only, winit reports no occlusion on Windows, and the rebuild after a
  lost GPU device, `recover_graphics`), `iced_widget` (two changes: the text
  editor hit-tests with the padding on swapped axes, and `Content` does not expose the
  editor whose scroll the line-number gutter is drawn from; see
  `vendor/iced_widget/ARBITER-FORK.md`) and `iced_wgpu` (what lets a lost device be
  freed; see "A lost GPU device" below and `vendor/iced_wgpu/ARBITER-FORK.md`). An iced
  upgrade means re-copying all three from the registry and re-applying their diffs.
- **Verify UI changes with a debug build**, not just `--release`. iced states several
  layout contracts as `debug_assert!`, which release compiles out: a release build runs
  a mis-specified layout silently where `cargo run` panics at once. A panic goes to
  `panic.log` in the data dir, and `ARBITER_OPEN_FILE` opens files in the editor at
  startup so a fault reproduces without driving the UI by hand.

## Agent adapters: one file per agent CLI (2026-09-24)

Everything specific to an agent CLI lives in `src/agent/<id>.rs` behind the `Agent` trait
(`src/agent/mod.rs`); the rest of the app talks to agents only through it. **Adding an
agent is one file plus one line in the `agents!` registry**, which also defines `AgentKind`
(serde lowercase, the saved form). An adapter supplies: which process it is
(`matches_process`, fed argv with leaked `KEY=VALUE` entries already dropped by
`procscan`), how its launcher hands it Arbiter's hooks (`launch`: files, env, and
`(flag, env var)` arguments, so the launcher scripts never quote a value), how its hook
payloads map onto `AgentEvent` (`normalize_hook`), its `ScreenProfile` (pure data: chrome,
working footer and rows, chooser rule, background-work rows, prompt shape, activity glyphs)
and `SignalProfile` (titles, OSC 9 / 777 notifications, OSC 9;4 progress, bell), and how to
resume (`resume_local` never falls back to "most recent here", `resume_remote` does).

- **Shared machinery.** `agent_shim` writes a POSIX and a `.cmd` launcher per agent binary
  (marker "Generated by Arbiter" in the first 128 bytes, real binary in
  `ARBITER_REAL_<BIN>`; on Windows only for a binary found at setup, since the `.cmd`
  cannot look one up later). A launcher marks the agent it starts
  (`ARBITER_AGENT_LAUNCHED`), and an agent that agent runs (Claude's Bash tool running
  `claude -p` or `codex exec`) is launched untouched: its hooks would otherwise speak for
  the pane's own agent. It also serves `arbiter agent-hook <id> [args]`, which always exits 0
  and never writes stdout (SessionStart stdout becomes Claude context; exit 2 from Stop
  would block it). The hook appends to `<data>/agent-events/<ARBITER_PANE_ID>.jsonl`, one
  `{"v":1,"agent":…,"ev":…,"sid":…}` line per event; `agent_watch` applies each pane's
  log from a byte offset, so two events inside one debounce are both kept. Events for a
  pane whose agent is not running are dropped except `SessionBound`, and the OSC-133 idle
  edge deletes the log (`AgentHandle::end_session`). The 1.6 subcommands stay for one
  release: `claude-hook` runs the Claude hook, `claude-statusline` does nothing.
- **Each agent can be switched off (Settings, Agents, 2026-10-09).** `Settings::agents`
  holds the choices made by hand, by `Agent::id`; an agent absent there is on unless
  `Agent::experimental()` (Copilot, Codex). The app applies them at startup, before the
  first shell sets up the launchers, into `agent::set_enabled` (a bitmask; every agent
  until then, which is what the tests see). Off means no new binding by any source:
  `procscan::agent_under`, `VtTerm::detect`, `session::classify_launch` and hook events
  in `agent_watch` (except for a pane already bound to that agent, followed until it
  exits) all skip it, a saved pane's agent is not resumed (`saved_to_config`), and its
  launchers leave the PATH (`ShimSetup::sync_launchers`, also run on each toggle). The
  launch env is exported for every agent regardless, so one switched on works at once in
  an open shell. Tests flip nothing global: `sync_launchers_by` takes the decision.
- **Arbiter no longer owns any agent's status line.** Claude's session id comes from a
  `SessionStart` hook (startup, resume, `/clear`, compaction), with `transcript_path`,
  which is what the resumable check tests. A user with `disableAllHooks` gets no id and
  restores to a bare `claude`.
- **State.** `AgentHandle` (`agent_state.rs`) is the lifecycle state machine for every
  agent; `ConnHandle` (`conn.rs`) holds the SSH side. The few transitions touching both are
  free functions in `session.rs` (`connection_edge`, `forget_connection`, `note_screen`).
  Which agent a pane runs is bound by source, hook > process scan > typed command (or the
  follow-up Arbiter itself queued) > chrome; a weaker source cannot rebind a live agent,
  and a new kind resets the lifecycle but never `finish_seq`, and moves `epoch`, which the
  UI (`AgentSeen`) treats as a first sighting. Edges decide events, levels decide states,
  and a visible level beats a contradicting edge; a hook's `TurnStarted` holds Working for
  at most `TURN_START_HOLD_MS`.
- **Remote detection** reads the bound agent's chrome, or with none bound
  `VtTerm::detect`, the first profile whose chrome matches and whose `chrome_veto` does
  not (Codex draws Claude's "? for shortcuts" beside its "% context left").
- **Saved layout.** `SavedNode::Leaf` has `agent: {kind, session, remote}`, read leniently
  (an unknown kind costs that pane its agent, not the whole layout); the four Claude
  fields are still written for Claude panes so a downgrade to 1.6 relaunches Claude, and
  `SavedNode::saved_agent` reads either.
- **Claude's screen is pinned against the old code.** `agent_screen::tests` keeps the
  pre-adapter Claude matchers verbatim as an oracle and checks every row and screen of a
  corpus against them; a profile change that alters a Claude reading fails there.
- **Copilot and Codex are UNVERIFIED** (built from Copilot 1.0.88's docs and Codex
  rust-v0.156.1's source; neither was installed when written). Copilot: hooks through a
  plugin dir (`--plugin-dir`, root `plugin.json` plus `.claude-plugin/plugin.json` for
  copilot-cli#2010, `hooks.json` with `exec`/`args` entries, each told its event name as an
  argument), `--session-id` up front, `--resume=<id>`, OSC 9;4 progress as working, an
  empty chrome list until a recording exists. Codex: hooks need a trust hash of a
  normalized TOML identity that cannot be reproduced safely from outside (a mismatch is
  skipped or shown as "needs review"), so it uses `-c notify=[arbiter, agent-hook, codex,
  --notify]` with call-through to the user's own top-level `notify`
  (`ARBITER_CODEX_USER_NOTIFY`), `-c tui.notification_method='osc9'` and
  `notification_condition='always'` (a pane never reports focus), its title (Braille
  spinner = working, "Action Required" = attention), and `codex resume <id>`; no id can be
  set up front. To verify either: run it in a pane with `ARBITER_CLAUDE_DEBUG=1` (the hook
  logs its raw payload), `ARBITER_RECORD_PTY=1` and `ARBITER_SCREEN_DUMP=1` (recordings in
  the temp dir; they hold whatever was on screen, secrets included), then turn what they
  show into the adapter's fixtures and profile.

## Remote Claude resume — how the session id is known

Local panes resume exactly (`claude --resume <id>`) because the `SessionStart` hook Arbiter
hands Claude reports each pane's session id (see "Agent adapters"). Over SSH nothing on the
far host can tell Arbiter that, so **Arbiter names the conversation** when the setting
`name_remote_claude_sessions` is on (Settings, General; off by default because it visibly
edits typed input): on Enter in a remote pane, a bare launch of an agent with a
`name_flag` (`claude`, `copilot`) at a shell prompt is completed with ` --session-id <uuid>`
before the CR goes through (`Session::on_remote_enter`, each adapter's `classify_launch`).
A typed resume naming an id is read as is regardless of the setting. The id lives in
`AgentHandle::remote_session`, is persisted as `remote_session` and in `agent` on
`SavedNode::Leaf`, and the relaunch is the adapter's `resume_remote` (Claude:
`claude --resume <id> || claude -c || claude`). A pane with an id never contends for its
directory in `connect_pane`'s `claimed` set. It relies on Claude keeping the id across
`--resume`, which current versions do; if that changed, panes would degrade to the `-c`
fallback. A hand-typed `claude -c` is unknowable and stays `-c`. Codex cannot be given an
id up front, so a remote Codex comes back by the id only when one was typed
(`codex resume <id>`), else through `codex resume --last`.

A rejected alternative, for the record: a status-line script on the far host printing
`ESC ] 7777 ; arbiter ; session=<id> ; cwd=<path> BEL` for the reader to parse. It works
but needs a file copied to every host, which the user did not want (2026-09-12).

## Pane environment — how a shell knows it is in Arbiter

Every pane's shell gets `ARBITER_PANE_ID` (the agents' hooks key their events by it) and
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
(ctrl+End) ↓" (`ScreenProfile::scrolled`, `AgentHandle::set_scrolled`): a turn live at
that moment is held as working until the view returns (`scroll_holds_working`), else the
frameless two seconds read as a turn end and raised a false "Claude finished" card; and no
nudge is sent while scrolled, a repaint there could lose the reader's place. Only agents
whose adapter says `wheel_nudge()` get it (Claude), and only while the pane is Working:
idle or at a prompt nothing animates, so every gesture read as frozen, and the repaint
cleared and redrew a waiting plan-approval prompt, which raised its card again (fixed
2026-10-05). Remove once Claude fixes the issue.
Rejected: scrolling
Arbiter's own scrollback instead (the transcript is not in it under the fullscreen UI) and
a row resize (shifts the whole layout for a frame; a column does not).

## Notification cards and what counts as a turn end (2026-09-21)

- **A card must never take the keyboard.** Each card is its own iced window opened with
  the fork's `NEXT_WINDOW_INACTIVE`. On Windows the flag lives in winit's window state, so
  iced's hidden-then-`set_visible(true)` route shows it `SW_SHOWNOACTIVATE`; on macOS
  `set_visible(true)` is `makeKeyAndOrderFront` and stole the key window from the main
  one, so the fork (`vendor/iced_winit/src/program.rs`) creates such a window visible and
  lets winit's creation `orderFront` it. Ctrl+Shift+P raises a test card
  (`Message::TestNotification`), Settings or not.
- **Working is read from "esc to interrupt", which 2.1.28x draws in the footer**, in the slot
  "? for shortcuts" holds while idle, not on the spinner row as older releases did
  (Claude's `ScreenProfile::working_footer`, read by `VtTerm::screen_state` in the same
  cursor-anchored window as `VtTerm::chrome`). The hint giving way to the idle box is the
  hookless turn end, counted only if it still holds `ROW_END_CONFIRM_MS` later with no
  chooser up (`AgentHandle::confirm_row_end`): a mode line keeps the box reading as present
  all turn, so one split repaint would otherwise be a finish. Esc or Ctrl+C
  (`note_interrupt`) voids it; the Stop hook never fires then. Esc arrives as
  `Message::EscapeKey`, not `Message::Input`, so both handlers call it.
- **A prompt leaving the screen is confirmed, not read.** A full repaint (resize, the wheel
  nudge) clears the screen before redrawing a chooser, and reading that gap as "answered"
  raised a second card and turned a user-opened `/model` into Claude's question. So the
  menu counts as gone only after `MENU_GONE_CONFIRM_MS` without it
  (`AgentHandle::menu_gap`), and only then are the hook's attention and the slash-command
  claim cleared.
- **A conversation being painted at its start is not work (2026-11-10).** Continuing a
  conversation (`claude -c`, `--resume`) flashed working for one TTL: the transcript is
  painted in chunks, its old spinner rows ("✻ Brewed for 56s") differ between chunks,
  and that pairs like a spinner animating (`note_activity`). A new conversation has no
  such rows, so a first launch never did it. `bind` and `SessionBound` now open a
  `START_QUIET_MS` window in which frames cannot START working (the same
  `suppress_activity` a resize uses); the status row, title and progress are levels and
  still do, and Enter ends the window (`clear_suppression`). Found with
  `ARBITER_CLAUDE_DEBUG=1`, which logs each lifecycle change with every input it read
  (`lifecycle:` lines, `AgentHandle::why`); a headless resume did not reproduce it.
- **One finish is counted once.** The Stop hook and the row end (or, for Codex, `notify`
  and its OSC 9) report the same turn end; a second within `FINISH_DEDUPE_MS` with no
  work in between is dropped (`AgentHandle::count_finish`).
- **Waiting on background agents is not idle.** "Waiting for N background agents to
  finish": the turn is over (Stop hook fired, spinner still), yet Claude resumes by itself
  when they report, and they always do. The reader scans the visible rows for the row's
  shape (Claude's `ScreenProfile::waiting_rows` in `agent/claude.rs`, matched by
  `agent_screen::waiting_in`: a spinner glyph, a space, the phrase; prose starts with a
  bullet or an indent), and only as the last row above the input box: aborting the agents
  leaves the row in the transcript with "● All background agents stopped" under it, and
  anywhere-on-screen held the pane Working for good. With no input box on screen any
  visible row counts. `AgentHandle::set_waiting_background` holds `Working` while it shows,
  which also keeps the Stop hook from raising "Claude finished". The off edge stamps a
  fresh activity TTL so the resumed turn's first frames have time to pair up; Escape
  during the wait ends it the same way, one TTL later, with a "Claude finished" card.
- **A background shell is not work (2026-10-09).** A shell Claude left running (a dev
  server) can run all day, and holding the pane Working for it kept the 60 fps clock on
  and the "finished" card back for as long as it lived. It is a count beside the pane
  instead: Claude's mode line under the box says "⏵⏵ auto mode on · 1 shell · ← for
  agents" (2.1.291, captured in a ConPTY: "⏸ manual mode on · 2 shells"), read in the
  cursor-anchored chrome window (`ScreenProfile::shells`, `agent_screen::shell_count`, a
  whole `·` segment, so the summary line's "· 2 shells still running" never counts). It is
  read only with the chrome on screen; a chooser over the footer keeps the last count. It
  shows as a static muted chip (console icon, "1 shell") in the pane header and the
  overview, a hollow green ring around the workspace tab's dot (a layer apart: the dot
  keeps its amber > azure > green order and the ring hides nothing), and "· 1 shell
  running" on the "finished" card. The quit dialog Claude shows ("Background work is
  running", each shell listed) is this same count.

## The agent banner: pixel art at the top of an agent's terminal (2026-11-10)

It replaced the Agents Office (a room of desks in its own window, removed). The long
version of this section, with what was tried and thrown away, what was measured and what
is still open, is `docs/AGENT-BANNER.md`; read it before changing when the banner shows
or what counts as working. Three files:

- **`src/agent_banner.rs` draws it**, with rectangle fills only: `render(scene, w, t)` is
  a pure function to an RGBA strip `H` (46) art pixels tall at any width, the world
  anchored on the centre column so a resize never moves scenery already in view. Robots
  do rounds (`stop`, `act`): three jobs near one another, then somewhere else in the
  strip; a stop within `WALK_MAX` is walked to, a further one flown to. `Troupe` is the
  one stateful part: it remembers where each robot was drawn and flies it to a place
  that has jumped (strip resized, turn ended and it goes fishing, helper added), and
  flies a helper that is no longer wanted off by the nearer edge instead of dropping it.
  `cargo test --lib agent_banner::tests::sheet -- --ignored --nocapture` writes frames to
  the temp dir; judge art changes by looking at them.
- **`src/bin/pane_banner.rs` puts it in panes.** One `PaneBanner` per pane in
  `State::banners`, keyed by `history_id` (survives a reconnect and a shell switch).
  `refresh` runs after every message (the `update` wrapper), draws only frames that
  changed, and only for the workspace on screen. No clock of its own: frames come from
  the fast tick, which runs while any agent works, plus `needs_fast_tick` while a robot
  is flying to its place (`settling`). Scene time is the pane's working time since the
  app started and is never reset, so the day and the weather carry on between turns.
- **A banner that comes and goes lies over the terminal; only a permanent one has rows
  of its own.** With `banner_hide_idle` (the default) the banner is a layer over the
  terminal's first rows (`pane_banner::overlay`, in the terminal's own stack): the PTY
  is never resized for it, and what it covers is hidden while the agent works. It
  first took rows from the terminal instead, which resized the PTY and made the agent
  repaint at every turn's start and end; the content jumped each time, and it looped
  (2026-11-10): the banner went, the repaint read as work, the banner came back, for
  as long as the pane sat idle. The misread was an old "Waiting for N background
  agents" row in the transcript, which counts as live while a repaint has not drawn
  the input box yet. With the setting off the banner stays between turns, so it has
  rows of its own (`pane_banner::slot`, `PaneBanner::pushing`) and the PTY is resized
  when the agent starts and exits, or when the banner stops fitting. For that mode
  the guards stay: `settle` calls `suppress_activity` on the edge,
  `AgentHandle::set_waiting_background` does not believe a row first seen inside that
  window, and a banner that changed within `RECENT_FLIP_MS` only changes back once the
  opposite has been asked for `STEADY_MS` without a break (`follow`). The slot in the
  pane's column is always there, at no height without a banner of that kind, so the
  terminal keeps its place in the widget tree (see the iced conditional-wrap note). A pane under `MIN_W` art pixels wide or `MIN_PANE_H` high
  gets no banner and an icon in its header instead (`pane_banner::fit`).
- **Settings:** `show_agent_banner` (off by default; the titlebar's robot button toggles
  it) and `banner_hide_idle` (on by default).
- **Hidden when idle, the banner follows the working bar, exactly** (`pane_banner::wanted`,
  `overlay`): there while the lifecycle is Working, gone otherwise, the same rule the
  bar uses, so the two cannot disagree. Do not add holds to it. Three were tried on
  2026-11-10 and each read as wrong to the user: staying while sub-agents ran under an
  agent between turns, staying as the green flag for `DONE_MS` after a turn's end
  ("work did not stop" after a cancel), and waiting before leaving (still blinked).
  What made it steady was fixing what counts as working for the whole pane (see "An
  agent paused on its sub-agents is working" below), not the banner. With the setting
  off the banner is permanent, and there it shows the done flag for `DONE_MS`
  (`flag_is_up`: Esc or Ctrl+C since takes it down).
- **Sub-agents are helper robots.** `Scene::helpers` is a mask of slots, each a robot of
  its own (colour, round); `PaneBanner::seat` keeps a sub-agent in one slot while it
  runs, so the robot that leaves is the one whose sub-agent finished. Width decides how
  many are drawn (`helper_room`, one per 30 art pixels); `MAX_HELPERS` is the ceiling.
  They are known from the agent's own reports: `AgentEvent::SubagentStarted` / `SubagentStopped`, which
  Claude's adapter maps from its `SubagentStart` and `SubagentStop` hooks (`agent_id`).
  `AgentHandle` keeps the live ids, not a number, so a repeated start is one and a stop
  whose start was missed changes nothing; `AgentStatus::subagents` is the count. A
  turn's end does not clear them (a background sub-agent outlives its turn); Esc or
  Ctrl+C (`note_interrupt`), a new conversation and the agent exiting do, and one with
  no stop after `SUBAGENT_MAX_MS` is dropped. Copilot and Codex report none. Live check:
  `cargo test --lib agent_shim::tests::live_claude_reports_a_subagent -- --ignored
  --nocapture` (measured with Claude 2.1.322: start and stop carry the same `agent_id`).
- **An agent paused on its sub-agents is working (2026-11-10).** A Claude given long
  work starts sub-agents in the background, ends its turn at once (the Stop hook
  fires), and is woken as each reports for a turn of a second: one job was dozens of
  turns, and the bar, the dot, the banner and the "finished" card followed every one.
  Now `AgentHandle::waiting_on_agents` (the old screen row OR any sub-agent running)
  holds the lifecycle at Working and keeps a Stop or a row end from counting as a
  finish. The signals, all Claude's own and measured live:
  - `SubagentStart` / `SubagentStop` keep the set of running ids current.
  - Every `Stop` carries `background_tasks` (`{"id","type","status",…}`; documented as
    there to tell "done" from "paused waiting for background work"). The adapter turns
    its running `subagent` entries into `AgentEvent::BackgroundAgents`, emitted BEFORE
    `TurnEnded`, which replaces the set: a lost start or stop is put right at the next
    turn end. A final Stop has an empty list and is the one finish.
  - The `idle_prompt` notification (a minute after a real stop, and held back by
    Claude while a background agent runs) empties the set too.
  - Claude also fires `SubagentStop` for helpers of its own it never announced (ids
    not in any list, `agent_type` empty, about a second after each Stop). Unknown ids
    change nothing, which is why this is a set and not a counter.
  - When the last sub-agent stops, `agents_done_ms` holds Working for one TTL, until
    the turn Claude is woken for draws its row; a Stop since ends the hold.
  - **An empty list is not always the end.** A sub-agent that finishes while Claude
    is in a wake-up turn has not been heard yet: that turn's Stop lists nothing, and
    Claude is woken for the report a tenth of a second later. A real run's log showed
    the pane idle for a second there, twice in one job, each with a false finish. So
    `UserPromptSubmit` is registered, only to hear the wake-ups (a prompt starting
    `<agent-message` or `<task-notification`; a typed one emits nothing), and a Stop
    is held (`turn_ended`, `WAKE_WAIT_MS`) when a sub-agent that had outlived a turn
    stopped after the last wake-up (`report_owed_ms` against `turn_started_ms`). The
    finish is counted when that wait runs out with no wake-up; a wake-up cancels it.
    Claude can also hand a report over in the middle of a turn that is still calling
    tools, and then the wait is 1.5 s of working after the real end: bounded, rare.
  A `shell` entry is deliberately not counted (see "A background shell is not work"):
  nothing tells `sleep 20` from a dev server, so a Claude waiting on a background
  command still reads as idle between turns. The other task types (monitor, workflow,
  teammate, cloud session, MCP task) are unseen and left out. Live check, which fails
  if the pane reads idle mid-job or counts more than one finish:
  `ARBITER_LIVE_CWD=<a folder Claude trusts> cargo test --lib
  session::tests::live_background -- --ignored --nocapture`.
- **`src/bin/banner_preview.rs`** is a test window (`ARBITER_OPEN_BANNER=1`): one fake
  pane, a menu for the state, helpers, hour and weather. Use it to work on the art.

## Claude usage bars: a webview in a helper process

The titlebar meters (5h, 7d, per-model weekly caps) come from claude.ai's own usage API,
read by a hidden webview signed in to claude.ai, in a second process that is this same
binary run with `--usage-helper` (`src/bin/usage_helper.rs`). It posts one JSON line per
fetch on stdout; the app drives it over stdin (`show`, `fetch`, `reload`, `org:<uuid>`,
`signout`), owns the 120 s cadence on a background thread, and climbs a ladder when the
helper goes silent (refetch, reload, restart the process). Nothing here touches Claude
Code's login, token or hooks. `docs/CLAUDE-USAGE.md` has the whole of it: the protocol,
the Windows hidden-renderer handling, the state rules, the dead ends, and what to change
where. Read it before touching the helper or the poll.

## Priority under full CPU load (Windows, 2026-10-09)

With a CPU-bound program on every core (the Fenfire balance runner), the window stayed
responsive but the terminals froze for seconds to minutes. Measured headless with 32 busy
threads on 16 cores, typing into a Claude pane through `Session`: the redraw wake, which
passes through a timer (the `WakeHold` settle on the `session::schedule` thread), stalled
past 3.5 s, and keystroke echo, which also passes through the pane's console host and
agent, reached a p90 of 278 ms; the hops that wake for I/O stayed under a second. The
likely cause is that Windows boosts a thread woken for input or I/O ahead of busy threads
of its priority more readily than one woken by a timer (the working tick is a tokio timer
too); the boost rules themselves were not measured. With everything raised: 26 ms and
4.5 ms; Arbiter alone fixed the redraws, not the echo.

`src/priority.rs` raises, all to ABOVE_NORMAL, each from an event and never on a timer:
Arbiter itself (`raise_self`, GUI path of `main` only); each pane's shell and every console
host Arbiter owns (`raise_pane`, as `agent_monitor` starts); the shell a launcher started
(`raise_launched_shell`, at the pane's first OSC-133 prompt in `reader_loop`: Git Bash's
`bin\bash.exe` runs `usr\bin\bash.exe`); the agent or ssh client the busy-edge scan finds
(`procscan::agent_under` / `ssh_under` return the pid); and the agent whenever its hook
reports a session (`raise_agent_under`, from `agent_watch`, via `AgentHandle::shell_pid`),
which covers one the scan's ten-second window missed. A raised class is not inherited (only
idle and below-normal are), so whatever a pane runs stays normal. Tests:
`priority::tests::a_pane_runs_above_normal_and_what_it_starts_does_not` (shell, host,
launched shell, and a command under it staying normal) and
`an_agent_its_hook_binds_is_raised` (a copy of the test binary named `claude-probe-*.exe`
as the agent). Use a bounded burner for any load test: it freezes the panes of an older
Arbiter, this session's included.

## A lost GPU device: a driver update is survived, not a crash (2026-10-19)

Installing an NVIDIA driver under a running Arbiter crashed it: wgpu 0.19 panics when the
device is lost (`Error in Queue::submit ... Parent device is lost`; a failed submit,
present or surface configure is fatal there, no error handler sees it). Now:

- **The iced_winit fork catches it** (`gpu_call` around present, surface configure and
  window creation) and runs `recover_graphics`: every window moves to iced's software
  renderer (`C::with_backend(.., Some("tiny-skia"))`) so all of the old device's objects
  can go first, then a new compositor is made and every window gets a new surface and
  renderer from it. App state is untouched; the terminal renderers live in iced's
  shader storage and are rebuilt by the next `prepare`. It waits up to 4 s for a
  hardware adapter (mid-update only WARP is listed), then settles for a fallback and
  tries for the GPU again on frames, every 10 s for 5 minutes.
- **The old device must really be freed.** On DX12, while anything of a removed device
  lived, every device made after it on that adapter was removed too, and the window's
  old swapchain blocked a new one. Three things in wgpu 0.19 kept it alive: a resource
  dropped after its device is never freed (iced dropped its engine after the device),
  the staging belt's `map_async` callbacks hold buffers until a poll, and a panic inside
  present dropped the frame, view and encoder while unwinding, when wgpu skips their
  cleanup. The iced_wgpu fork fixes all three (`vendor/iced_wgpu/ARBITER-FORK.md`).
- **Arbiter's own errors never panic.** `on_new_device` (first terminal frame on each
  device) replaces wgpu's panicking error handler with logging: `TermPrimitive::prepare`
  uploads while holding the pane's terminal lock, and a panic there poisoned it, which
  killed the pane's reader for good. A lost D3D12 device does not always say "lost": an
  upload fails as "Creation of a resource failed".
- **Test it with `ARBITER_TEST_GPU_LOSS=<seconds>`**: each device is removed that long
  after its first terminal frame (`ID3D12Device5::RemoveDevice` on DX12, `destroy()`
  elsewhere), three times a run at most. The loss shows on the next frame, so something
  must redraw (a resize does). Never force a system-wide reset (`dxcap -forcetdr`): it
  takes down every other Arbiter running.

## A restored layout starts in order (2026-11-10)

All of a saved layout's shells used to start at once. On 42 panes they reached their
prompts together 5 to 6 s after launch, the visible ones included, and the 7 Claudes
they resumed reported at 7 s. `src/start_queue.rs` starts them a `Rank` at a time:
the workspace on screen, then panes with a local agent to resume, then the rest. The
next rank begins when every shell of the one before gave up its `StartSlot`: at its
first prompt (`StartupGate::note_prompt`), or for an agent pane when the agent's hook
reports a session (`agent_watch`, `AgentHandle::start_slot`), or when it exits, or
after `SHELL_MAX` / `AGENT_MAX`. Everything still starts at launch.

- **A restored pane exists before its shell does.** `Session::spawn_queued` builds
  the session and queues a `Launch` (the child, its exit watcher, the agent monitor);
  `restore_workspaces` calls `start_queue::global().start()` once the whole layout is
  queued. Until its turn the pane is blank, resizes, and holds what is typed into it:
  the writer thread waits for the launch, so a resumed agent's command goes in after
  the shell exists, as before. Every other pane (`Session::spawn`) starts at once.
- **No cap on how many start together.** Five at a time put the last shell at 10.7 s
  against 6 s: about half a second of a shell's start is waiting, so a cap only lowers
  the rate. And with the rest released at the agents' prompts, not at their session
  report, the Claudes took 4 s to report from there, not 1.
- **Measured** (macOS, ten cores, one run each): visible panes at 1.2 s, Claude
  reporting at 3 s, the last shell at 8 s (2 s later than before), all ready at 8 s
  (was 9 s). Windows, 31 panes with 11 Claudes, PowerShell and Git Bash (2026-11-10,
  from the debug log of two runs): focused shells at 0.5 s, every Claude reporting by
  3.5 to 5.3 s, the rest started after that, no pane left blank. A restored ssh pane
  under the queue is untested on either platform.
- **To time a launch:** copy `session.json` to a scratch folder, run the release build
  with `ARBITER_DATA_DIR` there plus `ARBITER_RECORD_PTY=1` and `ARBITER_CLAUDE_DEBUG=1`
  from a clean environment (`env -i`), and read each pane's first `OSC 133` from the
  recordings and the `SessionBound` lines from the debug log. It resumes the saved
  agents a second time, so leave out any conversation in use.

## Pane folders survive a crash (2026-10-19)

A crash used to bring panes back in the wrong folder, for three reasons, all fixed:

- **Shell integration is installed once per run** (`shell::install_integration`). Every
  pane spawn rewrote `arbiter-init.ps1` (and the zsh files), and a PowerShell started a
  moment earlier holds it open, so the write failed with os error 32 and the pane fell
  back to a bare `powershell -NoExit`: no OSC 7, no OSC 133, no private history. Four of
  26 PowerShell panes of one restore came up like that (their command lines show it).
  A file whose content is unchanged is never rewritten, and one that cannot be updated is
  used as it is. The scripts live under `app_data_dir()`, so a debug build no longer
  rewrites the installed app's copy.
- **A pane's cwd starts as the folder it was spawned in** (`Session::spawn`), so a save
  before the first prompt, or from a shell that never reports one, keeps it.
- **A local `cd` sets `SAVE_DIRTY`** (the OSC 7 handler), so it is on disk at once. The
  remote folder always did. `persist::save` writes a sibling file and renames it over
  `session.json`, so a crash mid-save leaves the old layout whole.

## The editor is the viewport, and the gutter follows it (2026-09-22)

`text_editor` is `Length::Fill` and scrolls itself, so cosmic-text shapes and iced
highlights only the lines on screen. The line numbers are a small widget
(`src/bin/gutter.rs`) stacked over it inside its own left padding, drawn from
`content.editor().buffer().scroll()` every frame, never from a copy kept alongside it,
which is what the earlier whole-document layout was avoiding by brute force, and what made
an open cost a shaping pass over every line of the file, three times over. Consequences
worth knowing:

- **The scroll in the buffer is not where the editor will settle.** `Editor::perform` applies
  a wheel notch as a raw `scroll.vertical += lines * line_h` and does NOT re-shape, so the
  value `view` reads is unclamped; the buffer only settles it at the next layout, after the
  frame has been built. Mid-document the two draw the same picture, which is why this looked
  right at first: at either end they do not, and the numbers scrolled in files the text could
  not scroll. `editor::gutter_window` applies the same clamp cosmic-text is about to apply.
  Anything else reading that scroll needs to do likewise.
- **Never build a `Content` with `with_text` for a file.** It makes a buffer with no height,
  so cosmic-text lays out the whole document, and the first layout throws all of it away when
  it applies the editor's font: 2.8 s of a 6.9 s open (debug, 2,400 lines). A file is pasted
  into a buffer the widget has already sized (`set_text`).
- **A buffer that has been on screen is carried to the tab being opened**
  (`take_spare_buffer`), because it is already sized: that is what puts a file's text in the
  same frame the editor appears in rather than the frame after. Only the first open of a
  session has nothing to carry, and it fills one frame later (`Msg::Fill`).
- **Both of cosmic-text's bulk edits are quadratic in the document**, for the same reason
  each time: a per-line push onto the front of a vector.
  - Insert: paste in blocks (`PASTE_LINES`): 191 ms for 10,000 lines against 6.5 ms.
  - Delete: no way around it from outside, so a buffer over `CARRY_MAX_LINES` is not carried
    over at all (48 ms to clear 10,500 lines, against the 16 ms frame it would save).
- `ARBITER_TIME_OPEN=1` prints the phases of every open and reload to stderr, and the number
  of frames before the text was on screen.

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
  the suite). The DirectWrite context is per thread and is never dropped by a thread's
  exit (`ManuallyDrop` in `dwrite::CTX`): Rust runs thread-local destructors under the
  loader lock, and releasing its COM objects there hung the next thread's first render in
  WIC/D2D (`a_thread_that_rasterised_and_exited_leaves_the_next_one_working`). Only the
  UI thread rasterises today, so the app never hit it; the tests, one thread each, did. The macOS side cannot be type-checked from Windows (`objc_exception` needs a
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
- **Scrollback while an agent owns a pane is `term::AGENT_SCROLLBACK`** (the reader loop
  switches it on the `running || on_screen` edge). Blank pre-allocated alacritty rows
  were the bulk of the Rust heap: rows are allocated 1000 at a time as history grows.
- **The 16 MiB zero-filled heap blocks** seen in the long run (13 to 14 of them, appearing
  under sustained 60 fps rendering, occasionally recycled) are not produced by any Rust
  allocation site in this tree or its dependencies, and `ARBITER_MEM_DIAG` saw none in
  isolated runs. The best-fitting owner is the NVIDIA in-game overlay's capture hook
  (`nvspcap64.dll` was loaded in the process). Disable the overlay to test.
- **Windows asks wgpu for DX12 alone, after a probe** (`gpu::windows_backend`, set as
  `WGPU_BACKEND` in `main` by `choose_graphics_backend`). Order: DX12 on a hardware adapter
  (WARP is always listed, so it does not count), then Vulkan, then WARP with a message box.
  Settings, Display, Graphics can put Vulkan first or insist on DX12
  (`Settings::graphics_backend`, read from the saved state before iced starts, so it
  applies after a restart; a forced backend with no adapter falls back to the other) and
  names the backend in use. A `WGPU_BACKEND` already in the environment beats both, so
  one Arbiter set for itself is kept out of panes (`gpu::BACKEND_ENV_IS_OURS`,
  `Session::spawn`); a second Arbiter run from a pane took it for the user's.
  Vulkan was the default until 2026-09-23 and flickers every textured thing on Intel
  graphics (Core Ultra 5 225U, latest driver); DX12 does not.
  DX12 used to draw every unmaximised window blurry: wgpu-hal created the DXGI swapchain
  with `DXGI_SCALING_STRETCH`, and winit's `undecorated_shadow` hack (`WM_NCCALCSIZE`:
  `top += 1; bottom += 1`) makes the client rect one row taller than the window shows, so
  DWM squeezed the frame by a pixel: edges crisp at the top, half a pixel soft mid-window,
  the phase drifting one pixel over the height. Confirmed by measuring the user's
  screenshots (17 grey levels in a text crop under Vulkan, 199 under DX12, 17 again with the
  shadow off), not by theory. A maximised window takes winit's other `WM_NCCALCSIZE` branch
  and was unaffected. The fix is a third fork, `vendor/wgpu-hal` (`DXGI_SCALING_NONE` for a
  window's swapchain, see its `ARBITER-FORK.md`): DWM presents 1:1 and clips the hidden row,
  as Vulkan always did. Dropping the fork brings the blur back.
- **Do not attach process-memory readers, ETW heap tracing or keystroke automation to a
  running Arbiter from a shell descended from it.** Defender's behaviour classifier
  attributed exactly that to `arbiter.exe` (Trojan:Win32/Bearfoos.A!ml) and killed the
  app. Use `ARBITER_MEM_DIAG`, `Get-Process` counters, or an elevated console you open
  yourself.
