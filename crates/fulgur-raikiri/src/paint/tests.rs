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
        &page.paint_order(),
        &page.text_runs(),
        &mut fonts,
        &mut svg,
        &mut raster,
        &mut crate::tagging::Tags::disabled(),
        None,
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
        &page.paint_order(),
        &page.text_runs(),
        &mut fonts,
        &mut svg,
        &mut raster,
        &mut crate::tagging::Tags::disabled(),
        None,
    )
    .unwrap();
    surface.finish();
    pdf_page.finish();
    let bytes = pdf.finish().unwrap();
    let parsed = lopdf::Document::load_mem(&bytes).unwrap();
    let page_id = *parsed.get_pages().values().next().unwrap();
    let content = lopdf::content::Content::decode(&parsed.get_page_content(page_id)).unwrap();
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
        &mut crate::tagging::Tags::disabled(),
        None,
    )
    .unwrap();
    surface.finish();
    pdf_page.finish();
    let parsed = lopdf::Document::load_mem(&pdf.finish().unwrap()).unwrap();
    let page_id = *parsed.get_pages().values().next().unwrap();
    let content = lopdf::content::Content::decode(&parsed.get_page_content(page_id)).unwrap();
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

#[test]
fn legacy_pages_skip_the_background_given_to_the_canvas() {
    let resources = RenderResources::new();
    let document = raikiri_html::parse_html_with_resources(
        &b"<style>@page{size:100px 100px;margin:0}body{margin:0;height:50px;background:rgb(0,128,0)}</style>"[..],
        &resources,
    )
    .unwrap();
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
    let owner = canvas::owner(&page).map(|(owner, _)| owner);
    assert!(owner.is_some());
    // The body paints its own box only when it did not give its background
    // to the canvas.
    let fill_count = |canvas_owner: Option<NodeId>| {
        let mut pdf = krilla::Document::new();
        let mut pdf_page = pdf.start_page_with(PageSettings::from_wh(100.0, 100.0).unwrap());
        let mut surface = pdf_page.surface();
        let mut fonts = FontCache::default();
        let mut svg = svg::SvgCache::new(crate::RenderOptions::default());
        let mut raster = raster::RasterCache::for_tests(resources.image_pixel_source_ref());
        paint_legacy(
            &mut surface,
            &page,
            &page.paint_order(),
            &page.text_runs(),
            &mut fonts,
            &mut svg,
            &mut raster,
            &mut crate::tagging::Tags::disabled(),
            canvas_owner,
        )
        .unwrap();
        surface.finish();
        pdf_page.finish();
        let parsed = lopdf::Document::load_mem(&pdf.finish().unwrap()).unwrap();
        let id = *parsed.get_pages().get(&1).unwrap();
        lopdf::content::Content::decode(&parsed.get_page_content(id))
            .unwrap()
            .operations
            .iter()
            .filter(|op| op.operator == "f")
            .count()
    };
    assert_eq!(fill_count(None), 1);
    assert_eq!(fill_count(owner), 0);
}

#[test]
fn a_root_without_a_box_gives_no_canvas() {
    // CSS Backgrounds 3 §2.11: a `display: none` root leaves the canvas
    // transparent, and its body does not take over.
    let resources = RenderResources::new();
    let document = raikiri_html::parse_html_with_resources(
        &b"<html style='display:none'><body style='background:rgb(0,128,0)'>x</body></html>"[..],
        &resources,
    )
    .unwrap();
    let LayoutStatus::Completed(layout) = raikiri_html::layout(
        &document,
        PageDefaults::default(),
        LayoutConfig::default(),
        LayoutOptions::new(),
    )
    .unwrap() else {
        panic!("expected completed layout")
    };
    let page = layout.page(0).expect("a page");
    assert!(canvas::owner(&page).is_none());
}

/// Whether `got` is the same stretch of events as `want`.
fn same_events(got: &[PaintEvent<'_>], want: &[PaintEvent<'_>]) -> bool {
    got.len() == want.len() && std::ptr::eq(got.as_ptr(), want.as_ptr())
}

#[test]
fn the_root_transform_wrapper_of_a_paint_order_is_left_to_the_caller() {
    let root = PaintTransform::translate(40.0, 30.0);
    let own = PaintTransform::translate(5.0, 5.0);
    let events = [
        PaintEvent::PushTransform(root),
        PaintEvent::PushTransform(own),
        PaintEvent::PopTransform,
        PaintEvent::PopTransform,
    ];
    // The wrapper is dropped, and the transforms nested in it stay.
    assert!(same_events(
        inside_root_transform(&events, RootSpace::Moved(root)),
        &events[1..3]
    ));
    // Without a root transform to open, nothing is dropped.
    assert!(same_events(
        inside_root_transform(&events, RootSpace::Plain),
        &events
    ));
    // A wrapper that is another transform is an element's own.
    assert!(same_events(
        inside_root_transform(&events, RootSpace::Moved(own)),
        &events
    ));
    // The first transform closes before the end: siblings, not a wrapper.
    let siblings = [
        PaintEvent::PushTransform(root),
        PaintEvent::PopTransform,
        PaintEvent::PushTransform(root),
        PaintEvent::PopTransform,
    ];
    assert!(same_events(
        inside_root_transform(&siblings, RootSpace::Moved(root)),
        &siblings
    ));
    // A paint order that does not wrap the document is used as it is.
    let bare = [PaintEvent::PushOpacity(0.5), PaintEvent::PopOpacity];
    assert!(same_events(
        inside_root_transform(&bare, RootSpace::Moved(root)),
        &bare
    ));
}
