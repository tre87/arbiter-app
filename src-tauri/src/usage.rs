use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindowBuilder};

pub const AUTH_WINDOW_LABEL: &str = "auth";

// Injected into the auth WebView. Uses the browser's own session cookies.
pub const USAGE_INIT_SCRIPT: &str = r#"
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
            // Attach account info for display in settings
            try {
                const accResp = await fetch('/api/account');
                if (accResp.ok) {
                    const acc = await accResp.json();
                    usage.__account_email = acc.email_address || acc.email || null;
                    usage.__account_name = acc.full_name || acc.name || null;
                }
            } catch(_) {}
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

pub struct UsageCache {
    data: Option<UsageResponse>,
    auth: AuthState,
}

impl UsageCache {
    pub fn new() -> Self { Self { data: None, auth: AuthState::Pending } }
    fn store(&mut self, data: UsageResponse) {
        self.auth = AuthState::Ok;
        self.data = Some(data);
    }
    fn set_needs_login(&mut self) { self.auth = AuthState::NeedsLogin; }
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

    cache.0.lock().unwrap().store(UsageResponse {
        five_hour, seven_day, seven_day_opus, seven_day_sonnet, plan,
        account_email, account_name,
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
pub fn report_auth_error(cache: State<'_, Cache>, app: AppHandle) {
    cache.0.lock().unwrap().set_needs_login();
    app.emit("usage-updated", ()).ok();
}

// Logs out by clearing WebView2 cookies and resetting usage cache
#[tauri::command]
pub async fn logout_usage(cache: State<'_, Cache>, app: AppHandle) -> Result<(), String> {
    // Clear the auth WebView cookies by navigating to a logout-triggering script
    if let Some(w) = app.get_webview_window(AUTH_WINDOW_LABEL) {
        w.eval(r#"
            document.cookie.split(';').forEach(function(c) {
                document.cookie = c.replace(/^ +/, '').replace(/=.*/, '=;expires=' + new Date().toUTCString() + ';path=/');
            });
            if (window.cookieStore) {
                cookieStore.getAll().then(function(cookies) {
                    cookies.forEach(function(c) { cookieStore.delete(c.name); });
                });
            }
            fetch('/api/logout', { method: 'POST' }).catch(function(){});
        "#).map_err(|e| e.to_string())?;
        w.hide().ok();
    }

    // Reset cache to needs-login state
    {
        let mut guard = cache.0.lock().unwrap();
        guard.data = None;
        guard.set_needs_login();
    }
    app.emit("usage-updated", ()).ok();

    Ok(())
}

// Returns cached usage; errors distinguish "still loading" from "must log in"
#[tauri::command]
pub fn get_usage(cache: State<'_, Cache>) -> Result<UsageResponse, String> {
    let guard = cache.0.lock().unwrap();
    match (&guard.auth, &guard.data) {
        (_, Some(d)) => Ok(d.clone()),
        (AuthState::NeedsLogin, _) => Err("needs_login".to_string()),
        _ => Err("pending".to_string()),
    }
}

// Shows the auth WebView window so the user can log in
#[tauri::command]
pub fn open_login_window(app: AppHandle) -> Result<(), String> {
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
