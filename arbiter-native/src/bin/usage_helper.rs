//! Usage helper — logs into claude.ai in a native webview (WebView2 on Windows,
//! WKWebView on macOS via `wry`) and streams Claude usage JSON to the main app over
//! **stdout**, one compact line per poll. It runs in its OWN process (so the webview
//! never touches the terminal's event loop) but in the SAME binary: `iced_shell`
//! re-spawns itself with `--usage-helper` and calls [`run`]. One binary → no
//! separate build/placement step. Compiled only with `--features usage-helper`.
//!
//! Protocol (one JSON object per stdout line):
//!   { "ok": true, "plan": "Pro|Max|Free",
//!     "five_hour": {"utilization": 0-100, "resets_at_ms": <epoch ms|null>} | null,
//!     "seven_day": …, "seven_day_opus": …, "seven_day_sonnet": … }
//!   { "ok": false, "error": "needs_login" }   ← shows the sign-in window
//!
//! Exits when the parent closes (parent pipes our stdin; EOF on stdin = parent gone).

use std::io::Write;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

#[derive(Debug, Clone)]
enum UserEvent {
    Show,
    Hide,
    /// Use this org uuid for usage (from the app's org selector / saved choice).
    SetOrg(String),
    /// Clear ONLY this webview's claude.ai session (usage sign-out) + reload.
    SignOut,
    /// Refetch usage now (the app's refresh button / countdown rollover).
    Fetch,
}

/// Where WebView2 / WKWebView stores the claude.ai session (cookies) — a stable
/// per-user "arbiter" appdata folder so the login persists. Honors `ARBITER_DATA_DIR`
/// (tests/isolation) so it never touches the real profile under test.
fn webview_data_dir() -> Option<std::path::PathBuf> {
    if let Some(d) = std::env::var_os("ARBITER_DATA_DIR") {
        return Some(std::path::PathBuf::from(d).join("webview"));
    }
    Some(dirs::data_dir()?.join("arbiter"))
}

/// macOS: bring this app to the front (so its key window receives ⌘V etc.).
#[cfg(target_os = "macos")]
fn macos_activate() {
    use objc2::{class, msg_send, runtime::AnyObject};
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if !app.is_null() {
            let _: () = msg_send![app, activateIgnoringOtherApps: true];
        }
    }
}

/// Run the usage-helper webview loop (this process was re-spawned with
/// `--usage-helper`). Diverges: runs the event loop until the parent's stdin closes.
pub fn run() {
    eprintln!("[usage-helper] started"); // DIAGNOSTIC (Windows usage debugging)
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // macOS: start as an Accessory app — NO dock icon — and DON'T grab activation
    // on launch (else we'd steal focus from the main app). When the user actually
    // signs in we flip to Regular (dock icon + menu bar + frontmost) so the window
    // is findable and ⌘V works, then back to Accessory once usage loads.
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_activate_ignoring_other_apps(false);
    }
    let proxy = event_loop.create_proxy();

    // A standard Edit menu so ⌘X/⌘C/⌘V/⌘A work in the claude.ai sign-in webview
    // (macOS binds those shortcuts via menu items). Kept alive for the app.
    #[cfg(target_os = "macos")]
    let _menu = {
        use muda::{Menu, PredefinedMenuItem, Submenu};
        let menu = Menu::new();
        let app_menu = Submenu::new("Arbiter", true);
        let edit = Submenu::new("Edit", true);
        let _ = edit.append_items(&[
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ]);
        let _ = menu.append(&app_menu);
        let _ = menu.append(&edit);
        menu.init_for_nsapp();
        menu
    };

    // Drive window visibility from the parent over stdin: it sends "show\n" when the
    // user clicks the titlebar Sign-in button. EOF on stdin = parent gone → exit.
    let stdin_proxy = proxy.clone();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) => {
                    let t = l.trim();
                    if t == "show" {
                        let _ = stdin_proxy.send_event(UserEvent::Show);
                    } else if t == "fetch" {
                        let _ = stdin_proxy.send_event(UserEvent::Fetch);
                    } else if t == "signout" {
                        let _ = stdin_proxy.send_event(UserEvent::SignOut);
                    } else if let Some(uuid) = t.strip_prefix("org:") {
                        let _ = stdin_proxy.send_event(UserEvent::SetOrg(uuid.to_string()));
                    }
                }
                Err(_) => break,
            }
        }
        std::process::exit(0);
    });

    let window = WindowBuilder::new()
        .with_title("Arbiter — Claude sign-in")
        .with_inner_size(LogicalSize::new(460.0, 680.0))
        .with_visible(false) // stay hidden unless the user asks to sign in
        .build(&event_loop)
        .expect("usage-helper: build window");

    // Persist the claude.ai login in a stable per-user folder (appdata/arbiter) so
    // it survives restarts and works from a read-only install dir (Program Files).
    let data_dir = webview_data_dir();
    if let Some(d) = &data_dir {
        let _ = std::fs::create_dir_all(d);
    }
    let mut web_context = wry::WebContext::new(data_dir);

    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new(&window)
        .with_web_context(&mut web_context)
        .with_url("https://claude.ai/")
        .with_initialization_script(INIT_SCRIPT)
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            let body = req.into_body();
            // DIAGNOSTIC: every IPC message from the page → stderr (inherited by the
            // parent, so it shows in the terminal). Remove once usage works on Windows.
            eprintln!("[usage-helper] ipc<- {}", body.chars().take(200).collect::<String>());
            // `LOG …` lines are JS breadcrumbs (not usage data) — don't relay to stdout.
            if let Some(rest) = body.strip_prefix("LOG ") {
                let _ = rest;
                return;
            }
            // Relay the line to the main app.
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{body}");
            let _ = out.flush();
            // Hide the sign-in window once usage actually loads (login succeeded).
            // We do NOT auto-show on needs_login — the app's Sign-in button does
            // that (via stdin "show"), so the window never pops unprompted.
            if body.contains("\"ok\":true") {
                let _ = ipc_proxy.send_event(UserEvent::Hide);
            }
        })
        .build()
        .expect("usage-helper: build webview");

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        // Keep the webview + its web context alive for the loop's lifetime.
        let _keep = (&webview, &web_context);
        match event {
            Event::UserEvent(UserEvent::Show) => {
                // Become a normal foreground app: dock icon (findable), menu bar
                // (⌘V), and frontmost so the webview's window is key.
                #[cfg(target_os = "macos")]
                {
                    use tao::platform::macos::{ActivationPolicy, EventLoopWindowTargetExtMacOS};
                    _target.set_activation_policy_at_runtime(ActivationPolicy::Regular);
                }
                window.set_visible(true);
                window.set_focus();
                #[cfg(target_os = "macos")]
                macos_activate();
            }
            Event::UserEvent(UserEvent::Hide) => {
                window.set_visible(false);
                #[cfg(target_os = "macos")]
                {
                    use tao::platform::macos::{ActivationPolicy, EventLoopWindowTargetExtMacOS};
                    _target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);
                }
            }
            Event::UserEvent(UserEvent::SetOrg(uuid)) => {
                let js = format!("window.__arbiterSetOrg && window.__arbiterSetOrg({uuid:?})");
                let _ = webview.evaluate_script(&js);
            }
            Event::UserEvent(UserEvent::Fetch) => {
                let _ = webview
                    .evaluate_script("window.__arbiterRefetchUsage && window.__arbiterRefetchUsage()");
            }
            Event::UserEvent(UserEvent::SignOut) => {
                // Clears ONLY this webview's data (claude.ai cookies) — nothing else
                // on the system. Reload so the script re-runs and reports needs_login.
                let _ = webview.clear_all_browsing_data();
                let _ = webview.load_url("https://claude.ai/");
            }
            // Closing the sign-in window just hides it; we keep polling in the bg.
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                window.set_visible(false);
                #[cfg(target_os = "macos")]
                {
                    use tao::platform::macos::{ActivationPolicy, EventLoopWindowTargetExtMacOS};
                    _target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);
                }
            }
            _ => {}
        }
    });
}

/// Injected into the claude.ai page (a port of the web app's `USAGE_INIT_SCRIPT`):
/// fetch the org list + usage with the page's own session cookies, build a compact
/// shape (utilization 0–100 + reset as epoch ms), and post it to Rust via wry IPC.
/// Multi-org: uses the app-chosen org (via `__arbiterSetOrg`), the only org, or
/// reports `needs_org` with the list so the app can show its picker.
const INIT_SCRIPT: &str = r#"
(function () {
  function post(x) { try { window.ipc.postMessage(JSON.stringify(x)); } catch (_) {} }
  // DIAGNOSTIC breadcrumb to the helper's stderr (Windows usage debugging).
  function dbg(m) { try { window.ipc.postMessage('LOG ' + m); } catch (_) {} }
  dbg('init ' + location.protocol + '//' + location.hostname + location.pathname);
  function per(p) {
    if (!p) return null;
    var r = null;
    try { if (p.resets_at) r = new Date(p.resets_at).getTime(); } catch (_) {}
    var u = typeof p.utilization === 'number' ? p.utilization : 0;
    if (u < 0) u = 0; if (u > 100) u = 100;
    return { utilization: u, resets_at_ms: r };
  }
  async function usageFor(uuid) {
    try { var u = await fetch('/api/organizations/' + uuid + '/usage'); if (!u.ok) return null; return await u.json(); }
    catch (_) { return null; }
  }
  // The chosen org uuid (set by the app's selector / saved choice via __arbiterSetOrg),
  // preserved across script re-injection on the same page.
  window.__arbiterOrg = window.__arbiterOrg || null;
  window.__arbiterSetOrg = function (u) { window.__arbiterOrg = u; fetchUsage(); };
  // The app calls this (refresh button / countdown rollover) to refetch on demand.
  window.__arbiterRefetchUsage = function () { fetchUsage(); };
  async function fetchUsage() {
    dbg('fetch ' + location.hostname + location.pathname);
    if (location.protocol !== 'https:' || location.hostname !== 'claude.ai') {
      if (location.href !== 'https://claude.ai/') location.href = 'https://claude.ai/';
      return;
    }
    var o;
    // A network failure (offline / claude.ai unreachable) is transient — report
    // 'error' (Usage unavailable), NOT 'needs_login', so an outage never looks like
    // you've been signed out.
    try { o = await fetch('/api/organizations'); } catch (_) { dbg('org-catch'); post({ ok: false, error: 'error' }); return; }
    dbg('org ' + o.status);
    // Only 401/403 means genuinely unauthenticated → sign in. Any other non-OK
    // (5xx/429/…) is a server-side problem while still signed in → transient error.
    if (o.status === 401 || o.status === 403) { post({ ok: false, error: 'needs_login' }); return; }
    if (!o.ok) { post({ ok: false, error: 'error' }); return; }
    var raw;
    // A non-JSON 2xx body is almost always the login-redirect HTML page → sign in.
    try { raw = await o.json(); } catch (_) { post({ ok: false, error: 'needs_login' }); return; }
    var list = (Array.isArray(raw) ? raw : [raw])
      .map(function (x) { return { uuid: x.uuid || x.id, name: x.name || x.display_name || (x.uuid || x.id) }; })
      .filter(function (o) { return !!o.uuid; });
    if (!list.length) { post({ ok: false, error: 'needs_login' }); return; }
    // Determine the org: the chosen one (if still valid), else the only one, else
    // ask the app to show its org selector.
    var chosen = (window.__arbiterOrg && list.some(function (o) { return o.uuid === window.__arbiterOrg; }))
      ? window.__arbiterOrg
      : (list.length === 1 ? list[0].uuid : null);
    if (!chosen) { post({ ok: false, error: 'needs_org', orgs: list }); return; }
    dbg('orgs ' + list.length + ' chosen ' + (chosen || '-'));
    var usage = await usageFor(chosen);
    if (!usage) { dbg('usage-null'); post({ ok: false, error: 'error' }); return; }
    var plan = (usage.seven_day_opus || usage.seven_day_sonnet) ? 'Max' : (usage.seven_day ? 'Pro' : 'Free');
    dbg('ok ' + plan);
    var chosenName = (list.find(function (o) { return o.uuid === chosen; }) || {}).name || null;
    post({
      ok: true, plan: plan,
      // The chosen org + full list travel with every poll so Settings can show the
      // current org and offer the switcher without a re-fetch.
      org_uuid: chosen, org_name: chosenName, orgs: list,
      five_hour: per(usage.five_hour),
      seven_day: per(usage.seven_day),
      seven_day_opus: per(usage.seven_day_opus),
      seven_day_sonnet: per(usage.seven_day_sonnet)
    });
  }
  setTimeout(fetchUsage, 800);
  setInterval(fetchUsage, 120000);
  // Re-fetch once the SPA navigates away from /login (login just completed).
  var lw = setInterval(function () {
    if (!location.pathname.startsWith('/login')) { clearInterval(lw); setTimeout(fetchUsage, 800); }
  }, 1000);
  setTimeout(function () { clearInterval(lw); }, 600000);
})();
"#;
