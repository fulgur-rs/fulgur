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
