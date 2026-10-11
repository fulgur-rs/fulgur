use super::*;
use raikiri_html::{
    LayoutConfig, LayoutOptions, LayoutStatus, PageDefaults, PaintRect, RenderResources,
};

fn rule() -> raikiri_html::ColumnRule {
    let resources = RenderResources::new();
    let document = raikiri_html::parse_html_with_resources(
        &b"<style>@page{size:100px 100px;margin:0}body{margin:0}.mc{width:60px;column-count:2;column-gap:20px;column-rule:2px solid red}.mc>div{height:20px}</style><div class=mc><div></div><div></div></div>"[..],
        &resources,
    ).unwrap();
    let LayoutStatus::Completed(layout) = raikiri_html::layout(
        &document,
        PageDefaults::default(),
        LayoutConfig::default(),
        LayoutOptions::new(),
    )
    .unwrap() else {
        panic!("completed layout")
    };
    let page = layout.page(0).unwrap();
    let runs = page.text_runs();
    page.paint_order_for_text_runs(&runs)
        .into_iter()
        .find_map(|event| {
            if let raikiri_html::PaintEvent::ColumnRule(rule) = event {
                Some(rule)
            } else {
                None
            }
        })
        .expect("producer column rule")
}

fn content(rule: Option<&raikiri_html::ColumnRule>) -> Vec<u8> {
    let mut pdf = krilla::Document::new();
    let mut page = pdf.start_page_with(krilla::page::PageSettings::from_wh(100.0, 100.0).unwrap());
    let mut surface = page.surface();
    if let Some(rule) = rule {
        paint_column_rule(&mut surface, rule);
    }
    // Subsequent content must remain drawable without an invalid rule's clip.
    surface.set_fill(Some(fill(CssColor {
        r: 0,
        g: 0,
        b: 255,
        a: 255,
    })));
    surface.draw_path(&RoundedRect::rect(70.0, 70.0, 10.0, 10.0).path().unwrap());
    surface.finish();
    page.finish();
    let parsed = lopdf::Document::load_mem(&pdf.finish().unwrap()).unwrap();
    let page = *parsed.get_pages().values().next().unwrap();
    parsed.get_page_content(page).unwrap()
}

#[test]
fn invisible_or_empty_column_rules_leave_subsequent_pdf_content_unchanged() {
    let expected = content(None);
    let original = rule();
    assert_ne!(content(Some(&original)), expected);
    for variant in 0..7 {
        let mut rule = original;
        match variant {
            0 => rule.rect.width = 0.0,
            1 => rule.rect.height = 0.0,
            2 => rule.pattern_height = 0.0,
            3 => rule.color.a = 0,
            4 => rule.style = BorderStyle::None,
            5 => rule.style = BorderStyle::Hidden,
            6 => rule.rect = PaintRect::new(10.0, 10.0, -2.0, 20.0),
            _ => unreachable!(),
        }
        assert_eq!(content(Some(&rule)), expected, "variant {variant}");
    }
}

#[test]
fn unrepresentable_column_rule_clips_leave_subsequent_pdf_content_unchanged() {
    let expected = content(None);
    for coordinate in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut rule = rule();
        rule.rect.x = coordinate;
        assert_eq!(content(Some(&rule)), expected, "coordinate {coordinate}");
    }
}
