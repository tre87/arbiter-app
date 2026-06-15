# Changelog

All notable changes to Arbiter are documented here. The format roughly follows
[Keep a Changelog](https://keepachangelog.com/); version numbers track
`Cargo.toml`. This changelog covers the **native** app (1.0.0 onward); earlier
history belongs to the prior Tauri/Vue web app it replaced.

## [Unreleased]

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
