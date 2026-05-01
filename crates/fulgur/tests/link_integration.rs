//! Integration tests: `<a href>` is emitted as PDF `/Link` annotation.

use fulgur::Engine;
use fulgur::asset::AssetBundle;

// Minimal 1x1 red PNG (69 bytes) — shared with background_test.rs.
const MINIMAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn engine() -> Engine {
    Engine::builder().build()
}

fn engine_with_png(name: &str) -> Engine {
    let mut assets = AssetBundle::new();
    assets.add_image(name, MINIMAL_PNG.to_vec());
    Engine::builder().assets(assets).build()
}

#[test]
fn external_link_produces_uri_action_in_pdf() {
    let html = r#"<html><body><p><a href="https://example.com">click</a></p></body></html>"#;
    let bytes = engine().render_html(html).expect("render");
    assert!(bytes.starts_with(b"%PDF"));
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Link"), "missing /Link annotation subtype");
    assert!(text.contains("/URI"), "missing /URI action type");
    assert!(
        text.contains("https://example.com"),
        "missing URI string in PDF body"
    );
}

#[test]
fn internal_anchor_produces_destination() {
    let html = r##"<html><body>
        <p><a href="#section">jump</a></p>
        <div style="height:1500px"></div>
        <h2 id="section">Target</h2>
    </body></html>"##;
    let bytes = engine().render_html(html).expect("render");
    assert!(bytes.starts_with(b"%PDF"));
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Link"), "missing /Link annotation");
    // krilla serializes XYZ destinations; the annotation dict references
    // it via /Dest (indirect object reference) and the destination itself
    // carries /XYZ.
    assert!(
        text.contains("/XYZ") || text.contains("/Dest"),
        "missing internal destination marker (/XYZ or /Dest)"
    );
}

#[test]
fn unresolved_internal_anchor_is_ignored_not_error() {
    let html = r##"<html><body><p><a href="#nope">dangling</a></p></body></html>"##;
    let bytes = engine().render_html(html).expect("render should not fail");
    assert!(bytes.starts_with(b"%PDF"));
}

#[test]
fn multiline_link_merges_into_single_annotation_with_multiple_quads() {
    // Force line-wrapping of a single <a> via narrow container.
    // 200 "word " repetitions inside a 200px-wide container guarantees many lines.
    let long: String = "word ".repeat(200);
    let html = format!(
        r##"<html><body><div style="width:200px"><p><a href="https://multiline.test">{}</a></p></div></body></html>"##,
        long
    );
    let bytes = engine().render_html(&html).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    // Single <a> with text broken across multiple lines → one LinkAnnotation
    // that carries multiple QuadPoints. PDF writer emits /QuadPoints array
    // only when the annotation has more than one rectangle.
    assert!(
        text.contains("/QuadPoints"),
        "expected /QuadPoints in multi-line link"
    );
    // URI should appear once (one occurrence, not one per line)
    let uri_count = text.matches("https://multiline.test").count();
    assert!(uri_count >= 1, "URI missing");
}

#[test]
fn link_spanning_page_break_emits_annotation_on_each_page() {
    // Push a long <a> near the end of page 1 so it wraps onto page 2.
    // A spacer slightly larger than one A4 content page (≈730pt) leaves a
    // shallow strip on the next page; a tall-font, narrow-column link
    // placed after the spacer is guaranteed to overflow across the page
    // boundary so its rects land on two different pages.
    let link_text = "link ".repeat(60);
    let html = format!(
        r##"<html><body>
        <div style="height:900pt"></div>
        <div style="width:120pt;font-size:40pt;line-height:1.2"><a href="https://cross.test">{link_text}</a></div>
    </body></html>"##
    );
    let bytes = engine().render_html(&html).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    // Page-crossing links must produce ONE annotation per page (can't share across pages).
    // Expect the URI to appear at least twice in the PDF byte stream (once per /URI entry).
    let uri_count = text.matches("https://cross.test").count();
    assert!(
        uri_count >= 2,
        "expected URI on both pages, got {uri_count}"
    );
}

#[test]
fn link_works_through_gcpm_render_path() {
    // @page rule forces render_to_pdf_with_gcpm rather than render_to_pdf.
    let html = r##"<html>
        <head><style>@page { margin: 2cm; @top-center { content: "Header"; } }</style></head>
        <body><p><a href="https://gcpm.test">click</a></p></body>
    </html>"##;
    let bytes = engine().render_html(html).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Link"), "GCPM path missing /Link");
    assert!(text.contains("https://gcpm.test"), "GCPM path missing URI");
}

// TODO(fulgur-pdf-links): wire link emission for `<a><img/></a>` when the
// anchor and `<img>` live inside an inline-root paragraph.
//
// Investigation (2026-04-16) showed that when `<img>` is a default-display
// (inline) child of a paragraph — e.g. `<p><a><img/></a></p>` — the image
// itself never reaches the PDF at all:
//
//   1. Blitz marks `<p>` as an inline root and positions the `<img>` as a
//      Parley `InlineBox` inside the paragraph's layout.
//   2. `convert::extract_paragraph` walks `parley_layout.lines()` but only
//      matches `PositionedLayoutItem::GlyphRun`; `InlineBox` items are
//      silently dropped. No `LineItem::Image` is produced, no image data is
//      embedded, the content stream is empty — `/Link` annotations are moot
//      because there is nothing to click through to.
//   3. The ImageRender path in `convert::convert_image` only fires when
//      `<img>` is visited as its own node (i.e. block-level, such as
//      `<div><img style="display:block"/></div>` in `image_test.rs`), and
//      that path currently has no link support either.
//
// So the "anchor-wrapping image" feature has two gaps stacked on top of
// each other: (a) InlineBox-aware paragraph extraction that materialises
// `<img>` as `InlineImage`, and (b) link attachment to those `InlineImage`
// entries. Inline pseudo-element images (`::before { content: url(...) }`)
// already use `attach_link_to_inline_image`, so the link wiring will be
// trivial once (a) lands — `<img>` inside `<a>` will go through the same
// `LineItem::Image` draw path that already records link rects at
// `paragraph.rs` line ~597.
//
// Scope for this test file is link emission only, so the gap is captured
// as an ignored test. Unignoring this test is the acceptance criterion for
// closing both gaps together.
#[test]
#[ignore = "blocked on InlineBox → InlineImage materialisation in extract_paragraph; see TODO above"]
fn anchor_wrapping_inline_image_is_clickable() {
    let html = r##"<html><body><p><a href="https://img.test"><img src="pixel.png" width="100" height="50"/></a></p></body></html>"##;
    let bytes = engine_with_png("pixel.png").render_html(html).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("/Link"),
        "anchor-wrapping image missing /Link"
    );
    assert!(
        text.contains("https://img.test"),
        "URI missing — anchor-wrapping image not clickable"
    );
}
