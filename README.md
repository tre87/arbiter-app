# Arbiter

A desktop app for running multiple Claude Code sessions side by side.

Arbiter lets you split your workspace into as many terminal panes as you need, each running an independent Claude Code agent. Spawn a session per feature, per repo, or per concern — then watch them work in parallel.

The name comes from the idea of a single commanding authority overseeing multiple agents beneath it. You are the arbiter.

## Stack

- [Tauri](https://tauri.app) — native shell
- [Vue 3](https://vuejs.org) + TypeScript
- [xterm.js](https://xtermjs.org) — terminal rendering

## Dev

```bash
npm install
npm run tauri dev
```

## Build

```bash
npm run tauri build
```
