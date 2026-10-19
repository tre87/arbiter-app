# `iced_wgpu`: Arbiter's fork

A verbatim copy of `iced_wgpu` 0.13.5 from crates.io with two changes, both in
`src/window/compositor.rs` and marked `ARBITER`, applied through `[patch.crates-io]`
in the workspace `Cargo.toml`. Together with the recovery in the `iced_winit` fork
(`recover_graphics` in `src/program.rs`) they let Arbiter survive a lost GPU device,
such as a graphics driver update or a driver reset, instead of crashing.

## The changes

1. **`present` never lets a panic leave the frame behind.** A lost device panics
   inside wgpu 0.19 (a failed submit, present or surface configure is fatal there),
   and wgpu skips the cleanup in its `Drop` impls while a panic unwinds. Upstream holds
   the surface texture, its view and the command encoder as locals of the panicking
   call, so the unwinding dropped them without cleanup: the surface stayed with an
   image acquired, the window's DXGI swapchain could never be released, and no new
   swapchain could be made for that window ("Invalid surface"). The fork holds the
   three outside the calls that can panic (`renderer.present`, `engine.submit`,
   `frame.present`), catches the panic, drops them normally and returns
   `SurfaceError::Lost`, which the `iced_winit` fork treats as a lost device.

2. **The compositor frees what it made before its device.** wgpu 0.19 never frees a
   resource that is dropped after its device: the resource waits in the device's queue
   of resources to free, which nothing runs again, and the two keep each other alive.
   Upstream declares `engine` (every pipeline, buffer and atlas, and the custom shader
   storage Arbiter's terminal renderers live in) after `device`, so it was dropped
   after it. The fork declares `engine` first, and a `Drop` impl polls the device
   (`Maintain::Wait`) before the fields drop: the engine's staging belt holds buffers
   in `map_async` callbacks that only a poll runs, and until it does they keep the
   device's whole wgpu instance alive. The poll is skipped while unwinding.

## Why the device has to go

On DX12, while any object of a removed device lives, every device made after it on
that adapter was removed too (the new device's first frame failed with "Out of
Memory", then `DXGI_ERROR_DEVICE_REMOVED`), and the window's old swapchain kept a new
one from being made. Measured 2026-10-19 on an RTX 5080 by removing the device with
`ID3D12Device5::RemoveDevice` (`ARBITER_TEST_GPU_LOSS=<seconds>` in Arbiter), with a
COM sentinel set as private data on each device to see when D3D12 destroyed it: before
these changes no removed device was ever destroyed; with them each one was, and three
removals in a row each came back on the GPU.

## Upgrading iced

Re-copy `iced_wgpu` from the registry and re-apply the two `ARBITER` changes. wgpu
releases after 0.19 reworked resource lifetimes and device loss, so check whether
either is still needed before re-applying it.
