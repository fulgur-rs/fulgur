use fulgur::config::PageSize;
use fulgur::engine::Engine;

/// Check whether a PDF byte stream contains a Transparency Group.
fn has_transparency_group(pdf: &[u8]) -> bool {
    pdf.windows(b"/S /Transparency".len())
        .any(|w| w == b"/S /Transparency")
}

#[test]
fn test_opacity_half() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <div style="opacity: 0.5;">
            <p>This text should be semi-transparent</p>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        has_transparency_group(&pdf),
        "opacity: 0.5 should produce a PDF Transparency Group"
    );
}

#[test]
fn test_opacity_zero() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <div style="opacity: 0;">
            <p>This text should be invisible but preserve layout</p>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    // opacity: 0 skips drawing entirely, so no transparency group needed
    assert!(
        !has_transparency_group(&pdf),
        "opacity: 0 should skip drawing, no Transparency Group"
    );
}

#[test]
fn test_visibility_hidden() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <div style="visibility: hidden;">
            <p>This text should be hidden</p>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    // visibility: hidden skips drawing, so no transparency group
    assert!(
        !has_transparency_group(&pdf),
        "visibility: hidden should skip drawing"
    );
}

#[test]
fn test_opacity_on_div_placeholder() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <div style="opacity: 0.7; width: 100px; height: 100px; background-color: blue;">
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        has_transparency_group(&pdf),
        "opacity: 0.7 should produce a Transparency Group"
    );
}

#[test]
fn test_nested_opacity() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <div style="opacity: 0.5;">
            <p>Outer semi-transparent</p>
            <div style="opacity: 0.5;">
                <p>Inner semi-transparent (effective 0.25)</p>
            </div>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        has_transparency_group(&pdf),
        "nested opacity should produce Transparency Groups"
    );
}

#[test]
fn test_opacity_with_background() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <div style="background-color: red; opacity: 0.5; padding: 20px;">
            <p>Semi-transparent red background</p>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        has_transparency_group(&pdf),
        "opacity: 0.5 with background should produce a Transparency Group"
    );
}

#[test]
fn test_visibility_hidden_preserves_layout() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <div style="visibility: hidden; height: 100px; background-color: blue;">
            <p>Hidden but takes space</p>
        </div>
        <div style="background-color: green; padding: 10px;">
            <p>This should appear below the hidden element</p>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 1000);
}

/// Regression test: styled inline root with visibility:hidden must hide text.
/// Exercises the convert.rs path where a ParagraphPageable is wrapped in a
/// BlockPageable for background/border — the inner paragraph must inherit visible.
#[test]
fn test_visibility_hidden_styled_inline_root() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html_hidden = r#"<html><body>
        <p style="visibility: hidden; background-color: red; padding: 10px;">
            Hidden text with styled background
        </p>
    </body></html>"#;
    let pdf_hidden = engine.render_html(html_hidden).unwrap();
    assert!(pdf_hidden.starts_with(b"%PDF"));
    // visibility:hidden should not produce a transparency group
    assert!(
        !has_transparency_group(&pdf_hidden),
        "styled inline root with visibility:hidden should not produce Transparency Group"
    );
}

/// Regression test: list item with visibility:hidden must hide marker and body.
/// Exercises the convert.rs path where ListItemPageable body is built without
/// the node's visibility — the body must inherit visible.
#[test]
fn test_visibility_hidden_list_item() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <ul>
            <li style="visibility: hidden;">Hidden list item</li>
            <li>Visible list item</li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 500);
    // Neither hidden nor visible list items (with default opacity) should produce transparency groups
    assert!(
        !has_transparency_group(&pdf),
        "list items without explicit opacity should not produce Transparency Group"
    );
}
