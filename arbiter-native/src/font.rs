//! Terminal font selection.
//!
//! macOS uses the system **Menlo** (matches what the webview resolved, and
//! CoreText can synthesise bold from the installed family). Windows bundles
//! **Cascadia Mono** (SIL OFL — Windows Terminal's font) so it looks right
//! without depending on what's installed, and ships the bold face too (the
//! swash rasteriser can't synthesise bold, so we carry a real bold font).

/// The regular (and optional bold) face for the terminal. `bold` is None when
/// the rasteriser can synthesise bold itself (CoreText on macOS); it's Some when
/// we must supply a real bold face (the swash path on Windows/Linux).
#[derive(Clone)]
pub struct FontSpec {
    pub name: String,
    pub regular: (Vec<u8>, u32),
    pub bold: Option<(Vec<u8>, u32)>,
}

/// Load the terminal font for this platform.
pub fn load() -> FontSpec {
    #[cfg(windows)]
    {
        // Bundled Cascadia Mono — Windows Terminal's font, OFL-licensed.
        FontSpec {
            name: "Cascadia Mono".to_string(),
            regular: (include_bytes!("../assets/CascadiaMono-Regular.ttf").to_vec(), 0),
            bold: Some((include_bytes!("../assets/CascadiaMono-Bold.ttf").to_vec(), 0)),
        }
    }
    #[cfg(not(windows))]
    {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let id = pick(&db);
        let name = db
            .face(id)
            .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
            .unwrap_or_else(|| "monospace".to_string());
        let (bytes, index) = db.with_face_data(id, |d, i| (d.to_vec(), i)).expect("font face data");
        eprintln!("[font] picked: {name:?}");
        // bold = None: CoreText synthesises bold from the installed family.
        FontSpec { name, regular: (bytes, index), bold: None }
    }
}

/// Pick a system monospace, preferring a clean stack (macOS lands on Menlo,
/// Linux on DejaVu/Liberation), then any monospace.
#[cfg(not(windows))]
fn pick(db: &fontdb::Database) -> fontdb::ID {
    const PREFERRED: &[&str] =
        &["Menlo", "SF Mono", "Monaco", "DejaVu Sans Mono", "Liberation Mono", "Cascadia Mono"];
    for name in PREFERRED {
        let q = fontdb::Query { families: &[fontdb::Family::Name(name)], ..Default::default() };
        if let Some(id) = db.query(&q) {
            return id;
        }
    }
    let q = fontdb::Query { families: &[fontdb::Family::Monospace], ..Default::default() };
    db.query(&q).expect("no monospace font")
}
