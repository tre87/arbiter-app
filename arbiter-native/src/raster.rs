//! Glyph rasterisation. To match the OS's own text rendering — and therefore the
//! webview we're replacing — we rasterise with the platform-native engine:
//! CoreText on macOS (matches WKWebView), with swash as the fallback for other
//! platforms (DirectWrite on Windows is a follow-up). The returned coverage is a
//! tightly-packed 8-bit alpha bitmap, rows top-down.

/// A rasterised glyph plus placement relative to the pen. `left` is the x of the
/// bitmap's left column relative to the pen origin; `top` is the distance from the
/// baseline up to the top of the bitmap. `coverage` is 8-bit alpha (1 byte/px,
/// rows top-down) for a normal glyph, or straight-alpha sRGB RGBA (4 bytes/px) when
/// `color` is set — i.e. a colour glyph like an emoji.
pub struct GlyphBitmap {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,
    pub color: bool,
}

/// Rasterise `ch` at `em_px` (the CSS-style em size in device px), in bold if
/// requested. Returns None for a missing glyph (`.notdef`) — callers leave the
/// cell blank and fall back.
pub fn rasterize(
    font_name: &str,
    font_data: &[u8],
    font_index: u32,
    em_px: f32,
    ch: char,
    bold: bool,
) -> Option<GlyphBitmap> {
    #[cfg(target_os = "macos")]
    {
        let _ = (font_data, font_index);
        mac::rasterize(font_name, em_px, ch, bold)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // TODO: synthetic/real bold on the swash path (Windows DirectWrite).
        let _ = (font_name, bold);
        swash_raster::rasterize(font_data, font_index, em_px, ch)
    }
}

/// swash rasteriser (non-macOS fallback, and the Windows path until DirectWrite).
/// Hinting on, so stems grid-fit to the pixel grid (crisp at small sizes).
#[cfg(not(target_os = "macos"))]
mod swash_raster {
    use super::GlyphBitmap;

    pub fn rasterize(font_data: &[u8], font_index: u32, em_px: f32, ch: char) -> Option<GlyphBitmap> {
        use swash::scale::{Render, Source};
        use swash::zeno::Format;
        thread_local! {
            static SCALE_CTX: std::cell::RefCell<swash::scale::ScaleContext> =
                std::cell::RefCell::new(swash::scale::ScaleContext::new());
        }
        let font = swash::FontRef::from_index(font_data, font_index as usize)?;
        let glyph_id = font.charmap().map(ch);
        if glyph_id == 0 {
            return None;
        }
        let image = SCALE_CTX.with(|c| {
            let mut ctx = c.borrow_mut();
            let mut scaler = ctx.builder(font).size(em_px).hint(true).build();
            Render::new(&[Source::Outline]).format(Format::Alpha).render(&mut scaler, glyph_id)
        })?;
        if image.placement.width == 0 || image.placement.height == 0 {
            return None;
        }
        Some(GlyphBitmap {
            left: image.placement.left,
            top: image.placement.top,
            width: image.placement.width,
            height: image.placement.height,
            coverage: image.data,
            // Colour emoji on the swash path need font fallback to an emoji font,
            // which this single-font scaler doesn't do yet (Windows DirectWrite TODO).
            color: false,
        })
    }
}

/// CoreText rasteriser (macOS). Renders the glyph into a grayscale CGBitmap with
/// font smoothing on, so the result matches WKWebView's text exactly.
#[cfg(target_os = "macos")]
mod mac {
    use super::GlyphBitmap;
    use core_foundation::base::{CFRange, CFRelease, TCFType};
    use core_foundation::string::{CFString, CFStringRef};
    use core_graphics::base::{kCGImageAlphaNone, kCGImageAlphaPremultipliedLast};
    use core_graphics::color_space::CGColorSpace;
    use core_graphics::context::CGContext;
    use core_graphics::geometry::CGPoint;
    use core_text::font::{CTFont, CTFontRef};
    use std::cell::RefCell;

    /// kCTFontBoldTrait — the bold bit in CTFontSymbolicTraits.
    const BOLD_TRAIT: u32 = 1 << 1;

    extern "C" {
        // Returns a font (the base, or a system fallback) that can render `string`
        // — the same cascade the webview uses for glyphs the base font lacks.
        fn CTFontCreateForString(current: CTFontRef, string: CFStringRef, range: CFRange) -> CTFontRef;
        // Returns a copy of `font` with the given symbolic traits (e.g. bold), or
        // null if the family has no such face.
        fn CTFontCreateCopyWithSymbolicTraits(
            font: CTFontRef,
            size: f64,
            matrix: *const std::ffi::c_void,
            sym_trait_value: u32,
            sym_trait_mask: u32,
        ) -> CTFontRef;
        // Copies a font table by tag, or null if the font lacks it. Used to detect
        // colour fonts (sbix/COLR/CBDT) so emoji are rendered in colour.
        fn CTFontCopyTable(font: CTFontRef, table: u32, options: u32) -> *const std::ffi::c_void;
    }

    /// Whether `font` carries a colour-glyph table (Apple Color Emoji = sbix; COLR/
    /// CBDT for others) → its glyphs must be rendered as RGBA, not coverage.
    fn is_color_font(font: &CTFont) -> bool {
        // FourCC tags, big-endian (as CoreText expects): 'sbix', 'COLR', 'CBDT'.
        const TAGS: [u32; 3] = [0x73626978, 0x434F4C52, 0x43424454];
        TAGS.iter().any(|&tag| {
            let t = unsafe { CTFontCopyTable(font.as_concrete_TypeRef(), tag, 0) };
            if t.is_null() {
                false
            } else {
                unsafe { CFRelease(t) };
                true
            }
        })
    }

    struct Cached {
        name: String,
        em_key: u32,
        regular: CTFont,
        bold: CTFont,
    }

    thread_local! {
        static FONT: RefCell<Option<Cached>> = RefCell::new(None);
    }

    pub fn rasterize(font_name: &str, em_px: f32, ch: char, bold: bool) -> Option<GlyphBitmap> {
        let em_key = em_px.round() as u32;
        FONT.with(|cell| {
            let mut cell = cell.borrow_mut();
            let stale = match cell.as_ref() {
                Some(c) => c.name != font_name || c.em_key != em_key,
                None => true,
            };
            if stale {
                let regular = core_text::font::new_from_name(font_name, em_px as f64).ok()?;
                // Real bold face via the bold trait; fall back to regular if none.
                let bold_ref = unsafe {
                    CTFontCreateCopyWithSymbolicTraits(
                        regular.as_concrete_TypeRef(),
                        em_px as f64,
                        std::ptr::null(),
                        BOLD_TRAIT,
                        BOLD_TRAIT,
                    )
                };
                let bold = if bold_ref.is_null() {
                    regular.clone()
                } else {
                    unsafe { CTFont::wrap_under_create_rule(bold_ref) }
                };
                *cell = Some(Cached { name: font_name.to_string(), em_key, regular, bold });
            }
            let c = cell.as_ref().unwrap();
            let font = if bold { &c.bold } else { &c.regular };
            rasterize_with(font, ch)
        })
    }

    /// Resolve `ch` to (font, glyph): the base font if it has the glyph, else a
    /// system fallback via CoreText's cascade (so ▶, emoji, etc. still render).
    fn resolve_glyph(base: &CTFont, units: &[u16], ch: char) -> Option<(CTFont, u16)> {
        let mut g = [0u16; 2];
        let have = unsafe {
            base.get_glyphs_for_characters(units.as_ptr(), g.as_mut_ptr(), units.len() as isize)
        };
        if have && g[0] != 0 {
            return Some((base.clone(), g[0]));
        }
        let s = CFString::new(&ch.to_string());
        let range = CFRange { location: 0, length: units.len() as isize };
        let fb_ref =
            unsafe { CTFontCreateForString(base.as_concrete_TypeRef(), s.as_concrete_TypeRef(), range) };
        if fb_ref.is_null() {
            return None;
        }
        let fb = unsafe { CTFont::wrap_under_create_rule(fb_ref) };
        let mut g2 = [0u16; 2];
        let ok = unsafe {
            fb.get_glyphs_for_characters(units.as_ptr(), g2.as_mut_ptr(), units.len() as isize)
        };
        if !ok || g2[0] == 0 {
            return None;
        }
        Some((fb, g2[0]))
    }

    fn rasterize_with(base: &CTFont, ch: char) -> Option<GlyphBitmap> {
        let mut utf16 = [0u16; 2];
        let units = ch.encode_utf16(&mut utf16);
        let (font, glyph) = resolve_glyph(base, units, ch)?;

        // Ink bounding box (baseline-relative, y-up) at the font's px size.
        let bbox = font.get_bounding_rects_for_glyphs(0, &[glyph]);
        let pad = 1.0_f64;
        let w = (bbox.size.width.ceil() + 2.0 * pad) as usize;
        let h = (bbox.size.height.ceil() + 2.0 * pad) as usize;
        if w == 0 || h == 0 {
            return None;
        }
        // Placement, top-down + baseline-relative (matches the swash convention).
        let left = (bbox.origin.x - pad).round() as i32;
        let top = (h as f64 - (pad - bbox.origin.y)).round() as i32;
        let pen = CGPoint::new(pad - bbox.origin.x, pad - bbox.origin.y);

        // Colour glyph (emoji) → render RGBA; otherwise grayscale coverage.
        if is_color_font(&font) {
            let cs = CGColorSpace::create_device_rgb();
            let mut ctx =
                CGContext::create_bitmap_context(None, w, h, 8, w * 4, &cs, kCGImageAlphaPremultipliedLast);
            ctx.set_should_antialias(true);
            ctx.set_should_smooth_fonts(false);
            font.draw_glyphs(&[glyph], &[pen], ctx.clone());
            let stride = ctx.bytes_per_row();
            let data = ctx.data();
            let mut rgba = vec![0u8; w * h * 4];
            for row in 0..h {
                for x in 0..w {
                    let s = row * stride + x * 4;
                    let a = data[s + 3] as u32;
                    // Un-premultiply to straight alpha so the shader composites it
                    // over the cell bg (CoreText gives premultiplied RGBA).
                    let un = |c: u8| if a > 0 { ((c as u32 * 255 / a) as u8).min(255) } else { 0 };
                    let d = (row * w + x) * 4;
                    rgba[d] = un(data[s]);
                    rgba[d + 1] = un(data[s + 1]);
                    rgba[d + 2] = un(data[s + 2]);
                    rgba[d + 3] = a as u8;
                }
            }
            return Some(GlyphBitmap {
                left,
                top,
                width: w as u32,
                height: h as u32,
                coverage: rgba,
                color: true,
            });
        }

        // Grayscale bitmap (zero-init = black background, white glyph).
        let cs = CGColorSpace::create_device_gray();
        let mut ctx = CGContext::create_bitmap_context(None, w, h, 8, w, &cs, kCGImageAlphaNone);
        // Grayscale antialiasing WITHOUT font smoothing — matches the web's
        // `-webkit-font-smoothing: antialiased`. Smoothing adds macOS's
        // stem-darkening/glow, which reads as a haze against the dark terminal.
        ctx.set_should_antialias(true);
        ctx.set_should_smooth_fonts(false);
        ctx.set_gray_fill_color(1.0, 1.0);

        // Place the ink's bottom-left (bbox.origin) at (pad, pad) in the bitmap.
        font.draw_glyphs(&[glyph], &[pen], ctx.clone());

        // Repack to a tight width-stride buffer (CG may pad bytes_per_row).
        let stride = ctx.bytes_per_row();
        let data = ctx.data();
        let mut coverage = vec![0u8; w * h];
        for row in 0..h {
            let src = &data[row * stride..row * stride + w];
            coverage[row * w..row * w + w].copy_from_slice(src);
        }

        Some(GlyphBitmap { left, top, width: w as u32, height: h as u32, coverage, color: false })
    }
}
