//! Integration test for fulgur-mq5: `@page { size: A4 landscape }` inside
//! an inline `<style>` block must produce a landscape PDF, matching the
//! behavior of the same CSS loaded via `<link rel="stylesheet">`.

use fulgur::Engine;

/// Returns true if any page has a landscape A4 MediaBox.
///
/// Krilla emits `MediaBox [0 0 841.89 595.28]` for landscape A4; portrait is
/// `[0 0 595.28 841.89]`. krilla 0.8 stores page dicts inside object streams,
/// so the MediaBox is no longer visible in the raw bytes — parse with lopdf
/// (which decompresses object streams) and read the page dict's `MediaBox`
/// array directly, checking landscape orientation (width > height) and the
/// A4 long/short edge dimensions.
fn has_landscape_a4_mediabox(pdf: &[u8]) -> bool {
    let doc = lopdf::Document::load_mem(pdf).expect("load PDF for MediaBox check");
    doc.get_pages().values().any(|&page_id| {
        let Ok(dict) = doc.get_object(page_id).and_then(|o| o.as_dict()) else {
            return false;
        };
        let Ok(mb) = dict.get(b"MediaBox").and_then(|o| o.as_array()) else {
            return false;
        };
        if mb.len() != 4 {
            return false;
        }
        // Operands are a mix of Integer (`0`) and Real; `as_float` accepts both.
        let coord = |i: usize| mb[i].as_float().ok();
        let (Some(x0), Some(y0), Some(x1), Some(y1)) = (coord(0), coord(1), coord(2), coord(3))
        else {
            return false;
        };
        let width = x1 - x0;
        let height = y1 - y0;
        // A4 landscape: 841.89 × 595.28 pt, width > height.
        width > height && (width - 841.89).abs() < 1.0 && (height - 595.28).abs() < 1.0
    })
}

#[test]
fn page_size_landscape_from_inline_style_block() {
    let html = r#"<!doctype html><html><head>
        <style>@page { size: A4 landscape; } body { margin: 0; }</style>
    </head><body>test</body></html>"#;

    let engine = Engine::builder().build();
    let pdf = engine.render(html).expect("render");
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
    let pdf = engine.render(&html).expect("render");
    assert!(
        has_landscape_a4_mediabox(&pdf),
        "expected A4 landscape from <link> stylesheet"
    );
}
