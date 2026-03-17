use fulgur_core::config::{PageSize, Margin};
use fulgur_core::engine::Engine;

#[test]
fn test_render_simple_html() {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build();

    let html = "<html><body><h1>Hello World</h1><p>This is a test.</p></body></html>";
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 100);
}

#[test]
fn test_convert_html_convenience() {
    let pdf = fulgur_core::convert_html("<h1>Test</h1>").unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
