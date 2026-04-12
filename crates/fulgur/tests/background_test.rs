use fulgur::asset::AssetBundle;
use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;

// Minimal 1x1 red PNG (69 bytes)
const MINIMAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn build_engine() -> Engine {
    let mut assets = AssetBundle::new();
    assets.add_image("bg.png", MINIMAL_PNG.to_vec());
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build()
}

#[test]
fn test_background_image_renders_to_pdf() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg.png)">Hello</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));

    // PDF with background image should be larger than without (verifies image is embedded)
    let pdf_no_bg = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
        .render_html(
            r#"<html><body><div style="width:200px;height:200px">Hello</div></body></html>"#,
        )
        .unwrap();
    assert!(
        pdf.len() > pdf_no_bg.len(),
        "PDF with background-image ({} bytes) should be larger than without ({} bytes)",
        pdf.len(),
        pdf_no_bg.len()
    );
}

#[test]
fn test_background_no_repeat() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg.png);background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_size_cover() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg.png);background-size:cover;background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_position_center() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg.png);background-position:center;background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_multiple_layers() {
    let mut assets = AssetBundle::new();
    assets.add_image("bg1.png", MINIMAL_PNG.to_vec());
    assets.add_image("bg2.png", MINIMAL_PNG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg1.png),url(bg2.png);background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_clip_padding_box() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;padding:20px;border:5px solid black;background-image:url(bg.png);background-clip:padding-box;background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_image_svg_renders() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="50"><circle cx="25" cy="25" r="20" fill="green"/></svg>"#;
    let mut assets = AssetBundle::new();
    assets.add_image("circle.svg", svg.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();

    let html = r#"<html><body>
        <div style="width:100px;height:100px;background-image:url(circle.svg);background-size:contain;background-repeat:no-repeat"></div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"), "output should be a PDF");

    // PDF with SVG background should be larger than without (verifies SVG is embedded)
    let pdf_no_bg = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
        .render_html(r#"<html><body><div style="width:100px;height:100px"></div></body></html>"#)
        .unwrap();
    assert!(
        pdf.len() > pdf_no_bg.len(),
        "PDF with SVG background-image ({} bytes) should be larger than without ({} bytes)",
        pdf.len(),
        pdf_no_bg.len()
    );
}
