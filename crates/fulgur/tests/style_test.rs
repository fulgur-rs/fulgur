use fulgur::config::PageSize;
use fulgur::engine::Engine;

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

#[test]
fn test_overflow_hidden_produces_different_pdf_than_visible() {
    let engine = fulgur::engine::Engine::builder()
        .page_size(fulgur::config::PageSize::A4)
        .build();

    // overflow:hidden parent with a large child that overflows
    let html_hidden = r#"<html><body>
        <div style="width:100px;height:100px;overflow:hidden;background:#eee">
            <div style="width:300px;height:300px;background:red"></div>
        </div>
    </body></html>"#;
    let pdf_hidden = engine.render_html(html_hidden).unwrap();
    assert!(pdf_hidden.starts_with(b"%PDF"));

    let html_visible = r#"<html><body>
        <div style="width:100px;height:100px;overflow:visible;background:#eee">
            <div style="width:300px;height:300px;background:red"></div>
        </div>
    </body></html>"#;
    let pdf_visible = engine.render_html(html_visible).unwrap();
    assert!(pdf_visible.starts_with(b"%PDF"));

    // overflow:hidden emits a clip path in the content stream, so the PDF
    // bytes must differ from the overflow:visible baseline.
    assert_ne!(
        pdf_hidden, pdf_visible,
        "overflow:hidden should produce a different PDF than overflow:visible"
    );
}

#[test]
fn test_overflow_clip_keyword_renders() {
    let engine = fulgur::engine::Engine::builder()
        .page_size(fulgur::config::PageSize::A4)
        .build();
    let html = r#"<html><body>
        <div style="width:100px;height:100px;overflow:clip">
            <div style="width:300px;height:300px;background:blue"></div>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_overflow_scroll_and_auto_also_clip() {
    let engine = fulgur::engine::Engine::builder()
        .page_size(fulgur::config::PageSize::A4)
        .build();
    // scroll and auto should both collapse to Clip in PDF (no scroll concept)
    let html_scroll = r#"<html><body>
        <div style="width:100px;height:100px;overflow:scroll">
            <div style="width:300px;height:300px;background:green"></div>
        </div>
    </body></html>"#;
    let pdf_scroll = engine.render_html(html_scroll).unwrap();
    assert!(pdf_scroll.starts_with(b"%PDF"));

    let html_auto = r#"<html><body>
        <div style="width:100px;height:100px;overflow:auto">
            <div style="width:300px;height:300px;background:green"></div>
        </div>
    </body></html>"#;
    let pdf_auto = engine.render_html(html_auto).unwrap();
    assert!(pdf_auto.starts_with(b"%PDF"));

    let html_visible = r#"<html><body>
        <div style="width:100px;height:100px;overflow:visible">
            <div style="width:300px;height:300px;background:green"></div>
        </div>
    </body></html>"#;
    let pdf_visible = engine.render_html(html_visible).unwrap();

    assert_ne!(
        pdf_scroll, pdf_visible,
        "overflow:scroll should clip just like hidden in PDF output"
    );
    assert_ne!(
        pdf_auto, pdf_visible,
        "overflow:auto should clip just like hidden in PDF output"
    );
}

#[test]
fn test_overflow_hidden_on_bare_block_without_visual_style() {
    // Regression for the `needs_block_wrapper` fix: a block with overflow
    // as the only non-default style (no background, no border, no padding,
    // no radius) must still be wrapped in a BlockPageable so that the clip
    // path is actually emitted.
    let engine = fulgur::engine::Engine::builder()
        .page_size(fulgur::config::PageSize::A4)
        .build();
    let html_hidden = r#"<html><body>
        <div style="width:100px;height:100px;overflow:hidden">
            <div style="width:300px;height:300px;background:red"></div>
        </div>
    </body></html>"#;
    let pdf_hidden = engine.render_html(html_hidden).unwrap();
    assert!(pdf_hidden.starts_with(b"%PDF"));

    let html_visible = r#"<html><body>
        <div style="width:100px;height:100px">
            <div style="width:300px;height:300px;background:red"></div>
        </div>
    </body></html>"#;
    let pdf_visible = engine.render_html(html_visible).unwrap();

    assert_ne!(
        pdf_hidden, pdf_visible,
        "overflow:hidden on a bare block should still emit a clip path"
    );
}

#[test]
fn test_overflow_hidden_on_table_clips() {
    // Regression for TablePageable::draw clip wiring: a table with
    // overflow:hidden must clip its cells to the padding box.
    let engine = fulgur::engine::Engine::builder()
        .page_size(fulgur::config::PageSize::A4)
        .build();
    let html_hidden = r#"<html><body>
        <table style="width:100px;height:60px;overflow:hidden">
            <tr><td style="width:300px;height:300px;background:orange">cell</td></tr>
        </table>
    </body></html>"#;
    let pdf_hidden = engine.render_html(html_hidden).unwrap();
    assert!(pdf_hidden.starts_with(b"%PDF"));

    let html_visible = r#"<html><body>
        <table style="width:100px;height:60px">
            <tr><td style="width:300px;height:300px;background:orange">cell</td></tr>
        </table>
    </body></html>"#;
    let pdf_visible = engine.render_html(html_visible).unwrap();

    assert_ne!(
        pdf_hidden, pdf_visible,
        "overflow:hidden on a table should emit a clip path different from default"
    );
}

#[test]
fn test_overflow_x_only_renders() {
    let engine = fulgur::engine::Engine::builder()
        .page_size(fulgur::config::PageSize::A4)
        .build();
    let html = r#"<html><body>
        <div style="width:100px;height:100px;overflow-x:hidden;overflow-y:visible">
            <div style="width:300px;height:300px;background:purple"></div>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));

    let html_visible = r#"<html><body>
        <div style="width:100px;height:100px">
            <div style="width:300px;height:300px;background:purple"></div>
        </div>
    </body></html>"#;
    let pdf_visible = engine.render_html(html_visible).unwrap();
    assert_ne!(
        pdf, pdf_visible,
        "overflow-x:hidden should emit a clip path different from default"
    );
}
