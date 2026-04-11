use fulgur::asset::AssetBundle;
use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;

// Minimal 1x1 red PNG (69 bytes) — reused from background_test.rs pattern.
const MINIMAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn build_engine() -> Engine {
    let mut assets = AssetBundle::new();
    assets.add_image("bullet.png", MINIMAL_PNG.to_vec());
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build()
}

fn pdf_contains(pdf: &[u8], needle: &[u8]) -> bool {
    pdf.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn test_list_style_image_png_embeds_xobject() {
    let engine = build_engine();
    let html = r#"<html><body>
        <ul style="list-style: disc url(bullet.png)">
            <li>Item one</li>
            <li>Item two</li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    // A rendered raster image marker causes krilla to emit an Image XObject.
    assert!(
        pdf_contains(&pdf, b"/Subtype /Image") || pdf_contains(&pdf, b"/Subtype/Image"),
        "PDF should embed an Image XObject when list-style-image is a PNG"
    );

    let text_only = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
        .render_html(r#"<html><body><ul><li>Item one</li><li>Item two</li></ul></body></html>"#)
        .unwrap();
    assert!(
        !pdf_contains(&text_only, b"/Subtype /Image")
            && !pdf_contains(&text_only, b"/Subtype/Image"),
        "text-only control should not embed Image XObject"
    );
}

#[test]
fn test_list_style_image_unresolved_url_falls_back_to_text() {
    let engine = build_engine(); // bundle has bullet.png but not missing.png
    let html = r#"<html><body>
        <ul style="list-style: disc url(missing.png)">
            <li>Item</li>
        </ul>
    </body></html>"#;
    // Must not panic — falls through to text marker silently.
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

const MINIMAL_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red"/></svg>"#;

#[test]
fn test_list_style_image_svg_renders() {
    let mut assets = AssetBundle::new();
    assets.add_image("bullet.svg", MINIMAL_SVG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();
    let html = r#"<html><body>
        <ul style="list-style: disc url(bullet.svg)">
            <li>SVG bullet item</li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));

    let text_only = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
        .render_html(r#"<html><body><ul><li>SVG bullet item</li></ul></body></html>"#)
        .unwrap();
    // The SVG marker replaces the text bullet glyph with a vector path draw,
    // so the rendered PDF must differ from the text-only control.
    assert_ne!(
        pdf, text_only,
        "PDF with SVG marker should differ from text-only output"
    );
}
