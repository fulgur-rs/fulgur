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
    });
    let source = "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><g opacity='0.5'><rect width='10' height='10'/></g></svg>";
    let first: *const usvg::Tree = cache.tree(source.into(), None).unwrap();
    let second: *const usvg::Tree = cache.tree(source.into(), None).unwrap();
    assert_eq!(first, second);
    let neutralized: *const usvg::Tree = cache.tree(source.into(), Some(0.5)).unwrap();
    assert_ne!(first, neutralized);
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
    });
    let source =
        "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><text y='8'>A</text></svg>";
    cache.tree(source.into(), None).unwrap();
    assert!(cache.fonts.is_some());
}
