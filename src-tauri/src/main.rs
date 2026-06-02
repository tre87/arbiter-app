// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Headless subcommand: Claude invokes `arbiter claude-statusline` as its
    // statusLine command (via the --settings we inject for Arbiter-launched
    // sessions). Handle it before any Tauri/GUI initialization and exit.
    if std::env::args().nth(1).as_deref() == Some("claude-statusline") {
        arbiter_lib::claude_shim::run_statusline_capture();
        return;
    }
    arbiter_lib::run();
}
