use fulgur_core::config::PageSize;
use fulgur_core::engine::Engine;

#[test]
fn test_render_styled_html() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <div style="background-color: lightblue; padding: 20px; border: 2px solid navy;">
            <h1>Styled Content</h1>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    // Styled PDF should be larger due to graphics commands
    assert!(pdf.len() > 1000);
}

#[test]
fn test_render_colored_background() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <div style="background-color: red; padding: 10px;">
            <p>Red background</p>
        </div>
        <div style="background-color: green; padding: 10px;">
            <p>Green background</p>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
