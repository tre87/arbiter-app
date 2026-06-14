<div align="center">

<img src="docs/arbiter.svg" alt="Arbiter" width="120" />

# Arbiter

**Cross-platform terminal application with Claude support.**

One window. Many agents. You decide who works on what.

</div>

---

<p align="center">
  <img src="docs/screenshot.png" alt="Arbiter screenshot" width="100%" />
</p>

## What it is

Arbiter is a desktop app for running a bunch of [Claude Code](https://claude.com/claude-code) sessions at once, in the same window. Split your space into as many terminal panes as you want. Each pane is its own shell with its own agent, so you can open one per feature, one per repo, or one per branch and let them all work in parallel.

The name is the idea behind it. One authority keeping an eye on the agents below. You are the arbiter.

## Workspaces

Workspaces live in tabs along the top. Open as many as you like, move between them with `Ctrl+Tab`, or jump straight to one with `Ctrl+1` through `Ctrl+9`. There are two kinds.

**Terminal workspace.** A tiling grid of shell panes. Split a pane to the right with `Ctrl+Shift+R` or down with `Ctrl+Shift+D`, drag the borders with the mouse or nudge them with `Alt+Shift+Arrow`, and move focus between panes with `Ctrl+Shift+Arrow`. Every pane is a real PTY running bash, zsh, or PowerShell, so Claude Code is the obvious thing to put in there, but anything works.

**Project workspace.** Built around a Git repository. The middle is a tiling split just like a terminal workspace, wrapped by two project panels. On the left is a file explorer with live filesystem watching and per-file Git status. On the right is a worktrees panel. Create a worktree from any branch and Arbiter gives it a dedicated Claude pane and shell. Switching worktrees is instant, and each one keeps its own layout and Claude session. Every worktree also shows a small status dot for idle, working, or needs attention, plus a token meter, so one glance tells you who is busy and who is waiting on you.

## Sessions stick around

Arbiter remembers your setup between launches. That covers the pane layout of every workspace with exact split sizes, each terminal's working directory and Claude session id (so `claude --resume` picks up right where you left off), the window size and position, and your worktree and file explorer state. It saves continuously, so even an unexpected quit will not cost you your layout.

## A few more things

- **Claude-aware footer.** Each pane shows the active model, token use, context percentage, and whether Claude is idle, working, or waiting. The numbers come straight from Claude's own status line, so they match what Claude reports.
- **Usage in the title bar.** Your 5 hour and 7 day Claude usage sits up top and refreshes on its own from your claude.ai session.
- **Overview window.** A second window with live thumbnails of every pane across every workspace. Click one to jump to it.
- **Drag and drop.** Drop files or images onto a Claude pane to attach them. `Ctrl+Shift+S` grabs a screenshot and `Ctrl+Shift+A` opens a file picker.
- **Quiet when idle.** Everything is event driven, so CPU use stays flat when nothing is happening.

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+T` | New workspace |
| `Ctrl+Tab` | Next workspace |
| `Ctrl+Shift+Tab` | Previous workspace |
| `Ctrl+1` to `Ctrl+9` | Switch to workspace 1 to 9 |
| `Ctrl+Shift+W` | Close pane or workspace |
| `Ctrl+Shift+R` | Split right |
| `Ctrl+Shift+D` | Split down |
| `Ctrl+Shift+Arrow` | Navigate panes |
| `Alt+Shift+Arrow` | Resize panes |
| `Ctrl+Shift+E` | Equalize pane sizes |
| `Ctrl+F` | Find in terminal |
| `Ctrl+Shift+O` | Workspace overview |
| `Ctrl+Shift+S` | Attach screenshot |
| `Ctrl+Shift+A` | Attach files |

## Built with

- [Rust](https://www.rust-lang.org) and [iced](https://iced.rs), drawing every pane on the GPU with [wgpu](https://wgpu.rs)
- [alacritty_terminal](https://crates.io/crates/alacritty_terminal) for VT parsing
- [portable-pty](https://crates.io/crates/portable-pty) for real PTYs on Windows, macOS, and Linux

## Run it

```bash
cargo run --bin arbiter
```

## Build it

A release binary:

```bash
cargo build --release --bin arbiter
```

Installers (.dmg on macOS, .exe on Windows) are produced with [cargo-packager](https://crates.io/crates/cargo-packager):

```bash
cargo packager --release
```

## Downloads

Grab the latest build for your platform from the [Releases page](https://github.com/tre87/arbiter-app/releases/latest).
