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
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_font_data(
        include_bytes!("../../../../fulgur-ruby/spec/fixtures/noto_sans.ttf").to_vec(),
    );
    let tree = parse(
        &svg.source,
        &usvg::Options {
            fontdb: Arc::new(fonts),
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
