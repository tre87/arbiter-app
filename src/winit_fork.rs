//! Checks on the vendored winit (`vendor/winit/ARBITER-FORK.md`): a scale change puts the window
//! where Windows suggests. An upgrade that loses the change fails here.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, SendMessageW, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE,
};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId};

#[derive(Default)]
struct Probe {
    window: Option<Window>,
    // (what was checked, the rect expected, the rect the window got)
    seen: Vec<(&'static str, RECT, RECT)>,
}

fn scale_change(hwnd: HWND, dpi: u32, suggested: RECT) -> RECT {
    let wparam = WPARAM((dpi | dpi << 16) as usize);
    let lparam = LPARAM(&suggested as *const RECT as isize);
    let mut got = RECT::default();
    unsafe {
        SendMessageW(hwnd, WM_DPICHANGED, wparam, lparam);
        GetWindowRect(hwnd, &mut got).unwrap();
    }
    got
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let (work, dpi) = unsafe {
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            assert!(GetMonitorInfoW(monitor, &mut info).as_bool());
            let (mut x, mut y) = (0, 0);
            GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y).unwrap();
            (info.rcWork, x)
        };
        let attributes = Window::default_attributes()
            .with_visible(false)
            .with_decorations(false)
            .with_inner_size(PhysicalSize::new(800, 600))
            .with_position(PhysicalPosition::new(work.left + 50, work.top + 50));
        let window = event_loop.create_window(attributes).unwrap();
        let RawWindowHandle::Win32(handle) = window.window_handle().unwrap().as_raw() else {
            unreachable!()
        };
        let hwnd = HWND(handle.hwnd.get() as *mut c_void);

        // Neither is the size winit would work out itself (the 800x600 scaled). `inside` is on
        // the primary monitor's work area; `overhanging` is the same size, 200 px past its left.
        let inside = RECT {
            left: work.left + 40,
            top: work.top + 30,
            right: work.left + 1040,
            bottom: work.top + 730,
        };
        let overhanging = RECT { left: work.left - 200, right: work.left + 800, ..inside };
        let fitted = RECT { left: work.left, right: work.left + 1000, ..inside };

        // A DPI no monitor has, then the primary's, so winit's scale changes every time.
        let other = dpi * 2;
        let got = scale_change(hwnd, other, inside);
        self.seen.push(("taken as suggested", inside, got));

        unsafe { SendMessageW(hwnd, WM_ENTERSIZEMOVE, WPARAM(0), LPARAM(0)) };
        let got = scale_change(hwnd, dpi, overhanging);
        unsafe { SendMessageW(hwnd, WM_EXITSIZEMOVE, WPARAM(0), LPARAM(0)) };
        self.seen.push(("left under the cursor while dragged", overhanging, got));

        scale_change(hwnd, other, inside);
        let got = scale_change(hwnd, dpi, overhanging);
        self.seen.push(("kept on its monitor when not dragged", fitted, got));

        self.window = Some(window);
        event_loop.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

#[test]
fn a_scale_change_puts_the_window_where_windows_suggests() {
    let mut event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let mut probe = Probe::default();
    event_loop.run_app_on_demand(&mut probe).unwrap();
    assert_eq!(probe.seen.len(), 3);
    for (what, want, got) in probe.seen {
        assert_eq!(
            (got.left, got.top, got.right, got.bottom),
            (want.left, want.top, want.right, want.bottom),
            "{what}"
        );
    }
}
