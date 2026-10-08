use super::*;
use raikiri_html::{LayoutConfig, LayoutOptions, LayoutStatus, PageDefaults, RenderResources};

#[test]
fn legacy_pages_keep_inline_svg_root_opacity() {
    // The legacy painter has no opacity groups, so the SVG must keep its root
    // opacity instead of leaving it to a PushOpacity event.
    let resources = RenderResources::new();
    let document = raikiri_html::parse_html_with_resources(
        &b"<style>@page{size:100px 100px;margin:0}body{margin:0}svg{display:block;opacity:.5}</style><svg width='20' height='10'><rect width='20' height='10' fill='red'/></svg>"[..],
        &resources,
    ).unwrap();
    let LayoutStatus::Completed(layout) = raikiri_html::layout(
        &document,
        PageDefaults::default(),
        LayoutConfig::default(),
        LayoutOptions::new(),
    )
    .unwrap() else {
        panic!("expected completed layout")
    };
    let page = layout.page(0).unwrap();
    let mut pdf = krilla::Document::new();
    let mut pdf_page = pdf.start_page_with(PageSettings::from_wh(100.0, 100.0).unwrap());
    let mut surface = pdf_page.surface();
    let mut fonts = FontCache::default();
    let mut svg = svg::SvgCache::new(crate::RenderOptions::default());
    paint_legacy(&mut surface, &page, &page.text_runs(), &mut fonts, &mut svg).unwrap();
    surface.finish();
    pdf_page.finish();
    let parsed = lopdf::Document::load_mem(&pdf.finish().unwrap()).unwrap();
    assert!(
        parsed
            .objects
            .values()
            .filter_map(|object| object.as_dict().ok())
            .any(|dict| dict.get(b"ca").ok().and_then(|value| value.as_float().ok()) == Some(0.5))
    );
}

#[test]
fn legacy_pages_keep_inline_svg_vectors() {
    let resources = RenderResources::new();
    let document = raikiri_html::parse_html_with_resources(
        &b"<style>@page{size:100px 100px;margin:0}body{margin:0}svg{display:block}</style><svg width='20' height='10'><rect width='20' height='10' fill='red'/></svg>"[..],
        &resources,
    ).unwrap();
    let LayoutStatus::Completed(layout) = raikiri_html::layout(
        &document,
        PageDefaults::default(),
        LayoutConfig::default(),
        LayoutOptions::new(),
    )
    .unwrap() else {
        panic!("expected completed layout")
    };
    let page = layout.page(0).unwrap();
    let mut pdf = krilla::Document::new();
    let mut pdf_page = pdf.start_page_with(PageSettings::from_wh(100.0, 100.0).unwrap());
    let mut surface = pdf_page.surface();
    let mut fonts = FontCache::default();
    let mut svg = svg::SvgCache::new(crate::RenderOptions::default());
    paint_legacy(&mut surface, &page, &page.text_runs(), &mut fonts, &mut svg).unwrap();
    surface.finish();
    pdf_page.finish();
    let bytes = pdf.finish().unwrap();
    let parsed = lopdf::Document::load_mem(&bytes).unwrap();
    let page_id = *parsed.get_pages().values().next().unwrap();
    let content =
        lopdf::content::Content::decode(&parsed.get_page_content(page_id).unwrap()).unwrap();
    assert!(
        content
            .operations
            .iter()
            .any(|operation| operation.operator == "rg"
                && operation.operands == vec![1.into(), 0.into(), 0.into()])
    );
    assert!(
        content
            .operations
            .iter()
            .any(|operation| matches!(operation.operator.as_str(), "f" | "f*"))
    );
    assert!(!parsed.objects.values().any(|value| {
        value
            .as_stream()
            .ok()
            .and_then(|stream| stream.dict.get(b"Subtype").ok())
            .and_then(|value| value.as_name().ok())
            == Some(b"Image".as_slice())
    }));
}
