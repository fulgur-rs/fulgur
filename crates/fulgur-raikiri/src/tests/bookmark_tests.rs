use super::*;

fn outline_children<'a>(
    pdf: &'a lopdf::Document,
    parent: &lopdf::Dictionary,
) -> Vec<&'a lopdf::Dictionary> {
    let mut result = Vec::new();
    let mut next = parent
        .get(b"First")
        .ok()
        .map(|object| object.as_reference().unwrap());
    while let Some(id) = next {
        let item = pdf.get_dictionary(id).unwrap();
        result.push(item);
        next = item
            .get(b"Next")
            .ok()
            .map(|object| object.as_reference().unwrap());
    }
    result
}

fn title(item: &lopdf::Dictionary) -> String {
    lopdf::decode_text_string(item.get(b"Title").unwrap()).unwrap()
}

fn outline_root(pdf: &lopdf::Document) -> &lopdf::Dictionary {
    pdf.get_dictionary(
        pdf.catalog()
            .unwrap()
            .get(b"Outlines")
            .unwrap()
            .as_reference()
            .unwrap(),
    )
    .unwrap()
}

fn outline_destination<'a>(
    pdf: &'a lopdf::Document,
    item: &lopdf::Dictionary,
) -> &'a Vec<lopdf::Object> {
    pdf.get_object(item.get(b"Dest").unwrap().as_reference().unwrap())
        .unwrap()
        .as_array()
        .unwrap()
}

const BOOK_CSS: &str = "<style>@page {size:300px 200px;margin:0} body {margin:0} h1,h2 {margin:0;height:20px;font-size:10px;line-height:10px}</style>";

#[test]
fn bookmark_default_heading_hierarchy() {
    let (_dir, path) = input(&format!(
        "{BOOK_CSS}<h1>A</h1><h2>B</h2><h1 style='break-before:page'>C</h1>"
    ));
    let bytes = render(&path, &Config::builder().bookmarks(true).build()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let root = outline_root(&pdf);
    let items = outline_children(&pdf, root);
    assert_eq!(
        items.iter().map(|item| title(item)).collect::<Vec<_>>(),
        ["A", "C"]
    );
    let child = outline_children(&pdf, items[0]);
    assert_eq!(child.len(), 1);
    assert_eq!(title(child[0]), "B");
    assert_eq!(items[0].get(b"Count").unwrap().as_i64().unwrap(), -1);
    let dest_a = outline_destination(&pdf, items[0]);
    let dest_b = outline_destination(&pdf, child[0]);
    let dest_c = outline_destination(&pdf, items[1]);
    assert_eq!(dest_a[0].as_reference().unwrap(), pdf.get_pages()[&1]);
    assert_eq!(dest_a[2].as_float().unwrap(), 0.0);
    assert_eq!(dest_a[3].as_float().unwrap(), 150.0);
    assert_eq!(dest_b[3].as_float().unwrap(), 135.0);
    assert_eq!(dest_c[0].as_reference().unwrap(), pdf.get_pages()[&2]);
    assert_eq!(dest_c[3].as_float().unwrap(), 150.0);
}

#[test]
fn bookmark_author_level_label_and_none() {
    let (_dir, path) = input(&format!(
        r#"{BOOK_CSS}
        <h1 style='bookmark-label:"  Literal   label  "'>A</h1>
        <h2 data-label=' Attribute   label ' style='bookmark-level:1;bookmark-label:attr(data-label)'>B</h2>
        <h1 style='bookmark-label:content(text)'>Content   label</h1>
        <h1 style='bookmark-level:none'>None</h1>
        <h1 style='bookmark-level:0'>Zero</h1>
        <h1 style='bookmark-level:-1'>Negative</h1>
        <h1 style='bookmark-label:""'>Empty</h1>
        <h1 style='display:none'>Display hidden</h1>
        <h1 style='visibility:hidden'>Invisible</h1>"#
    ));
    let bytes = render(&path, &Config::builder().bookmarks(true).build()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let items = outline_children(&pdf, outline_root(&pdf));
    assert_eq!(
        items.iter().map(|item| title(item)).collect::<Vec<_>>(),
        ["Literal label", "Attribute label", "Content label"]
    );
    assert!(items.iter().all(|item| !item.has(b"First")));
}

#[test]
fn bookmark_empty_box_uses_first_rendered_descendant() {
    let (_dir, path) = input(&format!(
        r#"{BOOK_CSS}<h1 id=heading style='position:relative;width:0;height:0;bookmark-label:"Boxless"'><div style='position:absolute;left:20px;top:40px;width:40px;height:10px'>Child</div></h1>"#
    ));
    let document = completed(&path);
    let page = document.page(0).unwrap();
    let dom = page.dom();
    let heading = find_id(dom, dom.root(), "heading").unwrap();
    assert!(!page.fragments().any(|fragment| fragment.node() == heading
        && fragment.kind() == FragmentKind::Box
        && fragment.rect().width > 0.0
        && fragment.rect().height > 0.0));
    let bytes = render(&path, &Config::builder().bookmarks(true).build()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let items = outline_children(&pdf, outline_root(&pdf));
    assert_eq!(items.len(), 1);
    assert_eq!(title(items[0]), "Boxless");
    let dest = outline_destination(&pdf, items[0]);
    assert_eq!(dest[2].as_float().unwrap(), 15.0);
    assert_eq!(dest[3].as_float().unwrap(), 120.0);
}

#[test]
fn bookmark_large_level_uses_existing_hierarchy_only() {
    let (_dir, path) = input(&format!(
        "{BOOK_CSS}<h1 style='bookmark-level:2147483647'>Deep</h1><h1>Top</h1>"
    ));
    let bytes = render(&path, &Config::builder().bookmarks(true).build()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let items = outline_children(&pdf, outline_root(&pdf));
    assert_eq!(
        items.iter().map(|item| title(item)).collect::<Vec<_>>(),
        ["Deep", "Top"]
    );
}

#[test]
fn bookmark_disabled_has_no_outline() {
    let (_dir, path) = input("<h1>Heading</h1>");
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert!(!pdf.catalog().unwrap().has(b"Outlines"));
}

#[test]
fn bookmark_collection_is_discarded_on_abort_or_observer_error() {
    use raikiri_traits::{ConsumerPropertyEvent, ConsumerPropertyObserver};
    for fail_observer in [false, true] {
        let resources = RenderResources::new().stylesheet(bookmarks::heading_stylesheet());
        let document =
            parse_html_with_resources("<h1>A</h1><h2>B</h2>".as_bytes(), &resources).unwrap();
        let registrations = bookmarks::registrations();
        let mut collector = bookmarks::BookmarkCollector::default();
        let controller = AbortController::new();
        let mut events = 0;
        let status = {
            let mut observer = |event: ConsumerPropertyEvent| {
                collector.observe_event(event)?;
                events += 1;
                if fail_observer {
                    return Err(std::io::Error::other("observer failed"));
                }
                controller.abort();
                Ok(())
            };
            layout(
                &document,
                PageDefaults::default(),
                LayoutConfig::builder()
                    .signal(Some(controller.signal.clone()))
                    .build(),
                LayoutOptions::new()
                    .resources(&resources)
                    .consumer_properties(&registrations, &mut observer),
            )
        };
        assert!(events > 0);
        if !fail_observer {
            assert!(matches!(status, Ok(LayoutStatus::Aborted)));
        }
        let result = status
            .map_err(|error| Error::Layout(error.to_string()))
            .and_then(|status| {
                draw(
                    status,
                    &resources,
                    &Config::builder().bookmarks(true).build(),
                    &collector,
                    &url::Url::parse("file:///input.html").unwrap(),
                    &RenderOptions::default(),
                )
            });
        assert!(matches!(result, Err(Error::Layout(_))));
    }
}

#[test]
fn bookmark_with_layout_discards_already_aborted_result() {
    let (_dir, path) = input("<h1>A</h1>");
    let controller = AbortController::new();
    controller.abort();
    let config = Config::builder().bookmarks(true).build();
    let result = with_layout(
        &path,
        &config,
        &RenderOptions::default(),
        LayoutConfig::builder()
            .signal(Some(controller.signal.clone()))
            .build(),
        |status, resources, collector, document_url| {
            draw(
                status,
                resources,
                &config,
                collector,
                document_url,
                &RenderOptions::default(),
            )
        },
    );
    assert!(matches!(result, Err(Error::Layout(_))));
}

#[test]
fn bookmark_bundle_defaults_yield_to_author_style() {
    let (_dir, path) = input(&format!(
        r#"{BOOK_CSS}<h1 style='bookmark-level:1;bookmark-label:"Author"'>A</h1><h1>B</h1>"#
    ));
    let mut bundle = fulgur_core::AssetBundle::new();
    bundle.add_css("h1 {bookmark-level:2;bookmark-label:'Bundle'}");
    let bytes = render_with_options(
        &path,
        &Config::builder().bookmarks(true).build(),
        &RenderOptions {
            assets: Some(&bundle),
            system_fonts: true,
        },
    )
    .unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let items = outline_children(&pdf, outline_root(&pdf));
    assert_eq!(items.len(), 1);
    assert_eq!(title(items[0]), "Author");
    let children = outline_children(&pdf, items[0]);
    assert_eq!(children.len(), 1);
    assert_eq!(title(children[0]), "Bundle");
}
