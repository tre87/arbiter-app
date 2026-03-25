use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde::Serialize;
use tauri::WebviewWindowBuilder;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

struct PtySession {
    writer: Box<dyn Write + Send>,
    // Keep master alive so PTY doesn't close
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

struct Sessions(Mutex<HashMap<String, PtySession>>);

#[tauri::command]
fn create_session(app: AppHandle, sessions: State<Sessions>, cols: Option<u16>, rows: Option<u16>) -> Result<String, String> {
    let session_id = Uuid::new_v4().to_string();
    let sid = session_id.clone();

    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.unwrap_or(24),
            cols: cols.unwrap_or(80),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    let mut cmd = build_shell_command();
    cmd.env("TERM", "xterm-256color");

    let _child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    // slave must be dropped after spawning so the master gets EOF when child exits
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    // Spawn thread to stream PTY output to the frontend
    let app_handle = app.clone();
    let event_name = format!("pty-output-{}", sid);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let output = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = app_handle.emit(&event_name, output);
                }
            }
        }
    });

    sessions.0.lock().unwrap().insert(
        session_id.clone(),
        PtySession {
            writer,
            _master: pair.master,
        },
    );

    Ok(session_id)
}

#[tauri::command]
fn write_to_session(
    session_id: String,
    data: String,
    sessions: State<Sessions>,
) -> Result<(), String> {
    let mut map = sessions.0.lock().unwrap();
    if let Some(session) = map.get_mut(&session_id) {
        session
            .writer
            .write_all(data.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn resize_session(
    session_id: String,
    cols: u16,
    rows: u16,
    sessions: State<Sessions>,
) -> Result<(), String> {
    let map = sessions.0.lock().unwrap();
    if let Some(session) = map.get(&session_id) {
        session
            ._master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn close_session(session_id: String, sessions: State<Sessions>) {
    sessions.0.lock().unwrap().remove(&session_id);
}

// ── Usage cache ────────────────────────────────────────────────────────────

const AUTH_WINDOW_LABEL: &str = "auth";

// Injected into the auth WebView. Uses the browser's own session cookies.
const USAGE_INIT_SCRIPT: &str = r#"
(function() {
    async function fetchUsage() {
        try {
            const orgsResp = await fetch('/api/organizations');
            if (orgsResp.status === 401 || orgsResp.status === 403) {
                window.__TAURI_INTERNALS__.invoke('report_auth_error', {});
                return;
            }
            if (!orgsResp.ok) return;
            const orgs = await orgsResp.json();
            const org = Array.isArray(orgs) ? orgs[0] : orgs;
            const orgId = org && (org.uuid || org.id);
            if (!orgId) return;
            const usageResp = await fetch('/api/organizations/' + orgId + '/usage');
            if (!usageResp.ok) return;
            const usage = await usageResp.json();
            window.__TAURI_INTERNALS__.invoke('report_usage', { data: JSON.stringify(usage) });
        } catch (_) {}
    }

    // Initial fetch after page settles
    setTimeout(fetchUsage, 800);

    // Periodic refresh every 2 minutes
    setInterval(fetchUsage, 120000);

    // Watch for login completion (SPA navigation away from /login)
    var loginWatcher = setInterval(function() {
        if (!window.location.pathname.startsWith('/login')) {
            clearInterval(loginWatcher);
            setTimeout(fetchUsage, 800);
        }
    }, 1000);
    setTimeout(function() { clearInterval(loginWatcher); }, 600000);
})();
"#;

// Three-state auth: waiting for first WebView response, login required, or ok
#[derive(PartialEq)]
enum AuthState { Pending, NeedsLogin, Ok }

struct UsageCache {
    data: Option<UsageResponse>,
    auth: AuthState,
}

impl UsageCache {
    fn new() -> Self { Self { data: None, auth: AuthState::Pending } }
    fn store(&mut self, data: UsageResponse) {
        self.auth = AuthState::Ok;
        self.data = Some(data);
    }
    fn set_needs_login(&mut self) { self.auth = AuthState::NeedsLogin; }
}

struct Cache(Mutex<UsageCache>);

// ── Usage stats ────────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
struct UsagePeriod {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Serialize, Clone)]
struct UsageResponse {
    five_hour: Option<UsagePeriod>,
    seven_day: Option<UsagePeriod>,
    seven_day_opus: Option<UsagePeriod>,
    seven_day_sonnet: Option<UsagePeriod>,
    plan: String,
}

fn parse_usage_period(obj: &serde_json::Value) -> Option<UsagePeriod> {
    let utilization = obj.get("utilization")?.as_f64()?;
    let resets_at = obj.get("resets_at").and_then(|v| v.as_str()).map(|s| s.to_string());
    Some(UsagePeriod {
        utilization: utilization.clamp(0.0, 100.0),
        resets_at,
    })
}

// Called by the auth WebView's injected script after fetching usage data
#[tauri::command]
async fn report_usage(data: String, cache: State<'_, Cache>, app: AppHandle) -> Result<(), String> {
    let json: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| format!("Parse error: {e}"))?;

    let five_hour = json.get("five_hour").and_then(parse_usage_period);
    let seven_day = json.get("seven_day").and_then(parse_usage_period);
    let seven_day_opus = json.get("seven_day_opus").and_then(parse_usage_period);
    let seven_day_sonnet = json.get("seven_day_sonnet").and_then(parse_usage_period);

    let plan = if seven_day_opus.is_some() || seven_day_sonnet.is_some() {
        "Max"
    } else if seven_day.is_some() {
        "Pro"
    } else {
        "Free"
    }.to_string();

    cache.0.lock().unwrap().store(UsageResponse {
        five_hour, seven_day, seven_day_opus, seven_day_sonnet, plan,
    });

    // Hide the login window once we have data
    if let Some(w) = app.get_webview_window(AUTH_WINDOW_LABEL) {
        w.hide().ok();
    }

    // Notify the main window to refresh its display
    app.emit("usage-updated", ()).ok();

    Ok(())
}

// Called by the auth WebView when it receives a 401/403
#[tauri::command]
fn report_auth_error(cache: State<'_, Cache>, app: AppHandle) {
    cache.0.lock().unwrap().set_needs_login();
    app.emit("usage-updated", ()).ok();
}

// Returns cached usage; errors distinguish "still loading" from "must log in"
#[tauri::command]
fn get_usage(cache: State<'_, Cache>) -> Result<UsageResponse, String> {
    let guard = cache.0.lock().unwrap();
    match (&guard.auth, &guard.data) {
        (_, Some(d)) => Ok(d.clone()),
        (AuthState::NeedsLogin, _) => Err("needs_login".to_string()),
        _ => Err("pending".to_string()),
    }
}

// Shows the auth WebView window so the user can log in
#[tauri::command]
fn open_login_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(AUTH_WINDOW_LABEL) {
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let url: url::Url = "https://claude.ai".parse().map_err(|e: url::ParseError| e.to_string())?;
    WebviewWindowBuilder::new(&app, AUTH_WINDOW_LABEL, tauri::WebviewUrl::External(url))
        .title("Sign in to Claude")
        .inner_size(960.0, 720.0)
        .initialization_script(USAGE_INIT_SCRIPT)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ── Shell ───────────────────────────────────────────────────────────────────

fn build_shell_command() -> CommandBuilder {
    #[cfg(target_os = "windows")]
    {
        CommandBuilder::new("powershell.exe")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(Sessions(Mutex::new(HashMap::new())))
        .manage(Cache(Mutex::new(UsageCache::new())))
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_session,
            write_to_session,
            resize_session,
            close_session,
            get_usage,
            report_usage,
            report_auth_error,
            open_login_window,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
