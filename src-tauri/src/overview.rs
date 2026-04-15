use tauri::{AppHandle, Manager};

use crate::config::config_path;

pub const OVERVIEW_WINDOW_LABEL: &str = "overview";

pub const OVERVIEW_DEFAULT_WIDTH: f64 = 240.0;
pub const OVERVIEW_DEFAULT_HEIGHT: f64 = 320.0;

/// Check if a point is visible on any monitor
fn is_position_on_screen(app: &AppHandle, x: i32, y: i32, w: u32, h: u32) -> bool {
    if let Ok(monitors) = app.available_monitors() {
        for mon in monitors {
            let mp = mon.position();
            let ms = mon.size();
            // Window is on-screen if at least 50px of it overlaps a monitor
            let overlap_x = (x + w as i32).min(mp.x + ms.width as i32) - x.max(mp.x);
            let overlap_y = (y + h as i32).min(mp.y + ms.height as i32) - y.max(mp.y);
            if overlap_x >= 50 && overlap_y >= 30 {
                return true;
            }
        }
    }
    false
}

fn center_overview_on_main(app: &AppHandle, w: &tauri::WebviewWindow) {
    if let Some(main) = app.get_webview_window("main") {
        if let (Ok(mp), Ok(ms)) = (main.outer_position(), main.outer_size()) {
            let x = mp.x + (ms.width as i32 - OVERVIEW_DEFAULT_WIDTH as i32) / 2;
            let y = mp.y + (ms.height as i32 - OVERVIEW_DEFAULT_HEIGHT as i32) / 2;
            w.set_position(tauri::PhysicalPosition::new(x, y)).ok();
        }
    }
    w.set_size(tauri::PhysicalSize::new(OVERVIEW_DEFAULT_WIDTH as u32, OVERVIEW_DEFAULT_HEIGHT as u32)).ok();
}

#[tauri::command]
pub fn show_overview_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        // Validate position is on-screen (may have moved off after sleep/monitor change)
        if let (Ok(pos), Ok(size)) = (w.outer_position(), w.inner_size()) {
            if !is_position_on_screen(&app, pos.x, pos.y, size.width, size.height) {
                center_overview_on_main(&app, &w);
            }
        }
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn hide_overview_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        w.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn get_overview_state(app: AppHandle) -> Result<Option<serde_json::Value>, String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        let visible = w.is_visible().unwrap_or(false);
        if visible {
            let pos = w.outer_position().map_err(|e| e.to_string())?;
            let size = w.inner_size().map_err(|e| e.to_string())?;
            return Ok(Some(serde_json::json!({
                "x": pos.x,
                "y": pos.y,
                "width": size.width,
                "height": size.height,
            })));
        }
    }
    Ok(None)
}

#[tauri::command]
pub fn restore_overview_window(app: AppHandle, x: i32, y: i32, width: u32, height: u32) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        if is_position_on_screen(&app, x, y, width, height) {
            w.set_position(tauri::PhysicalPosition::new(x, y)).map_err(|e| e.to_string())?;
            w.set_size(tauri::PhysicalSize::new(width, height)).map_err(|e| e.to_string())?;
        } else {
            center_overview_on_main(&app, &w);
        }
        w.show().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn reset_overview_window(app: AppHandle, to_default: bool) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        if to_default {
            center_overview_on_main(&app, &w);
        } else {
            // Restore to saved config position
            let path = config_path(&app)?;
            if path.exists() {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Ok(config) = serde_json::from_str::<serde_json::Value>(&data) {
                        if let Some(ov) = config.get("overview") {
                            let x = ov.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                            let y = ov.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                            let width = ov.get("width").and_then(|v| v.as_u64()).unwrap_or(OVERVIEW_DEFAULT_WIDTH as u64) as u32;
                            let height = ov.get("height").and_then(|v| v.as_u64()).unwrap_or(OVERVIEW_DEFAULT_HEIGHT as u64) as u32;
                            if is_position_on_screen(&app, x, y, width, height) {
                                w.set_position(tauri::PhysicalPosition::new(x, y)).ok();
                                w.set_size(tauri::PhysicalSize::new(width, height)).ok();
                                w.show().map_err(|e| e.to_string())?;
                                w.set_focus().map_err(|e| e.to_string())?;
                                return Ok(());
                            }
                        }
                    }
                }
            }
            // Fallback to default if no saved config or position is off-screen
            center_overview_on_main(&app, &w);
        }
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}
