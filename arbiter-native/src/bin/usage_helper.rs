//! `arbiter-usage-helper` — a tiny sidecar that logs into claude.ai in a native
//! webview (WebView2 on Windows, WKWebView on macOS via `wry`) and streams Claude
//! usage JSON to the main app over **stdout**, one compact line per poll. The
//! webview is isolated in this process so the terminal app stays fully native.
//!
//! Protocol (one JSON object per stdout line):
//!   { "ok": true, "plan": "Pro|Max|Free",
//!     "five_hour": {"utilization": 0-100, "resets_at_ms": <epoch ms|null>} | null,
//!     "seven_day": …, "seven_day_opus": …, "seven_day_sonnet": … }
//!   { "ok": false, "error": "needs_login" }   ← shows the sign-in window
//!
//! Built only with `--features usage-helper` (the `[[bin]]` requires it), so wry/
//! tao stay out of normal main-app builds. Exits when the parent closes (parent
//! pipes our stdin; EOF on stdin = parent gone).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::Write;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

#[derive(Debug, Clone, Copy)]
enum UserEvent {
    Show,
    Hide,
}

fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Drive window visibility from the parent over stdin: it sends "show\n" when the
    // user clicks the titlebar Sign-in button. EOF on stdin = parent gone → exit.
    let stdin_proxy = proxy.clone();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) if l.trim() == "show" => {
                    let _ = stdin_proxy.send_event(UserEvent::Show);
                }
                Ok(_) => {}
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

    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new(&window)
        .with_url("https://claude.ai/")
        .with_initialization_script(INIT_SCRIPT)
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            let body = req.into_body();
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

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let _keep = &webview; // keep the webview alive for the loop's lifetime
        match event {
            Event::UserEvent(UserEvent::Show) => {
                window.set_visible(true);
                window.set_focus();
            }
            Event::UserEvent(UserEvent::Hide) => window.set_visible(false),
            // Closing the sign-in window just hides it; we keep polling in the bg.
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                window.set_visible(false);
            }
            _ => {}
        }
    });
}

/// Injected into the claude.ai page (a port of the web app's `USAGE_INIT_SCRIPT`):
/// fetch the org list + usage with the page's own session cookies, build a compact
/// shape (utilization 0–100 + reset as epoch ms), and post it to Rust via wry IPC.
/// Picks the first org (multi-org selection is a later refinement).
const INIT_SCRIPT: &str = r#"
(function () {
  function post(x) { try { window.ipc.postMessage(JSON.stringify(x)); } catch (_) {} }
  function per(p) {
    if (!p) return null;
    var r = null;
    try { if (p.resets_at) r = new Date(p.resets_at).getTime(); } catch (_) {}
    var u = typeof p.utilization === 'number' ? p.utilization : 0;
    if (u < 0) u = 0; if (u > 100) u = 100;
    return { utilization: u, resets_at_ms: r };
  }
  async function fetchUsage() {
    if (location.protocol !== 'https:' || location.hostname !== 'claude.ai') {
      if (location.href !== 'https://claude.ai/') location.href = 'https://claude.ai/';
      return;
    }
    var o;
    try { o = await fetch('/api/organizations'); } catch (_) { post({ ok: false, error: 'needs_login' }); return; }
    if (o.status === 401 || o.status === 403 || !o.ok) { post({ ok: false, error: 'needs_login' }); return; }
    var raw;
    try { raw = await o.json(); } catch (_) { post({ ok: false, error: 'needs_login' }); return; }
    var list = (Array.isArray(raw) ? raw : [raw]).map(function (x) { return x.uuid || x.id; }).filter(Boolean);
    if (!list.length) { post({ ok: false, error: 'needs_login' }); return; }
    var org = list[0];
    // Signed in (orgs fetched) but the usage call failed → 'error' (warning), not login.
    var u;
    try { u = await fetch('/api/organizations/' + org + '/usage'); } catch (_) { post({ ok: false, error: 'error' }); return; }
    if (!u.ok) { post({ ok: false, error: 'error' }); return; }
    var usage;
    try { usage = await u.json(); } catch (_) { post({ ok: false, error: 'error' }); return; }
    var plan = (usage.seven_day_opus || usage.seven_day_sonnet) ? 'Max' : (usage.seven_day ? 'Pro' : 'Free');
    post({
      ok: true, plan: plan,
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
