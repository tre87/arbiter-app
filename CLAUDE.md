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

## Terminal renderer — known limitation (intentional)

The GPU renderer draws **one opaque quad per cell**, so a glyph cannot overflow its cell
without painting over (erasing) the neighbour. Consequences, both intentional / accepted:

- **Single-width emoji render small.** A 1-cell text-default emoji like ⏸ (U+23F8, the
  plan-mode indicator) is downscaled to the ~7px cell. Windows Terminal instead lets the
  glyph overflow the cell, so it looks bigger there. Double-width emoji (👋, 2 cells) render
  full-size in both. Fixing this would need **overflow rendering** (alpha-composited glyphs
  or variable-size atlas tiles) — a real renderer change, deliberately deferred.
- `fit_to_box` (`src/gpu.rs`) instead **center-clips** a mono fallback symbol that's only
  slightly wider than the cell (e.g. ✻), keeping full height, and downscales anything
  larger. Don't re-add an "upscale undersized symbols" path — it enlarges glyphs like ⏵
  past their natural size (reverted once already).
