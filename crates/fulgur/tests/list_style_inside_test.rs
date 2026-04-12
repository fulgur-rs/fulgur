use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;

fn build_engine() -> Engine {
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
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
    assert!(pdf.len() > 500, "PDF should have non-trivial content, got {} bytes", pdf.len());
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
    assert!(pdf.len() > 500, "PDF should have non-trivial content, got {} bytes", pdf.len());
}
