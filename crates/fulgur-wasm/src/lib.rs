//! WebAssembly bindings for fulgur.
//!
//! This crate exposes two entry points:
//!
//! 1. [`render_html`] (B-1 compatible) — single-shot, no fonts/CSS/images.
//! 2. [`Engine`] — builder mirror with `add_font` (B-2), `add_css` /
//!    `add_image` (B-3a), and `configure` (B-3c) for registering
//!    `Uint8Array` asset payloads and POJO-style configuration. WOFF2 is
//!    auto-decoded by `fulgur::AssetBundle::add_font_bytes`; WOFF1 is
//!    rejected.
//!
//! Browser-class targets (`wasm32-unknown-unknown`) only. WASI requires
//! a different `getrandom` backend selection (see
//! `crates/fulgur/Cargo.toml`).
//!
//! Tracking: fulgur-iym (strategic v0.7.0), fulgur-7js9 (B-2),
//! fulgur-xi6c (B-3a), fulgur-ufda (this step, B-3c).

use fulgur::{AssetBundle, Margin, PageSize};
use serde::Deserialize;
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
    page_size: Option<PageSize>,
    margin: Option<Margin>,
    landscape: Option<bool>,
    title: Option<String>,
    authors: Vec<String>,
    description: Option<String>,
    keywords: Vec<String>,
    creator: Option<String>,
    producer: Option<String>,
    creation_date: Option<String>,
    lang: Option<String>,
    bookmarks: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngineOptions {
    #[serde(default)]
    page_size: Option<PageSizeOption>,
    #[serde(default)]
    margin: Option<MarginOption>,
    #[serde(default)]
    landscape: Option<bool>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    authors: Option<Vec<String>>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    keywords: Option<Vec<String>>,
    #[serde(default)]
    creator: Option<String>,
    #[serde(default)]
    producer: Option<String>,
    #[serde(default)]
    creation_date: Option<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    bookmarks: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PageSizeOption {
    Named(String),
    // `rename_all` on the enum only renames variant names; for untagged
    // enums whose variants are struct-shaped, each variant needs its own
    // `rename_all` to camelCase its fields.
    #[serde(rename_all = "camelCase")]
    Custom {
        width_mm: f32,
        height_mm: f32,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MarginOption {
    Mm {
        mm: f32,
    },
    Pt {
        pt: f32,
    },
    #[serde(rename_all = "camelCase")]
    Full {
        top_mm: f32,
        right_mm: f32,
        bottom_mm: f32,
        left_mm: f32,
    },
}

impl PageSizeOption {
    fn to_page_size(&self) -> Result<PageSize, String> {
        match self {
            Self::Named(name) => match name.to_ascii_lowercase().as_str() {
                "a4" => Ok(PageSize::A4),
                "a3" => Ok(PageSize::A3),
                "letter" => Ok(PageSize::LETTER),
                other => Err(format!("unknown page size: {other}")),
            },
            Self::Custom {
                width_mm,
                height_mm,
            } => Ok(PageSize::custom(*width_mm, *height_mm)),
        }
    }
}

impl MarginOption {
    fn to_margin(&self) -> Margin {
        match self {
            Self::Mm { mm } => Margin::uniform_mm(*mm),
            Self::Pt { pt } => Margin::uniform(*pt),
            Self::Full {
                top_mm,
                right_mm,
                bottom_mm,
                left_mm,
            } => {
                let to_pt = |mm: f32| mm * 72.0 / 25.4;
                Margin {
                    top: to_pt(*top_mm),
                    right: to_pt(*right_mm),
                    bottom: to_pt(*bottom_mm),
                    left: to_pt(*left_mm),
                }
            }
        }
    }
}

impl Engine {
    fn add_font_impl(&mut self, bytes: Vec<u8>) -> fulgur::Result<()> {
        self.assets.add_font_bytes(bytes)
    }

    fn apply_options(&mut self, opts: EngineOptions) -> Result<(), String> {
        if let Some(ps) = opts.page_size {
            self.page_size = Some(ps.to_page_size()?);
        }
        if let Some(m) = opts.margin {
            self.margin = Some(m.to_margin());
        }
        if let Some(l) = opts.landscape {
            self.landscape = Some(l);
        }
        if let Some(t) = opts.title {
            self.title = Some(t);
        }
        if let Some(a) = opts.authors {
            self.authors = a;
        }
        if let Some(d) = opts.description {
            self.description = Some(d);
        }
        if let Some(k) = opts.keywords {
            self.keywords = k;
        }
        if let Some(c) = opts.creator {
            self.creator = Some(c);
        }
        if let Some(p) = opts.producer {
            self.producer = Some(p);
        }
        if let Some(cd) = opts.creation_date {
            self.creation_date = Some(cd);
        }
        if let Some(l) = opts.lang {
            self.lang = Some(l);
        }
        if let Some(b) = opts.bookmarks {
            self.bookmarks = Some(b);
        }
        Ok(())
    }

    fn render_impl(&self, html: &str) -> fulgur::Result<Vec<u8>> {
        let mut builder = fulgur::Engine::builder().assets(self.assets.clone());
        if let Some(s) = self.page_size {
            builder = builder.page_size(s);
        }
        if let Some(m) = self.margin {
            builder = builder.margin(m);
        }
        if let Some(l) = self.landscape {
            builder = builder.landscape(l);
        }
        if let Some(ref t) = self.title {
            builder = builder.title(t.clone());
        }
        if !self.authors.is_empty() {
            builder = builder.authors(self.authors.clone());
        }
        if let Some(ref d) = self.description {
            builder = builder.description(d.clone());
        }
        if !self.keywords.is_empty() {
            builder = builder.keywords(self.keywords.clone());
        }
        if let Some(ref c) = self.creator {
            builder = builder.creator(c.clone());
        }
        if let Some(ref p) = self.producer {
            builder = builder.producer(p.clone());
        }
        if let Some(ref cd) = self.creation_date {
            builder = builder.creation_date(cd.clone());
        }
        if let Some(ref l) = self.lang {
            builder = builder.lang(l.clone());
        }
        if let Some(b) = self.bookmarks {
            builder = builder.bookmarks(b);
        }
        builder.build().render_html(html)
    }
}

#[wasm_bindgen]
impl Engine {
    /// Create a new engine with no registered assets.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            assets: AssetBundle::new(),
            page_size: None,
            margin: None,
            landscape: None,
            title: None,
            authors: Vec::new(),
            description: None,
            keywords: Vec::new(),
            creator: None,
            producer: None,
            creation_date: None,
            lang: None,
            bookmarks: None,
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

    /// Apply configuration options from a JS object (B-3c).
    ///
    /// 受け付けるキーは `pageSize` / `margin` / `landscape` / `title` /
    /// `authors` / `description` / `keywords` / `creator` / `producer` /
    /// `creationDate` / `lang` / `bookmarks`。`pageSize` は `"A4"` /
    /// `"Letter"` / `"A3"` の文字列か `{ widthMm, heightMm }` object。
    /// `margin` は `{ mm }` / `{ pt }` / `{ topMm, rightMm, bottomMm,
    /// leftMm }` のいずれか。未知のキーや不明な page size 名はエラー。
    /// 複数回呼び出すと後勝ちで partial merge される。
    ///
    /// 内部実装は `JSON.stringify` で options を一度文字列化してから
    /// `serde_json::from_str` で deserialize する。`serde-wasm-bindgen`
    /// の `Reflect::get` ベースの deserializer は `deny_unknown_fields`
    /// を honor せず typo を silently 通してしまうため、検証ゲートを
    /// 効かせる経路として JSON 経由を採用している。
    pub fn configure(&mut self, options: JsValue) -> Result<(), JsError> {
        let json = js_sys::JSON::stringify(&options)
            .map_err(|_| JsError::new("invalid options: value cannot be converted to JSON"))?;
        let json_str = json
            .as_string()
            .ok_or_else(|| JsError::new("invalid options: JSON.stringify returned non-string"))?;
        let opts: EngineOptions = serde_json::from_str(&json_str)
            .map_err(|e| JsError::new(&format!("invalid options: {e}")))?;
        self.apply_options(opts).map_err(|e| JsError::new(&e))
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
impl Engine {
    /// Test-only entry point that bypasses `JsValue` (which cannot be
    /// constructed on non-wasm targets). Drives the real
    /// `apply_options` + `EngineOptions` deserialize path so the same
    /// validation (`deny_unknown_fields`, page-size resolution, partial
    /// merge) is exercised under `cargo test`.
    fn configure_json(&mut self, json: serde_json::Value) -> Result<(), String> {
        let opts: EngineOptions =
            serde_json::from_value(json).map_err(|e| format!("invalid options: {e}"))?;
        self.apply_options(opts)
    }
}

// `wasm-pack test --node` runs these against the real wasm-bindgen path
// (`configure(JsValue)` → `JSON.stringify` → `serde_json::from_str`).
// The native `configure_json` tests exercise the same `serde_json`
// deserializer, but these wasm tests additionally cover the
// `JSON.stringify` round-trip (JS value → JSON string) and confirm
// that the full JsValue entry point works end-to-end.
#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn options(json_text: &str) -> JsValue {
        // Use `JSON.parse` to obtain a real JS object so `configure` runs
        // its real production path (`JSON.stringify` + `serde_json::from_str`)
        // against a value that originated outside Rust.
        js_sys::JSON::parse(json_text).expect("parse JSON")
    }

    #[wasm_bindgen_test]
    fn configure_named_page_size_via_jsvalue() {
        let mut engine = Engine::new();
        engine
            .configure(options(r#"{"pageSize":"Letter","landscape":true}"#))
            .expect("configure should succeed");
        let pdf = engine
            .render(r#"<div style="width:10px;height:10px"></div>"#)
            .expect("render should succeed");
        assert_eq!(&pdf[..4], b"%PDF");
        // landscape Letter は w (792) > h (612) になる
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
        let mb = super::tests::find_media_box(&doc).expect("MediaBox");
        assert!(
            (mb.0 - 792.0).abs() < 1.0 && (mb.1 - 612.0).abs() < 1.0,
            "expected Letter landscape via JsValue path, got {mb:?}",
        );
    }

    #[wasm_bindgen_test]
    fn configure_custom_page_size_via_jsvalue() {
        // PageSizeOption::Custom (untagged struct variant) を JsValue 経由で。
        // serde-wasm-bindgen が untagged + struct variant を正しく解決しないと
        // ここで panic する。native の configure_json では拾えない。
        let mut engine = Engine::new();
        engine
            .configure(options(
                r#"{"pageSize":{"widthMm":100.0,"heightMm":200.0}}"#,
            ))
            .expect("configure should succeed for custom pageSize");
        let pdf = engine.render("<p>x</p>").expect("render");
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
        let mb = super::tests::find_media_box(&doc).expect("MediaBox");
        assert!(
            (mb.0 - 283.46).abs() < 1.0 && (mb.1 - 566.93).abs() < 1.0,
            "expected ~283 x 567, got {mb:?}",
        );
    }

    #[wasm_bindgen_test]
    fn configure_margin_variants_via_jsvalue() {
        // MarginOption の 3 variant (mm / pt / Full) すべてが JsValue 経由で
        // 解決できることを確認する。Full は untagged + struct variant なので
        // ここが本命の信号。
        for margin_json in [
            r#"{"margin":{"mm":5.0}}"#,
            r#"{"margin":{"pt":14.17}}"#,
            r#"{"margin":{"topMm":5.0,"rightMm":5.0,"bottomMm":5.0,"leftMm":5.0}}"#,
        ] {
            let mut engine = Engine::new();
            engine
                .configure(options(margin_json))
                .unwrap_or_else(|e| panic!("configure failed for {margin_json}: {e:?}"));
            let pdf = engine.render("<p>x</p>").expect("render");
            assert_eq!(&pdf[..4], b"%PDF", "PDF magic missing for {margin_json}");
        }
    }

    #[wasm_bindgen_test]
    fn configure_metadata_via_jsvalue() {
        let mut engine = Engine::new();
        engine
            .configure(options(
                r#"{"title":"WT Title","authors":["Alice"],"lang":"ja"}"#,
            ))
            .expect("configure should succeed for metadata");
        let pdf = engine.render("<p>x</p>").expect("render");
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
        let title = super::tests::find_info_string(&doc, b"Title").expect("Title");
        assert!(title.contains("WT Title"), "Title was: {title:?}");
    }

    #[wasm_bindgen_test]
    fn configure_rejects_unknown_field_via_jsvalue() {
        // deny_unknown_fields が JsValue 経由でも有効か確認。
        let mut engine = Engine::new();
        let result = engine.configure(options(r#"{"pageSizeTypo":"A4"}"#));
        assert!(result.is_err(), "unknown field should be rejected");
    }

    #[wasm_bindgen_test]
    fn configure_rejects_unknown_page_size_via_jsvalue() {
        let mut engine = Engine::new();
        let result = engine.configure(options(r#"{"pageSize":"Foo"}"#));
        assert!(result.is_err(), "unknown page size should be rejected");
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

    pub(super) fn find_media_box(doc: &lopdf::Document) -> Option<(f32, f32)> {
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

    pub(super) fn find_info_string(doc: &lopdf::Document, key: &[u8]) -> Option<String> {
        let info_ref = doc.trailer.get(b"Info").ok()?;
        let info = doc.dereference(info_ref).ok()?.1.as_dict().ok()?;
        let raw = info.get(key).ok()?;
        let bytes = raw.as_str().ok()?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    // 注: テストは `configure_json` を使う。`configure` は `JsValue` を受けるが
    // wasm-bindgen の imported function は non-wasm では panic する
    // (`cannot call wasm-bindgen imported functions on non-wasm targets`)。
    // `configure_json` は同じ `EngineOptions` deserialize + `apply_options` を
    // 通すので validation 経路は等価。

    #[test]
    fn configure_applies_landscape_and_page_size() {
        // Letter landscape (792 x 612 pt) を要求し、PDF MediaBox がその寸法に
        // なることを直接検証する。configure を通っていないと A4 portrait
        // (~595 x 842) のまま出てくるので壊れたら検知できる。
        let mut engine = Engine::new();
        engine
            .configure_json(serde_json::json!({
                "pageSize": "Letter",
                "landscape": true,
            }))
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
            .configure_json(serde_json::json!({
                "title": "B3C Test",
                "authors": ["Alice", "Bob"],
            }))
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
            .configure_json(serde_json::json!({
                "pageSize": { "widthMm": 100.0, "heightMm": 200.0 },
            }))
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
        let result = engine.configure_json(serde_json::json!({
            "pageSize": "Foo",
        }));
        assert!(result.is_err(), "unknown page size should be rejected");
    }

    #[test]
    fn configure_rejects_unknown_field() {
        let mut engine = Engine::new();
        let result = engine.configure_json(serde_json::json!({
            "pageSizeTypo": "A4",
        }));
        assert!(result.is_err(), "unknown field should be rejected");
    }

    #[test]
    fn configure_margin_full_changes_content_area() {
        // marginFull が反映されると content area が縮み、
        // 同じ HTML でも default margin との PDF byte が変わる。
        // ここでは出力サイズで「default vs full margin」が変化することを確認し、
        // marginFull / mm / pt の 3 variant を順に通すことで
        // MarginOption の deserialize 経路が壊れていないことを保証する。
        let html = r#"<div style="background:#000; width:100px; height:100px"></div>"#;

        let default_engine = Engine::new();
        let pdf_default = default_engine.render(html).expect("default render");

        let mut full_margin = Engine::new();
        full_margin
            .configure_json(serde_json::json!({
                "margin": { "topMm": 5.0, "rightMm": 5.0, "bottomMm": 5.0, "leftMm": 5.0 },
            }))
            .expect("full margin");
        let pdf_full = full_margin.render(html).expect("full-margin render");
        assert_ne!(pdf_default, pdf_full, "marginFull should affect output");

        let mut uniform_mm = Engine::new();
        uniform_mm
            .configure_json(serde_json::json!({"margin": {"mm": 5.0}}))
            .expect("uniform mm");
        let pdf_mm = uniform_mm.render(html).expect("mm render");
        // 5mm uniform == { topMm:5, ...} なので pdf_full とほぼ同一の content area。
        // 完全 byte 一致は producer/timestamps で揺れる可能性があるので、
        // ここでは「両方とも default と異なる」ことだけ確認する。
        assert_ne!(
            pdf_default, pdf_mm,
            "uniform mm margin should affect output"
        );

        let mut uniform_pt = Engine::new();
        uniform_pt
            .configure_json(serde_json::json!({"margin": {"pt": 14.17}})) // 約 5mm
            .expect("uniform pt");
        let pdf_pt = uniform_pt.render(html).expect("pt render");
        assert_ne!(
            pdf_default, pdf_pt,
            "uniform pt margin should affect output"
        );
    }

    #[test]
    fn configure_all_metadata_round_trip() {
        // 全 string/array メタデータが PDF Info dict に正しく出ることを保証する。
        // 1 個でも writer 側で名前を取り違えていると壊れる。
        let mut engine = Engine::new();
        engine
            .configure_json(serde_json::json!({
                "title": "Round Trip Title",
                "authors": ["Alpha", "Beta"],
                "description": "round trip description",
                "keywords": ["k1", "k2"],
                "creator": "round trip creator",
                "producer": "round trip producer",
                "creationDate": "D:20260425000000Z",
                "lang": "ja",
            }))
            .expect("configure should succeed");
        let pdf = engine.render("<p>x</p>").expect("render");
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");

        for (key, expected) in [
            (b"Title".as_slice(), "Round Trip Title"),
            (b"Author", "Alpha"),
            (b"Subject", "round trip description"),
            (b"Keywords", "k1"),
            (b"Creator", "round trip creator"),
            (b"Producer", "round trip producer"),
        ] {
            let value = find_info_string(&doc, key)
                .unwrap_or_else(|| panic!("Info /{} missing", String::from_utf8_lossy(key)));
            assert!(
                value.contains(expected),
                "/{} should contain {expected:?}, got {value:?}",
                String::from_utf8_lossy(key),
            );
        }
    }

    #[test]
    fn configure_partial_merge_preserves_earlier_values() {
        // 2 回呼んで一部だけ上書き、他のフィールドは前の値が維持されること。
        let mut engine = Engine::new();
        engine
            .configure_json(serde_json::json!({
                "title": "First",
                "landscape": true,
            }))
            .unwrap();
        engine
            .configure_json(serde_json::json!({
                "title": "Second",
            }))
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
