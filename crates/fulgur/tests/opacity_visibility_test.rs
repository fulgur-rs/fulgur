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
    assert!(has_transparency_group(&pdf), "opacity: 0.5 should produce a PDF Transparency Group");
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
    assert!(!has_transparency_group(&pdf), "opacity: 0 should skip drawing, no Transparency Group");
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
    assert!(!has_transparency_group(&pdf), "visibility: hidden should skip drawing");
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
    assert!(has_transparency_group(&pdf), "opacity: 0.7 should produce a Transparency Group");
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
    assert!(has_transparency_group(&pdf), "nested opacity should produce Transparency Groups");
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
    assert!(has_transparency_group(&pdf), "opacity: 0.5 with background should produce a Transparency Group");
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
