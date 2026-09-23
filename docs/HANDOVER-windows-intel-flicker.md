# Handover: text flicker on Intel graphics (Windows)

Written 2026-09-23, to be picked up on a Windows machine. Nothing here is fixed yet.

## The fault

On one Windows PC, every textured thing in the main window flickers: the terminal
text, the tab labels, all other UI text, the titlebar logo and the icons. Flat colour
does not flicker: window frame, tab backgrounds, pane backgrounds. It starts at
launch, when a context menu opens, and after the window is minimised and restored.
Moving the pointer keeps it going, and it settles once things are still.

- Machine: HP laptop, Intel Core Ultra 5 225U, integrated Intel graphics only (no
  second GPU). Seen with the laptop docked (two monitors, 60 Hz and 100 Hz) and
  undocked, on every screen, with the latest Intel driver from Intel.
- Not a regression: 1.5.0 flickers the same way on that PC. (An earlier report said
  1.5.1 was clean; that PC had actually been running 1.5.0, and 1.5.0 flickers too.)
- A fresh Windows laptop with AMD graphics runs 1.6.0 without any flicker.
- `ICED_PRESENT_MODE` = `fifo`, `mailbox`, `immediate`: no effect.
- **`WGPU_BACKEND=dx12`: no flicker.** So the fault is in the Intel Vulkan path.
  1.5.0 used wgpu's own choice (`Backends::all()`), which also lands on Vulkan there;
  1.5.1 onwards forces Vulkan (`gpu::windows_backend`).

## Why not just switch to DX12

DX12 was dropped in 1.5.1 (commit `987ac65`) because it draws every **unmaximised**
window soft, measured on the RTX 5080 at home: the same text crop had 19 grey levels
under Vulkan against 176 under DX12. A maximised window is sharp on both.

The cause, as found then and confirmed in the sources:

- winit 0.30.13, `src/platform_impl/windows/event_loop.rs`, `WM_NCCALCSIZE`: for a
  window with `undecorated_shadow` that is not maximised it does
  `params.rgrc[0].top += 1; params.rgrc[0].bottom += 1;`. The client rect keeps its
  height H but slides down a pixel, so its last row lies outside the window and only
  H-1 rows show. The maximised branch sets the rect to the monitor work area instead,
  which is why maximised windows are unaffected.
- wgpu-hal 0.19.5, `src/dx12/mod.rs` around line 706: the swapchain is created with
  `scaling: d3d12::Scaling::Stretch` and `FlipDiscard`, hard-coded. So DWM stretches
  the H-row frame into the H-1 rows it shows: crisp at the top, half a pixel soft
  mid-window, the phase drifting a pixel over the height.
- Vulkan presents 1:1 instead, and the hidden bottom row is simply clipped, which is
  why Vulkan is sharp.

Arbiter sets `undecorated_shadow = true` on the main window, the overview, the office
and the cards (`src/bin/iced_shell.rs`, search `undecorated_shadow`).

## Where the backend is chosen

- `src/gpu.rs`, `windows_backend()`: probes for a Vulkan adapter and returns
  `"vulkan"`, else `"dx12"` with a message box.
- `src/bin/iced_shell.rs`, `main()`: sets `WGPU_BACKEND` from it, unless the user set
  it already. That is why the `WGPU_BACKEND=dx12` test works without a build.

## Ways forward

Test everything on both machines: the Intel PC (flicker) and the RTX 5080 (blur).

1. **Use DX12 on Intel adapters.** Have `windows_backend()` look at the Vulkan
   adapters' vendor (`AdapterInfo::vendor == 0x8086` is Intel) and return `"dx12"`
   for Intel. Small and targeted. On its own it hands Intel users the soft
   unmaximised window, so it needs one of 2 to 4 as well.
2. **Make the frame the size DWM shows, so there is nothing to stretch.** In the
   vendored `iced_winit` (`vendor/iced_winit/src/program.rs`, where
   `compositor.configure_surface` is called, and wherever the window state takes its
   physical size from winit), use H-1 rows while the window is borderless with the
   shadow and not maximised, for the surface AND the viewport together. A surface
   smaller than the viewport, or the reverse, makes iced scale on its own. Restrict it
   to DX12 at first, and check whether Vulkan is fine either way. Stays within an
   existing fork, but the size must stay consistent for resize, maximise and restore.
3. **Drop `undecorated_shadow` when running on DX12.** No `WM_NCCALCSIZE` hack, no
   mismatch. It costs the drop shadow; check whether Windows 11 still draws one for a
   window with rounded corners (`winround`, `DWMWA_WINDOW_CORNER_PREFERENCE`), and
   look at the edge-resize hit zones (`resize_overlay`), which assume the current
   geometry.
4. **Present without stretching:** `DXGI_SCALING_NONE` in wgpu-hal. The cleanest
   picture, but it means vendoring and patching wgpu-hal (0.19.5), a third fork.

Recommended: 1 with 2, or 1 with 3 if the shadow turns out not to matter. Before
committing to either, check the blur measurement: screenshot an unmaximised window
under `WGPU_BACKEND=dx12` on the RTX 5080 and compare a text crop against Vulkan.
Also re-run the Intel repro (launch, open the pane context menu, minimise, restore,
circle the pointer over the panes) with the result.

If DX12 stays sharp once fixed, making it the default everywhere on Windows is worth
considering, but it needs the NVIDIA and AMD checks first.

## Decision (2026-09-23)

Fix the DX12 blur first. If DX12 is then sharp on the RTX 5080, make it the primary
backend on Windows, in this order:

1. DX12, if an adapter is found (`enumerate_adapters(Backends::DX12)`).
2. Vulkan otherwise.
3. The existing warning when there is neither.

The reasons are that DX12 is Windows' native API, every Windows driver has it, and it
is the backend that works on the Intel PC.

Memory is not one of the reasons. The saving measured in the September memory work
came from pinning one backend instead of iced loading all three (Vulkan, DX12 and
OpenGL). Pinning Vulkan already gets that. DX12 against Vulkan has not been measured:
measure it (working set, `Get-Process` counters) before claiming a saving.

Mac: nothing changes if the changes are gated. Backend selection is already
`#[cfg(windows)]` (macOS is Metal), and `undecorated_shadow` and the winit
`WM_NCCALCSIZE` shift behind the blur are Windows-only. The exception is option 2's
change in `vendor/iced_winit/src/program.rs`, which is shared by every platform. It
must be `#[cfg(windows)]`, or every Mac window loses a row.

## Ruled out (don't chase again)

The rendering code is identical between 1.5.1 and 1.6.0, and all of the following were
compared or tested:

- `gpu.rs`, the terminal widget and shader primitive, and the iced, wgpu and winit
  versions (identical);
- the vendored `iced_widget` (same 0.13.4 plus its two editor changes, same manifest);
- the release workflow, and icon and logo caching (every handle is cached);
- icon rotation (`Solid(0)` is iced's default);
- the occlusion hook (winit reports no occlusion on Windows);
- present modes and the dock;
- the Intel driver version (latest installed, still flickers).

## Also open

Two small changes are uncommitted on the Mac, unrelated to this fault:

- `src/claude_status.rs`: the working state is held for `ROW_END_CONFIRM_MS` after
  the interrupt hint was last seen, so one ConPTY chunk missing the hint does not
  flip a pane to ready; there is a test for it. Its comment claims the wordmark
  flashed, which was never observed and the wordmark is not animated. Reword the
  comment before committing.
- `src/bin/iced_shell.rs`: `office_refresh` and `office_grow` return
  `Option<Task>`, so `update` does not wrap every message in an empty
  `Task::batch`.
