# `winit`: Arbiter's fork

A verbatim copy of `winit` 0.30.13 from crates.io with one change, applied
through `[patch.crates-io]` in the workspace `Cargo.toml`. The crate's own
`Cargo.lock` was dropped.

## The change

`src/platform_impl/windows/event_loop.rs`, the `WM_DPICHANGED` handler: a
window whose scale changes takes the rectangle Windows suggests in `lParam`.

- **Normal window:** the suggested rect, as is. Its client size is what the
  `ScaleFactorChanged` event offers through `inner_size_writer`. If the app
  asks for another size, that size is placed at the suggested client origin.
- **Not being dragged:** the rect is then shrunk and moved as little as it
  takes to lie inside the work area of the monitor at the new DPI that it
  covers most (`fit_on_monitor`). This covers Win+Shift+Arrow, a scale changed
  in Settings and a display unplugged. A window being dragged is left where
  Windows put it, under the cursor.
- **Maximized or fullscreen:** not moved, Windows has already placed it. If
  the app asks for a new size, the old path applies (maximized is cleared, and
  the size is set at the suggested origin).

Upstream discarded the suggested rect. It scaled the old client size itself,
moved the window to keep the cursor's relative x during a drag, then nudged
it a pixel at a time until `MonitorFromRect` matched `MonitorFromWindow`: the
monitor the window was on at that moment, not necessarily the one whose DPI
the message announced. All of that is gone, with the imports only it used.

## Why

A user with a 150% laptop and two 100% monitors saw Arbiter keep zooming when
dragged between them, then land back on the first monitor. Win+Shift+Arrow did
the same (reported after 1.8.0). Microsoft's guidance ("High DPI Desktop Application
Development on Windows", Common Pitfalls) says the suggested rect must be used
because it keeps the cursor in place during a drag and prevents "a recursive
dpi-change cycle where one DPI change triggers a subsequent DPI change". The
fix was made from reading the code. No mixed-DPI machine was at hand, so the
loop itself was never reproduced.

Firefox applies the suggested rect too (`nsWindow::OnDPIChanged`). It skips a
maximized window and a live resize, clamps the size to the screen, and clamps
the left and top edges unless the window is being moved.

## Test

`winit_fork::a_scale_change_puts_the_window_where_windows_suggests` in the
Arbiter crate (`src/winit_fork.rs`, `cargo test --lib winit_fork`) sends
`WM_DPICHANGED` to a hidden window and checks the rect it gets: as suggested,
as suggested while dragged even when overhanging its monitor, and fitted onto
the monitor when not dragged. Against upstream winit it fails on the first
check (1600x1200, its own doubling of 800x600, instead of the suggested
1000x700).

## Keeping it up to date

On a winit upgrade, re-copy the matching version from the registry, re-apply
the change, and run the test. Check first whether upstream now applies the
suggested rect, in which case the fork can go. The same manual step applies to
`vendor/iced_winit`, `vendor/iced_widget`, `vendor/iced_wgpu` and
`vendor/wgpu-hal`.
