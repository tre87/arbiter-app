//! macOS-only window chrome helpers.

use objc2::{msg_send, rc::Retained};
use objc2_app_kit::{NSView, NSWindow, NSWindowButton};

/// Traffic-light inset. MUST stay in sync with `trafficLightPosition` in
/// `tauri.conf.json` — Tauri uses that value at window creation, and we re-apply
/// the same one below after geometry restores so the result is identical.
const TRAFFIC_LIGHT_X: f64 = 14.0;
const TRAFFIC_LIGHT_Y: f64 = 22.0;

/// Re-apply the custom traffic-light position to a window's native buttons.
///
/// Tauri sets `trafficLightPosition` at creation and relies on tao re-applying
/// it from the content view's `drawRect:`. In release builds the WKWebView
/// composites its own layers, so the host view's `drawRect:` stops firing after
/// a programmatic `set_size`/`set_position`; AppKit's resize then snaps the
/// buttons back to their default offset and nothing moves them back (in dev the
/// live Vite server keeps the view redrawing, which is why it only shows up in
/// release). Calling this after our geometry restore and on every resize
/// re-applies the inset deterministically.
///
/// This is a faithful copy of tao's private `inset_traffic_lights`
/// (tao `src/platform_impl/macos/view.rs`), operating on the raw `NSWindow`
/// pointer from `WebviewWindow::ns_window()` / `Window::ns_window()`.
pub fn apply_traffic_light_position(ns_window_ptr: *mut std::ffi::c_void) {
    if ns_window_ptr.is_null() {
        return;
    }
    // SAFETY: `ns_window_ptr` is the live NSWindow for this window, obtained
    // from Tauri's `ns_window()`. `Retained::retain` bumps the refcount and
    // releases on drop, so we only borrow it. Only ever called on the main
    // thread (setup() and on_window_event), matching tao's own usage.
    unsafe {
        let Some(ns_window) = Retained::retain(ns_window_ptr as *mut NSWindow) else {
            return;
        };
        let (Some(close), Some(miniaturize), Some(zoom)) = (
            ns_window.standardWindowButton(NSWindowButton::CloseButton),
            ns_window.standardWindowButton(NSWindowButton::MiniaturizeButton),
            ns_window.standardWindowButton(NSWindowButton::ZoomButton),
        ) else {
            return;
        };

        let Some(superview) = close.superview() else {
            return;
        };
        let Some(title_bar_container_view) = superview.superview() else {
            return;
        };

        let close_rect = NSView::frame(&close);
        let title_bar_frame_height = close_rect.size.height + TRAFFIC_LIGHT_Y;
        let mut title_bar_rect = NSView::frame(&title_bar_container_view);
        title_bar_rect.size.height = title_bar_frame_height;
        title_bar_rect.origin.y = ns_window.frame().size.height - title_bar_frame_height;
        let _: () = msg_send![&title_bar_container_view, setFrame: title_bar_rect];

        // Compute spacing from the current layout before the buttons move.
        let space_between = NSView::frame(&miniaturize).origin.x - close_rect.origin.x;
        for (i, button) in [close, miniaturize, zoom].into_iter().enumerate() {
            let mut rect = NSView::frame(&button);
            rect.origin.x = TRAFFIC_LIGHT_X + (i as f64 * space_between);
            button.setFrameOrigin(rect.origin);
        }
    }
}
