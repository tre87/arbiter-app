use std::path::PathBuf;
use tauri::{AppHandle, Manager};

pub fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("config.json"))
}

#[tauri::command]
pub fn save_config(app: AppHandle, config: serde_json::Value) -> Result<(), String> {
    let path = config_path(&app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    // Atomic write: write to temp file, then rename for crash safety
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn load_config(app: AppHandle) -> Result<Option<serde_json::Value>, String> {
    let path = config_path(&app)?;
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    match serde_json::from_str::<serde_json::Value>(&data) {
        Ok(config) => Ok(Some(config)),
        Err(e) => {
            // Quarantine the corrupt config so the autosave that runs after
            // load failure can't overwrite recoverable state. Without this,
            // a single bad write or a forward-incompatible field would wipe
            // the user's workspace tree on the next save tick.
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = path.with_extension(format!("json.corrupt-{ts}"));
            if let Err(rename_err) = std::fs::rename(&path, &backup) {
                eprintln!("load_config: failed to quarantine corrupt {}: {rename_err}", path.display());
            }
            Err(format!("config parse error (corrupt file moved to {}): {e}", backup.display()))
        }
    }
}
