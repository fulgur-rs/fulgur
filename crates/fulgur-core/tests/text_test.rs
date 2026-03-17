use fulgur_core::config::{Margin, PageSize};
use fulgur_core::engine::Engine;

#[test]
fn test_render_html_with_text() {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build();

    let html = "<html><body><h1>Hello World</h1><p>This is fulgur.</p></body></html>";
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    // PDF should be larger than empty doc due to font embedding
    assert!(pdf.len() > 1000);
}

#[test]
fn test_render_multiline_text() {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build();

    let html = r#"<html><body>
        <p>Line one of the paragraph.</p>
        <p>Line two of the paragraph.</p>
        <p>Line three of the paragraph.</p>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
