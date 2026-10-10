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
    let mut raster = raster::RasterCache::for_tests(resources.image_pixel_source_ref());
    paint_legacy(
        &mut surface,
        &page,
        &page.text_runs(),
        &mut fonts,
        &mut svg,
        &mut raster,
    )
    .unwrap();
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
    let mut raster = raster::RasterCache::for_tests(resources.image_pixel_source_ref());
    paint_legacy(
        &mut surface,
        &page,
        &page.text_runs(),
        &mut fonts,
        &mut svg,
        &mut raster,
    )
    .unwrap();
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

mod raster_tests;

#[test]
fn producer_fragmentainer_clip_is_applied_before_its_opacity_group() {
    let resources = RenderResources::new();
    let document = raikiri_html::parse_html_with_resources(
        &b"<style>@page{size:100px 100px;margin:0}body{margin:0}div{width:60px;height:60px;background:red}</style><div></div>"[..],
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
    let dom = page.dom();
    let fragment = page
        .fragments()
        .find(|fragment| dom.local_name(fragment.node()) == Some("div"))
        .unwrap();
    let events = [
        PaintEvent::PushClip(
            raikiri_html::PaintClip::new(PaintRect::new(20.0, 20.0, 10.0, 10.0)),
            raikiri_html::ClipKind::Fragmentainer,
        ),
        PaintEvent::PushOpacity(0.5),
        PaintEvent::Box(fragment),
        PaintEvent::PopOpacity,
        PaintEvent::PopClip,
    ];
    let mut pdf = krilla::Document::new();
    let mut pdf_page = pdf.start_page_with(PageSettings::from_wh(100.0, 100.0).unwrap());
    let mut surface = pdf_page.surface();
    paint_ordered(
        &mut surface,
        &page,
        &events,
        &[],
        &mut FontCache::default(),
        &mut svg::SvgCache::new(crate::RenderOptions::default()),
        &mut raster::RasterCache::for_tests(resources.image_pixel_source_ref()),
    )
    .unwrap();
    surface.finish();
    pdf_page.finish();
    let parsed = lopdf::Document::load_mem(&pdf.finish().unwrap()).unwrap();
    let page_id = *parsed.get_pages().values().next().unwrap();
    let content =
        lopdf::content::Content::decode(&parsed.get_page_content(page_id).unwrap()).unwrap();
    let before_clip: Vec<_> = content
        .operations
        .iter()
        .take_while(|operation| operation.operator != "W")
        .collect();
    let vertices: Vec<_> = before_clip
        .iter()
        .filter(|operation| matches!(operation.operator.as_str(), "m" | "l"))
        .map(|operation| {
            operation
                .operands
                .iter()
                .map(|value| value.as_float().unwrap())
                .collect::<Vec<_>>()
        })
        .collect();
    // PDF coordinates grow upwards from the bottom of the 100pt page.
    assert_eq!(
        vertices,
        [
            vec![20.0, 80.0],
            vec![30.0, 80.0],
            vec![30.0, 70.0],
            vec![20.0, 70.0],
            vec![20.0, 80.0]
        ]
    );
    assert_eq!(
        content
            .operations
            .iter()
            .filter(|operation| operation.operator == "W")
            .count(),
        1
    );
    assert!(
        parsed
            .objects
            .values()
            .filter_map(|object| object.as_dict().ok())
            .any(|dict| dict.get(b"ca").ok().and_then(|value| value.as_float().ok()) == Some(0.5))
    );
    let clip_step = content
        .operations
        .iter()
        .position(|operation| operation.operator == "W")
        .unwrap();
    let group_step = content
        .operations
        .iter()
        .position(|operation| operation.operator == "Do")
        .unwrap();
    assert!(
        clip_step < group_step,
        "the opacity group's form inherits its column clip"
    );

    assert_eq!(
        content
            .operations
            .iter()
            .filter(|operation| operation.operator == "q")
            .count(),
        content
            .operations
            .iter()
            .filter(|operation| operation.operator == "Q")
            .count()
    );
}
