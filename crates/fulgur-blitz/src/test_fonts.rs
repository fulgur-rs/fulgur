//! Font fixtures shared by this crate's unit tests.
//!
//! The fixture lives under this crate's own `tests/fixtures/` so the tests
//! still run from the packaged `.crate` archive, which does not include
//! sibling workspace directories.

use std::sync::{Arc, OnceLock};

/// NotoSans-Regular as the TTF bytes that `AssetBundle::fonts` stores after
/// `add_font_bytes` decodes the bundled WOFF2 fixture.
pub(crate) fn noto_sans_regular_ttf() -> Arc<Vec<u8>> {
    static TTF: OnceLock<Arc<Vec<u8>>> = OnceLock::new();
    Arc::clone(TTF.get_or_init(|| {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fonts/NotoSans-Regular.woff2");
        let woff2 = std::fs::read(&fixture).expect("NotoSans-Regular.woff2 missing");
        let mut bundle = crate::asset::AssetBundle::new();
        bundle.add_font_bytes(woff2).expect("WOFF2 decode failed");
        Arc::clone(&bundle.fonts[0])
    }))
}
