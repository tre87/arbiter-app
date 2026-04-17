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

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("Path does not exist: {}", path));
    }
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        // `cmd /C start "" "path"` honours file associations without opening a
        // visible shell window. The empty quoted string is the window title
        // argument that `start` requires when the target is quoted.
        Command::new("cmd")
            .args(["/C", "start", "", &path])
            .spawn()
            .map_err(|e| format!("Failed to open path: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        Command::new("open").arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open path: {}", e))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::process::Command;
        Command::new("xdg-open").arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open path: {}", e))?;
    }
    Ok(())
}

#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("Path does not exist: {}", path));
    }
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        // `explorer /select,path` requires no space after the comma and
        // exits with code 1 on success, which isn't an error for us.
        Command::new("explorer")
            .arg(format!("/select,{}", path))
            .spawn()
            .map_err(|e| format!("Failed to reveal path: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        Command::new("open").args(["-R", &path])
            .spawn()
            .map_err(|e| format!("Failed to reveal path: {}", e))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // No cross-DE way to select a specific file on Linux; fall back to
        // opening the parent directory.
        use std::process::Command;
        let parent = p.parent().ok_or_else(|| "Path has no parent".to_string())?;
        Command::new("xdg-open").arg(parent)
            .spawn()
            .map_err(|e| format!("Failed to reveal path: {}", e))?;
    }
    Ok(())
}

#[tauri::command]
pub fn rename_path(old_path: String, new_name: String) -> Result<String, String> {
    let old = std::path::Path::new(&old_path);
    if !old.exists() {
        return Err(format!("Path does not exist: {}", old_path));
    }
    if new_name.is_empty() || new_name.contains('/') || new_name.contains('\\') {
        return Err("Invalid name".to_string());
    }
    let parent = old.parent().ok_or_else(|| "Path has no parent".to_string())?;
    let new_path = parent.join(&new_name);
    if new_path.exists() {
        return Err(format!("{} already exists", new_name));
    }
    std::fs::rename(&old, &new_path).map_err(|e| format!("Failed to rename: {}", e))?;
    Ok(new_path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn trash_path(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("Path does not exist: {}", path));
    }
    trash::delete(p).map_err(|e| format!("Failed to move to trash: {}", e))?;
    Ok(())
}
