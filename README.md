<div align="center">

<img src="public/arbiter.svg" alt="Arbiter" width="120" />

# Arbiter

**Run many Claude Code sessions side by side.**

One window. Many agents. You decide who works on what.

</div>

---

<p align="center">
  <img src="docs/screenshot.png" alt="Arbiter screenshot" width="100%" />
</p>

## About

Arbiter is a cross-platform desktop app for running multiple [Claude Code](https://claude.com/claude-code) CLI sessions in parallel. Split your workspace into as many terminal panes as you need — each one an independent shell with its own agent. Spawn a session per feature, per repo, or per concern, and watch them work side by side.

The name comes from the idea of a single commanding authority overseeing many agents below it. You are the arbiter.

## Workspaces

Arbiter has two kinds of workspaces, each in its own tab. Open as many as you like, switch between them with `Ctrl+Tab` or `Ctrl+1`–`Ctrl+9`.

### Terminal Workspace

A free-form tiling grid of shell panes. Split any pane vertically (`Ctrl+Shift+R`) or horizontally (`Ctrl+Shift+D`), resize boundaries with `Alt+Shift+Arrow`, and jump between panes with `Ctrl+Shift+Arrow`. Each pane is a real PTY running `bash`, `zsh`, or PowerShell — Claude Code is the obvious tenant, but anything goes.

### Project Workspace

Built around a Git repository. The center is a tiling split just like a terminal workspace, but flanked by two project-aware panels:

- **File explorer** on the left, with live filesystem watching, expand-state persistence, and per-file Git status indicators.
- **Worktrees panel** on the right. Create a worktree from any branch and Arbiter spins up a dedicated Claude pane and shell for it. Switch between worktrees instantly — each keeps its own pane layout, file explorer state, and Claude session.

Every worktree carries its own Claude lifecycle indicator (idle / working / needs-attention) and token meter, so a glance at the panel tells you which agents are busy and which are waiting on you.

## Sessions stick around

Arbiter remembers everything between launches:

- The full pane tree of every workspace, with exact split percentages
- Each terminal's working directory and Claude session ID (so `claude --resume` picks up right where you left off)
- Window size, position, and which workspace was active
- Worktree configuration, file-explorer expand state, and active worktree per project
- Workspace overview window position and bounds

State is autosaved continuously to the Tauri app data directory, so unexpected exits don't cost you your layout.

## Other features

- **Claude-aware footer.** Each pane shows the active model, token usage, context %, and idle/working state, parsed straight from Claude's JSONL output.
- **Usage at a glance.** 5-hour and 7-day Claude utilization in the title bar, refreshed automatically from your `claude.ai` session.
- **Workspace overview window.** A second window that shows live thumbnails of every pane across every workspace — click one to jump there.
- **Drag-and-drop attachments.** Drop files or images onto a Claude pane to attach them; `Ctrl+Shift+S` grabs a screenshot, `Ctrl+Shift+A` opens a file picker.
- **Event-driven, not polled.** Filesystem watchers, OSC escape parsing, and Tauri events keep CPU idle when nothing is happening.

## Keyboard shortcuts

| Shortcut             | Action                          |
| -------------------- | ------------------------------- |
| `Ctrl+Shift+T`       | New workspace                   |
| `Ctrl+Tab`           | Next workspace                  |
| `Ctrl+Shift+Tab`     | Previous workspace              |
| `Ctrl+1`–`Ctrl+9`    | Switch to workspace 1–9         |
| `Ctrl+Shift+W`       | Close pane / workspace          |
| `Ctrl+Shift+R`       | Split right                     |
| `Ctrl+Shift+D`       | Split down                      |
| `Ctrl+Shift+Arrow`   | Navigate panes                  |
| `Alt+Shift+Arrow`    | Resize panes                    |
| `Ctrl+Shift+E`       | Equalize pane sizes             |
| `Ctrl+Shift+O`       | Workspace overview              |
| `Ctrl+Shift+S`       | Attach screenshot               |
| `Ctrl+Shift+A`       | Attach files                    |

## Stack

- [Tauri 2](https://tauri.app) — native shell (Rust)
- [Vue 3](https://vuejs.org) + TypeScript + [Pinia](https://pinia.vuejs.org)
- [xterm.js](https://xtermjs.org) — terminal rendering with the WebGL renderer
- [portable-pty](https://crates.io/crates/portable-pty) — real PTYs on Windows, macOS, and Linux

## Develop

```bash
npm install
npm run tauri dev
```

## Build

```bash
npm run tauri build
```

Bundles are written to `src-tauri/target/release/bundle/`.

## Downloads

Grab the latest installer for your platform from the [Releases page](https://github.com/tre87/arbiter-app/releases/latest).
