//! Integration test for fulgur-mq5: `@page { size: A4 landscape }` inside
//! an inline `<style>` block must produce a landscape PDF, matching the
//! behavior of the same CSS loaded via `<link rel="stylesheet">`.

use fulgur::Engine;

/// Returns true if the PDF bytes contain a landscape A4 MediaBox.
///
/// Krilla emits `/MediaBox [0 0 841.89 595.28]` for landscape A4; portrait
/// is `/MediaBox [0 0 595.28 841.89]`. PDF bodies contain binary streams
/// that invalidate `std::str::from_utf8`, so scan the raw byte slice for
/// the ASCII landscape-width signature.
fn has_landscape_a4_mediabox(pdf: &[u8]) -> bool {
    let needle: &[u8] = b"/MediaBox [0 0 841";
    pdf.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn page_size_landscape_from_inline_style_block() {
    let html = r#"<!doctype html><html><head>
        <style>@page { size: A4 landscape; } body { margin: 0; }</style>
    </head><body>test</body></html>"#;

    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        has_landscape_a4_mediabox(&pdf),
        "expected A4 landscape (841 × 595) from inline <style>"
    );
}

#[test]
fn page_size_landscape_from_link_stylesheet() {
    // Control: the same CSS via `<link>` already works — guards against
    // accidentally breaking it while fixing the inline case.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("page.css"),
        "@page { size: A4 landscape; } body { margin: 0; }",
    )
    .expect("css write");
    let html_path = dir.path().join("index.html");
    std::fs::write(
        &html_path,
        r#"<!doctype html><html><head>
            <link rel="stylesheet" href="page.css">
        </head><body>test</body></html>"#,
    )
    .expect("html write");
    let html = std::fs::read_to_string(&html_path).expect("html read");

    let engine = Engine::builder().base_path(dir.path()).build();
    let pdf = engine.render_html(&html).expect("render");
    assert!(
        has_landscape_a4_mediabox(&pdf),
        "expected A4 landscape from <link> stylesheet"
    );
}
