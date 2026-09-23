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

/// macOS tiles a window dragged to an edge unless it says not to, which is what
/// `NSWindowCollectionBehaviorDisallowsTiling` is for. Takes the `NSView` iced hands
/// out, and asks it for its window.
#[cfg(target_os = "macos")]
pub fn disable_snap_view(ns_view: *mut std::ffi::c_void) {
    use objc2::{msg_send, runtime::AnyObject};
    /// `NSWindowCollectionBehaviorDisallowsTiling`, which AppKit defines as 1 << 12.
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
