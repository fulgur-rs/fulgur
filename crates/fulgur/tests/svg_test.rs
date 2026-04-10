use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;

fn build_engine() -> Engine {
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
}

#[test]
fn test_inline_svg_renders_to_pdf() {
    let engine = build_engine();
    let html = r#"<html><body>
        <svg xmlns="http://www.w3.org/2000/svg" width="100" height="50" style="display:block">
            <rect width="100" height="50" fill="red"/>
        </svg>
    </body></html>"#;

    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"), "output should be a PDF");
    assert!(!pdf.is_empty());

    // A page that successfully drew the SVG is larger than an empty page:
    let empty_html = r#"<html><body></body></html>"#;
    let empty_pdf = engine.render_html(empty_html).unwrap();
    assert!(
        pdf.len() > empty_pdf.len(),
        "PDF with SVG ({} bytes) must be larger than empty PDF ({} bytes)",
        pdf.len(),
        empty_pdf.len()
    );
}
