//! The background of an inline element (`<span>`, `<mark>`) is drawn behind
//! its text, as in browsers (fulgur-rs/fulgur#973).

use fulgur::{AssetBundle, Engine};
use krilla::SerializeSettings;
use std::path::PathBuf;

fn render(html: &str) -> String {
    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets.add_font_file(&font_path).unwrap();
    assets.add_css("body { font-family: 'Noto Sans'; }");
    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(SerializeSettings {
            compress_content_streams: false,
            ascii_compatible: true,
            ..Default::default()
        })
        .build()
        .render(html)
        .expect("render");
    String::from_utf8_lossy(&pdf).into_owned()
}

/// How many times the content streams set a given RGB fill color (`r g b rg`).
fn fills(pdf: &str, rgb: &str) -> usize {
    pdf.matches(&format!("{rgb} rg")).count()
}

#[test]
fn span_background_is_drawn() {
    let pdf = render(
        r#"<p>Text with a <span style="background: #ff0">highlighted span</span> in it.</p>"#,
    );
    assert_eq!(fills(&pdf, "1 1 0"), 1, "one yellow box");
}

#[test]
fn mark_gets_its_user_agent_background() {
    // The UA stylesheet gives `<mark>` the `Mark` system color, which Stylo
    // resolves to rgb(255, 235, 59) as Firefox does.
    let pdf = render("<p>Text with a <mark>mark element</mark> in it.</p>");
    assert_eq!(
        fills(&pdf, "1 0.92156863 0.23137255"),
        1,
        "the Mark color behind <mark>"
    );
}

#[test]
fn a_span_over_two_lines_gets_a_box_on_each() {
    let pdf = render(
        r#"<p style="width: 120px">aa <span style="background: #ff0">one two three four five</span></p>"#,
    );
    assert!(fills(&pdf, "1 1 0") >= 2, "a box per line");
}

#[test]
fn nested_spans_draw_both_backgrounds() {
    let pdf = render(
        r#"<p><span style="background: #ff0">outer <span style="background: #f00">inner</span> outer</span></p>"#,
    );
    assert_eq!(fills(&pdf, "1 0 0"), 1, "the inner red box");
    assert!(fills(&pdf, "1 1 0") >= 1, "the outer yellow box");
}

#[test]
fn text_without_inline_backgrounds_draws_no_boxes() {
    let pdf = render("<p>Plain <span>text</span> only.</p>");
    assert_eq!(fills(&pdf, "1 1 0") + fills(&pdf, "1 0 0"), 0);
}
