# `iced_widget` — Arbiter's fork

A verbatim copy of `iced_widget` 0.13.4 from crates.io with two changes, applied
through `[patch.crates-io]` in the workspace `Cargo.toml`.

## The first change

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

## The second change

`src/text_editor.rs`, on `Content`:

```rust
pub fn editor(&self) -> std::cell::Ref<'_, R::Editor> {
    std::cell::Ref::map(self.0.borrow(), |internal| &internal.editor)
}
```

The editor's line-number gutter is drawn from the editor's own scroll rather
than from a copy kept beside it, which is what lets the editor be the viewport
and scroll itself: cosmic-text then shapes, and iced then highlights, only the
lines on screen. Opening a 2,400 line file went from 6.9 s to 0.17 s that way
(debug build), and memory stopped following the size of the file.

The scroll is reachable from the concrete editor (`iced_graphics::text::Editor`
has a public `buffer()`, and `cosmic_text::Buffer::scroll()` is public), but
`Content` kept it private, and `iced_widget` is generic over the renderer so it
cannot read it either. Exposing the editor is the smallest opening that works
from the application side; nothing in the widget's own behaviour changes.


## Keeping it up to date

Nothing else is modified. On an iced upgrade, re-copy the new `iced_widget` from
the registry, re-apply both changes above, and check whether upstream has fixed
the hit-test (in which case that half goes; the accessor stays until `Content`
exposes the scroll itself). The same manual step already applies to
`vendor/iced_winit`.
