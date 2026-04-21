//! Integration: inline-box is actually rendered (non-empty PDF bytes).

use fulgur::engine::Engine;

fn render(html: &str) -> Vec<u8> {
    let engine = Engine::builder().build();
    engine.render_html(html).expect("render ok")
}

#[test]
fn inline_block_with_background_produces_output() {
    // Same inline-block geometry with vs without a visible `background:red`.
    // The Parley layout is identical between the two, so any byte delta comes
    // from the draw() path actually emitting the background rect. Without the
    // InlineBox draw arm wired up, the two PDFs are byte-for-byte identical.
    // Use a larger box + border so the draw ops (fill rect, stroke rect)
    // outweigh Deflate compression of a few bytes of rgb color.
    let plain = render(
        r#"<!DOCTYPE html><html><body><p>hello <span style="display:inline-block;width:200px;height:100px"></span> world</p></body></html>"#,
    );
    let colored = render(
        r#"<!DOCTYPE html><html><body><p>hello <span style="display:inline-block;width:200px;height:100px;background:red;border:4px solid blue"></span> world</p></body></html>"#,
    );
    assert!(
        colored.len() > plain.len() + 50,
        "inline-block `background:red` should add draw ops to the PDF: plain={}, colored={}",
        plain.len(),
        colored.len()
    );
}

#[test]
fn inline_block_inside_anchor_gets_link_rect() {
    let html = r#"<!DOCTYPE html><html><body><p><a href="https://example.com"><span style="display:inline-block;width:40px;height:20px;background:red"></span></a></p></body></html>"#;
    let bytes = render(html);
    // krilla emits link annotations as /Annot objects carrying /Link subtype
    // and /URI destinations. At least one of these markers must appear when
    // an inline-block is wrapped in <a href>.
    let s = String::from_utf8_lossy(&bytes);
    assert!(
        s.contains("/Link") || s.contains("/URI"),
        "expected a link annotation in the PDF for an inline-block inside <a>"
    );
}

#[test]
fn inline_block_inside_anchor_does_not_emit_duplicate_links() {
    // Before lifting `LinkCache` onto `ConvertContext`, the recursive
    // extraction path (`extract_paragraph → convert_inline_box_node →
    // convert_node → extract_paragraph`) allocated a fresh `LinkCache` per
    // paragraph. The same `<a href>` would then spawn two separate
    // `Arc<LinkSpan>` — one for the inline-box rect and one for the inner
    // glyph run — defeating `LinkCollector`'s `Arc::ptr_eq`-based dedup and
    // emitting the URI twice. The unique marker below lets us count the
    // number of times the href is embedded in the PDF.
    let html = r#"<!DOCTYPE html><html><body><p><a href="https://example-unique.invalid"><span style="display:inline-block;width:40px;height:20px;background:red">x</span></a></p></body></html>"#;
    let bytes = render(html);
    let needle = b"example-unique.invalid";
    let count = bytes.windows(needle.len()).filter(|w| *w == needle).count();
    assert_eq!(
        count, 1,
        "expected the unique anchor href to appear exactly once in the PDF, got {count}"
    );
}

#[test]
fn hidden_inline_block_anchor_does_not_emit_link_rect() {
    // An inline-block with `visibility: hidden` should not render its link
    // rect — the `!ib.visible` guard in `draw_shaped_lines` skips the whole
    // InlineBox arm before link emission. Since `visibility` inherits, the
    // inner anchor is also hidden and should contribute nothing to the PDF.
    // The unique href below acts as the marker; it must not appear at all.
    let html = r#"<!DOCTYPE html><html><body><p><a href="https://hidden-inline-box-marker.invalid"><span style="visibility:hidden;display:inline-block;width:40px;height:20px;background:red">x</span></a></p></body></html>"#;
    let bytes = render(html);
    let needle = b"hidden-inline-box-marker.invalid";
    let present = bytes.windows(needle.len()).any(|w| w == needle);
    assert!(
        !present,
        "hidden inline-block anchor should not leak a /Link entry"
    );
}
