use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

#[derive(Serialize, Clone)]
pub struct DirEntry {
    name: String,
    path: String,
    is_dir: bool,
    is_symlink: bool,
}

pub struct FileWatchers(pub Arc<Mutex<HashMap<String, RecommendedWatcher>>>);

#[tauri::command]
pub fn get_project_model(project_path: String) -> Option<String> {
    let settings_path = std::path::Path::new(&project_path).join(".claude").join("settings.json");
    let content = std::fs::read_to_string(&settings_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("model").and_then(|v| v.as_str()).map(|s| s.to_string())
}

#[tauri::command]
pub fn read_directory(path: String, show_hidden: Option<bool>) -> Result<Vec<DirEntry>, String> {
    let show_hidden = show_hidden.unwrap_or(false);
    let dir = std::fs::read_dir(&path).map_err(|e| format!("Failed to read directory: {}", e))?;

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in dir {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden files unless requested
        if !show_hidden && name.starts_with('.') {
            continue;
        }

        // Skip .git directory
        if name == ".git" {
            continue;
        }

        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        let is_symlink = entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false);
        let is_dir = metadata.is_dir();

        let item = DirEntry {
            name: name.clone(),
            path: entry.path().to_string_lossy().to_string(),
            is_dir,
            is_symlink,
        };

        if is_dir {
            dirs.push(item);
        } else {
            files.push(item);
        }
    }

    // Sort: directories first (alphabetical), then files (alphabetical)
    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    dirs.extend(files);

    Ok(dirs)
}

#[tauri::command]
pub fn watch_directory(app: AppHandle, watchers: State<FileWatchers>, path: String, recursive: Option<bool>) -> Result<String, String> {
    let watcher_id = Uuid::new_v4().to_string();
    let app_handle = app.clone();
    let watcher_id_clone = watcher_id.clone();

    let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
        if let Ok(event) = res {
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                    let _ = app_handle.emit(&format!("fs-changed-{}", watcher_id_clone), ());
                }
                _ => {}
            }
        }
    }).map_err(|e| format!("Failed to create watcher: {}", e))?;

    let mode = if recursive.unwrap_or(false) {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    watcher.watch(std::path::Path::new(&path), mode)
        .map_err(|e| format!("Failed to watch directory: {}", e))?;

    watchers.0.lock().unwrap().insert(watcher_id.clone(), watcher);
    Ok(watcher_id)
}

#[tauri::command]
pub fn unwatch_directory(watcher_id: String, watchers: State<FileWatchers>) {
    watchers.0.lock().unwrap().remove(&watcher_id);
}
