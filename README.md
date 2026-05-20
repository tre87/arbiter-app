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

Arbiter is a desktop app that lets you run a bunch of [Claude Code](https://claude.com/claude-code) sessions at the same time, in the same window. Split your workspace into as many terminal panes as you want, and each one is its own independent shell with its own agent. Open a session per feature, per repo, per branch, whatever makes sense, then let them all work in parallel.

The name comes from the idea of a single commanding authority overseeing a bunch of agents below it. You are the arbiter.

## Workspaces

There are two kinds of workspaces in Arbiter, and each one lives in its own tab at the top. You can have as many open as you like, and flip between them with `Ctrl+Tab` or jump straight to one with `Ctrl+1` through `Ctrl+9`.

### Terminal Workspace

A free-form tiling grid of shell panes. Split any pane to the right with `Ctrl+Shift+R` or downwards with `Ctrl+Shift+D`, drag the boundaries with your mouse or resize them with `Alt+Shift+Arrow`, and hop between panes with `Ctrl+Shift+Arrow`. Every pane is a real PTY running `bash`, `zsh`, or PowerShell, so Claude Code is the obvious thing to put in there, but really anything works.

### Project Workspace

Built around a Git repository. The middle of the workspace is a tiling split just like a terminal workspace, but it's wrapped with two project-aware panels.

On the left is a file explorer with live filesystem watching, persisted expand state, and per-file Git status. On the right is a worktrees panel. Create a worktree from any branch and Arbiter spins up a dedicated Claude pane and shell terminal for it. Switching between worktrees is instant, and each one keeps its own pane layout, file explorer state, and Claude session.

Every worktree carries its own Claude lifecycle indicator (idle, working, needs attention) and a token meter, so a glance at the panel tells you which agents are busy and which ones are waiting on you.

## Sessions stick around

This is a big one. Arbiter remembers everything between launches, including:

- The full pane tree of every workspace, with exact split percentages
- Each terminal's working directory and Claude session ID, so `claude --resume` picks up right where you left off
- Window size, position, and which workspace was active
- Worktree configuration, file explorer expand state, and which worktree was active in each project
- Workspace overview window position and bounds

State is autosaved continuously to the Tauri app data directory, so even unexpected exits won't cost you your layout.

## Other features

- **Claude-aware footer.** Every pane shows the active model, token usage, context %, and idle/working state, parsed straight from Claude's JSONL output.
- **Usage at a glance.** 5-hour and 7-day Claude utilization sits in the title bar and refreshes on its own from your `claude.ai` session.
- **Workspace overview window.** A second window that shows live thumbnails of every pane across every workspace. Click one to jump there.
- **Drag and drop attachments.** Drop files or images onto a Claude pane to attach them. `Ctrl+Shift+S` grabs a screenshot, and `Ctrl+Shift+A` opens a file picker.
- **Event-driven, not polled.** Filesystem watchers, OSC escape parsing, and Tauri events keep CPU usage flat when nothing's happening.

## Keyboard shortcuts

| Shortcut             | Action                          |
| -------------------- | ------------------------------- |
| `Ctrl+Shift+T`       | New workspace                   |
| `Ctrl+Tab`           | Next workspace                  |
| `Ctrl+Shift+Tab`     | Previous workspace              |
| `Ctrl+1` to `Ctrl+9` | Switch to workspace 1 to 9      |
| `Ctrl+Shift+W`       | Close pane or workspace         |
| `Ctrl+Shift+R`       | Split right                     |
| `Ctrl+Shift+D`       | Split down                      |
| `Ctrl+Shift+Arrow`   | Navigate panes                  |
| `Alt+Shift+Arrow`    | Resize panes                    |
| `Ctrl+Shift+E`       | Equalize pane sizes             |
| `Ctrl+Shift+O`       | Workspace overview              |
| `Ctrl+Shift+S`       | Attach screenshot               |
| `Ctrl+Shift+A`       | Attach files                    |

## Stack

- [Tauri 2](https://tauri.app) for the native shell (Rust)
- [Vue 3](https://vuejs.org) with TypeScript and [Pinia](https://pinia.vuejs.org)
- [xterm.js](https://xtermjs.org) for terminal rendering, using the WebGL renderer
- [portable-pty](https://crates.io/crates/portable-pty) for real PTYs on Windows, macOS, and Linux

## Develop

```bash
npm install
npm run tauri dev
```

## Build

```bash
npm run tauri build
```

Bundles end up in `src-tauri/target/release/bundle/`.

## Downloads

Grab the latest installer for your platform from the [Releases page](https://github.com/tre87/arbiter-app/releases/latest).
