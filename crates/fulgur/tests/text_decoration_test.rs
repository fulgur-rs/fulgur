use fulgur::config::PageSize;
use fulgur::engine::Engine;

fn make_engine() -> Engine {
    Engine::builder().page_size(PageSize::A4).build()
}

#[test]
fn text_decoration_underline_renders() {
    let engine = make_engine();
    let html = r#"<p style="text-decoration: underline">underlined text</p>"#;
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(!pdf.is_empty(), "PDF should not be empty");
}

#[test]
fn text_decoration_line_through_renders() {
    let engine = make_engine();
    let html = r#"<p style="text-decoration: line-through">struck text</p>"#;
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(!pdf.is_empty(), "PDF should not be empty");
}

#[test]
fn text_decoration_overline_renders() {
    let engine = make_engine();
    let html = r#"<p style="text-decoration: overline">overlined text</p>"#;
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(!pdf.is_empty(), "PDF should not be empty");
}

#[test]
fn text_decoration_combined_renders() {
    let engine = make_engine();
    let html = r#"<p style="text-decoration: underline line-through">both</p>"#;
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(!pdf.is_empty(), "PDF should not be empty");
}

#[test]
fn text_decoration_color_renders() {
    let engine = make_engine();
    let html = r#"<p style="text-decoration: underline; text-decoration-color: red">colored underline</p>"#;
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(!pdf.is_empty(), "PDF should not be empty");
}

#[test]
fn text_decoration_styles_render() {
    for style in &["solid", "dashed", "dotted", "double", "wavy"] {
        let engine = make_engine();
        let html = format!(
            r#"<p style="text-decoration: underline; text-decoration-style: {style}">styled</p>"#
        );
        let pdf = engine
            .render_html(&html)
            .unwrap_or_else(|_| panic!("render with style={style} should succeed"));
        assert!(
            !pdf.is_empty(),
            "PDF with style={style} should not be empty"
        );
    }
}
