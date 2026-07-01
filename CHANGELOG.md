# Changelog

All notable changes to Arbiter are documented here. The format roughly follows
[Keep a Changelog](https://keepachangelog.com/); version numbers track
`Cargo.toml`. This changelog covers the **native** app (1.0.0 onward); earlier
history belongs to the prior Tauri/Vue web app it replaced.

## [Unreleased]

### Added
- **Terminal font size setting (Settings → Display → Terminal).** Pick a point size
  (8–32); it applies to every open terminal immediately. The renderer rebuilds at the new
  size and each terminal reflows its columns/rows, so cell alignment and box-drawing stay
  exact. Persists across restarts.
- **Open Claude config from Settings (Settings → Claude Usage → Config).** Buttons that open
  Claude's `settings.json` and `~/.claude.json` in your default editor (resolving
  `$CLAUDE_CONFIG_DIR` / `~/.claude`), on macOS and Windows. A missing file is created empty
  so the button always opens something.
- **Hiding usage now hides it everywhere.** The overview usage footer is hidden whenever
  usage is hidden globally (the same "Hide usage bar" setting the titlebar × toggles). The
  overview's own "Show usage footer" toggle still hides just the footer independently when
  usage is shown.
- **Dismiss & explain the titlebar usage sign-in.** The header "Claude Usage Sign In" prompt
  now has a small × to hide it outright (the same "Hide usage bar" setting, persisted).
  Clicking Sign in first shows a short, plain-language explanation of what signing in does
  and what data is read, with Sign in / Cancel. Cancel leaves the header as-is.

## [1.0.12] — 2026-06-23

### Fixed
- **Ctrl+C returns to the prompt:** pressing Ctrl+C (the `^C` interrupt, when there's no
  selection) while scrolled up in the scrollback now snaps the view back to the live bottom,
  matching what typing already did — so an interrupt jumps you to the prompt.
- **Overview Claude avatar alignment:** the static Claude starburst shown left of a terminal's
  title in the overview is raised 1px so it sits level with the title.

## [1.0.11] — 2026-06-23

### Changed
- **True idle when idle (lower CPU/GPU; lets the display sleep while focused).** The UI
  no longer runs on a repaint clock when nothing is animating. Previously even an idle
  window repainted ~1×/sec, which kept the wgpu swapchain presenting; on Windows the GPU
  driver responds to continuous presentation by raising the global timer resolution to
  1 ms (defeating CPU power management) and keeping the display/GPU awake — so a focused
  window never let the machine idle or sleep. Now, when idle, the app emits **zero
  frames** and repaints only on real events (terminal output, input, window events); the
  swapchain goes quiet, the driver drops the raised timer, and Windows can sleep the
  display even while Arbiter is focused. A pane **waiting at a prompt** (`Attention`) is
  now a **solid** amber dot instead of a pulsing one — a waiting prompt is idle, so it no
  longer pins the UI at 60fps (the overview's green "running" dot is likewise solid now,
  matching the tab/header). The usage auto-refresh no longer rides a UI tick at all: its
  120s cadence runs on a background thread that pokes the helper directly, so a logged-in
  idle window also emits **zero** frames. Claude actively *working* still animates (the ✻
  bloom / avatar) as before. macOS behaviour is unchanged in feel; the same idle path applies.
- **Usage refresh button is now just an icon** (no live `M:SS` countdown). A per-second
  countdown requires a ~1Hz repaint, which is exactly the idle clock that was removed, so
  it can't coexist with true idle. The button still refetches on click, the auto-refresh
  still runs every 120s, and the per-meter reset times (e.g. "7d: 1h 41m") are unchanged.

## [1.0.10] — 2026-06-18

### Added
- **Workspace tab "running" dot:** a workspace tab now also shows a solid **green** dot when
  one of its terminals has a (non-Claude) command running — a build, dev server, `sleep`,
  vim, etc. Full priority across the workspace's terminals is now amber (needs attention) →
  blue (Claude working) → green (command running). The green dot is solid and appears/hides
  on command start/end (event-driven), so a long-running command adds no idle CPU.

### Fixed
- **Paste jumps to the prompt:** pasting (Cmd/Ctrl+V, middle-click, or a file attach via
  drag-drop / the pickers) while scrolled up in the scrollback now snaps the view back to
  the live bottom and clears the selection — matching what typing already did — so the
  pasted text is visible where it lands at the prompt.
- **Text selection while scrolling:** dragging a selection past the top/bottom edge now
  auto-scrolls continuously while the mouse is held still (instead of stalling after a
  moment), at a controllable speed, and scrolling the wheel while holding the button keeps
  extending the selection (instead of stopping after one notch). Root cause: the terminal's
  overlay layer (working bar / info popover / scroll indicator / find bar) was added or
  removed as the gesture progressed, which changed the widget tree and reset the terminal's
  per-widget interaction state mid-drag. The terminal is now always the base layer of a
  stable stack, so its drag/scroll state survives. The auto-scroll is also event-driven
  (a self-sustaining frame request), adding no idle CPU.

### Added
- **Terminal header right-click menu:** right-clicking a terminal's header now opens the same
  context menu as right-clicking its body (Rename, Clear Buffer, Split, Select All, Copy,
  Paste, Close), anchored at the cursor. The menu's actions — including Select All — target
  the terminal whose header was clicked.

### Fixed
- **Overview title alignment / wrapping:** a long terminal title no longer pushes the status
  dot and git stats out of their column or wraps to a second line. The title now truncates
  with an "…" (shorter still when git stats are present, since those take priority), and the
  row is clamped to a single line and clipped, so it can never grow to two lines regardless
  of title length. The status dot and git stats stay pinned to a fixed right column.
- **Usage "Sign in" flash on slow loads:** when the usage helper was slow to respond, the
  titlebar/overview briefly showed the "Sign in" button, then it vanished and the real usage
  appeared. It came from an 8s "still Loading → assume Sign in" fallback that a slow-but-
  successful load tripped. "Sign in" is now driven by what actually happened: the helper
  reporting `needs_login` (logged out) or the helper process exiting (not built / crashed,
  surfaced immediately), with the speculative timeout lengthened to 30s purely for a helper
  that's alive but silent — so a slow load just stays "Loading" until the data arrives.
- **Claude not relaunching on restart (intermittent, mainly Windows):** detecting that Claude
  is running in a pane is now driven by the statusLine capture Claude writes via our injected
  settings, rather than a process scan. The scan only looked for ~2s after a command started
  and then never re-checked (a running Claude keeps the shell busy, so no new "command started"
  edge), so a slow cold launch (shell profile + shim + node + MCP, antivirus) was missed —
  `claude_running` stayed false, wasn't persisted, and the next reopen didn't relaunch Claude.
  Now a capture appearing for a pane marks it running, independent of launch speed and fully
  event-driven; stale files can't false-trigger (capture dirs are cleared on startup and a
  pane's capture is deleted when its command ends). The process scan remains a fallback (now
  with backoff to 10s) for Claude started outside the shim.

## [1.0.9] — 2026-06-15

### Added
- **Per-terminal command history:** each terminal now keeps its own private command history
  that persists across app exit and relaunch — a reopened terminal recalls only the commands
  it ran, and a brand-new terminal starts empty. Each pane gets a stable id (saved in the
  layout) backing a private history file under `<data-dir>/history/`; the file is removed when
  the terminal/workspace is permanently closed and capped at the most recent 1000 commands.
  Works on macOS (zsh + bash) and Windows (PowerShell + Git Bash). Note: this is a deliberate
  departure from the usual shared-history model (iTerm2 et al. share one history file across tabs).

### Fixed
- **Overview row alignment:** the working ✻ animation no longer makes overview rows jump by a
  pixel when it shows/hides (its slot is now a fixed height, so a dot and the animation occupy
  the same space), and the git stats, status dot, and ✻ now sit level with the terminal titles.
- **Git footer across sibling terminals:** a git command (staging, commit, branch switch) in
  one terminal now refreshes the git footer of *other* terminals open in the same repo, not
  just the one that ran it. The repo watcher previously ignored all `.git/` changes to avoid a
  CPU loop (`git status` rewriting `.git/index`); status reads now use `--no-optional-locks`
  (read-only, no index write), so the watcher can safely observe meaningful `.git/` metadata
  while still ignoring object/log/lock churn. No extra idle CPU — the watcher stays purely
  event-driven.

## [1.0.8] — 2026-06-15

### Added
- **Quit confirmation:** closing Arbiter (the window close button, Cmd+Q on macOS, Alt+F4 on
  Windows) now asks for confirmation first, so a stray close can't silently drop every open
  terminal. Also fires on macOS logout / restart / shutdown, so a system quit can't silently
  kill running sessions. On by default; turn it off under Settings → General → Quitting.

### Changed
- **Escape closes dialogs:** pressing Escape now dismisses the open Settings, shortcuts,
  confirmation, and other modal dialogs / menus (falling through to the terminal only when
  none is open).
- **Borderless dialogs:** modal dialogs drop their hairline border for a cleaner, consistent
  look (matching the quit / close-workspace confirmations).

### Fixed
- **False Claude detection from dev servers:** running a node tool (e.g. `npm run dev`) no
  longer false-detects Claude in that pane (it showed an idle Claude dot). On macOS a process
  that rewrites its title (npm) makes `sysinfo` report its *environment* among its args, which
  surfaced Arbiter's own injected `ARBITER_*` vars — whose values point at `claude-shim` /
  `claude-sessions` / the real claude bin and so contained "claude". Detection now ignores
  `KEY=VALUE` env assignments and matches only the genuine claude-code CLI (its package dir or
  a `claude`-named bin).
- **Workspace tab alignment:** the activity dot is nudged down 1px so it sits level with the
  type icon + title (which don't move); the close (×) is sized down to 12px (its top-heavy
  glyph read high at 13px) and left centred.
- **Truecolor advertisement:** spawned shells now export `COLORTERM=truecolor`, so programs
  like Claude Code emit their real 24-bit palette (e.g. the vivid orange ✻) instead of a
  duller 256-colour approximation. Previously Arbiter relied on inheriting `COLORTERM`, which
  a Finder-launched app doesn't have — so colours looked vivid only when launched from a
  terminal that set it (iTerm2) and dull otherwise.
- **Nested Arbiter launches:** running `cargo run --bin arbiter` (or any Arbiter) from inside
  another Arbiter's terminal left `claude` hanging — the child inherited the parent's
  `claude`-shim dir at the front of `PATH` and resolved *it* as the "real" claude, so the
  shim `exec`'d itself in an infinite loop. The shim resolver now skips the parent shim
  (`ARBITER_SHIM_BIN`) and any directory whose `claude` is an Arbiter launcher, falling
  through to the genuine binary. Stats/statusLine/hooks are unchanged.

## [1.0.7] — 2026-06-14

### Added
- **Close-workspace confirmation:** closing a workspace — the tab ×, either type, or the
  right-click "Close" — now asks for confirmation first, so a stray click can't silently
  drop a workspace and its open terminals.
- **Workspace tab status dot:** a workspace tab now shows a pulsing dot (after its name,
  before the close button) when one of its terminals has Claude working (azure) or needing
  attention (amber). Attention takes priority across the workspace's terminals.

### Fixed
- **Launch focus:** the main window now reliably comes to the front and takes focus on
  launch, instead of sometimes opening unfocused behind other windows.
- **Windows glyphs:** fallback symbols like ✻ (Claude's working spinner) keep their full
  height instead of being squashed into the narrow cell — sized like Windows Terminal.
- **Windows Claude working-detection:** newline / mode edit keys (Shift+Enter, Ctrl+Enter,
  Shift+Tab), even pressed in rapid succession, or resizing the window, no longer falsely
  start Claude's "working" animation. Those keys make Windows ConPTY repaint and re-emit an
  on-screen ✻; they now briefly suppress spinner-detection so a burst can't pair into a
  false positive — while a plain Enter/submit clears the suppression, so genuine working is
  still detected immediately (no delay).

## [1.0.6] — 2026-06-14

### Changed
- **Much lower idle CPU.** The terminal now redraws on PTY output (event-driven) and
  the title is a static gradient, so the 60fps animation tick pauses (→ 1s) whenever
  nothing is animating. Idle goes from a constant ~1–2% repaint to near-zero; the fast
  tick returns only while Claude is working, a status needs attention, or the scroll
  indicator is fading.
- The git-status file watcher no longer pins the CPU in a repo: it ignores `.git/`
  churn (so `git status`'s own index rewrite can't re-trigger it in a loop) and
  gitignored build/dep dirs (`target/`, `node_modules/`, …).
- **Titlebar & overview polish:** a static azure title gradient (no pale blue); the
  overview titlebar matches the main window (centered on macOS, left-aligned on Windows,
  same indent); and the overview usage bars stay legible on the dark background.

### Added
- **Bold text style** setting (Settings → Display → Terminal), mirroring Windows
  Terminal's `intenseTextStyle`: render bold/intense (SGR 1) text as a **bold font**,
  a **brighter colour** (regular weight, the classic xterm look), **both**, or **none**.
- **Background colour** setting (Settings → Display → Terminal): presets (Default
  `#0a0a0c`, Gray `#121212`, Black) or a custom hex, applied live to the terminals,
  sidebars and the overview's terminal-list area. The terminal header tracks it.

### Fixed
- **Windows terminal text** is sharper and now matches Windows Terminal. Grayscale
  antialiasing uses DirectWrite's gamma-correct blend (the gamma-1.8 algorithm from
  WT's own shader) instead of a hazy/thin gamma-space blend; glyphs render in the
  recommended NATURAL_SYMMETRIC mode with grid-fitting; and **bold** renders from the
  bundled Cascadia Mono Bold face instead of a soft synthesised faux-bold.

## [1.0.5] — 2026-06-12

### Added
- Workspace tabs can be **dragged to reorder** (persistent), with a blue insertion
  line shown between tabs while dragging.
- **Overview window** redesign:
  - A Claude **usage-bar footer** that shares the main app's fetch (updates on the
    same timer + refresh), toggleable in Settings → Display → Overview. The bars hold
    the header size and only shrink (dropping reset times last) when the window is
    too narrow.
  - The **same custom titlebar as the main window** — centered logo + "Overview"
    with the azure glow behind it, Windows caption buttons / macOS traffic lights,
    resizable with a minimum size.
- Overview window **always-on-top** by default, with a Settings toggle.

### Changed
- macOS: the window can be dragged (logo / empty titlebar) *and* tabs reordered — the
  window drag is handled manually so the two no longer conflict.
- The usage refresh countdown no longer jumps backward when fresh data lands.

### Fixed
- **Windows Claude usage** updates reliably in the background again: stop WebView2
  throttling/occluding the hidden webview, drive it through a suspend/resume
  lifecycle, and recover a discarded renderer (after long idle) by reloading. The
  manual refresh button always recovers.
- **Windows glyph rendering**: blend coverage in gamma space (matches Windows
  Terminal) and fit oversized fallback glyphs (e.g. `⏵`, `✻`) into the cell instead
  of clipping or stretching.
- Dim/faint (SGR 2) terminal text now renders dimmer.

## [1.0.4] — 2026-06-12

### Added
- Right-click **context menus** for workspace tabs and terminals (rename, etc.).

## [1.0.3] — 2026-06-12

### Added
- Terminal query responses (DA / DSR / cursor-position) answered via a dedicated PTY
  writer thread, so apps that probe the terminal (e.g. biovpn) behave.

### Changed
- Rewrote the README for the native app; restored the Arbiter SVG logo.

## [1.0.2] — 2026-06-12

The **native rewrite** moves to the repo root: Arbiter is now a native Rust app
(iced + wgpu, no webview), replacing the Tauri/Vue web app.

### Added
- GPU terminal renderer (alacritty_terminal + wgpu) matching the web look, with
  CoreText (macOS) / DirectWrite (Windows) rasterization and color emoji.
- Event-driven Claude status (statusLine + hooks shim), per-pane footer stats, the
  working animation, and the popout Overview window.
- Claude usage bars via an isolated webview sidecar (claude.ai session), with org
  selection.
- Project workspaces: git worktrees, 3-pane layout, file explorer, worktree cards
  with avatars.
- Native unified titlebar (macOS traffic-light inset; Windows custom caption buttons
  + borderless resize), session persistence, keyboard shortcuts, file attach, and the
  Settings dialog.

### Fixed
- Windows: crisp caption glyphs; the claude shim re-prepends PATH so `claude` is
  intercepted; no console flash or stderr leak when forwarding the user's statusLine.

## [1.0.1] — 2026-06-11

### Changed
- Worktree + overview status indicators centered and stabilized.
- Debug builds use a separate data dir + Claude login from release.
- Titlebar spacing, helper dock-flash, DMG layout, and window-refocus-lag polish.

## [1.0.0] — 2026-06-11

- Initial native release.
