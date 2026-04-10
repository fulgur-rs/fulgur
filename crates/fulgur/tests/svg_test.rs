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

#[test]
fn test_svg_with_border_and_padding_renders() {
    let engine = build_engine();
    let styled_html = r#"<html><body>
        <svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"
             style="display:block; border: 2px solid black; padding: 10px; background: #eee">
            <rect width="100" height="50" fill="blue"/>
        </svg>
    </body></html>"#;
    let plain_html = r#"<html><body>
        <svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"
             style="display:block">
            <rect width="100" height="50" fill="blue"/>
        </svg>
    </body></html>"#;

    let styled_pdf = engine.render_html(styled_html).unwrap();
    let plain_pdf = engine.render_html(plain_html).unwrap();

    assert!(styled_pdf.starts_with(b"%PDF"));
    assert!(plain_pdf.starts_with(b"%PDF"));

    // The styled SVG must produce a larger PDF than the plain one because
    // the BlockPageable wrapping branch adds border strokes and a background
    // fill on top of the same <rect> content. If the has_visual_style()
    // branch is ever broken, this assertion will catch it.
    assert!(
        styled_pdf.len() > plain_pdf.len(),
        "styled SVG PDF ({} bytes) must exceed plain SVG PDF ({} bytes) \
         because border/padding/background add content",
        styled_pdf.len(),
        plain_pdf.len()
    );
}

#[test]
fn test_multiple_svgs_on_same_page() {
    let engine = build_engine();
    let html = r#"<html><body>
        <svg xmlns="http://www.w3.org/2000/svg" width="50" height="50" style="display:block">
            <circle cx="25" cy="25" r="20" fill="red"/>
        </svg>
        <svg xmlns="http://www.w3.org/2000/svg" width="50" height="50" style="display:block">
            <circle cx="25" cy="25" r="20" fill="blue"/>
        </svg>
    </body></html>"#;

    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));

    // Two SVGs should produce a larger PDF than one
    let single_html = r#"<html><body>
        <svg xmlns="http://www.w3.org/2000/svg" width="50" height="50" style="display:block">
            <circle cx="25" cy="25" r="20" fill="red"/>
        </svg>
    </body></html>"#;
    let single_pdf = engine.render_html(single_html).unwrap();
    assert!(
        pdf.len() > single_pdf.len(),
        "PDF with 2 SVGs ({} bytes) should exceed PDF with 1 SVG ({} bytes)",
        pdf.len(),
        single_pdf.len()
    );
}

#[test]
fn test_svg_does_not_split_across_pages() {
    let engine = build_engine();
    // A4 minus uniform 72pt margin ≈ 698pt content height.
    // Place a filler consuming ~500pt, then a 300pt-tall SVG that
    // cannot fit in the remaining ~198pt — must move to page 2.
    let html = r#"<html><body>
        <div style="height: 500pt"></div>
        <svg xmlns="http://www.w3.org/2000/svg" width="200" height="300" style="display:block">
            <rect width="200" height="300" fill="green"/>
        </svg>
    </body></html>"#;

    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    // PDF should contain at least 2 pages.
    assert!(
        pdf.windows(8).any(|w| w == b"/Count 2") || pdf.windows(8).any(|w| w == b"/Count 3"),
        "expected /Count 2 or /Count 3 in Pages tree, indicating the SVG moved to a new page"
    );
}

#[test]
fn test_svg_with_parent_opacity() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="opacity: 0.5">
            <svg xmlns="http://www.w3.org/2000/svg" width="100" height="50" style="display:block">
                <rect width="100" height="50" fill="red"/>
            </svg>
        </div>
    </body></html>"#;

    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    // Smoke test: opacity propagation should not panic and should emit a valid PDF.
    // Byte-level opacity detection is fragile; the main goal here is to exercise
    // the draw_with_opacity codepath via a parent CSS opacity.
}

#[test]
fn test_svg_with_visibility_hidden_is_skipped() {
    let engine = build_engine();
    let visible_html = r#"<html><body>
        <svg xmlns="http://www.w3.org/2000/svg" width="100" height="50" style="display:block">
            <rect width="100" height="50" fill="red"/>
        </svg>
    </body></html>"#;
    let hidden_html = r#"<html><body>
        <svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"
             style="display:block; visibility: hidden">
            <rect width="100" height="50" fill="red"/>
        </svg>
    </body></html>"#;

    let visible_pdf = engine.render_html(visible_html).unwrap();
    let hidden_pdf = engine.render_html(hidden_html).unwrap();
    assert!(visible_pdf.starts_with(b"%PDF"));
    assert!(hidden_pdf.starts_with(b"%PDF"));

    // Hidden SVG should not emit path content, so the PDF should be
    // at most the same size (typically smaller).
    assert!(
        hidden_pdf.len() <= visible_pdf.len(),
        "hidden SVG PDF ({} bytes) must not be larger than visible SVG PDF ({} bytes)",
        hidden_pdf.len(),
        visible_pdf.len()
    );
}
