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

/// The em size an icon (see `is_icon`) is drawn at when the renderer gives it two cells,
/// relative to the text size. Measured at 16px text against an 8x11 capital H: at the
/// font's natural size (1.0) a tag is 14x14 and a database 14x17, a capital and a half,
/// which is what other terminals show and read as too big here; squeezed into one cell
/// they had been scaled down to 9 wide, which read as small. 0.8 gives 11x11 and 11x14,
/// about a capital, a quarter larger than the one-cell size, with a cell's worth of gap
/// left before the text. `gpu::fit_to_box` catches an icon that still overflows.
pub const ICON_EM_SCALE: f32 = 0.8;

/// A Private Use Area code point other than Powerline's (U+E0A0..=U+E0D7): an icon that
/// may spill into a following blank cell, as Windows Terminal and WezTerm let it (see
/// `TermGpu::prepare`). Powerline's separators are drawn to exactly one cell against
/// their segment colours and must stay put.
pub fn is_icon(ch: char) -> bool {
    let c = ch as u32;
    ((0xE000..=0xF8FF).contains(&c) && !(0xE0A0..=0xE0D7).contains(&c)) || (0xF0000..=0xFFFFD).contains(&c)
}

/// Rasterise `ch` at `em_px` (the CSS-style em size in device px), in bold if
/// requested. `icon` draws it at `ICON_EM_SCALE` times that, for a two-cell icon.
/// Returns None for a missing glyph (`.notdef`) — callers leave the cell blank.
pub fn rasterize(
    font_name: &str,
    font_data: &[u8],
    font_index: u32,
    bold_data: Option<&[u8]>,
    em_px: f32,
    ch: char,
    bold: bool,
    wide: bool,
    icon: bool,
) -> Option<GlyphBitmap> {
    #[cfg(target_os = "macos")]
    {
        let _ = (font_data, font_index, bold_data, wide);
        mac::rasterize(font_name, em_px, ch, bold, icon)
    }
    #[cfg(target_os = "windows")]
    {
        // DirectWrite: OS-native rendering + system font fallback + colour emoji.
        // bold_data (the bundled bold .ttf) is loaded into a real bold font face so
        // bold renders crisp instead of a synthesised faux-bold.
        let _ = (font_data, font_index);
        dwrite::rasterize(font_name, bold_data, em_px, ch, bold, wide, icon)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Linux: swash with fontdb fallback (incl. Noto Color Emoji).
        let _ = (font_name, bold, bold_data, wide);
        swash_raster::rasterize(font_data, font_index, em_px, ch, icon)
    }
}

/// swash rasteriser (non-macOS: Windows/Linux). Renders the primary font's glyphs
/// (hinted, grid-fit, crisp at small sizes) and, when the primary font lacks a
/// glyph, falls back to a system font found via `fontdb` — including colour emoji
/// fonts (Segoe UI Emoji's COLR / Noto Color Emoji's CBDT), rendered as RGBA.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod swash_raster {
    use super::GlyphBitmap;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use swash::scale::image::Content;
    use swash::scale::{Render, ScaleContext, Source, StrikeWith};
    use swash::zeno::Format;
    use swash::FontRef;

    thread_local! {
        static SCALE_CTX: RefCell<ScaleContext> = RefCell::new(ScaleContext::new());
        static FALLBACK: RefCell<Fallback> = RefCell::new(Fallback::new());
    }

    pub fn rasterize(font_data: &[u8], font_index: u32, em_px: f32, ch: char, icon: bool) -> Option<GlyphBitmap> {
        // 1. Primary (terminal) font: a normal mono glyph.
        let primary = FontRef::from_index(font_data, font_index as usize)?;
        let gid = primary.charmap().map(ch);
        if gid != 0 {
            return render(&primary, gid, em_px, false);
        }
        // 2. The bundled symbols font (see `font::SYMBOLS`), before any system font; an
        // icon given two cells is drawn larger (see `ICON_EM_SCALE`).
        if let Some(symbols) = FontRef::from_index(crate::font::SYMBOLS, 0) {
            let gid = symbols.charmap().map(ch);
            if gid != 0 {
                let em = if icon { em_px * super::ICON_EM_SCALE } else { em_px };
                return render(&symbols, gid, em, false);
            }
        }
        // 3. Fallback to a system font that has this glyph (emoji / symbols / CJK).
        FALLBACK.with(|fb| {
            let mut fb = fb.borrow_mut();
            let id = fb.resolve(ch)?;
            let rc = fb.bytes(id)?;
            let font = FontRef::from_index(&rc.0, rc.1 as usize)?;
            let gid = font.charmap().map(ch);
            if gid == 0 {
                return None;
            }
            render(&font, gid, em_px, true)
        })
    }

    /// Rasterise one glyph. `allow_color` enables the colour sources (emoji); the
    /// result is RGBA when the font supplies a colour glyph, else alpha coverage.
    fn render(font: &FontRef, gid: u16, em_px: f32, allow_color: bool) -> Option<GlyphBitmap> {
        SCALE_CTX.with(|c| {
            let mut ctx = c.borrow_mut();
            // Colour bitmaps/outlines aren't hinted; outline glyphs are (crisp stems).
            let mut scaler = ctx.builder(*font).size(em_px).hint(!allow_color).build();
            let sources: &[Source] = if allow_color {
                &[Source::ColorBitmap(StrikeWith::BestFit), Source::ColorOutline(0), Source::Outline]
            } else {
                &[Source::Outline]
            };
            let image = Render::new(sources).format(Format::Alpha).render(&mut scaler, gid)?;
            if image.placement.width == 0 || image.placement.height == 0 {
                return None;
            }
            Some(GlyphBitmap {
                left: image.placement.left,
                top: image.placement.top,
                width: image.placement.width,
                height: image.placement.height,
                coverage: image.data,
                color: matches!(image.content, Content::Color),
            })
        })
    }

    /// Rough emoji-codepoint test, to prefer colour-emoji fonts during fallback.
    fn is_emoji(ch: char) -> bool {
        let c = ch as u32;
        (0x1F000..=0x1FAFF).contains(&c)
            || (0x2600..=0x27BF).contains(&c)
            || (0x2B00..=0x2BFF).contains(&c)
            || (0xFE00..=0xFE0F).contains(&c) // variation selectors
            || (0x1F1E6..=0x1F1FF).contains(&c) // regional indicators
    }

    /// System-font fallback: a `fontdb` of installed fonts + caches of which font
    /// serves each char and the loaded font bytes.
    struct Fallback {
        db: fontdb::Database,
        resolved: HashMap<char, Option<fontdb::ID>>,
        loaded: HashMap<fontdb::ID, Rc<(Vec<u8>, u32)>>,
    }

    impl Fallback {
        fn new() -> Self {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            Fallback { db, resolved: HashMap::new(), loaded: HashMap::new() }
        }

        fn resolve(&mut self, ch: char) -> Option<fontdb::ID> {
            if let Some(&id) = self.resolved.get(&ch) {
                return id;
            }
            let id = self.search(ch);
            self.resolved.insert(ch, id);
            id
        }

        fn search(&self, ch: char) -> Option<fontdb::ID> {
            // Prefer the platform colour-emoji font for emoji; otherwise a symbol /
            // CJK font. First match that actually contains the glyph wins.
            const EMOJI: &[&str] = &["Segoe UI Emoji", "Noto Color Emoji", "Apple Color Emoji"];
            const OTHER: &[&str] = &[
                "Segoe UI Symbol", "Segoe UI", "Noto Sans Symbols 2", "Noto Sans Symbols",
                "Microsoft YaHei UI", "Yu Gothic UI", "Malgun Gothic", "Noto Sans CJK SC",
                "Noto Sans CJK JP", "Noto Sans CJK KR",
            ];
            let names = if is_emoji(ch) { EMOJI } else { OTHER };
            for name in names {
                let q = fontdb::Query {
                    families: &[fontdb::Family::Name(name)],
                    ..Default::default()
                };
                if let Some(id) = self.db.query(&q) {
                    if self.face_has(id, ch) {
                        return Some(id);
                    }
                }
            }
            None
        }

        fn face_has(&self, id: fontdb::ID, ch: char) -> bool {
            self.db
                .with_face_data(id, |d, i| {
                    FontRef::from_index(d, i as usize).map(|f| f.charmap().map(ch) != 0).unwrap_or(false)
                })
                .unwrap_or(false)
        }

        fn bytes(&mut self, id: fontdb::ID) -> Option<Rc<(Vec<u8>, u32)>> {
            if let Some(rc) = self.loaded.get(&id) {
                return Some(rc.clone());
            }
            let data = self.db.with_face_data(id, |d, i| (d.to_vec(), i))?;
            let rc = Rc::new(data);
            self.loaded.insert(id, rc.clone());
            Some(rc)
        }
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
    use core_graphics::data_provider::CGDataProvider;
    use core_graphics::font::CGFont;
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
        /// The bundled symbols font (`font::SYMBOLS`) at this size: the stop for an icon
        /// before CoreText's cascade, which has nothing for the Private Use Area.
        symbols: Option<CTFont>,
        /// The same at `ICON_EM_SCALE` times the size, for an icon given two cells.
        symbols_icon: Option<CTFont>,
    }

    thread_local! {
        static FONT: RefCell<Option<Cached>> = RefCell::new(None);
    }

    pub fn rasterize(font_name: &str, em_px: f32, ch: char, bold: bool, icon: bool) -> Option<GlyphBitmap> {
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
                let symbols = bundled_symbols(em_px);
                let symbols_icon = bundled_symbols(em_px * super::ICON_EM_SCALE);
                *cell = Some(Cached { name: font_name.to_string(), em_key, regular, bold, symbols, symbols_icon });
            }
            let c = cell.as_ref().unwrap();
            let font = if bold { &c.bold } else { &c.regular };
            // An icon given two cells comes from the larger symbols font (see
            // `ICON_EM_SCALE`); the base font is asked first either way.
            let symbols = if icon { c.symbols_icon.as_ref().or(c.symbols.as_ref()) } else { c.symbols.as_ref() };
            rasterize_with(font, symbols, ch)
        })
    }

    /// The bundled symbols font (`font::SYMBOLS`) at `size`, straight from its bytes.
    fn bundled_symbols(size: f32) -> Option<CTFont> {
        let provider = CGDataProvider::from_buffer(std::sync::Arc::new(crate::font::SYMBOLS));
        let cg = CGFont::from_data_provider(provider).ok()?;
        Some(core_text::font::new_from_CGFont(&cg, size as f64))
    }

    /// Resolve `ch` to (font, glyph): the base font if it has the glyph, else the bundled
    /// symbols font (icons, which no system font has), else a system fallback via
    /// CoreText's cascade (so ▶, emoji, etc. still render).
    fn resolve_glyph(base: &CTFont, symbols: Option<&CTFont>, units: &[u16], ch: char) -> Option<(CTFont, u16)> {
        let glyph_in = |font: &CTFont| {
            let mut g = [0u16; 2];
            let have = unsafe {
                font.get_glyphs_for_characters(units.as_ptr(), g.as_mut_ptr(), units.len() as isize)
            };
            (have && g[0] != 0).then_some(g[0])
        };
        if let Some(g) = glyph_in(base) {
            return Some((base.clone(), g));
        }
        if let Some(s) = symbols {
            if let Some(g) = glyph_in(s) {
                return Some((s.clone(), g));
            }
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

    fn rasterize_with(base: &CTFont, symbols: Option<&CTFont>, ch: char) -> Option<GlyphBitmap> {
        let mut utf16 = [0u16; 2];
        let units = ch.encode_utf16(&mut utf16);
        let (font, glyph) = resolve_glyph(base, symbols, units, ch)?;

        // Ink bounding box (baseline-relative, y-up) at the font's px size.
        let bbox = font.get_bounding_rects_for_glyphs(0, &[glyph]);
        let pad = 1.0_f64;
        let w = (bbox.size.width.ceil() + 2.0 * pad) as usize;
        // Snap the baseline to a WHOLE pixel row so every glyph shares the exact
        // same baseline. Previously the pen sat at a fractional `pad - origin.y`
        // while `top` was rounded per-glyph, so the displayed baseline drifted by
        // `origin.y - round(origin.y)` — which made deep-descender glyphs (g, q)
        // and ones that dip just below the baseline (ø) sit ~0.5px low vs x-height
        // letters. Split the bitmap into whole rows above + below the baseline so
        // the baseline lands on an integer row and `top` is exact (no rounding).
        let above = (bbox.origin.y + bbox.size.height).ceil().max(0.0) + pad; // rows above baseline
        let below = (-bbox.origin.y).ceil().max(0.0) + pad; // rows below (descenders)
        let h = (above + below) as usize;
        if w == 0 || h == 0 {
            return None;
        }
        // Placement, top-down + baseline-relative (matches the swash convention).
        let left = (bbox.origin.x - pad).round() as i32;
        let top = above as i32;
        // Pen at an INTEGER baseline (`below` rows up from the bitmap's bottom).
        let pen = CGPoint::new(pad - bbox.origin.x, below);

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

#[cfg(all(test, windows))]
mod tests {
    /// (width, height, ink) of a rendered glyph: enough to tell one glyph from another.
    fn sig(g: Option<super::GlyphBitmap>) -> (u32, u32, u64) {
        let g = g.expect("a glyph");
        assert!(!g.color, "a mono glyph, not a colour one");
        (g.width, g.height, g.coverage.iter().map(|&c| c as u64).sum::<u64>())
    }

    /// Powerline's triangle plus the Font Awesome and Octicons code points a status line
    /// uses: in the bundled symbols font, in no system font.
    const ICONS: [char; 12] = [
        '\u{E0B0}', '\u{F02B}', '\u{F49B}', '\u{F1C0}', '\u{F07B}', '\u{F418}', '\u{F00C}', '\u{F040}',
        '\u{F067}', '\u{F071}', '\u{F0AA}', '\u{F0AB}',
    ];

    /// A Private Use Area code point no font has, so the layout draws the terminal font's
    /// .notdef box: the bitmap an icon must NOT come out as.
    const NOWHERE: char = '\u{F8F0}';

    // Out of the box: the terminal font alone, nothing installed or configured, and the
    // icons still come out as glyphs (from the bundled symbols font); letters untouched.
    #[test]
    fn icons_render_with_nothing_installed_or_configured() {
        let raster = |ch: char| sig(super::rasterize("Cascadia Mono", &[], 0, None, 16.0, ch, false, false, false));
        let notdef = raster(NOWHERE);
        for ch in ICONS {
            assert_ne!(raster(ch), notdef, "U+{:04X} came out as the .notdef box", ch as u32);
        }
        assert!(raster('a').2 > 0);
    }

    // An icon at the text size is already wider than a cell and taller than a capital:
    // what the one-cell renderer had been shrinking (see `gpu::fit_to_box`'s tests).
    #[test]
    fn an_icon_is_naturally_wider_than_a_cell() {
        let h = sig(super::rasterize("Cascadia Mono", &[], 0, None, 16.0, 'H', false, false, false));
        let tag = sig(super::rasterize("Cascadia Mono", &[], 0, None, 16.0, '\u{F02B}', false, false, false));
        assert!(tag.0 > h.0 && tag.1 > h.1, "H {h:?} tag {tag:?}");
    }
}

#[cfg(test)]
mod icon_tests {
    // Icons are the Private Use Area minus Powerline, whose separators must keep one cell.
    #[test]
    fn powerline_is_not_an_icon_but_the_rest_of_the_pua_is() {
        use super::is_icon;
        assert!(is_icon('\u{E000}') && is_icon('\u{F013}') && is_icon('\u{F8FF}') && is_icon('\u{F0001}'));
        assert!(!is_icon('\u{E0B0}') && !is_icon('\u{E0A0}') && !is_icon('\u{E0D7}'), "Powerline");
        assert!(is_icon('\u{E09F}') && is_icon('\u{E0D8}'), "either side of Powerline");
        assert!(!is_icon('a') && !is_icon('\u{2022}') && !is_icon('\u{1F600}'));
    }
}

/// DirectWrite + Direct2D rasteriser (Windows). Renders one character through an
/// `IDWriteTextLayout` — which does system font fallback automatically (emoji →
/// Segoe UI Emoji, CJK/symbols → their fonts) — into a WIC RGBA bitmap with colour
/// fonts enabled. Mono glyphs come out as white-on-transparent (→ coverage from
/// alpha); colour glyphs come out coloured (→ RGBA). The OS-native equivalent of
/// the macOS CoreText path.
#[cfg(target_os = "windows")]
mod dwrite {
    use super::GlyphBitmap;
    use std::cell::RefCell;
    use windows::core::{Result, PCWSTR};
    use windows::Win32::Graphics::Direct2D::Common::{
        D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_POINT_2F,
    };
    use windows::Win32::Graphics::Direct2D::{
        D2D1CreateFactory, ID2D1Factory, ID2D1RenderTarget, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
        D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
        D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    };
    use windows::core::Interface;
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Graphics::DirectWrite::{
        DWriteCreateFactory, IDWriteFactory, IDWriteFactory3, IDWriteFactory5, IDWriteFontCollection,
        IDWriteFontFace, IDWriteFontFile, IDWriteFontSetBuilder1, IDWriteInMemoryFontFileLoader,
        IDWriteRenderingParams3, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FACE_TYPE_TRUETYPE,
        DWRITE_FONT_SIMULATIONS_NONE, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_GRID_FIT_MODE_ENABLED,
        DWRITE_LINE_METRICS, DWRITE_RENDERING_MODE1_NATURAL_SYMMETRIC,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapCacheOnLoad,
        WICBitmapLockRead, WICRect,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };

    struct Ctx {
        dwrite: IDWriteFactory,
        d2d: ID2D1Factory,
        wic: IWICImagingFactory,
        family: Vec<u16>, // null-terminated
        name: String,
        em: f32,
        /// Rendering params matching Windows Terminal: NATURAL_SYMMETRIC mode (smooth,
        /// fractional positioning — no GDI bold-distortion) with grid-fit ENABLED (small
        /// features like the counter of `B` stay crisp). Gamma 1.0 / no contrast so the
        /// atlas holds RAW coverage; the gamma-correction is applied in the GPU shader.
        params: IDWriteRenderingParams3,
        /// The bundled Cascadia Mono **Bold** face, loaded from our shipped bytes into a
        /// private font collection. Bold glyphs render through the SAME DrawTextLayout
        /// path as regular (just pointed at this collection), so they're as crisp as
        /// regular — instead of DirectWrite synthesising a soft faux-bold by name. None
        /// if the bytes weren't supplied or failed to load.
        bold: Option<PrivateFont>,
        /// The bundled symbols font (`font::SYMBOLS`) in a private collection: the stop
        /// for an icon before DirectWrite's own fallback, which has nothing for the
        /// Private Use Area.
        symbols: Option<PrivateFont>,
        /// The primary family's regular face, for the glyph-presence check that decides
        /// whether a symbol is laid out in `symbol` instead (see `render`).
        regular: Option<IDWriteFontFace>,
        /// "Segoe UI Symbol", null-terminated: the text-presentation home of the symbols
        /// DirectWrite's own fallback would otherwise take from Segoe UI Emoji.
        symbol: Vec<u16>,
    }

    /// A bundled font: its face (for the glyph-presence check) + the private collection
    /// wrapping it (for CreateTextFormat) + that collection's family name. `_loader` is
    /// kept alive + registered for the collection/face's lifetime (they read the
    /// in-memory bytes through it).
    struct PrivateFont {
        _loader: IDWriteInMemoryFontFileLoader,
        face: IDWriteFontFace,
        collection: IDWriteFontCollection,
        family: Vec<u16>, // null-terminated
    }

    thread_local! {
        static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
    }

    pub fn rasterize(
        font_name: &str,
        bold_data: Option<&[u8]>,
        em_px: f32,
        ch: char,
        bold: bool,
        wide: bool,
        icon: bool,
    ) -> Option<GlyphBitmap> {
        CTX.with(|cell| {
            let mut cell = cell.borrow_mut();
            let stale = cell
                .as_ref()
                .map_or(true, |c| c.name != font_name || (c.em - em_px).abs() > 0.5);
            if stale {
                *cell = build_ctx(font_name, bold_data, em_px).ok();
            }
            let ctx = cell.as_ref()?;
            // An icon given two cells is drawn larger (see `ICON_EM_SCALE`); the context
            // is size-independent apart from the layout, so no rebuild for that.
            let em = if icon { em_px * super::ICON_EM_SCALE } else { em_px };
            render(ctx, em, ch, bold, !wide && emoji_block(ch)).ok().flatten()
        })
    }

    /// Whether `ch` sits in a block that carries emoji, where DirectWrite's own fallback
    /// reaches for Segoe UI Emoji. In a NARROW cell such a character (`⏺`, `✳`, `⏸`, `▶`)
    /// has text presentation by definition, and the emoji font would give back its colour
    /// button glyph instead: a blue square where Claude draws a bullet. Claude on Windows
    /// avoids these characters for exactly that reason; Claude on a Mac, reached over
    /// ssh, does not.
    fn emoji_block(ch: char) -> bool {
        let c = ch as u32;
        (0x2190..=0x21FF).contains(&c) // Arrows
            || (0x2300..=0x23FF).contains(&c) // Miscellaneous Technical
            || (0x25A0..=0x25FF).contains(&c) // Geometric Shapes
            || (0x2600..=0x27BF).contains(&c) // Miscellaneous Symbols, Dingbats
            || (0x2B00..=0x2BFF).contains(&c) // Miscellaneous Symbols and Arrows
            || (0x1F000..=0x1FAFF).contains(&c)
    }

    /// Load a bundled .ttf into a private DirectWrite font collection (in-memory loader,
    /// no dependency on the system fonts) plus a font face for glyph-coverage checks, and
    /// read back the collection's family name. None on any failure (the caller then does
    /// without). The loader must outlive the collection/face.
    unsafe fn build_private_font(dwrite: &IDWriteFactory, data: &[u8]) -> Option<PrivateFont> {
        let f5: IDWriteFactory5 = dwrite.cast().ok()?;
        let loader = f5.CreateInMemoryFontFileLoader().ok()?;
        f5.RegisterFontFileLoader(&loader).ok()?;
        let file: IDWriteFontFile = loader
            .CreateInMemoryFontFileReference(
                &f5,
                data.as_ptr() as *const core::ffi::c_void,
                data.len() as u32,
                None,
            )
            .ok()?;
        // A face for the glyph-presence check (GetGlyphIndices).
        let face = dwrite
            .CreateFontFace(
                DWRITE_FONT_FACE_TYPE_TRUETYPE,
                &[Some(file.clone())],
                0,
                DWRITE_FONT_SIMULATIONS_NONE,
            )
            .ok()?;
        // A private collection wrapping the same file, so CreateTextFormat can resolve
        // the bold face by family name and DrawTextLayout renders it exactly like regular.
        let builder: IDWriteFontSetBuilder1 = f5.CreateFontSetBuilder().ok()?.cast().ok()?;
        builder.AddFontFile(&file).ok()?;
        let set = builder.CreateFontSet().ok()?;
        let coll = f5.CreateFontCollectionFromFontSet(&set).ok()?;
        // Family name of the (single) family in the collection, null-terminated.
        let names = coll.GetFontFamily(0).ok()?.GetFamilyNames().ok()?;
        let len = names.GetStringLength(0).ok()? as usize;
        let mut family = vec![0u16; len + 1];
        names.GetString(0, &mut family).ok()?;
        let collection: IDWriteFontCollection = coll.cast().ok()?;
        Some(PrivateFont { _loader: loader, face, collection, family })
    }

    fn build_ctx(font_name: &str, bold_data: Option<&[u8]>, em_px: f32) -> Result<Ctx> {
        unsafe {
            // WIC needs COM on this thread; harmless if already initialised.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let mut family: Vec<u16> = font_name.encode_utf16().collect();
            family.push(0);
            // We want RAW grayscale coverage here: the gamma-correction is applied in the
            // GPU shader (Windows Terminal's DirectWrite algorithm), so baking it in twice
            // would over-darken. Hence gamma 1.0 + no enhanced contrast.
            //
            // Rendering mode: NATURAL_SYMMETRIC + grid-fit ENABLED, the combination WT's
            // GetRecommendedRenderingMode returns at terminal sizes. NATURAL_SYMMETRIC uses
            // fractional positioning (no GDI bitmap-matching that over-thickens BOLD), while
            // grid-fit keeps stems/counters crisp on the pixel grid. We earlier tried
            // GDI_NATURAL (sharp but bold too heavy) and DEFAULT (lost grid-fit → soft);
            // this is the in-between WT actually uses. Needs IDWriteFactory3 (Win8.1+),
            // whose CreateCustomRenderingParams exposes mode + grid-fit as separate knobs.
            let def = dwrite.CreateRenderingParams()?;
            let f3: IDWriteFactory3 = dwrite.cast()?;
            let params = f3.CreateCustomRenderingParams(
                1.0, // gamma 1.0: capture raw coverage (shader applies WT's gamma 1.8)
                0.0, // enhanced contrast: none baked in (shader applies it against fg/bg)
                0.0, // grayscale enhanced contrast: none (shader applies it)
                0.0, // ClearType level: none (grayscale AA, no subpixel)
                def.GetPixelGeometry(),
                DWRITE_RENDERING_MODE1_NATURAL_SYMMETRIC,
                DWRITE_GRID_FIT_MODE_ENABLED,
            )?;
            // Load the bundled bold into a private collection (best-effort; falls back
            // to the by-name path if it fails).
            let bold = bold_data.and_then(|d| build_private_font(&dwrite, d));
            let symbols = build_private_font(&dwrite, crate::font::SYMBOLS);
            let regular = primary_face(&dwrite, &family);
            let mut symbol: Vec<u16> = "Segoe UI Symbol".encode_utf16().collect();
            symbol.push(0);
            crate::claude_shim::debug_log(&format!(
                "raster: dwrite ctx for {font_name:?} at {em_px}px: family installed={}, bundled bold={}, bundled symbols={}",
                regular.is_some(),
                bold.is_some(),
                symbols.is_some()
            ));
            Ok(Ctx {
                dwrite,
                d2d,
                wic,
                family,
                name: font_name.to_string(),
                em: em_px,
                params,
                bold,
                symbols,
                regular,
                symbol,
            })
        }
    }

    /// The regular face of the installed family `family` (null-terminated), if any.
    unsafe fn primary_face(dwrite: &IDWriteFactory, family: &[u16]) -> Option<IDWriteFontFace> {
        let mut collection: Option<IDWriteFontCollection> = None;
        dwrite.GetSystemFontCollection(&mut collection, false).ok()?;
        let collection = collection?;
        let mut index = 0u32;
        let mut exists = BOOL(0);
        collection.FindFamilyName(PCWSTR(family.as_ptr()), &mut index, &mut exists).ok()?;
        if !exists.as_bool() {
            return None;
        }
        collection
            .GetFontFamily(index)
            .ok()?
            .GetFirstMatchingFont(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)
            .ok()?
            .CreateFontFace()
            .ok()
    }

    /// Whether `face` has a glyph for `ch`.
    unsafe fn face_has(face: &IDWriteFontFace, ch: char) -> bool {
        let cps = [ch as u32];
        let mut gids = [0u16; 1];
        face.GetGlyphIndices(cps.as_ptr(), 1, gids.as_mut_ptr()).is_ok() && gids[0] != 0
    }

    fn render(ctx: &Ctx, em: f32, ch: char, bold: bool, text_presentation: bool) -> Result<Option<GlyphBitmap>> {
        unsafe {
            // Pick the font source. For bold, prefer our private collection holding the
            // REAL bundled bold face — DirectWrite would otherwise synthesise a soft
            // faux-bold by name (the installed Cascadia Mono has no static 700 face).
            // Only when the bold font actually has this glyph; otherwise use the system
            // collection by name so font fallback still serves emoji/symbols it lacks.
            // Either way it's the SAME DrawTextLayout path below, so bold is as crisp as
            // regular — only the collection + family differ.
            let bold_src = if bold { ctx.bold.as_ref().filter(|b| face_has(&b.face, ch)) } else { None };
            let weight = if bold { DWRITE_FONT_WEIGHT_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
            // A symbol the primary font lacks, in a narrow cell, is laid out in Segoe UI
            // Symbol by name rather than left to DirectWrite's fallback, which for these
            // blocks reaches for Segoe UI Emoji and returns its colour button glyph (a
            // blue square for ⏺, a teal one for ✳). If Segoe UI Symbol lacks it too, the
            // layout falls back from there as it always did.
            let primary_has = ctx.regular.as_ref().is_some_and(|f| face_has(f, ch));
            let in_symbol_font = text_presentation && bold_src.is_none() && !primary_has;
            // Then the bundled symbols font, for what nothing above has: the icons in the
            // Private Use Area, which DirectWrite's own fallback would leave as boxes.
            let symbols_src = if bold_src.is_none() && !primary_has && !in_symbol_font {
                ctx.symbols.as_ref().filter(|s| face_has(&s.face, ch))
            } else {
                None
            };
            let (collection, family): (Option<&IDWriteFontCollection>, &[u16]) = if let Some(b) = bold_src {
                (Some(&b.collection), &b.family)
            } else if in_symbol_font {
                (None, &ctx.symbol)
            } else if let Some(s) = symbols_src {
                (Some(&s.collection), &s.family)
            } else {
                (None, &ctx.family)
            };
            let locale: Vec<u16> = "en-us\0".encode_utf16().collect();
            let format = ctx.dwrite.CreateTextFormat(
                PCWSTR(family.as_ptr()),
                collection,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                em,
                PCWSTR(locale.as_ptr()),
            )?;

            let mut buf = [0u16; 2];
            let text: &[u16] = ch.encode_utf16(&mut buf);
            let boxw = (em * 2.5).ceil() + 4.0;
            let boxh = (em * 2.0).ceil() + 4.0;
            let layout = ctx.dwrite.CreateTextLayout(text, &format, boxw, boxh)?;

            // Baseline (box-top → baseline) for placement.
            let mut lm = [DWRITE_LINE_METRICS::default(); 1];
            let mut count = 0u32;
            let _ = layout.GetLineMetrics(Some(&mut lm), &mut count);
            let baseline = if lm[0].baseline > 0.0 { lm[0].baseline } else { em * 0.8 };

            let (w, h) = (boxw as u32, boxh as u32);
            let bitmap = ctx.wic.CreateBitmap(
                w,
                h,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapCacheOnLoad,
            )?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt: ID2D1RenderTarget = ctx.d2d.CreateWicBitmapRenderTarget(&bitmap, &props)?;
            // Grid-fitting rendering params (crisp counters); grayscale AA (the target
            // has alpha, so D2D uses grayscale, not ClearType — no subpixel fringing).
            rt.SetTextRenderingParams(&ctx.params);
            let white = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
            let clear = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
            let brush = rt.CreateSolidColorBrush(&white, None)?;
            rt.BeginDraw();
            rt.Clear(Some(&clear));
            rt.DrawTextLayout(
                D2D_POINT_2F { x: 0.0, y: 0.0 },
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
            rt.EndDraw(None, None)?;

            // Read the rendered PBGRA pixels.
            let rect = WICRect { X: 0, Y: 0, Width: w as i32, Height: h as i32 };
            let lock = bitmap.Lock(&rect, WICBitmapLockRead.0 as u32)?;
            let stride = lock.GetStride()? as usize;
            let mut size = 0u32;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            lock.GetDataPointer(&mut size, &mut ptr)?;
            if ptr.is_null() {
                return Ok(None);
            }
            let data = std::slice::from_raw_parts(ptr, size as usize);

            Ok(build_glyph(data, stride, w, h, baseline))
        }
    }

    /// Crop the rendered bitmap to the glyph's ink box and classify mono vs colour.
    /// PBGRA premultiplied: a white mono glyph has B==G==R==A; a colour glyph varies.
    fn build_glyph(data: &[u8], stride: usize, w: u32, h: u32, baseline: f32) -> Option<GlyphBitmap> {
        let (mut minx, mut miny, mut maxx, mut maxy) = (w, h, 0u32, 0u32);
        let (mut found, mut is_color) = (false, false);
        for y in 0..h {
            for x in 0..w {
                let i = y as usize * stride + x as usize * 4;
                let (b, g, r, a) = (data[i], data[i + 1], data[i + 2], data[i + 3]);
                if a == 0 {
                    continue;
                }
                found = true;
                minx = minx.min(x);
                maxx = maxx.max(x);
                miny = miny.min(y);
                maxy = maxy.max(y);
                if !(b == g && g == r) {
                    is_color = true;
                }
            }
        }
        if !found {
            return None;
        }
        let (bw, bh) = (maxx - minx + 1, maxy - miny + 1);
        let top = (baseline - miny as f32).round() as i32;
        let px = |x: u32, y: u32| y as usize * stride + x as usize * 4;
        if is_color {
            let mut rgba = vec![0u8; (bw * bh * 4) as usize];
            for yy in 0..bh {
                for xx in 0..bw {
                    let i = px(minx + xx, miny + yy);
                    let a = data[i + 3] as u32;
                    let un = |c: u8| if a > 0 { ((c as u32 * 255 / a).min(255)) as u8 } else { 0 };
                    let d = ((yy * bw + xx) * 4) as usize;
                    rgba[d] = un(data[i + 2]); // R (from BGRA)
                    rgba[d + 1] = un(data[i + 1]); // G
                    rgba[d + 2] = un(data[i]); // B
                    rgba[d + 3] = a as u8;
                }
            }
            Some(GlyphBitmap { left: minx as i32, top, width: bw, height: bh, coverage: rgba, color: true })
        } else {
            let mut cov = vec![0u8; (bw * bh) as usize];
            for yy in 0..bh {
                for xx in 0..bw {
                    cov[(yy * bw + xx) as usize] = data[px(minx + xx, miny + yy) + 3]; // alpha
                }
            }
            Some(GlyphBitmap { left: minx as i32, top, width: bw, height: bh, coverage: cov, color: false })
        }
    }
}

