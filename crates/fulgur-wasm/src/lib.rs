//! WebAssembly bindings for fulgur.
//!
//! This crate exposes two entry points:
//!
//! 1. [`render_html`] (B-1 compatible) — single-shot, no fonts/CSS/images.
//! 2. [`Engine`] — builder mirror with `add_font` (B-2), `add_css` /
//!    `add_image` (B-3a) for registering `Uint8Array` payloads. WOFF2 is
//!    auto-decoded by `fulgur::AssetBundle::add_font_bytes`; WOFF1 is rejected.
//!
//! Browser-class targets (`wasm32-unknown-unknown`) only. WASI requires
//! a different `getrandom` backend selection (see
//! `crates/fulgur/Cargo.toml`).
//!
//! Tracking: fulgur-iym (strategic v0.7.0), fulgur-7js9 (B-2),
//! fulgur-xi6c (this step, B-3a).

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

    /// Register a CSS stylesheet (B-3a).
    ///
    /// All registered CSS is concatenated and injected as a single
    /// `<style>` block at render time. Use this for any CSS that the
    /// HTML references via `<link rel="stylesheet">` — those tags are
    /// not resolved in the WASM target (no async NetProvider yet, see
    /// scope 3b in `project_wasm_resource_bridging.md`).
    pub fn add_css(&mut self, css: String) {
        self.assets.add_css(css);
    }

    /// Register an image asset (B-3a).
    ///
    /// `name` is the URL/path key referenced in the HTML — e.g.
    /// `<img src="hero.png">` should be registered with `name = "hero.png"`.
    /// A leading `./` is normalised away so `./hero.png` and `hero.png`
    /// resolve to the same asset.
    /// The supported formats are whatever fulgur's image pipeline accepts
    /// (PNG / JPEG / GIF / etc.); decoding happens at render time.
    pub fn add_image(&mut self, name: String, bytes: Vec<u8>) {
        self.assets.add_image(name, bytes);
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

    fn icon_png() -> Vec<u8> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/image/icon.png");
        std::fs::read(path).expect("icon.png fixture")
    }

    #[test]
    fn engine_renders_image_via_add_image() {
        let mut engine = Engine::new();
        engine.add_image("icon.png".into(), icon_png());

        let html = r#"<img src="icon.png" style="width:50px;height:50px">"#;
        let pdf = engine.render(html).expect("render should succeed");
        assert_eq!(&pdf[..4], b"%PDF");

        // 画像が PDF に embed されたことを XObject Image stream で検証する。
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
        let mut found_image = false;
        for obj in doc.objects.values() {
            if let lopdf::Object::Stream(stream) = obj {
                if let Ok(subtype) = stream.dict.get(b"Subtype") {
                    if matches!(subtype.as_name(), Ok(name) if name == b"Image") {
                        found_image = true;
                        break;
                    }
                }
            }
        }
        assert!(
            found_image,
            "Image XObject not embedded in rendered PDF (size: {} bytes)",
            pdf.len()
        );
    }

    // ----- B-3c (Engine.configure) tests ----------------------------------

    fn find_media_box(doc: &lopdf::Document) -> Option<(f32, f32)> {
        for obj in doc.objects.values() {
            let lopdf::Object::Dictionary(dict) = obj else {
                continue;
            };
            if dict.get(b"Type").and_then(|o| o.as_name()).ok() != Some(b"Page".as_slice()) {
                continue;
            }
            let mb = dict.get(b"MediaBox").ok()?.as_array().ok()?;
            if mb.len() == 4 {
                let w = mb[2].as_float().ok()?;
                let h = mb[3].as_float().ok()?;
                return Some((w, h));
            }
        }
        None
    }

    fn find_info_string(doc: &lopdf::Document, key: &[u8]) -> Option<String> {
        let info_ref = doc.trailer.get(b"Info").ok()?;
        let info = doc.dereference(info_ref).ok()?.1.as_dict().ok()?;
        let raw = info.get(key).ok()?;
        let bytes = raw.as_str().ok()?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    #[test]
    fn configure_applies_landscape_and_page_size() {
        // Letter landscape (792 x 612 pt) を要求し、PDF MediaBox がその寸法に
        // なることを直接検証する。configure を通っていないと A4 portrait
        // (~595 x 842) のまま出てくるので壊れたら検知できる。
        let mut engine = Engine::new();
        engine
            .configure(
                serde_wasm_bindgen::to_value(&serde_json::json!({
                    "pageSize": "Letter",
                    "landscape": true,
                }))
                .unwrap(),
            )
            .expect("configure should succeed");
        let pdf = engine
            .render(r#"<div style="width:10px;height:10px"></div>"#)
            .expect("render should succeed");
        assert_eq!(&pdf[..4], b"%PDF");

        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
        let media_box = find_media_box(&doc).expect("MediaBox missing");
        assert!(
            (media_box.0 - 792.0).abs() < 1.0 && (media_box.1 - 612.0).abs() < 1.0,
            "expected Letter landscape (792 x 612), got {media_box:?}",
        );
    }

    #[test]
    fn configure_applies_metadata() {
        // Info dictionary に title / author が反映されることを検証する。
        let mut engine = Engine::new();
        engine
            .configure(
                serde_wasm_bindgen::to_value(&serde_json::json!({
                    "title": "B3C Test",
                    "authors": ["Alice", "Bob"],
                }))
                .unwrap(),
            )
            .expect("configure should succeed");
        let pdf = engine.render("<p>x</p>").expect("render should succeed");

        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
        let title = find_info_string(&doc, b"Title").expect("Title missing");
        assert!(title.contains("B3C Test"), "Title was: {title:?}");
        let author = find_info_string(&doc, b"Author").expect("Author missing");
        assert!(author.contains("Alice"), "Author was: {author:?}");
    }

    #[test]
    fn configure_custom_page_size_mm() {
        // pageSize に { widthMm, heightMm } object を渡せること。
        let mut engine = Engine::new();
        engine
            .configure(
                serde_wasm_bindgen::to_value(&serde_json::json!({
                    "pageSize": { "widthMm": 100.0, "heightMm": 200.0 },
                }))
                .unwrap(),
            )
            .expect("configure should succeed");
        let pdf = engine.render("<p>x</p>").expect("render");
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
        let media_box = find_media_box(&doc).expect("MediaBox missing");
        // 100mm = 283.46 pt, 200mm = 566.93 pt
        assert!(
            (media_box.0 - 283.46).abs() < 1.0 && (media_box.1 - 566.93).abs() < 1.0,
            "expected ~283 x 567, got {media_box:?}",
        );
    }

    #[test]
    fn configure_rejects_unknown_page_size() {
        let mut engine = Engine::new();
        let result = engine.configure(
            serde_wasm_bindgen::to_value(&serde_json::json!({
                "pageSize": "Foo",
            }))
            .unwrap(),
        );
        assert!(result.is_err(), "unknown page size should be rejected");
    }

    #[test]
    fn configure_rejects_unknown_field() {
        let mut engine = Engine::new();
        let result = engine.configure(
            serde_wasm_bindgen::to_value(&serde_json::json!({
                "pageSizeTypo": "A4",
            }))
            .unwrap(),
        );
        assert!(result.is_err(), "unknown field should be rejected");
    }

    #[test]
    fn configure_partial_merge_preserves_earlier_values() {
        // 2 回呼んで一部だけ上書き、他のフィールドは前の値が維持されること。
        let mut engine = Engine::new();
        engine
            .configure(
                serde_wasm_bindgen::to_value(&serde_json::json!({
                    "title": "First",
                    "landscape": true,
                }))
                .unwrap(),
            )
            .unwrap();
        engine
            .configure(
                serde_wasm_bindgen::to_value(&serde_json::json!({
                    "title": "Second",
                }))
                .unwrap(),
            )
            .unwrap();
        let pdf = engine.render("<p>x</p>").expect("render");
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
        let title = find_info_string(&doc, b"Title").expect("Title missing");
        assert!(title.contains("Second"), "Title was: {title:?}");
        // landscape=true は維持されているはず → A4 landscape は w > h
        let media_box = find_media_box(&doc).expect("MediaBox missing");
        assert!(
            media_box.0 > media_box.1,
            "expected landscape (w > h), got {media_box:?}",
        );
    }

    #[test]
    fn engine_applies_added_css() {
        // CSS で背景色を効かせると div の領域が塗られ、PDF byte が CSS 無し版と差異を持つ。
        // engine が add_css を AssetBundle 経由で <style> に inject していないと、
        // 同じ HTML を render したときに pdf_with_css == pdf_without_css になる。
        let mut engine_with = Engine::new();
        engine_with
            .add_css("div.fulgur-test { background: #ff0000; width: 100px; height: 50px; }".into());
        let pdf_with = engine_with
            .render(r#"<div class="fulgur-test"></div>"#)
            .expect("render with CSS should succeed");

        let engine_without = Engine::new();
        let pdf_without = engine_without
            .render(r#"<div class="fulgur-test"></div>"#)
            .expect("render without CSS should succeed");

        assert_eq!(&pdf_with[..4], b"%PDF");
        assert_ne!(
            pdf_with, pdf_without,
            "add_css should change the rendered output"
        );
        assert!(
            pdf_with.len() > pdf_without.len(),
            "CSS-styled PDF ({} bytes) should be larger than unstyled ({} bytes)",
            pdf_with.len(),
            pdf_without.len(),
        );
    }
}
