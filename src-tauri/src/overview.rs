use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, WebviewWindow};

use crate::config::config_path;

pub const OVERVIEW_WINDOW_LABEL: &str = "overview";

// Geometry is stored in LOGICAL pixels (scale-independent — what the user sees).
// The window is *created* at its saved geometry in lib.rs setup() so its webview
// initialises on the correct monitor at the correct DPI scale: a window created
// on one monitor and later moved to a different-DPI monitor keeps the wrong
// devicePixelRatio (content then fills only part of the window). Avoid set_size
// after creation across monitors for that reason.
pub const OVERVIEW_DEFAULT_WIDTH: f64 = 240.0;
pub const OVERVIEW_DEFAULT_HEIGHT: f64 = 320.0;
const OVERVIEW_MIN_WIDTH: f64 = 180.0;
const OVERVIEW_MIN_HEIGHT: f64 = 120.0;
const OVERVIEW_MAX_DIM: f64 = 6000.0;

/// Is the logical rect (x, y, w, h) visible on any monitor? Each monitor's
/// physical bounds are converted to logical via its own scale so the comparison
/// happens in the same (logical) space as the window coords.
fn is_position_on_screen(app: &AppHandle, x: f64, y: f64, w: f64, h: f64) -> bool {
    if let Ok(monitors) = app.available_monitors() {
        for mon in monitors {
            let s = mon.scale_factor();
            let mp = mon.position().to_logical::<f64>(s);
            let ms = mon.size().to_logical::<f64>(s);
            let overlap_x = (x + w).min(mp.x + ms.width) - x.max(mp.x);
            let overlap_y = (y + h).min(mp.y + ms.height) - y.max(mp.y);
            if overlap_x >= 50.0 && overlap_y >= 30.0 {
                return true;
            }
        }
    }
    false
}

/// Logical overview geometry saved in config, if present and sane. Used at
/// startup to build the window in place (so the webview gets the right DPI).
pub fn saved_geometry(app: &AppHandle) -> Option<(f64, f64, f64, f64)> {
    let path = config_path(app).ok()?;
    let data = std::fs::read_to_string(&path).ok()?;
    let cfg: serde_json::Value = serde_json::from_str(&data).ok()?;
    let ov = cfg.get("overview")?;
    let x = ov.get("x")?.as_f64()?;
    let y = ov.get("y")?.as_f64()?;
    let width = ov.get("width")?.as_f64()?;
    let height = ov.get("height")?.as_f64()?;
    if !(OVERVIEW_MIN_WIDTH..=OVERVIEW_MAX_DIM).contains(&width)
        || !(OVERVIEW_MIN_HEIGHT..=OVERVIEW_MAX_DIM).contains(&height)
        || !is_position_on_screen(app, x, y, width, height)
    {
        return None;
    }
    Some((x, y, width, height))
}

fn center_overview_on_main(app: &AppHandle, w: &WebviewWindow) {
    if let Some(main) = app.get_webview_window("main") {
        if let (Ok(mp), Ok(ms)) = (main.outer_position(), main.outer_size()) {
            let scale = main.scale_factor().unwrap_or(1.0);
            let mpl = mp.to_logical::<f64>(scale);
            let msl = ms.to_logical::<f64>(scale);
            let x = mpl.x + (msl.width - OVERVIEW_DEFAULT_WIDTH) / 2.0;
            let y = mpl.y + (msl.height - OVERVIEW_DEFAULT_HEIGHT) / 2.0;
            w.set_position(LogicalPosition::new(x, y)).ok();
        }
    }
    w.set_size(LogicalSize::new(OVERVIEW_DEFAULT_WIDTH, OVERVIEW_DEFAULT_HEIGHT)).ok();
}

/// Current overview geometry in logical pixels (None when hidden/unavailable).
fn overview_logical_geometry(w: &WebviewWindow) -> Option<(f64, f64, f64, f64)> {
    let scale = w.scale_factor().ok()?;
    let pos = w.outer_position().ok()?.to_logical::<f64>(scale);
    let size = w.inner_size().ok()?.to_logical::<f64>(scale);
    Some((pos.x, pos.y, size.width, size.height))
}

#[tauri::command]
pub fn show_overview_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        // Validate position is on-screen (may have moved off after sleep/monitor change)
        if let Some((x, y, ww, hh)) = overview_logical_geometry(&w) {
            if !is_position_on_screen(&app, x, y, ww, hh) {
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
        if w.is_visible().unwrap_or(false) {
            if let Some((x, y, width, height)) = overview_logical_geometry(&w) {
                return Ok(Some(serde_json::json!({
                    "x": x.round(),
                    "y": y.round(),
                    "width": width.round(),
                    "height": height.round(),
                })));
            }
        }
    }
    Ok(None)
}

/// Move/resize an already-open overview (logical). Used by reset; startup uses
/// build-time geometry instead to keep the webview's DPI correct.
#[tauri::command]
pub fn restore_overview_window(app: AppHandle, x: f64, y: f64, width: f64, height: f64) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) {
        if is_position_on_screen(&app, x, y, width, height) {
            w.set_position(LogicalPosition::new(x, y)).ok();
            w.set_size(LogicalSize::new(width, height)).ok();
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
        match (to_default, saved_geometry(&app)) {
            (false, Some((x, y, width, height))) => {
                w.set_position(LogicalPosition::new(x, y)).ok();
                w.set_size(LogicalSize::new(width, height)).ok();
            }
            _ => center_overview_on_main(&app, &w),
        }
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Persist the overview window's geometry + visibility straight to config on app
/// exit. The JS autosave's flush only runs on the window-close path; Cmd+Q (and
/// the dock Quit) bypass it, so the overview's size/position were lost. This runs
/// from the RunEvent::ExitRequested handler, before windows are torn down.
///
/// Read-modify-write touching only the overview fields. Safe against the JS
/// writer: on Cmd+Q the frontend isn't saving; on the red-button path the
/// overview window is already destroyed (so this no-ops) and the JS flush has
/// persisted the full config.
pub fn persist_overview_geometry(app: &AppHandle) {
    let Some(w) = app.get_webview_window(OVERVIEW_WINDOW_LABEL) else { return };
    let visible = w.is_visible().unwrap_or(false);
    let Ok(path) = config_path(app) else { return };

    let mut cfg = std::fs::read_to_string(&path)
        .ok()
        .and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let Some(obj) = cfg.as_object_mut() else { return };

    obj.insert("overviewVisible".to_string(), serde_json::json!(visible));
    if visible {
        if let Some((x, y, width, height)) = overview_logical_geometry(&w) {
            obj.insert(
                "overview".to_string(),
                serde_json::json!({ "x": x.round(), "y": y.round(), "width": width.round(), "height": height.round() }),
            );
        }
    }

    if let Ok(json) = serde_json::to_string_pretty(&cfg) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}
