//! fulgur-9t3z: v2 path support for `<table style="overflow:hidden">`
//! cell clipping. Mirrors `style_test.rs::test_overflow_hidden_on_table_clips`
//! but routes explicitly through `render_html_v2`.

use fulgur::{Engine, PageSize};

#[test]
fn v2_table_overflow_hidden_emits_clip_path() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html_hidden = r#"<html><body>
        <table style="width:100px;height:60px;overflow:hidden">
            <tr><td style="width:300px;height:300px;background:orange">cell</td></tr>
        </table>
    </body></html>"#;
    let pdf_hidden = engine.render_html_v2(html_hidden).expect("render v2");
    assert!(pdf_hidden.starts_with(b"%PDF"));

    let html_visible = r#"<html><body>
        <table style="width:100px;height:60px">
            <tr><td style="width:300px;height:300px;background:orange">cell</td></tr>
        </table>
    </body></html>"#;
    let pdf_visible = engine.render_html_v2(html_visible).expect("render v2");

    assert_ne!(
        pdf_hidden, pdf_visible,
        "v2: overflow:hidden on a table should emit a clip path different from default"
    );
}

/// PR #320 Devin: an `overflow:hidden` table inside a `transform`
/// scope must still push its clip path. Without the table-clip arm
/// in `draw_under_transform`'s descendant dispatch chain, cells
/// would paint under the transform but lose the table boundary.
#[test]
fn v2_overflow_hidden_table_inside_transform_keeps_clip() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html_hidden = r#"<html><body>
        <div style="transform:rotate(5deg)">
            <table style="width:100px;height:60px;overflow:hidden">
                <tr><td style="width:300px;height:300px;background:orange">cell</td></tr>
            </table>
        </div>
    </body></html>"#;
    let pdf_hidden = engine.render_html_v2(html_hidden).expect("render v2");
    assert!(pdf_hidden.starts_with(b"%PDF"));

    let html_visible = r#"<html><body>
        <div style="transform:rotate(5deg)">
            <table style="width:100px;height:60px">
                <tr><td style="width:300px;height:300px;background:orange">cell</td></tr>
            </table>
        </div>
    </body></html>"#;
    let pdf_visible = engine.render_html_v2(html_visible).expect("render v2");

    assert_ne!(
        pdf_hidden, pdf_visible,
        "v2: overflow:hidden table inside transform must still emit a clip path"
    );
}

/// PR #320 Devin: a cell with its own `overflow:hidden` inside a
/// clipping table must still push its inner clip. Without the
/// nested-scope dispatch chain inside `draw_under_clip_table`,
/// the cell's 50px boundary would be lost.
#[test]
fn v2_table_clip_with_inner_cell_clip_keeps_inner_boundary() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html_inner_hidden = r#"<html><body>
        <table style="width:200px;height:120px;overflow:hidden">
            <tr><td style="width:50px;height:50px;overflow:hidden;background:orange">
                <div style="width:300px;height:300px;background:purple"></div>
            </td></tr>
        </table>
    </body></html>"#;
    let pdf_inner_hidden = engine.render_html_v2(html_inner_hidden).expect("render v2");
    assert!(pdf_inner_hidden.starts_with(b"%PDF"));

    let html_inner_visible = r#"<html><body>
        <table style="width:200px;height:120px;overflow:hidden">
            <tr><td style="width:50px;height:50px;background:orange">
                <div style="width:300px;height:300px;background:purple"></div>
            </td></tr>
        </table>
    </body></html>"#;
    let pdf_inner_visible = engine
        .render_html_v2(html_inner_visible)
        .expect("render v2");

    assert_ne!(
        pdf_inner_hidden, pdf_inner_visible,
        "v2: inner cell overflow:hidden inside a clipping table must still emit its own clip"
    );
}
