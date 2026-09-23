# `wgpu-hal` — Arbiter's fork

A verbatim copy of `wgpu-hal` 0.19.5 from crates.io with one change, applied
through `[patch.crates-io]` in the workspace `Cargo.toml`.

## The change

`src/dx12/mod.rs`, in `Surface::configure`, where a new swapchain is described:

```diff
-                    scaling: d3d12::Scaling::Stretch,
+                    scaling: match self.target {
+                        SurfaceTarget::WndHandle(_) => d3d12::Scaling::Identity,
+                        _ => d3d12::Scaling::Stretch,
+                    },
```

`Identity` is `DXGI_SCALING_NONE`: DWM presents the frame 1:1 instead of
scaling it to the window. Composition swapchains (`Visual`, `SwapChainPanel`,
`SurfaceHandle`) accept only `STRETCH`, so they keep it.

## Why

Arbiter's windows are borderless with `undecorated_shadow`. For such a window
that is not maximised, winit 0.30 answers `WM_NCCALCSIZE` with
`top += 1; bottom += 1`: the client rect keeps its height H but slides down a
pixel, and only H-1 of its rows show. With `STRETCH`, DWM squeezed the H-row
frame into those H-1 rows, and every unmaximised window drew soft: 17 grey
levels in a crop of terminal text under Vulkan against 199 under DX12
(RTX 5080, 2026-09-23). Setting `undecorated_shadow = false` made DX12
pixel-identical to Vulkan, which confirmed the cause. Vulkan presents 1:1 and
clips the hidden row; with this change DX12 does the same.

## Keeping it up to date

One line in `src/lib.rs` quiets lints
(`#![allow(unexpected_cfgs, unused_qualifications, unused_assignments, mismatched_lifetime_syntaxes)]`):
a path dependency reports the warnings a registry one hides. The crate's own
`Cargo.lock` was dropped.

On a wgpu upgrade, re-copy the matching `wgpu-hal` from the registry, re-apply
the change and that line, and check whether upstream has made the scaling
configurable. The same manual step applies to `vendor/iced_winit` and
`vendor/iced_widget`.
