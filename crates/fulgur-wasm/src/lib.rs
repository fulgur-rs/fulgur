//! WebAssembly bindings for fulgur.
//!
//! This crate exposes two entry points:
//!
//! 1. [`render_html`] (B-1 compatible) — single-shot, no fonts/CSS/images.
//! 2. [`Engine`] (B-2) — builder mirror with `add_font` for registering
//!    `Uint8Array` font payloads (TTF / OTF / WOFF2). WOFF2 is auto-decoded
//!    by `fulgur::AssetBundle::add_font_bytes`; WOFF1 is rejected.
//!
//! Browser-class targets (`wasm32-unknown-unknown`) only. WASI requires
//! a different `getrandom` backend selection (see
//! `crates/fulgur/Cargo.toml`).
//!
//! Tracking: fulgur-iym (strategic v0.7.0), fulgur-7js9 (this step, B-2).

use fulgur::AssetBundle;
use wasm_bindgen::prelude::*;

/// Render the given HTML string to a PDF byte array (B-1 compatible).
///
/// Equivalent to `Engine::new().render(html)`. Kept for back-compat with
/// callers built against the B-1 API; new code should use [`Engine`].
#[wasm_bindgen]
pub fn render_html(html: &str) -> Result<Vec<u8>, JsError> {
    Engine::new().render(html)
}

/// Builder-style engine that mirrors `fulgur::Engine`'s configuration
/// surface for the WASM target.
#[wasm_bindgen]
pub struct Engine {
    assets: AssetBundle,
}

impl Engine {
    fn add_font_impl(&mut self, bytes: Vec<u8>) -> fulgur::Result<()> {
        self.assets.add_font_bytes(bytes)
    }

    fn render_impl(&self, html: &str) -> fulgur::Result<Vec<u8>> {
        fulgur::Engine::builder()
            .assets(self.assets.clone())
            .build()
            .render_html(html)
    }
}

#[wasm_bindgen]
impl Engine {
    /// Create a new engine with no registered assets.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            assets: AssetBundle::new(),
        }
    }

    /// Register a font from raw bytes (TTF / OTF / WOFF2).
    ///
    /// `wasm-bindgen` accepts a `Uint8Array` from JS for the `bytes`
    /// parameter. WOFF2 is decoded to TTF in-process; WOFF1 is rejected.
    /// Family name is extracted from the font's `name` table — no
    /// `family` argument is needed.
    pub fn add_font(&mut self, bytes: Vec<u8>) -> Result<(), JsError> {
        self.add_font_impl(bytes)
            .map_err(|e| JsError::new(&format!("{e}")))
    }

    /// Render the given HTML string to a PDF byte array.
    pub fn render(&self, html: &str) -> Result<Vec<u8>, JsError> {
        self.render_impl(html)
            .map_err(|e| JsError::new(&format!("{e}")))
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noto_sans_regular() -> Vec<u8> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/.fonts/NotoSans-Regular.ttf"
        );
        std::fs::read(path).expect("Noto Sans Regular fixture")
    }

    #[test]
    fn engine_renders_with_added_font_embedded() {
        let mut engine = Engine::new();
        engine
            .add_font(noto_sans_regular())
            .expect("add_font should accept TTF bytes");

        // CSS で font-family を指定しないと parley の system fallback
        // (DejaVuSerif など) が選ばれ、登録した Noto Sans が使われない。
        // NotoSans-Regular.ttf の name table の family 名は "Noto Sans"。
        let html = "<style>body { font-family: 'Noto Sans'; }</style>\
                    <h1>Hello World</h1>";
        let pdf = engine.render(html).expect("render should succeed");
        assert_eq!(&pdf[..4], b"%PDF", "PDF magic missing");

        // フォントが PDF に embed されたことを font dictionary から検証する。
        // krilla は font subset を出力し `<prefix>+<FontName>` の形で
        // `BaseFont` を書き出すので、subset prefix に関係なく "Noto" が
        // 含まれることだけ確認する。
        // 文字列復元検証 (lopdf::extract_text や fulgur::inspect) は
        // krilla の ToUnicode CMap を lopdf 0.40 がパースできず使えなかった。
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
        let mut found_noto = false;
        for obj in doc.objects.values() {
            let lopdf::Object::Dictionary(dict) = obj else {
                continue;
            };
            if let Ok(name_obj) = dict.get(b"BaseFont") {
                if let Ok(name_bytes) = name_obj.as_name() {
                    if let Ok(s) = std::str::from_utf8(name_bytes) {
                        if s.contains("Noto") {
                            found_noto = true;
                            break;
                        }
                    }
                }
            }
        }
        assert!(
            found_noto,
            "Noto font not embedded in rendered PDF (size: {} bytes)",
            pdf.len()
        );
    }

    #[test]
    fn render_html_standalone_still_works() {
        let pdf = render_html(r#"<div style="background:red; width:100px; height:100px"></div>"#)
            .expect("render_html should succeed");
        assert_eq!(&pdf[..4], b"%PDF");
    }
}
