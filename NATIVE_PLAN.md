# Arbiter → Native (kill the webview)

## Why

WebView2 on Windows stutters badly: it re-composites our transparent full-window
WebGL canvas every frame (and DOM + webview overhead generally). macOS/WKWebView
hides it (we measured 120fps), but Windows is a stuttery mess "no matter what".
A native app renders the whole window into **one GPU surface** the OS composites
— which is the "one canvas, not N layers" win we engineered in WebGL, except free
and without the browser middleman. That's why WezTerm/Zed are smooth on Windows.

## Language: Rust (confirmed)

For a perf-critical, cross-platform terminal with deep system integration, Rust
is the right call (and what the whole modern fast-terminal cohort uses):
- **No GC** → no GC-pause stutter (the exact failure mode we're fighting).
- Ecosystem is Rust: `alacritty_terminal` (parse — already used), `portable-pty`
  (already used), `wgpu` (GPU).
- Our backend is **already Rust** — roughly half the app survives untouched.

Accepted tradeoff: Rust GUI iteration is slower than web; UI gets tweaked after
first cut.

## Architecture

- **One window = one GPU surface.** Chrome + every terminal render into it. No
  transparent overlay, no per-terminal layer, no WebView2 compositor.
- **No IPC.** The Tauri command/event/Channel layer is deleted; UI calls the
  backend directly (Rust → Rust) and shares state in-process.
- **Deleted complexity:** xterm.js, Tauri, the macOS fps-unlock hack, WebGL2
  fallback, WebGL context-loss handling, on-demand-rAF dance, transparent-canvas
  tricks.

### Framework decision (the one real fork)

The **terminal grid is a custom `wgpu` renderer** either way — we port our
glyph-atlas + instanced-quad design straight from `singleCanvasRenderer.ts` to
wgpu (logic is done; it's a language port). The fork is the **app shell** (tabs,
splits, footer, overview, settings, dialogs):

- **GPUI** (Zed's framework) — *leading candidate*. Literally "terminal + rich
  UI + cross-platform + GPU + smooth on Windows" = exactly Arbiter. Caveat: not a
  cleanly-published/semver'd framework — you track Zed's repo, thin docs, API
  churn. Tension with our stability motivation.
- **Iced** — published, documented, semver'd, wgpu-based, hosts a custom shader
  widget for the terminal. More sustainable/controllable; more chrome built by
  hand; no terminal precedent.
- (Hand-rolled wgpu like WezTerm — rejected: our UI is far richer than WezTerm's
  tab strip, so hand-rolling all of it is wasteful.)

**DECIDED: Iced.** Phase 0 (the raw winit+wgpu live-terminal spike) ran great on
**both Windows and macOS** — the smoothness thesis is confirmed (WebView2 was the
wall). Iced chosen for the shell: published/semver'd, wgpu-native (composites with
our terminal renderer via a custom `shader` widget), retained (good for the
complex stateful UI), and sustainable (no coupling to Zed's churn — matches the
stability motivation). The terminal renderer stays raw wgpu and is decoupled to
draw into a provided pass, then hosted inside Iced.

## Build ALONGSIDE — do NOT delete the webview app yet

The native app is a **separate binary** (`arbiter-native/`). On Windows you build
and run *that* — there is **no webview in it, by construction**. The webview app
stays as the working reference + macOS daily driver until native reaches parity.
Deleting it now would leave nothing runnable for the whole rewrite and wouldn't
make the native test any cleaner. The webview app is removed wholesale in
**Phase 8**, only once native is at parity.

## Reuse vs rebuild

- **Reused ~as-is (Rust backend, ~half the app):** `pty.rs` (portable-pty),
  `claude.rs` (process monitoring), `git.rs`, `claude_shim.rs`, `shell.rs`, the
  `alacritty_terminal` parsing + cell-grid (`termgrid::HeadlessTerm`), the
  status-engine logic. **Caveat:** these are currently coupled to Tauri
  (`AppHandle`/`State`/`Emitter`); Phase 1 extracts them into a Tauri-free `core`
  crate and replaces event-emits with native channels/callbacks.
- **Ported (logic known):** the glyph-atlas/instanced-quad renderer → wgpu; the
  Vue stores (`pane.ts`, `project.ts`, `paneClaudeEvents.ts`, usage) → Rust state
  models.
- **Rebuilt:** the UI components (same look, new layer). Design tokens (colors,
  spacing, SVG logo/icons) carry over.

## Phases

- **Phase 0 — De-risk (the gate).** Bake-off on **Windows**: a live terminal
  (real PTY → alacritty parse → wgpu draw) + a tab bar, in GPUI (and Iced if GPUI
  fights us). Confirm it's **butter-smooth on Windows** and the ergonomics are
  tolerable. *Nothing else proceeds until Windows is proven smooth.*
  - **0.0 (done):** standalone `arbiter-native` crate that spawns a PTY and
    parses it with `alacritty_terminal` — proves the backend core runs natively +
    cross-platform, no Tauri. `cd arbiter-native && cargo run`.
  - **0.1:** add a `wgpu` window (winit) and clear/draw a test frame on Windows.
  - **0.2:** port the glyph-atlas + instanced-quad renderer; draw the live grid.
  - **0.3:** wrap in the chosen shell framework (GPUI/Iced) with a tab bar.
- **Phase 1 — Skeleton + `core` crate.** Extract the Tauri-free backend into
  `core`; window + surface; the native app depends on `core`.
- **Phase 2 — Terminal renderer (production).** Full grid rendering: colors,
  cursor, scrollback, block/box glyphs, themes.
- **Phase 3 — Multiplexing.** Tabs + resizable splits + focus; the pane/workspace
  model (port `pane.ts`); many terminals in the one surface.
- **Phase 4 — Chrome.** Titlebar/window controls, per-pane footer (folder, git,
  model/context/tokens), status indicators + animations, usage bar.
- **Phase 5 — Claude integration.** Wire the (already-Rust) monitoring + shim to
  the UI; statusline, working/attention, launch buttons.
- **Phase 6 — Secondary surfaces.** Overview window, settings, project
  workspaces, confirm dialogs.
- **Phase 7 — Input + terminal features.** Keyboard/IME/paste, selection/copy,
  links, search, scrollback (alacritty/wezterm are the reference).
- **Phase 8 — Package + remove webview.** Windows installer + macOS .dmg/notarize
  via cargo bundler; **delete the entire Vue/Tauri/xterm app**; drop the WebView2
  dependency.

## Inspiration (cross-platform, native, open source)

- **WezTerm** (Rust, OpenGL/WebGPU) — cross-platform GPU terminal *with tabs +
  splits + multiplexing*; chrome drawn in its own GPU renderer.
- **Zed terminal / GPUI** (Rust, GPU) — terminal + rich UI, cross-platform incl.
  Windows; same parser.
- **Alacritty** (Rust, OpenGL) — reference GPU text renderer; uses the exact
  `alacritty_terminal` parser we use.
- **Rio** (Rust, WGPU) — modern GPU terminal (sugarloaf renderer).
- **Ghostty** (Zig, GPU) — very fast; mac/Linux, Windows WIP.

## The bet

Everything rides on **Phase 0**: does a live terminal render butter-smooth on
**Windows** in the chosen stack? Zed/WezTerm prove it's possible in general;
Phase 0 proves it's possible *for us* before we commit to the full rebuild.
