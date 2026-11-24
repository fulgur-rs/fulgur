//! The structure tree of a tagged PDF: every link annotation sits in
//! exactly one `Link` element, inline images keep their place in the
//! reading order, and headings carry a title. These tests read the
//! structure tree back with lopdf.

use fulgur::Engine;
use lopdf::{Dictionary, Document, Object, ObjectId};

const PAGE: &str = r#"<!DOCTYPE html><html lang="en"><title>Test</title><body>"#;

fn render_ua(html: &str) -> Result<Vec<u8>, String> {
    Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .title("Test")
        .build()
        .render(html)
        .map_err(|e| e.to_string())
}

fn render_tagged(html: &str) -> Vec<u8> {
    Engine::builder()
        .tagged(true)
        .lang("en")
        .title("Test")
        .build()
        .render(html)
        .expect("render")
}

fn load(pdf: &[u8]) -> Document {
    Document::load_mem(pdf).expect("PDF should parse")
}

fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    match obj {
        Object::Reference(id) => doc.get_object(*id).expect("dangling reference"),
        other => other,
    }
}

fn name_of(dict: &Dictionary, key: &[u8]) -> Option<String> {
    dict.get(key)
        .ok()
        .and_then(|o| o.as_name().ok())
        .map(|n| String::from_utf8_lossy(n).into_owned())
}

/// Every structure element in the file, by object id.
fn struct_elements(doc: &Document) -> Vec<(ObjectId, &Dictionary)> {
    doc.objects
        .iter()
        .filter_map(|(&id, obj)| {
            let dict = obj.as_dict().ok()?;
            let is_elem = name_of(dict, b"Type").as_deref() == Some("StructElem")
                || (dict.get(b"S").is_ok() && dict.get(b"P").is_ok());
            is_elem.then_some((id, dict))
        })
        .collect()
}

/// The kids of a structure element, as an owned list.
fn kids(dict: &Dictionary) -> Vec<Object> {
    match dict.get(b"K") {
        Ok(Object::Array(items)) => items.clone(),
        Ok(other) => vec![other.clone()],
        Err(_) => Vec::new(),
    }
}

/// The annotations referenced by an element's `OBJR` kids.
fn objr_targets(doc: &Document, dict: &Dictionary) -> Vec<ObjectId> {
    kids(dict)
        .iter()
        .filter_map(|k| {
            let d = resolve(doc, k).as_dict().ok()?;
            (name_of(d, b"Type").as_deref() == Some("OBJR"))
                .then(|| d.get(b"Obj").ok()?.as_reference().ok())
                .flatten()
        })
        .collect()
}

/// The pages that an element's marked content is on.
fn content_pages(doc: &Document, dict: &Dictionary) -> Vec<ObjectId> {
    let mut pages: Vec<ObjectId> = dict
        .get(b"Pg")
        .and_then(Object::as_reference)
        .into_iter()
        .collect();
    for k in kids(dict) {
        if let Ok(d) = resolve(doc, &k).as_dict()
            && d.get(b"MCID").is_ok()
            && let Ok(pg) = d.get(b"Pg").and_then(Object::as_reference)
        {
            pages.push(pg);
        }
    }
    pages
}

/// Every link annotation on every page, by object id.
fn link_annotations(doc: &Document) -> Vec<(ObjectId, Dictionary)> {
    let mut found = Vec::new();
    for page_id in doc.get_pages().values() {
        let page = doc.get_dictionary(*page_id).expect("page");
        let Ok(annots) = page.get(b"Annots") else {
            continue;
        };
        let annots = resolve(doc, annots).as_array().expect("Annots array");
        for annot in annots {
            let id = annot.as_reference().expect("annotation reference");
            let dict = doc.get_dictionary(id).expect("annotation");
            if name_of(dict, b"Subtype").as_deref() == Some("Link") {
                found.push((id, dict.clone()));
            }
        }
    }
    found
}

/// Look `key` up in a number tree (`/Nums`, possibly under `/Kids`).
fn number_tree_get(doc: &Document, node: &Dictionary, key: i64) -> Option<Object> {
    if let Ok(nums) = node.get(b"Nums") {
        let nums = resolve(doc, nums).as_array().ok()?;
        for pair in nums.chunks(2) {
            if pair[0].as_i64().ok()? == key {
                return pair.get(1).cloned();
            }
        }
    }
    if let Ok(node_kids) = node.get(b"Kids") {
        for kid in resolve(doc, node_kids).as_array().ok()? {
            let kid = resolve(doc, kid).as_dict().ok()?;
            if let Some(found) = number_tree_get(doc, kid, key) {
                return Some(found);
            }
        }
    }
    None
}

/// Check that every link annotation has a `/StructParent` that resolves
/// to a `Link` element, and that exactly one element of the structure
/// tree claims the annotation.
fn assert_each_annotation_in_one_link(pdf: &[u8]) {
    let doc = load(pdf);
    let annotations = link_annotations(&doc);
    assert!(!annotations.is_empty(), "the document has link annotations");
    let root = doc
        .catalog()
        .expect("catalog")
        .get(b"StructTreeRoot")
        .expect("a structure tree");
    let root = resolve(&doc, root).as_dict().expect("StructTreeRoot");
    let parent_tree = resolve(&doc, root.get(b"ParentTree").expect("ParentTree"))
        .as_dict()
        .expect("ParentTree");
    let elements = struct_elements(&doc);
    for (annot_id, annot) in &annotations {
        let key = annot
            .get(b"StructParent")
            .and_then(Object::as_i64)
            .unwrap_or_else(|_| panic!("annotation {annot_id:?} has no /StructParent"));
        let parent = number_tree_get(&doc, parent_tree, key)
            .unwrap_or_else(|| panic!("/StructParent {key} is not in the parent tree"));
        let parent_id = parent
            .as_reference()
            .expect("parent tree value is a reference");
        let parent = doc.get_dictionary(parent_id).expect("parent element");
        assert_eq!(name_of(parent, b"S").as_deref(), Some("Link"));
        let claimed_by: Vec<ObjectId> = elements
            .iter()
            .filter(|(_, e)| objr_targets(&doc, e).contains(annot_id))
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(
            claimed_by,
            vec![parent_id],
            "annotation {annot_id:?} is in exactly one Link element"
        );
    }
}

/// The kinds of a structure element's kids, in order: `MC` for marked
/// content, the structure type for a child element, `OBJR` for an
/// annotation. Consecutive marked content collapses to one `MC`.
fn kid_kinds(doc: &Document, dict: &Dictionary) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in kids(dict) {
        let kind = match resolve(doc, &k) {
            Object::Integer(_) => "MC".to_string(),
            Object::Dictionary(d) if d.get(b"MCID").is_ok() => "MC".to_string(),
            Object::Dictionary(d) if name_of(d, b"Type").as_deref() == Some("OBJR") => {
                "OBJR".to_string()
            }
            Object::Dictionary(d) => name_of(d, b"S").unwrap_or_default(),
            other => format!("{other:?}"),
        };
        if kind == "MC" && out.last().is_some_and(|l| l == "MC") {
            continue;
        }
        out.push(kind);
    }
    out
}

fn heading_titles(doc: &Document) -> Vec<String> {
    struct_elements(doc)
        .iter()
        .filter(|(_, e)| name_of(e, b"S").is_some_and(|s| s.starts_with('H')))
        .filter_map(|(_, e)| lopdf::decode_text_string(e.get(b"T").ok()?).ok())
        .collect()
}

// ── one Link element per annotation ─────────────────────────────────────

const LINK_SPANNING_BLOCKS: [&str; 4] = [
    r#"<a href="https://example.com" style="display:block"><h2>Title</h2>More text</a>"#,
    r#"<a href="https://example.com/post" style="display:block">Read the post<p>Summary</p></a>"#,
    r#"<a href=x style=display:block><b style=display:block>A</b><i style=display:block>B</i></a>"#,
    r#"<a href=x style=display:block><div style=opacity:.5><p>Faded</p></div><p>Plain</p></a>"#,
];

#[test]
fn a_link_spanning_blocks_puts_its_annotation_in_one_link_element() {
    for body in LINK_SPANNING_BLOCKS {
        let html = format!("{PAGE}<h1>Links</h1>{body}");
        let pdf = render_ua(&html).unwrap_or_else(|e| panic!("{body}: {e}"));
        assert_each_annotation_in_one_link(&pdf);
    }
}

#[test]
fn a_link_spanning_blocks_renders_when_tagged() {
    for body in LINK_SPANNING_BLOCKS {
        let pdf = render_tagged(&format!("{PAGE}<h1>Links</h1>{body}"));
        assert_each_annotation_in_one_link(&pdf);
    }
}

#[test]
fn a_link_spanning_pages_has_each_annotation_once() {
    // The link's paragraphs fall on two pages: one annotation per page,
    // each in one Link element.
    let pdf = render_ua(&format!(
        "{PAGE}<h1>Links</h1><a href=\"https://example.com\" style=\"display:block\">\
         <p style=\"height:900px\">First</p><p>Second</p></a>"
    ))
    .expect("render");
    let doc = load(&pdf);
    assert_eq!(doc.get_pages().len(), 2);
    assert_eq!(link_annotations(&doc).len(), 2);
    assert_each_annotation_in_one_link(&pdf);
    // Each annotation is in the Link element whose text is on its page.
    for (page_number, page_id) in doc.get_pages() {
        let page = doc.get_dictionary(page_id).expect("page");
        let annots = resolve(&doc, page.get(b"Annots").expect("Annots"))
            .as_array()
            .expect("Annots array");
        for annot in annots {
            let annot = annot.as_reference().expect("annotation reference");
            let (_, link) = struct_elements(&doc)
                .into_iter()
                .find(|(_, e)| objr_targets(&doc, e).contains(&annot))
                .expect("a Link element");
            assert!(
                content_pages(&doc, link).contains(&page_id),
                "the annotation on page {page_number} is with the text on that page"
            );
        }
    }
}

// ── inline boxes keep their place in the reading order ──────────────────

#[test]
fn an_inline_svg_sits_between_the_text_around_it() {
    let pdf = render_ua(&format!(
        "{PAGE}<h1>Settings</h1><p>Press <svg role=\"img\" aria-label=\"the gear icon\" \
         width=\"12\" height=\"12\"><circle cx=\"6\" cy=\"6\" r=\"5\"/></svg> to open settings.</p>"
    ))
    .expect("render");
    let doc = load(&pdf);
    let orders: Vec<Vec<String>> = struct_elements(&doc)
        .iter()
        .filter(|(_, e)| name_of(e, b"S").as_deref() == Some("P"))
        .map(|(_, e)| kid_kinds(&doc, e))
        .filter(|k| k.iter().any(|kind| kind == "Figure"))
        .collect();
    assert_eq!(orders, vec![vec!["MC", "Figure", "MC"]]);
}

#[test]
fn an_inline_svg_in_a_link_paragraph_keeps_its_place() {
    let pdf = render_ua(&format!(
        "{PAGE}<h1>Settings</h1><p><a href=\"https://example.com\">Docs</a> then \
         <svg role=\"img\" aria-label=\"gear\" width=\"12\" height=\"12\"><circle cx=\"6\" \
         cy=\"6\" r=\"5\"/></svg> and more.</p>"
    ))
    .expect("render");
    let doc = load(&pdf);
    let orders: Vec<Vec<String>> = struct_elements(&doc)
        .iter()
        .filter(|(_, e)| name_of(e, b"S").as_deref() == Some("P"))
        .map(|(_, e)| kid_kinds(&doc, e))
        .filter(|k| k.iter().any(|kind| kind == "Figure"))
        .collect();
    assert_eq!(orders, vec![vec!["Link", "MC", "Figure", "MC"]]);
    assert_each_annotation_in_one_link(&pdf);
}

// ── headings get a title from their content ─────────────────────────────

#[test]
fn a_heading_whose_text_is_in_a_flex_item_has_a_title() {
    let pdf = render_ua(&format!(
        "{PAGE}<h1 style=\"display:flex;gap:8px\"><svg aria-hidden=\"true\" width=\"16\" \
         height=\"16\"></svg>Dashboard</h1><p>Body</p>"
    ))
    .expect("PDF/UA accepts the heading");
    assert_eq!(heading_titles(&load(&pdf)), vec!["Dashboard".to_string()]);
}

#[test]
fn a_heading_with_a_block_child_has_a_title() {
    let pdf = render_ua(&format!(
        "{PAGE}<h1>Annual report<small style=\"display:block\">2026</small></h1><p>Body</p>"
    ))
    .expect("PDF/UA accepts the heading");
    assert_eq!(
        heading_titles(&load(&pdf)),
        vec!["Annual report 2026".to_string()]
    );
}

#[test]
fn a_heading_holding_only_an_image_takes_its_alt_text() {
    let pdf = render_ua(&format!(
        "{PAGE}<h1><svg role=\"img\" aria-label=\"Acme\" width=\"40\" height=\"20\" \
         style=\"display:block\"><rect width=\"40\" height=\"20\"/></svg></h1><p>Body</p>"
    ))
    .expect("PDF/UA accepts the heading");
    assert_eq!(heading_titles(&load(&pdf)), vec!["Acme".to_string()]);
}

// ── links in page margin boxes ──────────────────────────────────────────

const RUNNING_LINK_CSS: &str = "@page{@top-right{content:element(h)}} .h{position:running(h)}";
const RUNNING_LINK_BODY: &str =
    "<div class=\"h\"><a href=\"https://example.com\">example.com</a></div><h1>Body</h1>";

#[test]
fn a_link_in_a_margin_box_is_in_a_link_element() {
    let mut assets = fulgur::AssetBundle::new();
    assets.add_css(RUNNING_LINK_CSS);
    let pdf = Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .title("Test")
        .assets(assets)
        .build()
        .render(&format!("{PAGE}{RUNNING_LINK_BODY}"))
        .expect("render");
    let doc = load(&pdf);
    let annotations = link_annotations(&doc);
    assert_eq!(annotations.len(), 1, "only the margin box draws the link");
    assert_each_annotation_in_one_link(&pdf);
    // The link's text stays a header artifact: the Link element holds
    // only the annotation.
    let links: Vec<Vec<String>> = struct_elements(&doc)
        .iter()
        .filter(|(_, e)| name_of(e, b"S").as_deref() == Some("Link"))
        .map(|(_, e)| kid_kinds(&doc, e))
        .collect();
    assert_eq!(links, vec![vec!["OBJR"]]);
}

#[test]
fn a_link_in_a_margin_box_from_a_style_element_is_in_a_link_element() {
    let pdf = render_ua(&format!(
        "<!DOCTYPE html><html lang=\"en\"><title>Test</title>\
         <style>{RUNNING_LINK_CSS}</style><body>{RUNNING_LINK_BODY}"
    ))
    .expect("render");
    assert_each_annotation_in_one_link(&pdf);
}

fn ua_error(body: &str) -> Option<String> {
    fulgur::Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .title("T")
        .build()
        .render(&format!(
            "<!DOCTYPE html><html lang=\"en\"><title>T</title><body>{body}</body></html>"
        ))
        .err()
        .map(|e| e.to_string())
}

#[test]
fn headings_must_not_skip_a_level() {
    let err = ua_error("<h1>Report</h1><h3>Detail</h3>").expect("h1 then h3 is refused");
    assert!(
        err.contains("h1 is followed by h3") && err.contains("Detail"),
        "{err}"
    );
    let err = ua_error("<h2>Section</h2><p>x</p>").expect("starting at h2 is refused");
    assert!(err.contains("first heading is h2"), "{err}");
    // Going back up any number of levels is fine.
    assert_eq!(
        ua_error("<h1>A</h1><h2>B</h2><h3>C</h3><h2>D</h2><h1>E</h1>"),
        None
    );
    // A heading that is not drawn does not count.
    assert_eq!(
        ua_error("<h1>A</h1><h3 style=\"visibility:hidden\">x</h3><h2>B</h2>"),
        None
    );
}

#[test]
fn a_table_caption_is_a_caption_element() {
    // `<caption>` is a Caption structure element, holding the caption's text,
    // beside its table, rather than an anonymous paragraph.
    let html =
        format!("{PAGE}<table><caption>Quarterly totals</caption><tr><td>1</td></tr></table>");
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    let elements = struct_elements(&doc);
    let captions: Vec<&Dictionary> = elements
        .iter()
        .filter(|(_, d)| name_of(d, b"S").as_deref() == Some("Caption"))
        .map(|(_, d)| *d)
        .collect();
    assert_eq!(captions.len(), 1, "one Caption element");
    assert_eq!(kid_kinds(&doc, captions[0]), ["MC"], "it holds the text");
    let parent_kids: Vec<Vec<String>> = elements
        .iter()
        .filter(|(_, d)| kid_kinds(&doc, d).contains(&"Caption".to_string()))
        .map(|(_, d)| kid_kinds(&doc, d))
        .collect();
    assert_eq!(
        parent_kids,
        [["Caption", "Table"]],
        "Caption before its Table"
    );
}

#[test]
fn a_table_with_a_caption_passes_pdf_ua() {
    let html = format!(
        "{PAGE}<table><caption>Quarterly totals</caption><tr><th>Q</th></tr><tr><td>1</td></tr></table>"
    );
    render_ua(&html).expect("PDF/UA validation");
}

/// The `/Alt` of every Figure structure element.
fn figure_alts(doc: &Document) -> Vec<String> {
    struct_elements(doc)
        .into_iter()
        .filter(|(_, d)| name_of(d, b"S").as_deref() == Some("Figure"))
        .filter_map(|(_, d)| match d.get(b"Alt").ok()? {
            Object::String(bytes, _) => Some(if bytes.starts_with(&[0xFE, 0xFF]) {
                let units: Vec<u16> = bytes[2..]
                    .chunks(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            } else {
                String::from_utf8_lossy(bytes).into_owned()
            }),
            _ => None,
        })
        .collect()
}

#[test]
fn an_svg_labelled_by_another_element_takes_its_text() {
    // `aria-labelledby` names the element whose text is the label; an SVG
    // with only that has a text alternative, so PDF/UA accepts it.
    let pdf = render_ua(&format!(
        "{PAGE}<h1>Report</h1><p id=\"cap\">Monthly  sales</p>\
         <svg role=\"img\" aria-labelledby=\"cap\" width=\"20\" height=\"20\">\
         <rect width=\"20\" height=\"20\"/></svg>"
    ))
    .expect("PDF/UA accepts an SVG labelled by another element");
    assert_eq!(figure_alts(&load(&pdf)), ["Monthly sales"]);
}

#[test]
fn aria_labelledby_comes_before_aria_label_and_joins_its_ids() {
    let pdf = render_ua(&format!(
        "{PAGE}<h1>Report</h1><span id=\"a\">Sales</span> <span id=\"b\">2026</span>\
         <svg role=\"img\" aria-labelledby=\"a missing b\" aria-label=\"ignored\" \
         width=\"20\" height=\"20\"><rect width=\"20\" height=\"20\"/></svg>"
    ))
    .expect("PDF/UA");
    assert_eq!(figure_alts(&load(&pdf)), ["Sales 2026"]);
}

#[test]
fn an_inline_role_img_holds_its_text_in_place() {
    // The text of `<span role="img">` is the content of its Figure, between
    // the paragraph's text before and after it, not an empty Figure at the
    // end with the stars left in the paragraph.
    let pdf = render_ua(&format!(
        "{PAGE}<h1>Review</h1><p>Rating <span role=\"img\" aria-label=\"4 of 5 stars\">\u{2605}\u{2605}\u{2605}\u{2605}\u{2606}</span> overall</p>"
    ))
    .expect("PDF/UA accepts the rating");
    let doc = load(&pdf);
    let elements = struct_elements(&doc);
    let kinds = |s: &str| -> Vec<Vec<String>> {
        elements
            .iter()
            .filter(|(_, d)| name_of(d, b"S").as_deref() == Some(s))
            .map(|(_, d)| kid_kinds(&doc, d))
            .collect()
    };
    assert_eq!(kinds("P"), [["MC", "Figure", "MC"]]);
    assert_eq!(kinds("Figure"), [["MC"]]);
    assert_eq!(figure_alts(&doc), ["4 of 5 stars"]);
}
