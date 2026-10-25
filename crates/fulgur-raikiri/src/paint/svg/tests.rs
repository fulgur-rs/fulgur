use super::*;
use crate::RenderOptions;
use raikiri_html::{LayoutOptions, LayoutStatus, RenderResources};
use raikiri_traits::{LayoutConfig, PageDefaults};

fn render_svg_page(svg: &str, host_style: &str) -> lopdf::Document {
    render_svg_page_with(svg, host_style, |_| {})
}

fn render_svg_page_with(
    svg: &str,
    host_style: &str,
    configure: impl FnOnce(&mut RenderOptions<'_>),
) -> lopdf::Document {
    let html = format!(
        "<style>@page{{size:200px 100px;margin:0}}body{{margin:0}}svg{{display:block;{host_style}}}</style>{svg}"
    );
    let pdf = render_html_with_font(&html, configure).unwrap();
    lopdf::Document::load_mem(&pdf).unwrap()
}

/// Render `html` set in the bundled test face alone.
fn render_html_with_font(
    html: &str,
    configure: impl FnOnce(&mut RenderOptions<'_>),
) -> fulgur_core::Result<Vec<u8>> {
    let mut bundle = fulgur_core::AssetBundle::new();
    bundle
        .add_font_bytes(
            include_bytes!("../../../../fulgur-ruby/spec/fixtures/noto_sans.ttf").to_vec(),
        )
        .unwrap();
    let mut options = crate::RenderOptions {
        assets: Some(&bundle),
        system_fonts: false,
        ..Default::default()
    };
    configure(&mut options);
    crate::render_html_with_options(html, None, &fulgur_core::Config::default(), &options)
}

fn image_objects(pdf: &lopdf::Document) -> usize {
    pdf.objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .filter(|stream| {
            stream
                .dict
                .get(b"Subtype")
                .is_ok_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Image"))
        })
        .count()
}

fn embedded_fonts(pdf: &lopdf::Document) -> usize {
    pdf.objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .filter(|dict| dict.has(b"FontFile2") || dict.has(b"FontFile3"))
        .count()
}

#[test]
fn inline_svg_text_is_selectable_text_set_in_the_fonts_of_the_page() {
    let pdf = render_svg_page(
        "<svg width='200' height='50'><text x='5' y='30' font-size='20' fill='navy'>Vector</text></svg>\
         <p style='margin:0'>Body</p>",
        "",
    );
    let text = pdf.extract_text(&[1]).unwrap();
    assert_eq!(
        text.split_whitespace().collect::<Vec<_>>(),
        ["Vector", "Body"]
    );
    // The page's bundled face is the only font, for the SVG and the body.
    assert_eq!(embedded_fonts(&pdf), 1);
    assert_eq!(image_objects(&pdf), 0);
}

#[test]
fn inline_svg_paints_stay_vector_operations() {
    let pdf = render_svg_page(
        "<svg width='200' height='80' viewBox='0 0 100 40'>\
           <defs><linearGradient id='g'><stop offset='0' stop-color='red'/><stop offset='1' stop-color='blue'/></linearGradient>\
             <clipPath id='c'><rect width='60' height='40'/></clipPath></defs>\
           <g clip-path='url(#c)' opacity='.5'><rect width='100' height='40' fill='url(#g)' stroke='black' stroke-dasharray='4 2'/></g>\
         </svg>",
        "",
    );
    let operations = crate::tests::operations_of(&pdf);
    assert!(crate::tests::has_shading(&pdf, 2));
    assert_eq!(crate::tests::count(&operations, "d"), 1);
    assert!(crate::tests::count(&operations, "W") >= 1);
    assert_eq!(image_objects(&pdf), 0);
}

#[test]
fn inline_svg_with_a_mask_is_drawn_as_pixels() {
    // Masks are outside the vector subset, so Raikiri hands over the picture
    // of the whole image instead of mixing the two renderings.
    let pdf = render_svg_page(
        "<svg width='200' height='80' viewBox='0 0 100 40'>\
           <mask id='m'><rect width='50' height='40' fill='white'/></mask>\
           <rect width='100' height='40' fill='green' mask='url(#m)'/>\
         </svg>",
        "",
    );
    // The pixels and their soft mask, where the mask leaves some transparent.
    assert!(image_objects(&pdf) >= 1);
}

/// An SVG with a gradient of more stops than a scene holds.
fn svg_over_the_scene_budget() -> String {
    let stops: String = (0..1100)
        .map(|index| {
            format!(
                "<stop offset='{}' stop-color='{}'/>",
                f64::from(index) / 1100.0,
                if index % 2 == 0 { "red" } else { "blue" }
            )
        })
        .collect();
    format!(
        "<svg width='200' height='80' viewBox='0 0 100 40'>\
           <defs><linearGradient id='g'>{stops}</linearGradient></defs>\
           <rect width='100' height='40' fill='url(#g)'/>\
         </svg>"
    )
}

#[test]
fn inline_svg_over_a_budget_of_the_scene_is_drawn_as_pixels() {
    // The scene builder refuses the gradient, and the raster path has no such
    // limit, so Raikiri hands over pixels.
    let pdf = render_svg_page(&svg_over_the_scene_budget(), "");
    assert!(!crate::tests::has_shading(&pdf, 2));
    assert_eq!(image_objects(&pdf), 1);
}

#[test]
fn an_svg_source_drawn_at_another_size_gets_its_own_scene() {
    // The same markup at 30, 30 and 15 CSS px: the second drawing is served
    // from the picture that Raikiri kept for the first size, and the third
    // must not reuse it.
    let svg = "<svg viewBox='0 0 10 10'><rect width='10' height='10' fill='green'/></svg>";
    let pdf = render_svg_page_with(
        &svg.repeat(3),
        "width:30px;height:30px}svg:last-of-type{width:15px;height:15px",
        |_| {},
    );
    // The scene maps its 10 unit viewBox to the box, then the page maps CSS
    // pixels to points: 3 * 0.75 for the first two and 1.5 * 0.75 for the last.
    let scales: Vec<f32> = crate::tests::operations_of(&pdf)
        .iter()
        .filter(|operation| operation.operator == "cm")
        .filter_map(|operation| operation.operands.first()?.as_float().ok())
        .collect();
    assert_eq!(scales, [2.25, 2.25, 1.125]);
}

#[test]
fn the_opacity_of_an_inline_svg_composites_once_whoever_applies_it() {
    // The ordered painter composites the host opacity around the box; the
    // scene itself stays opaque.
    let pdf = render_svg_page(
        "<svg width='100' height='40'><rect width='100' height='40' fill='red'/></svg>",
        "opacity:.5",
    );
    let alphas: Vec<f32> = pdf
        .objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .filter_map(|dict| dict.get(b"ca").ok()?.as_float().ok())
        .collect();
    assert_eq!(
        alphas.iter().filter(|alpha| **alpha == 0.5).count(),
        1,
        "{alphas:?}"
    );
    // Equal states share one object, so also count where they are applied:
    // once around the box and not again inside it.
    assert_eq!(
        crate::tests::count(&crate::tests::operations_of(&pdf), "gs"),
        1
    );
}

/// The widths of the images of the page.
fn image_widths(pdf: &lopdf::Document) -> Vec<i64> {
    pdf.objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .filter(|stream| {
            stream
                .dict
                .get(b"Subtype")
                .is_ok_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Image"))
        })
        .filter_map(|stream| stream.dict.get(b"Width").ok()?.as_i64().ok())
        .collect()
}

const TEXT_AND_RECT: &str = "<svg width='100' height='40'><rect width='100' height='40' fill='red'/>\
    <text x='5' y='30' font-size='20' fill='navy'>Vector</text></svg><p style='margin:0'>Body</p>";

#[test]
fn an_element_that_asks_for_raster_gets_an_image_instead_of_vector_content() {
    let vector = render_svg_page(TEXT_AND_RECT, "");
    assert_eq!(image_objects(&vector), 0);
    assert!(vector.extract_text(&[1]).unwrap().contains("Vector"));

    let raster = render_svg_page(TEXT_AND_RECT, "--raikiri-svg-rendering:raster");
    assert_eq!(image_objects(&raster), 1);
    // The picture is pixels, text included.
    assert_eq!(
        raster
            .extract_text(&[1])
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["Body"]
    );
}

#[test]
fn the_option_decides_for_an_element_that_declares_nothing() {
    let by_option = |rendering| {
        render_svg_page_with(TEXT_AND_RECT, "", |options| {
            options.svg_rendering = rendering;
        })
    };
    assert_eq!(
        image_objects(&by_option(raikiri_html::SvgRendering::Raster)),
        1
    );
    assert_eq!(
        image_objects(&by_option(raikiri_html::SvgRendering::Vector)),
        0
    );

    // The element's own declaration wins over the option.
    let overridden =
        render_svg_page_with(TEXT_AND_RECT, "--raikiri-svg-rendering:vector", |options| {
            options.svg_rendering = raikiri_html::SvgRendering::Raster;
        });
    assert_eq!(image_objects(&overridden), 0);
}

#[test]
fn the_raster_scale_gives_the_same_picture_more_pixels() {
    let width_at = |scale| {
        let pdf =
            render_svg_page_with(TEXT_AND_RECT, "--raikiri-svg-rendering:raster", |options| {
                options.svg_raster_scale = scale;
            });
        image_widths(&pdf)
    };
    assert_eq!(width_at(1.0), [100]);
    assert_eq!(width_at(2.5), [250]);
    // A scale that is no size leaves the default.
    assert_eq!(width_at(0.0), [100]);
}

#[test]
fn the_opacity_of_a_raster_inline_svg_composites_once() {
    let pdf = render_svg_page(
        "<svg width='100' height='40'><rect width='100' height='40' fill='red'/></svg>",
        "opacity:.5;--raikiri-svg-rendering:raster",
    );
    let alphas: Vec<f32> = pdf
        .objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .filter_map(|dict| dict.get(b"ca").ok()?.as_float().ok())
        .collect();
    assert_eq!(
        alphas.iter().filter(|alpha| **alpha == 0.5).count(),
        1,
        "{alphas:?}"
    );
    // The pixels carry no transparency of their own, or it would apply twice.
    assert_eq!(image_objects(&pdf), 1);
}

/// Paints the first page of `html` with the painter that gives an SVG its
/// own opacity, and tells how many pictures the cache converted.
fn paint_without_groups(html: &str) -> (lopdf::Document, usize) {
    let resources = RenderResources::new();
    let document = raikiri_html::parse_html_with_resources(html.as_bytes(), &resources).unwrap();
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
    let mut pdf_page =
        pdf.start_page_with(krilla::page::PageSettings::from_wh(100.0, 100.0).unwrap());
    let mut surface = Surface::new(pdf_page.surface());
    let mut svg = SvgCache::default();
    crate::paint::paint_legacy(
        &mut surface,
        &page,
        &page.paint_order(),
        &page.text_runs(),
        &mut crate::paint::FontCache::default(),
        &mut svg,
        &mut crate::paint::raster::RasterCache::for_tests(resources.image_pixel_source_ref()),
        &mut crate::tagging::Tags::disabled(),
        None,
    )
    .unwrap();
    surface.finish();
    pdf_page.finish();
    let kept = svg.converted_images();
    (
        lopdf::Document::load_mem(&pdf.finish().unwrap()).unwrap(),
        kept,
    )
}

#[test]
fn a_raster_inline_svg_keeps_its_own_opacity_where_nothing_composites_it() {
    let one = "<svg width='20' height='10' style='display:block;opacity:.5;--raikiri-svg-rendering:raster'>\
        <rect width='20' height='10' fill='red'/></svg>";
    let html =
        format!("<style>@page{{size:100px 100px;margin:0}}body{{margin:0}}</style>{one}{one}{one}");
    let (pdf, kept) = paint_without_groups(&html);
    assert!(
        pdf.objects
            .values()
            .filter_map(|object| object.as_dict().ok())
            .any(|dict| dict.get(b"ca").ok().and_then(|value| value.as_float().ok()) == Some(0.5))
    );
    assert_eq!(image_objects(&pdf), 1);
    // Raikiri hands out one picture for the three SVGs, which is converted
    // once.
    assert_eq!(kept, 1);
}

#[test]
fn inline_svg_with_stroked_glyphs_off_a_similarity_is_drawn_as_pixels() {
    // Krilla strokes text in glyph space, so Raikiri hands over pixels for
    // glyphs that `textLength` stretches.
    let stretched = "<svg width='100' height='40'><text x='5' y='30' font-size='20' fill='none' stroke='navy' \
        textLength='90' lengthAdjust='spacingAndGlyphs'>Vector</text></svg>";
    let pdf = render_svg_page(stretched, "");
    assert!(image_objects(&pdf) >= 1);
    // The same text, filled and not stroked, stays text.
    let filled = stretched
        .replace("fill='none'", "fill='navy'")
        .replace(" stroke='navy'", "");
    let pdf = render_svg_page(&filled, "");
    assert_eq!(image_objects(&pdf), 0);
}

const FILTERED_SVG: &str = "<svg width='20' height='20'><filter id='f'><feGaussianBlur stdDeviation='1'/></filter>\
     <rect width='20' height='20' fill='red' filter='url(#f)'/></svg>";

/// Asserts that rendering `html` fails with the error of the SVG that cannot
/// be drawn, where it used to panic as Krilla dropped a surface with pushes
/// still open.
fn assert_rejects_the_filtered_svg(html: &str) {
    match render_html_with_font(html, |_| {}) {
        Err(fulgur_core::Error::Layout(message)) => {
            assert!(message.starts_with("inline SVG: "), "{message}");
        }
        other => panic!("expected the SVG error, got {other:?}"),
    }
}

#[test]
fn an_inline_svg_that_cannot_be_drawn_fails_the_render() {
    assert_rejects_the_filtered_svg(&format!(
        "<style>@page{{size:200px 100px;margin:0}}</style>{FILTERED_SVG}"
    ));
}

#[test]
fn the_error_of_an_inline_svg_unwinds_the_clips_groups_and_transforms_around_it() {
    assert_rejects_the_filtered_svg(&format!(
        "<style>@page{{size:200px 100px;margin:0}}</style>\
         <div style='overflow:hidden;opacity:.5;transform:translate(5px,5px)'>\
         <div style='overflow:hidden;opacity:.8'>{FILTERED_SVG}</div></div>"
    ));
}

#[test]
fn the_error_of_an_inline_svg_in_a_running_element_unwinds_its_margin_box() {
    assert_rejects_the_filtered_svg(&format!(
        "<style>@page{{size:200px 100px;margin:20px;@top-center{{content:element(head)}}}}\
         .head{{position:running(head)}}</style><div class=head>{FILTERED_SVG}</div><p>Body</p>"
    ));
}
