use super::*;
use krilla::page::PageSettings;
use raikiri_html::{
    LayoutConfig, LayoutOptions, LayoutStatus, NodeKind, PageDefaults, RenderResources,
};
use raikiri_traits::{DecodedImage, ImagePixelSource};
use std::sync::Arc;

struct RedPixels(Arc<DecodedImage>);

impl ImagePixelSource for RedPixels {
    fn get_decoded(&self, _url: &url::Url) -> Option<Arc<DecodedImage>> {
        Some(Arc::clone(&self.0))
    }
}

fn red() -> RedPixels {
    RedPixels(Arc::new(DecodedImage::new(
        9,
        9,
        [255, 0, 0, 255].repeat(81),
    )))
}

/// The computed style of the `<div>` in a document styled by `css`.
fn style(css: &str) -> ComputedValues {
    let resources = RenderResources::new();
    let html = format!("<style>@page{{size:100px 100px;margin:0}}{css}</style><div id=b></div>");
    let document = raikiri_html::parse_html_with_resources(html.as_bytes(), &resources).unwrap();
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
    let mut stack = vec![dom.root()];
    while let Some(node) = stack.pop() {
        if dom.kind(node) == Some(NodeKind::Element) && dom.attr(node, "id") == Some("b") {
            return page.computed(node).unwrap().clone();
        }
        stack.extend(dom.children(node));
    }
    panic!("no div")
}

/// Paint the border image of a 60×40 px box and return whether it replaced
/// the borders, with the PDF.
fn paint_pdf(
    style: &ComputedValues,
    source: Option<&dyn ImagePixelSource>,
    slice: Slice,
) -> (bool, lopdf::Document) {
    let mut pdf = krilla::Document::new();
    let mut page = pdf.start_page_with(PageSettings::from_wh(100.0, 100.0).unwrap());
    let mut surface = page.surface();
    let mut raster = raster::RasterCache::for_tests(source);
    let drawn = paint(
        &mut surface,
        &mut raster,
        PaintRect::new(10.0, 10.0, 60.0, 40.0),
        slice,
        style,
    )
    .unwrap();
    surface.finish();
    page.finish();
    (
        drawn,
        lopdf::Document::load_mem(&pdf.finish().unwrap()).unwrap(),
    )
}

/// Paint the border image of a 60×40 px box and return whether it replaced
/// the borders, with the PDF's image and shading counts.
fn paint_box(
    style: &ComputedValues,
    source: Option<&dyn ImagePixelSource>,
    slice: Slice,
) -> (bool, usize, usize) {
    let (drawn, parsed) = paint_pdf(style, source, slice);
    (drawn, count(&parsed, b"Image"), count(&parsed, b"Shading"))
}

/// The objects of `parsed` whose subtype is `kind`, or that are shadings or
/// patterns for those kinds.
fn count(parsed: &lopdf::Document, kind: &[u8]) -> usize {
    parsed
        .objects
        .values()
        .filter_map(|object| match object {
            lopdf::Object::Stream(stream) => Some(&stream.dict),
            lopdf::Object::Dictionary(dict) => Some(dict),
            _ => None,
        })
        .filter(|dict| {
            dict.get(b"Subtype").and_then(|value| value.as_name()).ok() == Some(kind)
                || (kind == b"Shading" && dict.has(b"ShadingType"))
                || (kind == b"Pattern" && dict.has(b"PatternType"))
        })
        .count()
}

#[test]
fn an_available_image_replaces_the_borders() {
    let style = style("div{border:3px solid blue;border-image:url(frame.png) 3 round}");
    let pixels = red();
    let (drawn, images, _) = paint_box(&style, Some(&pixels), Slice::default());
    assert!(drawn);
    assert_eq!(images, 1, "every tile shares one image");
}

#[test]
fn an_unavailable_image_leaves_the_borders() {
    let style = style("div{border:3px solid blue;border-image:url(frame.png) 3}");
    assert_eq!(paint_box(&style, None, Slice::default()), (false, 0, 0));
    let style = style_without_image();
    let pixels = red();
    assert_eq!(
        paint_box(&style, Some(&pixels), Slice::default()),
        (false, 0, 0)
    );
}

fn style_without_image() -> ComputedValues {
    style("div{border:3px solid blue}")
}

#[test]
fn gradients_fill_the_parts() {
    let style = style("div{border:4px solid;border-image:linear-gradient(red,blue) 25% fill}");
    let (drawn, images, shadings) = paint_box(&style, None, Slice::default());
    assert!(drawn);
    assert_eq!(images, 0);
    assert!(shadings > 0);
}

#[test]
fn a_fragment_without_border_widths_draws_nothing_but_hides_the_borders() {
    let style = style("div{border:3px solid blue;border-image:url(frame.png) 3}");
    let pixels = red();
    let broken = Slice {
        top: true,
        bottom: true,
        left: true,
        right: true,
    };
    assert_eq!(paint_box(&style, Some(&pixels), broken), (true, 0, 0));
}

/// Knows the image's size but cannot decode it at the size asked for.
struct Undecodable;

impl ImagePixelSource for Undecodable {
    fn get_decoded(&self, _url: &url::Url) -> Option<Arc<DecodedImage>> {
        None
    }
    fn intrinsic_size(&self, _url: &url::Url) -> Option<raikiri_traits::ImageIntrinsicSize> {
        Some(raikiri_traits::ImageIntrinsicSize::new(
            Some(9.0),
            Some(9.0),
            None,
        ))
    }
}

#[test]
fn an_image_that_cannot_be_decoded_leaves_the_borders() {
    let style = style("div{border:3px solid blue;border-image:url(frame.png) 3}");
    assert_eq!(
        paint_box(&style, Some(&Undecodable), Slice::default()),
        (false, 0, 0)
    );
}

#[test]
fn a_gradient_with_no_border_widths_draws_nothing_but_hides_the_borders() {
    let style = style("div{border:3px solid blue;border-image:linear-gradient(red,blue) 3}");
    let broken = Slice {
        top: true,
        bottom: true,
        left: true,
        right: true,
    };
    assert_eq!(paint_box(&style, None, broken), (true, 0, 0));
}

#[test]
fn many_tiles_share_one_pattern_per_part() {
    // 9 px slices from a 9 px image make 1 px tiles: 54 along the top and
    // bottom, 34 along the sides, so each edge fills one pattern.
    let style = style("div{border:3px solid blue;border-image:url(frame.png) 3 / 1px repeat}");
    let pixels = red();
    let (drawn, parsed) = paint_pdf(&style, Some(&pixels), Slice::default());
    assert!(drawn);
    assert_eq!(count(&parsed, b"Image"), 1, "patterns share the image");
    assert_eq!(count(&parsed, b"Pattern"), 4, "one per edge");
}

#[test]
fn a_few_tiles_are_drawn_one_by_one() {
    let style = style("div{border:3px solid blue;border-image:url(frame.png) 3 stretch}");
    let pixels = red();
    let (_, parsed) = paint_pdf(&style, Some(&pixels), Slice::default());
    assert_eq!(count(&parsed, b"Pattern"), 0);
}

#[test]
fn spaced_gradient_tiles_fill_a_pattern_with_gaps() {
    let style =
        style("div{border:4px solid;border-image:linear-gradient(red,blue) 25% / 2px space}");
    let (drawn, parsed) = paint_pdf(&style, None, Slice::default());
    assert!(drawn);
    assert!(count(&parsed, b"Pattern") > 0);
}

#[test]
fn uniform_steps_need_evenly_spaced_edges() {
    assert_eq!(uniform_step(&[5.0]), Some(0.0));
    assert_eq!(uniform_step(&[0.0, 2.0, 4.0]), Some(2.0));
    assert_eq!(uniform_step(&[0.0, 2.0, 5.0]), None);
}
