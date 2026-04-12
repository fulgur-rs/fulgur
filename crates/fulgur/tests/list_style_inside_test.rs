use fulgur::asset::AssetBundle;
use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;

// Minimal 1x1 red PNG (69 bytes) — copied from list_style_image_test.rs.
const MINIMAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn build_engine() -> Engine {
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
}

fn pdf_contains(pdf: &[u8], needle: &[u8]) -> bool {
    pdf.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn test_list_style_position_inside_text_marker_renders() {
    let engine = build_engine();
    let html = r#"<html><body>
        <ul style="list-style-position: inside">
            <li>Item one</li>
            <li>Item two</li>
            <li>Item three</li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        pdf.len() > 500,
        "PDF should have non-trivial content, got {} bytes",
        pdf.len()
    );
}

#[test]
fn test_list_style_position_inside_ordered_list() {
    let engine = build_engine();
    let html = r#"<html><body>
        <ol style="list-style-position: inside">
            <li>First item</li>
            <li>Second item</li>
            <li>Third item</li>
        </ol>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        pdf.len() > 500,
        "PDF should have non-trivial content, got {} bytes",
        pdf.len()
    );
}

#[test]
fn test_list_style_position_inside_image_marker() {
    let mut assets = AssetBundle::new();
    assets.add_image("bullet.png", MINIMAL_PNG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();
    let html = r#"<html><body>
        <ul style="list-style-position: inside; list-style-image: url(bullet.png)">
            <li>Image inside</li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"), "output should be a valid PDF");
    assert!(
        pdf_contains(&pdf, b"/Subtype /Image") || pdf_contains(&pdf, b"/Subtype/Image"),
        "PDF should embed an Image XObject for the inside-position image marker"
    );
}

#[test]
fn test_outside_markers_still_work_after_inside_changes() {
    let engine = build_engine();
    let html = r#"<html><body>
        <ul style="list-style-position: outside">
            <li>Outside one</li>
            <li>Outside two</li>
            <li>Outside three</li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        pdf.len() > 500,
        "PDF should have non-trivial content, got {} bytes",
        pdf.len()
    );
}

// Known limitation: when <li> contains only block children (not an inline
// root) or is empty, Blitz does not inject the marker into any inline
// layout, so the marker is not rendered. This matches upstream Blitz
// behavior — Blitz's own layout construction in
// blitz-dom/src/layout/construct.rs only injects inside markers in
// `build_inline_layout`, which is not called for non-inline-root elements.
//
// Tracked as a follow-up: see CHANGELOG and the `list-style-inside`
// limitations note. These tests guard that fulgur still produces a valid
// PDF in these edge cases (no panic, no crash) even though the marker
// is absent.
#[test]
fn test_inside_marker_with_block_child_does_not_crash() {
    let engine = build_engine();
    let html = r#"<html><body>
        <ul style="list-style-position: inside">
            <li><p>Nested paragraph</p></li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_inside_empty_li_does_not_crash() {
    let engine = build_engine();
    let html = r#"<html><body>
        <ul style="list-style-position: inside">
            <li></li>
            <li>Not empty</li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_inside_and_outside_in_same_document() {
    let engine = build_engine();
    let html = r#"<html><body>
        <ul style="list-style-position: outside">
            <li>Outside item A</li>
            <li>Outside item B</li>
        </ul>
        <ul style="list-style-position: inside">
            <li>Inside item A</li>
            <li>Inside item B</li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        pdf.len() > 500,
        "PDF should have non-trivial content, got {} bytes",
        pdf.len()
    );
}
