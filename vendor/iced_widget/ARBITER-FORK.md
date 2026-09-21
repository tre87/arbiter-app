# `iced_widget` — Arbiter's fork

A verbatim copy of `iced_widget` 0.13.4 from crates.io with one fix, applied
through `[patch.crates-io]` in the workspace `Cargo.toml`.

## The change

`src/text_editor.rs`, in `Update::from_event`, on both the click path and the
drag path:

```diff
-    - Vector::new(padding.top, padding.left);
+    - Vector::new(padding.left, padding.top);
```

`Vector::new` takes `(x, y)`, so upstream subtracts the TOP padding from the
pointer's x and the LEFT padding from its y. Hit-testing therefore disagrees
with drawing, which uses `bounds.shrink(padding)` correctly.

With a uniform padding the two values are equal and the bug is invisible, which
is why upstream has not noticed it. Arbiter's editor draws its line numbers
inside the editor's own left padding, so that the editor's bounds cover them and
a drag across them keeps extending the selection. That makes the padding
asymmetric (about 45px left against 5px top), and every click then landed two
lines above and five characters left of the pointer.

## Keeping it up to date

Nothing else is modified. On an iced upgrade, re-copy the new `iced_widget` from
the registry, re-apply the two-line diff above, and check whether upstream has
fixed it (in which case drop this fork and the `[patch.crates-io]` entry). The
same manual step already applies to `vendor/iced_winit`.
