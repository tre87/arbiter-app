mod claude;
pub mod claude_shim;
mod config;
mod fs;
mod git;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_fps;
mod overview;
mod pty;
mod shell;
mod spike;
mod termgrid;
mod usage;
mod util;

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
    // Destroy every webview window before `app.exit` to avoid a Chromium
    // `Failed to unregister class Chrome_WidgetWin_0` error on Windows.
    // Destroying main here is safe: the frontend awaits flushAutosave()
    // (which awaits save_config — synchronous fs::write + fs::rename in
    // Rust) before calling exit_app, so by the time we run, the save has
    // already returned to the renderer. There is no concurrent save in
    // flight that destroying main could interrupt.
    // `destroy()` bypasses CloseRequested handlers (the overview window
    // hides itself on close, which would otherwise keep it alive past exit).
    // https://github.com/tauri-apps/tauri/issues/7606
    for (_, window) in app.webview_windows() {
        let _ = window.destroy();
    }
    app.exit(0);
}

// Available in release builds because the `devtools` Cargo feature is enabled
// on the `tauri` crate (see Cargo.toml).
#[tauri::command]
fn open_devtools(webview_window: tauri::WebviewWindow) {
    webview_window.open_devtools();
}

#[tauri::command]
#[cfg_attr(not(windows), allow(unused_variables))]
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
        if let Ok(output) = crate::util::hidden_command("powershell")
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
            // Size/position and maximize state are handled in arbiter.json and
            // applied from our setup() before show() — owning maximize ourselves
            // avoids the launch blink that the plugin's async restore causes.
            // The plugin still handles fullscreen, which is rare and harder to
            // apply synchronously cross-platform.
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(tauri_plugin_window_state::StateFlags::FULLSCREEN)
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
        .manage(git::GitWatchers::new())
        .manage(spike::SpikeState::new())
        .manage(termgrid::TermGridState::new())
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
            // Build it AT its saved geometry so the webview initialises on the
            // correct monitor at the correct DPI scale — creating it on one
            // monitor and moving it to a different-DPI monitor later leaves the
            // webview at the wrong devicePixelRatio (content fills only part of
            // the window).
            let saved_overview = overview::saved_geometry(&app.handle());
            let mut overview_builder =
                WebviewWindowBuilder::new(app, OVERVIEW_WINDOW_LABEL, tauri::WebviewUrl::default())
                    .title("Arbiter – Overview")
                    .min_inner_size(180.0, 120.0)
                    .always_on_top(true)
                    .decorations(false)
                    .resizable(true)
                    .visible(false);
            overview_builder = match saved_overview {
                Some((x, y, width, height)) => overview_builder
                    .position(x, y)
                    .inner_size(width, height),
                None => overview_builder
                    .inner_size(overview::OVERVIEW_DEFAULT_WIDTH, overview::OVERVIEW_DEFAULT_HEIGHT),
            };
            let overview_window = overview_builder.build()?;
            // Same high-refresh unlock as the main window (see macos_fps.rs).
            #[cfg(target_os = "macos")]
            {
                let _ = overview_window.with_webview(|pw| unsafe {
                    macos_fps::unlock_high_fps(pw.inner());
                });
            }

            // Start the event-driven Claude session watcher
            let sessions_arc = app.state::<Sessions>().0.clone();
            let monitor_arc  = app.state::<ClaudeMonitor>().0.clone();
            let expected_arc = app.state::<ExpectedClaudeSessions>().0.clone();
            // Watch the Tier-2 statusLine capture dir and route Claude's exact
            // context usage to the matching pane.
            if let Ok(data_dir) = app.path().app_data_dir() {
                claude::start_capture_watcher(
                    app.handle().clone(),
                    monitor_arc.clone(),
                    data_dir.join(claude_shim::CAPTURE_SUBDIR),
                );
                claude::start_hook_watcher(
                    app.handle().clone(),
                    monitor_arc.clone(),
                    data_dir.join(claude_shim::HOOKS_SUBDIR),
                );
            }
            claude::start_claude_watcher(app.handle().clone(), sessions_arc, monitor_arc, expected_arc);

            // Show the main window after the window-state plugin has restored
            // its position/size so there's no visible jump.
            // On Windows/Linux, strip OS decorations — we ship custom chrome.
            // On macOS, decorations stay on so the native traffic lights render
            // on top of our overlay-style titlebar.
            if let Some(w) = app.get_webview_window("main") {
                #[cfg(not(target_os = "macos"))]
                { let _ = w.set_decorations(false); }

                // First launch (no config) → windowed at the default 1200x800
                // from tauri.conf.json. Otherwise apply saved geometry, or
                // maximize when the user last quit while maximized.
                let mut should_maximize = false;
                if let Ok(Some(cfg)) = config::load_config(app.handle().clone()) {
                    if let Some(win_cfg) = cfg.get("window") {
                        should_maximize = win_cfg.get("maximized")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        if !should_maximize {
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
                }

                // On Windows, w.maximize() is ShowWindow(SW_MAXIMIZE) — it
                // makes the hidden window visible at the maximized size in
                // one atomic OS call, so there's no windowed-then-maximized
                // size transition. The webview's pre-Vue paint is hidden by
                // the matching dark backgroundColor (tauri.conf.json).
                if should_maximize {
                    let _ = w.maximize();
                }
                w.show().unwrap_or_default();

                // The set_size/set_position/maximize calls above make AppKit
                // reset the native traffic-light buttons to their default
                // offset, discarding the trafficLightPosition from config.
                // Re-apply it now so the buttons stay inset on first paint.
                #[cfg(target_os = "macos")]
                if let Ok(ptr) = w.ns_window() {
                    macos::apply_traffic_light_position(ptr);
                }

                // Lift WebKit's ~60fps page-rendering cap so rAF runs at the
                // display's native refresh (e.g. 120/144 Hz). Private SPI —
                // guarded to no-op if the API changes. See macos_fps.rs.
                #[cfg(target_os = "macos")]
                {
                    let _ = w.with_webview(|pw| unsafe {
                        macos_fps::unlock_high_fps(pw.inner());
                    });
                }
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

            // Resizing/maximizing/fullscreen-exit and scale-factor changes reset
            // the native traffic lights to their default offset on macOS; re-inset
            // the main window's buttons each time so they stay aligned.
            #[cfg(target_os = "macos")]
            if window.label() == "main"
                && matches!(
                    event,
                    tauri::WindowEvent::Resized(_)
                        | tauri::WindowEvent::ScaleFactorChanged { .. }
                )
            {
                if let Ok(ptr) = window.ns_window() {
                    macos::apply_traffic_light_position(ptr);
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
            pty::pause_session,
            pty::resume_session,
            pty::get_session_replay,
            pty::get_session_size,
            usage::get_usage,
            usage::report_usage,
            usage::report_auth_error,
            usage::report_orgs,
            usage::get_selected_org_uuid,
            usage::get_available_orgs,
            usage::set_selected_org,
            usage::open_login_window,
            usage::logout_usage,
            config::save_config,
            config::load_config,
            pty::get_session_cwd,
            pty::get_session_shell_idle,
            git::get_session_git_info,
            git::watch_git,
            git::unwatch_git,
            claude::claude_persist_info,
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
            fs::open_path,
            fs::reveal_path,
            fs::rename_path,
            fs::trash_path,
            open_devtools,
            spike::spike_start,
            spike::spike_stop,
            spike::spike_write,
            spike::spike_resize,
            spike::spike_stress,
            spike::spike_stress_stop,
            termgrid::termgrid_start,
            termgrid::termgrid_attach,
            termgrid::termgrid_detach,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Cmd+Q / dock Quit bypass the JS close handler's autosave flush, so
            // persist the overview window's geometry here before teardown.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                overview::persist_overview_geometry(app_handle);
            }
        });
}
