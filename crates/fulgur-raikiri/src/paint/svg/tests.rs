use super::*;
use raikiri_html::{LayoutOptions, LayoutStatus, RenderResources};
use raikiri_traits::{LayoutConfig, PageDefaults};

#[test]
fn inherited_svg_font_faces_reach_the_vector_parser() {
    let resources = RenderResources::new();
    let document = raikiri_html::parse_html_with_resources(
        &b"<style>body{font-family:'Noto Sans Mono';font-weight:700;font-style:italic}svg{display:block}</style><svg width='100' height='30'><text y='20'>TEST</text></svg>"[..],
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
    let fragment = page
        .fragments()
        .find(|fragment| page.dom().local_name(fragment.node()) == Some("svg"))
        .unwrap();
    let svg = page.inline_svg(&fragment).unwrap().unwrap();
    let mut bundle = fulgur_core::AssetBundle::new();
    bundle
        .add_font_bytes(
            include_bytes!("../../../../fulgur-ruby/spec/fixtures/noto_sans.ttf").to_vec(),
        )
        .unwrap();
    let mut cache = SvgCache::new(RenderOptions {
        assets: Some(&bundle),
        system_fonts: true,
        ..Default::default()
    });
    let tree = parse(
        &svg.source,
        &usvg::Options {
            fontdb: cache.fonts(),
            ..usvg::Options::default()
        },
    )
    .unwrap();
    fn first_text(group: &usvg::Group) -> Option<&usvg::Text> {
        group.children().iter().find_map(|node| match node {
            usvg::Node::Text(text) => Some(text.as_ref()),
            usvg::Node::Group(group) => first_text(group),
            _ => None,
        })
    }
    let span = &first_text(tree.root()).unwrap().chunks()[0].spans()[0];
    assert_eq!(span.font().weight(), 700);
    assert_eq!(span.font().style(), usvg::FontStyle::Italic);
}

#[test]
fn bundled_family_backs_every_generic_svg_family() {
    let mut bundle = fulgur_core::AssetBundle::new();
    bundle
        .add_font_bytes(
            include_bytes!("../../../../fulgur-ruby/spec/fixtures/noto_sans.ttf").to_vec(),
        )
        .unwrap();
    let mut cache = SvgCache::new(RenderOptions {
        assets: Some(&bundle),
        system_fonts: true,
        ..Default::default()
    });
    let fonts = cache.fonts();
    let bundled = fonts.faces().next().unwrap().families[0].0.clone();
    for (name, family) in [
        ("serif", usvg::fontdb::Family::Serif),
        ("sans-serif", usvg::fontdb::Family::SansSerif),
        ("cursive", usvg::fontdb::Family::Cursive),
        ("fantasy", usvg::fontdb::Family::Fantasy),
        ("monospace", usvg::fontdb::Family::Monospace),
    ] {
        assert_eq!(fonts.family_name(&family), bundled, "{name}");
    }
}

#[test]
fn root_opacity_neutralization_preserves_viewbox_transforms() {
    let options = usvg::Options::default();
    let tree = parse("<svg xmlns='http://www.w3.org/2000/svg' width='100' height='50' viewBox='10 20 200 100' opacity='.5'><rect x='10' y='20' width='200' height='100' fill='red'/></svg>", &options).unwrap();
    let bounds = tree.root().abs_bounding_box();
    let tree = neutralize_root_opacity(tree, &options, 0.5).unwrap();
    assert_eq!(tree.root().abs_bounding_box(), bounds);
    fn assert_opaque(group: &usvg::Group) {
        assert_eq!(group.opacity().get(), 1.0);
        for node in group.children() {
            if let usvg::Node::Group(group) = node {
                assert_opaque(group);
            }
        }
    }
    assert_opaque(tree.root());
    assert!(!tree.root().children().is_empty());
}

#[test]
fn empty_svg_needs_no_opacity_group() {
    let options = usvg::Options::default();
    let tree = parse(
        "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'/>",
        &options,
    )
    .unwrap();
    let tree = neutralize_root_opacity(tree, &options, 0.5).unwrap();
    assert!(tree.root().children().is_empty());
}

#[test]
fn a_different_root_multiplier_is_not_removed() {
    let options = usvg::Options::default();
    let tree = parse("<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10' opacity='.5'><rect width='10' height='10'/></svg>", &options).unwrap();
    assert!(matches!(
        neutralize_root_opacity(tree, &options, 0.25),
        Err(Error::Layout(_))
    ));
}

#[test]
fn descendants_cannot_substitute_for_a_missing_root_multiplier() {
    let options = usvg::Options::default();
    for content in [
        "<rect width='10' height='10'/>",
        "<g opacity='.25'><rect width='10' height='10'/></g><g opacity='.75'><circle cx='5' cy='5' r='2'/></g>",
    ] {
        let tree = parse(&format!("<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10' viewBox='0 0 20 20'>{content}</svg>"), &options).unwrap();
        assert!(matches!(
            neutralize_root_opacity(tree, &options, 0.5),
            Err(Error::Layout(_))
        ));
    }
}

#[test]
fn document_font_rules_keep_relative_sizes_in_use_instances() {
    let resources = RenderResources::new();
    let document = raikiri_html::parse_html_with_resources(
        &b"<style>body{font-family:'Noto Sans Mono';font-size:12px}svg{display:block}.template{font-size:2em;font-weight:700}.instance{font-size:10px;font-weight:200}</style><svg width='100' height='40'><defs><g id='label' class='template'><text y='20'>TEST</text></g></defs><use class='instance' href='#label'/></svg>"[..], &resources,
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
    let fragment = page
        .fragments()
        .find(|fragment| page.dom().local_name(fragment.node()) == Some("svg"))
        .unwrap();
    let svg = page.inline_svg(&fragment).unwrap().unwrap();
    let mut bundle = fulgur_core::AssetBundle::new();
    bundle
        .add_font_bytes(
            include_bytes!("../../../../fulgur-ruby/spec/fixtures/noto_sans.ttf").to_vec(),
        )
        .unwrap();
    let mut cache = SvgCache::new(RenderOptions {
        assets: Some(&bundle),
        system_fonts: true,
        ..Default::default()
    });
    let tree = parse(
        &svg.source,
        &usvg::Options {
            fontdb: cache.fonts(),
            ..usvg::Options::default()
        },
    )
    .unwrap();
    fn first_text(group: &usvg::Group) -> Option<&usvg::Text> {
        group.children().iter().find_map(|node| match node {
            usvg::Node::Text(text) => Some(text.as_ref()),
            usvg::Node::Group(group) => first_text(group),
            _ => None,
        })
    }
    let span = &first_text(tree.root()).unwrap().chunks()[0].spans()[0];
    assert_eq!(span.font_size().get(), 20.0);
    assert_eq!(span.font().weight(), 700);
}

#[test]
fn repeated_sources_reuse_one_parsed_tree() {
    let mut cache = SvgCache::new(RenderOptions {
        assets: None,
        system_fonts: true,
        ..Default::default()
    });
    let source = "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><g opacity='0.5'><rect width='10' height='10'/></g></svg>";
    // A source drawn once is not kept.
    cache.tree(source.into(), None).unwrap();
    assert!(cache.trees.is_empty());
    let second: *const usvg::Tree = cache.tree(source.into(), None).unwrap();
    let third: *const usvg::Tree = cache.tree(source.into(), None).unwrap();
    assert_eq!(second, third);
    cache.tree(source.into(), Some(0.5)).unwrap();
    let neutralized: *const usvg::Tree = cache.tree(source.into(), Some(0.5)).unwrap();
    assert_ne!(second, neutralized);
    let again: *const usvg::Tree = cache.tree(source.into(), Some(0.5)).unwrap();
    assert_eq!(neutralized, again);
    assert_eq!(cache.trees.len(), 1);
    assert_eq!(cache.trees[source].len(), 2);
    // No text, so the font database was never built.
    assert!(cache.fonts.is_none());
}

#[test]
fn text_sources_build_the_font_database() {
    let mut cache = SvgCache::new(RenderOptions {
        assets: None,
        system_fonts: false,
        ..Default::default()
    });
    let source =
        "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><text y='8'>A</text></svg>";
    cache.tree(source.into(), None).unwrap();
    assert!(cache.fonts.is_some());
}

#[test]
fn a_full_cache_parses_without_keeping_trees() {
    let mut cache = SvgCache::new(RenderOptions {
        assets: None,
        system_fonts: false,
        ..Default::default()
    });
    cache.cached_source_bytes = MAX_CACHED_SOURCE_BYTES;
    let source = "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='10' height='10'/></svg>";
    for _ in 0..3 {
        cache.tree(source.into(), None).unwrap();
    }
    assert!(cache.trees.is_empty());
    assert!(cache.uncached.is_some());
}

fn render_svg_page(svg: &str, host_style: &str) -> lopdf::Document {
    render_svg_page_with(svg, host_style, |_| {})
}

fn render_svg_page_with(
    svg: &str,
    host_style: &str,
    configure: impl FnOnce(&mut RenderOptions<'_>),
) -> lopdf::Document {
    let mut bundle = fulgur_core::AssetBundle::new();
    bundle
        .add_font_bytes(
            include_bytes!("../../../../fulgur-ruby/spec/fixtures/noto_sans.ttf").to_vec(),
        )
        .unwrap();
    let html = format!(
        "<style>@page{{size:200px 100px;margin:0}}body{{margin:0}}svg{{display:block;{host_style}}}</style>{svg}"
    );
    let mut options = crate::RenderOptions {
        assets: Some(&bundle),
        system_fonts: false,
        ..Default::default()
    };
    configure(&mut options);
    let pdf =
        crate::render_html_with_options(&html, None, &fulgur_core::Config::default(), &options)
            .unwrap();
    lopdf::Document::load_mem(&pdf).unwrap()
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
fn inline_svg_the_scene_leaves_to_a_raster_is_drawn_from_the_parsed_tree() {
    // Masks are outside the vector subset, so this image is drawn the way it
    // was before scenes: still vector, through the parsed tree.
    let pdf = render_svg_page(
        "<svg width='200' height='80' viewBox='0 0 100 40'>\
           <mask id='m'><rect width='50' height='40' fill='white'/></mask>\
           <rect width='100' height='40' fill='green' mask='url(#m)'/>\
         </svg>",
        "",
    );
    let operations = crate::tests::operations_of(&pdf);
    assert!(
        operations
            .iter()
            .any(|operation| operation.operator == "rg" && operation.operands.len() == 3)
    );
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
fn inline_svg_over_a_budget_of_the_scene_is_drawn_from_the_parsed_tree() {
    // The scene builder refuses the gradient instead of asking for a raster,
    // so the image goes through the parsed tree and stays vector.
    let pdf = render_svg_page(&svg_over_the_scene_budget(), "");
    assert!(crate::tests::has_shading(&pdf, 2));
    assert_eq!(image_objects(&pdf), 0);
}

#[test]
fn an_svg_source_drawn_at_another_size_gets_its_own_scene() {
    // The same markup at 30, 30 and 15 CSS px: the second drawing makes the
    // memo keep the scene of the first size, and the third must not reuse it.
    // The scene memo is keyed by the prepared source and the host opacity
    // only, because Raikiri puts the resolved viewport size into the source
    // (`InlineSvg::source`); if it ever stops doing that, this fails and the
    // size has to join the key.
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
/// own opacity, and tells how many rasters the cache kept.
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
    let mut surface = pdf_page.surface();
    let mut svg = SvgCache::new(RenderOptions::default());
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
    let kept = svg.rasters.len();
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
    // The picture asked for again is kept, and the third SVG finds it there.
    assert_eq!(kept, 1);
}
