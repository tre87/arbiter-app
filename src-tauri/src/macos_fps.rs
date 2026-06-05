#![cfg(target_os = "macos")]
//! Defeat WKWebView's 60-fps-ish rendering cap on macOS.
//!
//! WebKit gates page rendering updates to "near 60fps" via the private feature
//! flag `PreferPageRenderingUpdatesNear60FPSEnabled` (default ON, even on
//! macOS 26). On a high-refresh display it renders at the integer division of
//! the refresh nearest 60 — measured here as 60 on a 120 Hz panel and 72 on a
//! 144 Hz panel (= 120/2, 144/2). There is no public API to lift it; the only
//! runtime toggle is the private `_features` / `_setEnabled:forFeature:` route
//! on `WKPreferences`. (The `_setBoolValue:forKey:` route silently no-ops on a
//! default config because its group identifier is empty.)
//!
//! This is private WebKit SPI — fine for direct/.dmg distribution, but it would
//! get the app rejected from the Mac App Store. Every selector is guarded with
//! `respondsToSelector:` and every pointer null-checked, so on a future macOS
//! that drops or renames the API this degrades to a silent no-op, never a crash.

use objc2::runtime::{AnyObject, Bool};
use objc2::{msg_send, sel};
use objc2_foundation::NSString;

/// Flip `PreferPageRenderingUpdatesNear60FPSEnabled` to NO so the webview's
/// `requestAnimationFrame` runs at the display's native refresh rate.
///
/// `wk_webview` is the raw `*mut c_void` from `PlatformWebview::inner()` (the
/// underlying `WKWebView`). Safe to call with a null pointer.
pub unsafe fn unlock_high_fps(wk_webview: *mut std::ffi::c_void) {
    let webview = wk_webview as *const AnyObject;
    if webview.is_null() {
        return;
    }

    // -[WKWebView configuration] -> -[WKWebViewConfiguration preferences]
    let config: *const AnyObject = msg_send![webview, configuration];
    if config.is_null() {
        return;
    }
    let prefs: *const AnyObject = msg_send![config, preferences];
    if prefs.is_null() {
        return;
    }

    // The setter we need: -[WKPreferences _setEnabled:forFeature:]
    let set_sel = sel!(_setEnabled:forFeature:);
    let responds_set: Bool = msg_send![prefs, respondsToSelector: set_sel];
    if !responds_set.as_bool() {
        return;
    }

    // `_features` returns NSArray<_WKFeature *>. It's a class method in current
    // WebKit, but hedge against an instance-method variant across versions.
    let features_sel = sel!(_features);
    let prefs_class: *const AnyObject = msg_send![prefs, class];
    let class_responds: Bool = msg_send![prefs_class, respondsToSelector: features_sel];
    let features: *const AnyObject = if class_responds.as_bool() {
        msg_send![prefs_class, _features]
    } else {
        let inst_responds: Bool = msg_send![prefs, respondsToSelector: features_sel];
        if !inst_responds.as_bool() {
            return;
        }
        msg_send![prefs, _features]
    };
    if features.is_null() {
        return;
    }

    let count: usize = msg_send![features, count];
    let target = NSString::from_str("PreferPageRenderingUpdatesNear60FPSEnabled");
    let mut i: usize = 0;
    while i < count {
        let feature: *const AnyObject = msg_send![features, objectAtIndex: i];
        i += 1;
        if feature.is_null() {
            continue;
        }
        let key_sel = sel!(key);
        let has_key: Bool = msg_send![feature, respondsToSelector: key_sel];
        if !has_key.as_bool() {
            continue;
        }
        let key: *const AnyObject = msg_send![feature, key];
        if key.is_null() {
            continue;
        }
        let is_match: Bool = msg_send![key, isEqualToString: &*target];
        if is_match.as_bool() {
            let _: () = msg_send![prefs, _setEnabled: Bool::NO, forFeature: feature];
            return;
        }
    }
}
