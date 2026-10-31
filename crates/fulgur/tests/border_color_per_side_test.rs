//! Per-side `border-*-color` regression tests (issue #906): each side must be
//! painted in its own color instead of falling back to the top side's color.

use fulgur::Engine;

/// Decoded content stream of page 1.
fn page_content(pdf: &[u8]) -> String {
    let doc = lopdf::Document::load_mem(pdf).expect("valid PDF");
    let page_id = *doc.get_pages().get(&1).expect("page 1");
    let bytes = doc.get_page_content(page_id);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn render(body_style: &str) -> String {
    let html = format!(
        r#"<!DOCTYPE html><html><head><style>body{{margin:0}}</style></head>
<body><div style="width:100pt;height:40pt;{body_style}"></div></body></html>"#
    );
    let pdf = Engine::builder()
        .build()
        .render(&html)
        .expect("render per-side border");
    page_content(&pdf)
}

#[test]
fn border_bottom_only_uses_its_own_color() {
    let ops = render("color:#000;border-bottom:3pt solid #ff0000");
    assert!(ops.contains("1 0 0 RG"), "bottom border must be red: {ops}");
}

#[test]
fn border_color_shorthand_with_distinct_sides() {
    let ops =
        render("border-width:2pt;border-style:solid;border-color:#ff0000 #00ff00 #0000ff #ffff00");
    for stroke in ["1 0 0 RG", "0 1 0 RG", "0 0 1 RG", "1 1 0 RG"] {
        assert!(ops.contains(stroke), "missing {stroke}: {ops}");
    }
}

#[test]
fn uniform_border_color_still_single_stroke() {
    let ops = render("border:2pt solid #ff0000");
    assert!(
        ops.contains("1 0 0 RG"),
        "uniform border must be red: {ops}"
    );
}

#[test]
fn rounded_border_with_distinct_colors_does_not_panic() {
    let ops =
        render("border:2pt solid;border-radius:6pt;border-color:#ff0000 #00ff00 #0000ff #ffff00");
    assert!(ops.contains("RG"), "{ops}");
}
