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

/// Extract the `/MediaBox [0 0 W H]` numbers from the PDF bytes.
///
/// The body carries binary streams, so scan the raw slice rather than
/// going through `str::from_utf8`.
fn media_box(pdf: &[u8]) -> Option<(f32, f32)> {
    let needle: &[u8] = b"/MediaBox [0 0 ";
    let start = pdf.windows(needle.len()).position(|w| w == needle)? + needle.len();
    let rest = &pdf[start..];
    let end = rest.iter().position(|&b| b == b']')?;
    let text = std::str::from_utf8(&rest[..end]).ok()?;
    let mut parts = text.split_whitespace();
    let w: f32 = parts.next()?.parse().ok()?;
    let h: f32 = parts.next()?.parse().ok()?;
    Some((w, h))
}

fn render_with_page_size(keyword: &str) -> (f32, f32) {
    let html = format!(
        r#"<!doctype html><html><head>
            <style>@page {{ size: {keyword}; }} body {{ margin: 0; }}</style>
        </head><body>test</body></html>"#
    );
    let pdf = Engine::builder().build().render(&html).expect("render");
    media_box(&pdf).expect("MediaBox in output")
}

/// fulgur-5oav: every keyword in CSS Paged Media Level 3 §4.1.1 must resolve
/// to its own sheet. `A5` in particular used to fall through to A4 with no
/// diagnostic, so a document laid out for A5 printed on A4 silently.
#[test]
fn css_page_size_keywords_resolve_to_distinct_sheets() {
    let mm = |v: f32| v * 72.0 / 25.4;
    let inch = |v: f32| v * 72.0;

    // (keyword, expected width pt, expected height pt)
    let cases: [(&str, f32, f32); 10] = [
        ("A3", 841.89, 1190.55),
        ("A4", 595.28, 841.89),
        ("A5", mm(148.0), mm(210.0)),
        ("B4", mm(250.0), mm(353.0)),
        ("B5", mm(176.0), mm(250.0)),
        ("JIS-B4", mm(257.0), mm(364.0)),
        ("JIS-B5", mm(182.0), mm(257.0)),
        ("letter", 612.0, 792.0),
        ("legal", inch(8.5), inch(14.0)),
        ("ledger", inch(11.0), inch(17.0)),
    ];

    for (keyword, want_w, want_h) in cases {
        let (got_w, got_h) = render_with_page_size(keyword);
        assert!(
            (got_w - want_w).abs() < 0.05 && (got_h - want_h).abs() < 0.05,
            "size: {keyword} — expected {want_w:.2} x {want_h:.2} pt, got {got_w:.2} x {got_h:.2}"
        );
    }
}

/// A5 is the one that motivated the fix; assert separately that it is not
/// simply A4 under another name.
#[test]
fn css_page_size_a5_is_not_a4() {
    assert_ne!(render_with_page_size("A5"), render_with_page_size("A4"));
}

/// Keywords stay case-insensitive, and the orientation suffix still applies
/// to the newly recognised sheets.
#[test]
fn css_page_size_keyword_is_case_insensitive_and_takes_orientation() {
    let (portrait_w, portrait_h) = render_with_page_size("a5");
    let (landscape_w, landscape_h) = render_with_page_size("A5 landscape");
    assert!(portrait_w < portrait_h, "A5 portrait should be taller");
    assert!((landscape_w - portrait_h).abs() < 0.05);
    assert!((landscape_h - portrait_w).abs() < 0.05);
}

/// An unknown keyword still has to produce a page, so A4 remains the
/// fallback — but it is now reported rather than silent (the warning goes
/// through `log`, which this test does not capture; the behavioural half is
/// that the fallback itself is unchanged).
#[test]
fn css_page_size_unknown_keyword_still_falls_back_to_a4() {
    assert_eq!(render_with_page_size("banana"), render_with_page_size("A4"));
}
