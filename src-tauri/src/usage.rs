use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindowBuilder};

pub const AUTH_WINDOW_LABEL: &str = "auth";

// Injected into the auth WebView. Uses the browser's own session cookies.
// Multi-org flow: enumerate orgs, ask the backend for a saved selection, and
// either fetch usage for the chosen org or report the org list so the user
// can pick one via the OrgSelectionDialog. The backend re-triggers this
// script via window.__arbiterRefetchUsage after a selection is saved.
pub const USAGE_INIT_SCRIPT: &str = r#"
(function() {
    function reportAuthError() {
        try { window.__TAURI_INTERNALS__.invoke('report_auth_error', {}); } catch(_) {}
    }

    async function fetchUsage() {
        // If we landed on about:blank (e.g. after logout) bounce back to
        // claude.ai so the init script re-runs in a sensible origin.
        if (location.protocol !== 'https:' || location.hostname !== 'claude.ai') {
            if (location.href !== 'https://claude.ai/') {
                location.href = 'https://claude.ai/';
            }
            return;
        }

        var orgsResp;
        try {
            orgsResp = await fetch('/api/organizations');
        } catch (_) {
            // Network error — treat as needs-login so the UI can show the button
            reportAuthError();
            return;
        }
        if (orgsResp.status === 401 || orgsResp.status === 403) {
            reportAuthError();
            return;
        }
        if (!orgsResp.ok) {
            reportAuthError();
            return;
        }

        var raw;
        try { raw = await orgsResp.json(); } catch (_) { reportAuthError(); return; }
        const list = (Array.isArray(raw) ? raw : [raw])
            .map(function(o) { return { uuid: o.uuid || o.id, name: o.name || (o.uuid || o.id) }; })
            .filter(function(o) { return !!o.uuid; });
        if (list.length === 0) { reportAuthError(); return; }

        var chosen;
        if (list.length === 1) {
            chosen = list[0];
        } else {
            var savedUuid;
            try { savedUuid = await window.__TAURI_INTERNALS__.invoke('get_selected_org_uuid'); } catch (_) {}
            chosen = list.find(function(o) { return o.uuid === savedUuid; });
            if (!chosen) {
                try { await window.__TAURI_INTERNALS__.invoke('report_orgs', { orgs: list }); } catch (_) {}
                return;
            }
        }

        var usageResp;
        try { usageResp = await fetch('/api/organizations/' + chosen.uuid + '/usage'); }
        catch (_) { reportAuthError(); return; }
        if (!usageResp.ok) { reportAuthError(); return; }
        var usage;
        try { usage = await usageResp.json(); } catch (_) { reportAuthError(); return; }

        // We send the full org list every time so Settings → "Switch
        // organization" can render even when the user already has a saved
        // choice and never hit the report_orgs path.
        usage.__org_uuid = chosen.uuid;
        usage.__org_name = chosen.name;
        usage.__has_multiple_orgs = list.length > 1;
        usage.__orgs = list;
        try {
            const accResp = await fetch('/api/account');
            if (accResp.ok) {
                const acc = await accResp.json();
                usage.__account_email = acc.email_address || acc.email || null;
                usage.__account_name = acc.full_name || acc.name || null;
            }
        } catch(_) {}
        try { window.__TAURI_INTERNALS__.invoke('report_usage', { data: JSON.stringify(usage) }); } catch(_) {}
    }

    // Expose so the backend can re-trigger after the user picks an org.
    window.__arbiterRefetchUsage = fetchUsage;

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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OrgInfo {
    pub uuid: String,
    pub name: String,
}

// Four-state auth: waiting for first response, login required, multi-org
// detected with no valid saved choice, or ok with data.
#[derive(PartialEq)]
enum AuthState { Pending, NeedsLogin, NeedsOrgSelection, Ok }

pub struct UsageCache {
    data: Option<UsageResponse>,
    auth: AuthState,
    available_orgs: Vec<OrgInfo>,
}

impl UsageCache {
    pub fn new() -> Self {
        Self { data: None, auth: AuthState::Pending, available_orgs: Vec::new() }
    }
    fn store(&mut self, data: UsageResponse) {
        self.auth = AuthState::Ok;
        self.data = Some(data);
    }
    fn set_needs_login(&mut self) {
        self.auth = AuthState::NeedsLogin;
        self.available_orgs.clear();
    }
    fn set_needs_org_selection(&mut self, orgs: Vec<OrgInfo>) {
        self.auth = AuthState::NeedsOrgSelection;
        self.available_orgs = orgs;
    }
}

pub struct Cache(pub Mutex<UsageCache>);

#[derive(Serialize, Clone)]
pub struct UsagePeriod {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct UsageResponse {
    five_hour: Option<UsagePeriod>,
    seven_day: Option<UsagePeriod>,
    seven_day_opus: Option<UsagePeriod>,
    seven_day_sonnet: Option<UsagePeriod>,
    plan: String,
    account_email: Option<String>,
    account_name: Option<String>,
    org_name: Option<String>,
    has_multiple_orgs: bool,
}

fn parse_usage_period(obj: &serde_json::Value) -> Option<UsagePeriod> {
    let utilization = obj.get("utilization")?.as_f64()?;
    let resets_at = obj.get("resets_at").and_then(|v| v.as_str()).map(|s| s.to_string());
    Some(UsagePeriod {
        utilization: utilization.clamp(0.0, 100.0),
        resets_at,
    })
}

fn org_selection_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    // Same dev/release split as `config.rs` — keeps the dev build's org
    // picker state separate from the installed release's.
    let file = if cfg!(debug_assertions) { "org-selection-dev.json" } else { "org-selection.json" };
    Ok(dir.join(file))
}

fn load_selected_org(app: &AppHandle) -> Option<OrgInfo> {
    let path = org_selection_path(app).ok()?;
    if !path.exists() { return None; }
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_selected_org_to_disk(app: &AppHandle, org: &OrgInfo) -> Result<(), String> {
    let path = org_selection_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(org).map_err(|e| e.to_string())?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
    Ok(())
}

fn clear_saved_org(app: &AppHandle) -> Result<(), String> {
    let path = org_selection_path(app)?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// Called by the auth WebView's injected script after fetching usage data
#[tauri::command]
pub async fn report_usage(data: String, cache: State<'_, Cache>, app: AppHandle) -> Result<(), String> {
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

    let account_email = json.get("__account_email").and_then(|v| v.as_str()).map(|s| s.to_string());
    let account_name = json.get("__account_name").and_then(|v| v.as_str()).map(|s| s.to_string());
    let org_name = json.get("__org_name").and_then(|v| v.as_str()).map(|s| s.to_string());
    let has_multiple_orgs = json.get("__has_multiple_orgs").and_then(|v| v.as_bool()).unwrap_or(false);
    let orgs: Vec<OrgInfo> = json.get("__orgs")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    {
        let mut guard = cache.0.lock().unwrap();
        if !orgs.is_empty() {
            guard.available_orgs = orgs;
        }
        guard.store(UsageResponse {
            five_hour, seven_day, seven_day_opus, seven_day_sonnet, plan,
            account_email, account_name, org_name, has_multiple_orgs,
        });
    }

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
pub fn report_auth_error(cache: State<'_, Cache>, app: AppHandle) {
    cache.0.lock().unwrap().set_needs_login();
    app.emit("usage-updated", ()).ok();
}

// Called by the auth WebView when multiple orgs are detected and no valid
// saved choice exists. The frontend opens OrgSelectionDialog on this signal.
#[tauri::command]
pub fn report_orgs(orgs: Vec<OrgInfo>, cache: State<'_, Cache>, app: AppHandle) {
    cache.0.lock().unwrap().set_needs_org_selection(orgs);
    app.emit("usage-updated", ()).ok();
}

#[tauri::command]
pub fn get_selected_org_uuid(app: AppHandle) -> Option<String> {
    load_selected_org(&app).map(|o| o.uuid)
}

#[tauri::command]
pub fn get_available_orgs(cache: State<'_, Cache>) -> Vec<OrgInfo> {
    cache.0.lock().unwrap().available_orgs.clone()
}

// Frontend calls this when the user picks an org from the dialog. Persists
// the choice to disk and re-triggers the auth WebView's fetch immediately.
#[tauri::command]
pub fn set_selected_org(org: OrgInfo, app: AppHandle) -> Result<(), String> {
    save_selected_org_to_disk(&app, &org)?;
    if let Some(w) = app.get_webview_window(AUTH_WINDOW_LABEL) {
        w.eval("if (window.__arbiterRefetchUsage) window.__arbiterRefetchUsage();")
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// Logs out by invalidating the claude.ai session and resetting the cache.
// We do the in-process state reset FIRST so the UI updates instantly, then
// run the JS side-effects best-effort. Claude's HttpOnly session cookie
// can't be touched from document.cookie — we rely on /api/logout returning
// Set-Cookie headers that clear it. The fetch is awaited inside the eval'd
// IIFE so the response (and its Set-Cookie) lands before we navigate away.
#[tauri::command]
pub async fn logout_usage(cache: State<'_, Cache>, app: AppHandle) -> Result<(), String> {
    // 1. Forget the saved org so a fresh sign-in re-prompts. Propagate the
    //    error: a silent failure here means the next sign-in lands on the
    //    previous account's org, which is exactly the bug class to avoid.
    clear_saved_org(&app)?;

    // 2. Reset cache to needs-login state and notify the UI
    {
        let mut guard = cache.0.lock().unwrap();
        guard.data = None;
        guard.set_needs_login();
    }
    app.emit("usage-updated", ()).ok();

    // 3. Best-effort WebView teardown — never block the command on this
    if let Some(w) = app.get_webview_window(AUTH_WINDOW_LABEL) {
        // Delete every claude.ai cookie via Tauri's runtime cookie API. This
        // reaches HttpOnly cookies that document.cookie can't touch — without
        // it, the session survives across restarts and the next launch sees
        // the previous account's orgs.
        if let Ok(url) = url::Url::parse("https://claude.ai") {
            if let Ok(cookies) = w.cookies_for_url(url) {
                for c in cookies {
                    w.delete_cookie(c).ok();
                }
            }
        }
        // Server-side logout + clear JS-accessible storage, then park the
        // page on about:blank so the init script doesn't fire again with
        // stale in-memory state.
        w.eval(r#"
            (async function() {
                try { await fetch('/api/logout', { method: 'POST', credentials: 'include' }); } catch (_) {}
                try { localStorage.clear(); sessionStorage.clear(); } catch (_) {}
                window.location.href = 'about:blank';
            })();
        "#).ok();
        w.hide().ok();
    }

    Ok(())
}

// Returns cached usage; errors distinguish "still loading" from "must log in"
// or "must pick an org".
#[tauri::command]
pub fn get_usage(cache: State<'_, Cache>) -> Result<UsageResponse, String> {
    let guard = cache.0.lock().unwrap();
    match (&guard.auth, &guard.data) {
        (_, Some(d)) => Ok(d.clone()),
        (AuthState::NeedsLogin, _) => Err("needs_login".to_string()),
        (AuthState::NeedsOrgSelection, _) => Err("needs_org_selection".to_string()),
        _ => Err("pending".to_string()),
    }
}

// Shows the auth WebView window so the user can log in
#[tauri::command]
pub fn open_login_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(AUTH_WINDOW_LABEL) {
        // After logout we leave the WebView on about:blank — navigate back to
        // claude.ai so the user actually sees the sign-in page.
        w.eval("if (window.location.href !== 'https://claude.ai/') window.location.href = 'https://claude.ai/';").ok();
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
