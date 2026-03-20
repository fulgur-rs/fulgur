use fulgur_core::config::{Margin, PageSize};
use fulgur_core::engine::Engine;

fn make_engine() -> Engine {
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
}

#[test]
fn test_unordered_list_renders() {
    let engine = make_engine();
    let html = r#"
        <html><body>
            <ul>
                <li>Item one</li>
                <li>Item two</li>
                <li>Item three</li>
            </ul>
        </body></html>
    "#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 100);
}

#[test]
fn test_ordered_list_renders() {
    let engine = make_engine();
    let html = r#"
        <html><body>
            <ol>
                <li>First</li>
                <li>Second</li>
                <li>Third</li>
            </ol>
        </body></html>
    "#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 100);
}

#[test]
fn test_nested_list_renders() {
    let engine = make_engine();
    let html = r#"
        <html><body>
            <ul>
                <li>Parent item
                    <ul>
                        <li>Nested item</li>
                    </ul>
                </li>
            </ul>
        </body></html>
    "#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 100);
}

#[test]
fn test_mixed_list_styles_render() {
    let engine = make_engine();
    let html = r#"
        <html><body>
            <ol style="list-style-type: lower-alpha">
                <li>Alpha item</li>
                <li>Beta item</li>
            </ol>
            <ul style="list-style-type: square">
                <li>Square item</li>
            </ul>
        </body></html>
    "#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 100);
}
