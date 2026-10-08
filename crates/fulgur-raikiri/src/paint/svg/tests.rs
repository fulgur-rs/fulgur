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
