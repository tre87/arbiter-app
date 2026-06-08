//! One-shot: bake `assets/logo.svg` into a high-res `assets/logo.png`.
//!
//! iced draws SVGs with the nearest-neighbour sampler, so the logo pixelates
//! when its rasterised size doesn't exactly match the on-screen size. A high-res
//! PNG drawn with linear filtering downscales smoothly and stays crisp at any
//! display scale. Re-run after editing logo.svg:  `cargo run --example rasterize_logo`
//! (uses resvg — the same renderer iced uses internally).

use resvg::{tiny_skia, usvg};

const SIZE: u32 = 128; // 4× the 28px display box — plenty of headroom for 2–3× displays.

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let svg = std::fs::read(dir.join("logo.svg")).expect("read logo.svg");
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).expect("parse svg");

    let mut pixmap = tiny_skia::Pixmap::new(SIZE, SIZE).expect("pixmap");
    let s = tree.size();
    let scale = (SIZE as f32 / s.width()).min(SIZE as f32 / s.height());
    let ts = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, ts, &mut pixmap.as_mut());

    let out = dir.join("logo.png");
    pixmap.save_png(&out).expect("save png");
    println!("wrote {} ({SIZE}x{SIZE})", out.display());
}
