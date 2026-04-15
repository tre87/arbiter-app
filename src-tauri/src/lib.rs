mod claude;
mod config;
mod fs;
mod git;
mod overview;
mod pty;
mod shell;
mod usage;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, WebviewWindowBuilder};

use crate::claude::{ClaudeMonitor, ExpectedClaudeSessions};
use crate::fs::FileWatchers;
use crate::pty::{PtySession, Sessions};
use crate::usage::{Cache, UsageCache, AUTH_WINDOW_LABEL, USAGE_INIT_SCRIPT};
use crate::overview::OVERVIEW_WINDOW_LABEL;

#[tauri::command]
fn exit_app(app: AppHandle) {
    app.exit(0);
}

// `open_devtools` is only available on debug builds — Tauri strips the method
// from `WebviewWindow` in release unless the `devtools` Cargo feature is on.
// We expose a no-op release shim so the frontend can keep invoking the same
// command unconditionally.
#[cfg(debug_assertions)]
#[tauri::command]
fn open_devtools(webview_window: tauri::WebviewWindow) {
    webview_window.open_devtools();
}

#[cfg(not(debug_assertions))]
#[tauri::command]
fn open_devtools(_webview_window: tauri::WebviewWindow) {}

#[tauri::command]
fn focus_webview(webview_window: tauri::WebviewWindow) {
    #[cfg(windows)]
    {
        let _ = webview_window.with_webview(|webview| {
            unsafe {
                use webview2_com::Microsoft::Web::WebView2::Win32::*;
                let controller = webview.controller();
                let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            }
        });
    }
}

#[tauri::command]
fn get_locale() -> String {
    // On Windows, sys-locale returns the display language (e.g. en-US) not the
    // regional format (e.g. da-DK). Read the regional format from the registry.
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-Command", "(Get-ItemProperty 'HKCU:\\Control Panel\\International').LocaleName"])
            .output()
        {
            let locale = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !locale.is_empty() {
                return locale;
            }
        }
    }
    sys_locale::get_locale().unwrap_or_else(|| "en-US".to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            // We save position/size ourselves in arbiter.json; the plugin only
            // tracks maximized/fullscreen so it doesn't fight our custom restore
            // (which runs after the decoration flip on Windows).
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::MAXIMIZED
                        | tauri_plugin_window_state::StateFlags::FULLSCREEN,
                )
                .with_denylist(&[AUTH_WINDOW_LABEL, OVERVIEW_WINDOW_LABEL])
                .build(),
        )
        .manage({
            let inner: Arc<Mutex<HashMap<String, PtySession>>> = Arc::new(Mutex::new(HashMap::new()));
            Sessions(inner)
        })
        .manage(ClaudeMonitor(Arc::new(Mutex::new(HashMap::new()))))
        .manage(ExpectedClaudeSessions(Arc::new(Mutex::new(HashMap::new()))))
        .manage(Cache(Mutex::new(UsageCache::new())))
        .manage(FileWatchers(Arc::new(Mutex::new(HashMap::new()))))
        .setup(|app| {
            // Create a hidden auth WebView at startup. If the user has a valid
            // session (persisted WebView2 cookies), the injected script will
            // silently fetch usage and populate the cache without any user action.
            let url: url::Url = "https://claude.ai".parse().unwrap();
            WebviewWindowBuilder::new(app, AUTH_WINDOW_LABEL, tauri::WebviewUrl::External(url))
                .title("Sign in to Claude")
                .inner_size(960.0, 720.0)
                .visible(false)
                .initialization_script(USAGE_INIT_SCRIPT)
                .build()?;

            // Create a hidden overview window at startup so WebView2
            // initialises during the event-loop setup phase (avoids the
            // deadlock that occurs when building a window from a command).
            WebviewWindowBuilder::new(app, OVERVIEW_WINDOW_LABEL, tauri::WebviewUrl::default())
                .title("Arbiter – Overview")
                .inner_size(240.0, 320.0)
                .min_inner_size(180.0, 120.0)
                .always_on_top(true)
                .decorations(false)
                .resizable(true)
                .visible(false)
                .build()?;

            // Start the event-driven Claude session watcher
            let sessions_arc = app.state::<Sessions>().0.clone();
            let monitor_arc  = app.state::<ClaudeMonitor>().0.clone();
            let expected_arc = app.state::<ExpectedClaudeSessions>().0.clone();
            claude::start_claude_watcher(app.handle().clone(), sessions_arc, monitor_arc, expected_arc);

            // Show the main window after the window-state plugin has restored
            // its position/size so there's no visible jump.
            // On Windows/Linux, strip OS decorations — we ship custom chrome.
            // On macOS, decorations stay on so the native traffic lights render
            // on top of our overlay-style titlebar.
            if let Some(w) = app.get_webview_window("main") {
                #[cfg(not(target_os = "macos"))]
                { let _ = w.set_decorations(false); }

                // Apply saved size/position BEFORE show() so the user doesn't
                // see the window flash at default geometry and then snap.
                if let Ok(Some(cfg)) = config::load_config(app.handle().clone()) {
                    if let Some(win_cfg) = cfg.get("window") {
                        let width = win_cfg.get("width").and_then(|v| v.as_f64());
                        let height = win_cfg.get("height").and_then(|v| v.as_f64());
                        let x = win_cfg.get("x").and_then(|v| v.as_f64());
                        let y = win_cfg.get("y").and_then(|v| v.as_f64());
                        if let (Some(width), Some(height), Some(x), Some(y)) = (width, height, x, y) {
                            if width > 200.0 && height > 200.0
                                && x > -10000.0 && y > -10000.0
                                && x < 10000.0 && y < 10000.0
                            {
                                let _ = w.set_size(tauri::PhysicalSize::new(width as u32, height as u32));
                                let _ = w.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
                            }
                        }
                    }
                }

                w.show().unwrap_or_default();
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Intercept close on the overview window — hide instead of destroy
            if window.label() == OVERVIEW_WINDOW_LABEL {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    window.hide().unwrap_or_default();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            pty::create_session,
            claude::set_expected_claude_session,
            claude::clear_claude_monitor,
            pty::write_to_session,
            pty::resize_session,
            pty::close_session,
            pty::get_session_replay,
            pty::get_session_size,
            usage::get_usage,
            usage::report_usage,
            usage::report_auth_error,
            usage::open_login_window,
            usage::logout_usage,
            config::save_config,
            config::load_config,
            pty::get_session_cwd,
            pty::get_session_shell_idle,
            git::get_session_git_info,
            exit_app,
            get_locale,
            focus_webview,
            shell::check_git_bash,
            overview::show_overview_window,
            overview::hide_overview_window,
            overview::get_overview_state,
            overview::restore_overview_window,
            overview::reset_overview_window,
            git::git_worktree_list,
            git::git_worktree_add,
            git::git_is_branch_merged,
            git::git_worktree_remove,
            git::git_worktree_prune,
            git::git_worktree_restore,
            git::git_merge_branch,
            git::git_push_and_create_pr,
            git::git_repo_root,
            git::git_list_branches,
            fs::read_directory,
            git::git_file_status,
            fs::watch_directory,
            fs::unwatch_directory,
            fs::get_project_model,
            open_devtools,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
