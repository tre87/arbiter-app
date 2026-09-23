//! The notification sound: `assets/notification.wav`, bundled into the binary and played
//! through the OS with no audio stack of our own: winmm's `PlaySound` on Windows, `afplay`
//! on macOS (both always present), `paplay` or `aplay` where a Linux desktop has one.

/// The bundled sound. 16-bit PCM mono WAV with a plain 44-byte header, which is all
/// `PlaySound` takes; the test below holds the file to that. Swap the file to change the
/// sound, re-exported quieter or louder rather than scaled at play time.
pub const SOUND: &[u8] = include_bytes!("../assets/notification.wav");

/// Play the sound once, without blocking: the OS plays on after this returns. Failing to
/// play is silent; a notification without its sound is not worth reporting.
pub fn play_sound() {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FLAGS, SND_MEMORY, SND_NODEFAULT};
        // SND_ASYNC reads the buffer after this returns; a static lives for the process.
        let flags = SND_FLAGS(SND_MEMORY.0 | SND_ASYNC.0 | SND_NODEFAULT.0);
        let _ = unsafe { PlaySoundW(PCWSTR(SOUND.as_ptr().cast()), HMODULE::default(), flags) };
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(path) = sound_file() {
            let _ = quiet_command("afplay").arg(path).spawn();
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(path) = sound_file() {
            if quiet_command("paplay").arg(&path).spawn().is_err() {
                let _ = quiet_command("aplay").arg("-q").arg(&path).spawn();
            }
        }
    }
}

#[cfg(unix)]
fn quiet_command(program: &str) -> std::process::Command {
    use std::process::Stdio;
    let mut cmd = std::process::Command::new(program);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd
}

/// The sound as a file in the data dir, written once per run, for players that take a path.
#[cfg(unix)]
fn sound_file() -> Option<std::path::PathBuf> {
    static PATH: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let dir = crate::shell::app_data_dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("notify.wav");
        std::fs::write(&path, SOUND).ok()?;
        Some(path)
    })
    .clone()
}

/// The primary monitor's area clear of the taskbar or Dock, in the logical coordinates
/// winit places windows in: origin at that monitor's top left, y down, scaled by its
/// DPI (which iced applies to a `Position::Specific` for a window landing there).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkArea {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// None where the desktop cannot say (Linux, for now): the caller lets the window
/// manager place the window.
#[cfg(windows)]
pub fn primary_work_area() -> Option<WorkArea> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetMonitorInfoW(monitor, &mut info).ok().ok()?;
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = dpi_x.max(1) as f32 / 96.0;
        let work = info.rcWork;
        Some(WorkArea {
            left: work.left as f32 / scale,
            top: work.top as f32 / scale,
            right: work.right as f32 / scale,
            bottom: work.bottom as f32 / scale,
        })
    }
}

// The first screen is the one with the menu bar; its frame's origin is the desktop's.
// AppKit measures from the bottom left, winit from the top left, so y is flipped
// against the full frame height. Points already, no scaling.
#[cfg(target_os = "macos")]
pub fn primary_work_area() -> Option<WorkArea> {
    use objc2::{class, msg_send, runtime::AnyObject};
    use objc2_foundation::NSRect;
    unsafe {
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return None;
        }
        let count: usize = msg_send![screens, count];
        if count == 0 {
            return None;
        }
        let primary: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
        let frame: NSRect = msg_send![primary, frame];
        let visible: NSRect = msg_send![primary, visibleFrame];
        Some(WorkArea {
            left: visible.origin.x as f32,
            top: (frame.size.height - visible.origin.y - visible.size.height) as f32,
            right: (visible.origin.x + visible.size.width) as f32,
            bottom: (frame.size.height - visible.origin.y) as f32,
        })
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn primary_work_area() -> Option<WorkArea> {
    None
}

/// Every display's work area, for asking whether a saved window position is on any
/// of them. On Windows each monitor is scaled by its own DPI, which is exact for the
/// monitor a window sits on and close enough to answer yes or no; `work_area_at` is
/// the one to use for picking a monitor. Empty where the desktop cannot say.
#[cfg(windows)]
pub fn work_areas() -> Vec<WorkArea> {
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    unsafe extern "system" fn cb(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = &mut *(data.0 as *mut Vec<WorkArea>);
        let mut info =
            MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut info).as_bool() {
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let s = dx.max(1) as f32 / 96.0;
            let w = info.rcWork;
            out.push(WorkArea {
                left: w.left as f32 / s,
                top: w.top as f32 / s,
                right: w.right as f32 / s,
                bottom: w.bottom as f32 / s,
            });
        }
        BOOL(1)
    }

    let mut found: Vec<WorkArea> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(cb),
            LPARAM(&mut found as *mut Vec<WorkArea> as isize),
        );
    }
    found
}

#[cfg(target_os = "macos")]
pub fn work_areas() -> Vec<WorkArea> {
    use objc2::{class, msg_send, runtime::AnyObject};
    use objc2_foundation::NSRect;
    let mut out = Vec::new();
    unsafe {
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return out;
        }
        let count: usize = msg_send![screens, count];
        if count == 0 {
            return out;
        }
        // Flipped against the first screen, as in `work_area_at`.
        let primary: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
        let root: NSRect = msg_send![primary, frame];
        for i in 0..count {
            let screen: *mut AnyObject = msg_send![screens, objectAtIndex: i];
            let v: NSRect = msg_send![screen, visibleFrame];
            out.push(WorkArea {
                left: v.origin.x as f32,
                top: (root.size.height - v.origin.y - v.size.height) as f32,
                right: (v.origin.x + v.size.width) as f32,
                bottom: (root.size.height - v.origin.y) as f32,
            });
        }
    }
    out
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn work_areas() -> Vec<WorkArea> {
    Vec::new()
}

/// How much of a window's top edge has to be on a display for it to count as on
/// screen: enough of the title bar to grab and drag it back.
const GRAB_W: f32 = 120.0;
const GRAB_H: f32 = 24.0;

/// Where a window saved at `pos` with `size` should open on the displays there are
/// now. A window whose title bar is on some display keeps its place, shrunk to that
/// display if it has grown too big for it. One whose title bar is on none (saved at
/// home on a monitor that is not here at work) opens centred on the primary display,
/// shrunk to fit it. One never placed (`pos` is `None`) is left to the window manager,
/// shrunk to the primary display. With no displays known, nothing changes.
pub fn place_on_screen(
    size: (f32, f32),
    pos: Option<(f32, f32)>,
    areas: &[WorkArea],
    primary: Option<WorkArea>,
) -> ((f32, f32), Option<(f32, f32)>) {
    let Some(primary) = primary.or_else(|| areas.first().copied()) else {
        return (size, pos);
    };
    let fit = |a: &WorkArea| (size.0.min(a.right - a.left), size.1.min(a.bottom - a.top));
    let Some((x, y)) = pos else { return (fit(&primary), None) };
    let grabbable = |a: &&WorkArea| {
        let w = (x + size.0).min(a.right) - x.max(a.left);
        let h = (y + GRAB_H).min(a.bottom) - y.max(a.top);
        w >= GRAB_W.min(size.0) && h >= GRAB_H
    };
    if let Some(a) = areas.iter().find(grabbable) {
        return (fit(a), Some((x, y)));
    }
    (fit(&primary), Some(centred_in(fit(&primary), &primary)))
}

/// The top-left that centres a window of `size` in `a`.
pub fn centred_in(size: (f32, f32), a: &WorkArea) -> (f32, f32) {
    (
        a.left + ((a.right - a.left) - size.0).max(0.0) / 2.0,
        a.top + ((a.bottom - a.top) - size.1).max(0.0) / 2.0,
    )
}

/// The work area of the display `p` falls on, or the primary one when it falls on
/// none. For putting a window somewhere on the screen it is already on, rather than
/// dragging it back to the main display to do it.
///
/// `p` is in the caller's logical space, which winit derives with the window's own
/// `scale`; the answer comes back in that space too, ready for `move_to`. The monitor
/// is found in physical pixels: dividing each monitor by its OWN DPI overlapped a 100%
/// display with a 150% one beside it, and the point matched the wrong one.
#[cfg(windows)]
pub fn work_area_at(p: (f32, f32), scale: f32) -> Option<WorkArea> {
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    };

    unsafe extern "system" fn cb(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = &mut *(data.0 as *mut Vec<WorkArea>);
        let mut info =
            MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut info).as_bool() {
            let w = info.rcWork;
            out.push(WorkArea {
                left: w.left as f32,
                top: w.top as f32,
                right: w.right as f32,
                bottom: w.bottom as f32,
            });
        }
        BOOL(1)
    }

    let s = if scale > 0.0 { scale } else { 1.0 };
    let (px, py) = (p.0 * s, p.1 * s);
    let mut found: Vec<WorkArea> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(cb),
            LPARAM(&mut found as *mut Vec<WorkArea> as isize),
        );
    }
    found
        .iter()
        .find(|a| px >= a.left && px < a.right && py >= a.top && py < a.bottom)
        .map(|a| WorkArea { left: a.left / s, top: a.top / s, right: a.right / s, bottom: a.bottom / s })
        .or_else(primary_work_area)
}

/// macOS works in points throughout, the same for every display, so `scale` is not
/// needed there.
#[cfg(target_os = "macos")]
pub fn work_area_at(p: (f32, f32), _scale: f32) -> Option<WorkArea> {
    use objc2::{class, msg_send, runtime::AnyObject};
    use objc2_foundation::NSRect;
    unsafe {
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return None;
        }
        let count: usize = msg_send![screens, count];
        if count == 0 {
            return None;
        }
        // Every screen's y is flipped against the FIRST screen's height, because that
        // is the one whose top left is the desktop's origin in winit's space.
        let primary: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
        let root: NSRect = msg_send![primary, frame];
        for i in 0..count {
            let screen: *mut AnyObject = msg_send![screens, objectAtIndex: i];
            let v: NSRect = msg_send![screen, visibleFrame];
            let area = WorkArea {
                left: v.origin.x as f32,
                top: (root.size.height - v.origin.y - v.size.height) as f32,
                right: (v.origin.x + v.size.width) as f32,
                bottom: (root.size.height - v.origin.y) as f32,
            };
            if p.0 >= area.left && p.0 < area.right && p.1 >= area.top && p.1 < area.bottom {
                return Some(area);
            }
        }
    }
    primary_work_area()
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn work_area_at(_p: (f32, f32), _scale: f32) -> Option<WorkArea> {
    None
}

/// Whether the desktop is in a state to show a card: not a full-screen game or video, a
/// presentation, or another fullscreen app. Windows' own rule for its toasts, asked the
/// same way. Elsewhere always true: a macOS fullscreen app has its own Space, which a
/// floating window of another app does not enter.
pub fn desktop_accepts_notifications() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_ACCEPTS_NOTIFICATIONS, QUNS_QUIET_TIME};
        match unsafe { SHQueryUserNotificationState() } {
            Ok(state) => state == QUNS_ACCEPTS_NOTIFICATIONS || state == QUNS_QUIET_TIME,
            Err(_) => true,
        }
    }
    #[cfg(not(windows))]
    true
}

/// Bring a minimized window back (a card was clicked while the app sat in the taskbar);
/// focusing alone leaves a minimized window where it is. A window that is not minimized
/// is left alone, maximized or not.
#[cfg(windows)]
pub fn restore_if_minimized(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, ShowWindow, SW_RESTORE};
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }
}

/// Stop the desktop offering to resize a window when it is dragged against a screen
/// edge. Wanted by a window whose size is its own business: the Agents Office is a
/// picture at a whole-number zoom, and half a screen is not one of those.
///
/// Windows drives Aero Snap off the maximize box, so taking that one style bit away
/// disables snapping while leaving the window freely resizable by its edges.
#[cfg(windows)]
pub fn disable_snap(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_STYLE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_MAXIMIZEBOX,
    };
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        if style == 0 {
            return;
        }
        let stripped = style & !(WS_MAXIMIZEBOX.0 as isize);
        if stripped != style {
            SetWindowLongPtrW(hwnd, GWL_STYLE, stripped);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

/// Keeps a window out of full-screen Split View tiles, which is all
/// `NSWindowCollectionBehaviorFullScreenDisallowsTiling` governs (NSWindow.h). macOS
/// 15's drag-to-edge tiling has no public opt-out; it rides AppKit's own window drag,
/// which the office never uses, since `trafficlights::begin_drag` moves it by hand.
/// Takes the `NSView` iced hands out, and asks it for its window.
#[cfg(target_os = "macos")]
pub fn disable_snap_view(ns_view: *mut std::ffi::c_void) {
    use objc2::{msg_send, runtime::AnyObject};
    /// `NSWindowCollectionBehaviorFullScreenDisallowsTiling`, 1 << 12 in NSWindow.h.
    const DISALLOWS_TILING: usize = 1 << 12;
    if ns_view.is_null() {
        return;
    }
    unsafe {
        let view = ns_view as *mut AnyObject;
        let window: *mut AnyObject = msg_send![view, window];
        if window.is_null() {
            return;
        }
        let behavior: usize = msg_send![window, collectionBehavior];
        let _: () = msg_send![window, setCollectionBehavior: behavior | DISALLOWS_TILING];
    }
}

/// Mark a window so that clicking it never activates it (`WS_EX_NOACTIVATE`): a click
/// on a notification card leaves the keyboard where it was.
#[cfg(windows)]
pub fn keep_window_inactive(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE};
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le16(at: usize) -> u16 {
        u16::from_le_bytes([SOUND[at], SOUND[at + 1]])
    }
    fn le32(at: usize) -> u32 {
        u32::from_le_bytes(SOUND[at..at + 4].try_into().unwrap())
    }

    // What the playback paths need of the file: plain PCM, mono, 16-bit, a canonical
    // header with the data chunk at byte 36, and short enough to be a notification.
    #[test]
    fn the_bundled_sound_is_a_short_mono_pcm_wav() {
        assert_eq!(&SOUND[0..4], b"RIFF");
        assert_eq!(le32(4) as usize, SOUND.len() - 8);
        assert_eq!(&SOUND[8..12], b"WAVE");
        assert_eq!(&SOUND[12..16], b"fmt ");
        assert_eq!(le16(20), 1, "PCM");
        assert_eq!(le16(22), 1, "mono");
        assert_eq!(le16(34), 16, "16-bit");
        assert_eq!(&SOUND[36..40], b"data");
        let data_len = le32(40) as usize;
        assert_eq!(data_len, SOUND.len() - 44);
        let seconds = data_len as f32 / (le32(24) as f32 * 2.0);
        assert!(seconds < 2.0, "{seconds} s");
    }
}

#[cfg(test)]
mod placement_tests {
    use super::{place_on_screen, WorkArea};

    const LAPTOP: WorkArea = WorkArea { left: 0.0, top: 25.0, right: 1512.0, bottom: 982.0 };
    const EXTERNAL: WorkArea = WorkArea { left: 1512.0, top: 0.0, right: 4072.0, bottom: 1440.0 };

    #[test]
    fn a_window_on_a_display_keeps_its_place() {
        let got = place_on_screen((1200.0, 800.0), Some((1700.0, 100.0)), &[LAPTOP, EXTERNAL], Some(LAPTOP));
        assert_eq!(got, ((1200.0, 800.0), Some((1700.0, 100.0))));
    }

    // Saved at home on the external monitor; at work only the laptop is there.
    #[test]
    fn a_window_saved_on_a_missing_display_opens_centred_on_the_primary() {
        let ((w, h), pos) = place_on_screen((1800.0, 1200.0), Some((2000.0, 100.0)), &[LAPTOP], Some(LAPTOP));
        assert_eq!((w, h), (1512.0, 957.0), "shrunk to the laptop");
        assert_eq!(pos, Some((0.0, 25.0)));
        let (_, pos) = place_on_screen((800.0, 600.0), Some((-3000.0, 50.0)), &[LAPTOP], Some(LAPTOP));
        assert_eq!(pos, Some((356.0, 203.5)));
    }

    // Mostly off the edge is fine while enough of the title bar is left to grab.
    #[test]
    fn a_window_with_its_title_bar_in_reach_stays_put() {
        let (_, pos) = place_on_screen((800.0, 600.0), Some((1300.0, 900.0)), &[LAPTOP], Some(LAPTOP));
        assert_eq!(pos, Some((1300.0, 900.0)));
        // A title bar above the top of the screen cannot be grabbed.
        let (_, pos) = place_on_screen((800.0, 600.0), Some((100.0, -200.0)), &[LAPTOP], Some(LAPTOP));
        assert_eq!(pos, Some((356.0, 203.5)));
    }

    #[test]
    fn a_window_never_placed_is_left_to_the_window_manager() {
        assert_eq!(place_on_screen((2000.0, 600.0), None, &[LAPTOP], Some(LAPTOP)), ((1512.0, 600.0), None));
    }

    #[test]
    fn with_no_displays_known_nothing_changes() {
        assert_eq!(place_on_screen((800.0, 600.0), Some((5.0, 6.0)), &[], None), ((800.0, 600.0), Some((5.0, 6.0))));
    }
}
