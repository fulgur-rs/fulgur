use crate::render;
use fulgur_core::Config;
use lopdf::{Dictionary, Document, Object};

fn tagged_pdf(html: &str, config: &Config) -> Document {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.html");
    std::fs::write(&path, html).unwrap();
    let bytes = render(&path, config).unwrap();
    Document::load_mem(&bytes).unwrap()
}

fn tagged() -> Config {
    Config::builder().tagged(true).build()
}

fn resolve<'a>(pdf: &'a Document, object: &'a Object) -> &'a Object {
    match object {
        Object::Reference(id) => pdf.get_object(*id).unwrap(),
        object => object,
    }
}

/// The structure tree as `Tag[children]`, with `#` for a marked-content
/// reference and `@` for an annotation reference.
fn outline(pdf: &Document) -> String {
    let catalog = pdf.catalog().unwrap();
    let root = resolve(pdf, catalog.get(b"StructTreeRoot").unwrap())
        .as_dict()
        .unwrap();
    let mut out = String::new();
    kids(pdf, root.get(b"K").unwrap(), &mut out);
    out
}

fn kids(pdf: &Document, object: &Object, out: &mut String) {
    match resolve(pdf, object) {
        Object::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(' ');
                }
                kids(pdf, item, out);
            }
        }
        Object::Integer(_) => out.push('#'),
        Object::Dictionary(dict) => element(pdf, dict, out),
        other => panic!("unexpected structure kid {other:?}"),
    }
}

fn element(pdf: &Document, dict: &Dictionary, out: &mut String) {
    match dict.get(b"Type").and_then(Object::as_name) {
        Ok(b"MCR") => return out.push('#'),
        Ok(b"OBJR") => return out.push('@'),
        _ => {}
    }
    out.push_str(std::str::from_utf8(dict.get(b"S").unwrap().as_name().unwrap()).unwrap());
    if let Ok(k) = dict.get(b"K") {
        out.push('[');
        kids(pdf, k, out);
        out.push(']');
    }
}

/// Every structure element with the role `tag`.
fn elements<'a>(pdf: &'a Document, tag: &[u8]) -> Vec<&'a Dictionary> {
    pdf.objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .filter(|dict| dict.get(b"S").and_then(Object::as_name).ok() == Some(tag))
        .collect()
}

fn text(dict: &Dictionary, key: &[u8]) -> String {
    lopdf::decode_text_string(dict.get(key).unwrap()).unwrap()
}

#[test]
fn untagged_output_has_no_structure_tree() {
    let pdf = tagged_pdf("<p>Hello</p>", &Config::default());
    assert!(!pdf.catalog().unwrap().has(b"StructTreeRoot"));
}

#[test]
fn headings_and_paragraphs_follow_document_order() {
    let pdf = tagged_pdf(
        "<section><h1>Title</h1><p>One <span>two</span> three</p></section><p>Four</p>",
        &tagged(),
    );
    assert_eq!(
        outline(&pdf),
        "Document[Div[H1[#] P[# Span[#] #]] P[#]]",
        "{}",
        outline(&pdf)
    );
    assert_eq!(text(elements(&pdf, b"H1")[0], b"T"), "Title");
}

#[test]
fn text_outside_classified_elements_gets_a_paragraph() {
    let pdf = tagged_pdf("Loose text<p>Para</p>", &tagged());
    assert_eq!(outline(&pdf), "Document[P[#] P[#]]");
}

#[test]
fn loose_text_around_a_paragraph_keeps_reading_order() {
    let pdf = tagged_pdf("Before<p>Middle</p>After", &tagged());
    let catalog = pdf.catalog().unwrap();
    let root = resolve(&pdf, catalog.get(b"StructTreeRoot").unwrap())
        .as_dict()
        .unwrap();
    let children = |dict: &Dictionary| match resolve(&pdf, dict.get(b"K").unwrap()) {
        Object::Array(kids) => kids.clone(),
        kid => vec![kid.clone()],
    };
    let top = children(root);
    assert_eq!(top.len(), 1);
    let doc = resolve(&pdf, &top[0]).as_dict().unwrap();
    let mut paragraphs = Vec::new();
    for kid in children(doc) {
        let paragraph = resolve(&pdf, &kid).as_dict().unwrap();
        assert_eq!(paragraph.get(b"S").unwrap().as_name().unwrap(), b"P");
        let mut text = String::new();
        for item in children(paragraph) {
            let id = match resolve(&pdf, &item) {
                Object::Integer(id) => *id,
                Object::Dictionary(dict) => dict.get(b"MCID").unwrap().as_i64().unwrap(),
                other => panic!("unexpected paragraph kid {other:?}"),
            };
            let page = pdf.get_pages()[&1];
            let mut content = lopdf::content::Content::decode(&pdf.get_page_content(page)).unwrap();
            let mut open = Vec::new();
            content.operations.retain(|op| {
                match op.operator.as_str() {
                    "BDC" => open.push(
                        op.operands[1]
                            .as_dict()
                            .ok()
                            .and_then(|d| d.get(b"MCID").ok())
                            .and_then(|v| v.as_i64().ok()),
                    ),
                    "BMC" => open.push(None),
                    "EMC" => {
                        open.pop();
                    }
                    "TJ" | "Tj" => return open.iter().rev().flatten().next() == Some(&id),
                    _ => {}
                }
                true
            });
            let mut copy = pdf.clone();
            copy.change_page_content(page, content.encode().unwrap())
                .unwrap();
            text.extend(copy.extract_text(&[1]).unwrap().split_whitespace());
        }
        paragraphs.push(text);
    }
    assert_eq!(paragraphs, ["Before", "Middle", "After"]);
}

#[test]
fn transparent_text_is_still_tagged() {
    let pdf = tagged_pdf("<p style='color: transparent'>Hidden ink</p>", &tagged());
    assert_eq!(outline(&pdf), "Document[P[#]]");
}

#[test]
fn list_items_have_label_and_body() {
    let pdf = tagged_pdf("<ol><li>First</li><li>Second</li></ol>", &tagged());
    assert_eq!(
        outline(&pdf),
        "Document[L[LI[Lbl[#] LBody[#]] LI[Lbl[#] LBody[#]]]]"
    );
    let list = elements(&pdf, b"L")[0];
    let attributes = resolve(&pdf, list.get(b"A").unwrap());
    assert!(
        format!("{attributes:?}").contains("Decimal"),
        "{attributes:?}"
    );
}

#[test]
fn tables_carry_header_scope() {
    let pdf = tagged_pdf(
        "<table><thead><tr><th scope=COL>H</th></tr></thead><tbody><tr><td>D</td></tr></tbody></table>",
        &tagged(),
    );
    assert_eq!(
        outline(&pdf),
        "Document[Table[THead[TR[TH[#]]] TBody[TR[TD[#]]]]]"
    );
    let header = elements(&pdf, b"TH")[0];
    let attributes = resolve(&pdf, header.get(b"A").unwrap());
    assert!(
        format!("{attributes:?}").contains("Column"),
        "{attributes:?}"
    );
}

#[test]
fn links_hold_their_content_and_annotation() {
    let pdf = tagged_pdf(
        "<p>See <a href='https://example.com/'>the site</a>.</p>",
        &tagged(),
    );
    assert_eq!(outline(&pdf), "Document[P[# Link[# @] #]]");
    let page = pdf.get_dictionary(pdf.get_pages()[&1]).unwrap();
    let annotation = resolve(&pdf, &page.get(b"Annots").unwrap().as_array().unwrap()[0])
        .as_dict()
        .unwrap();
    assert!(annotation.has(b"StructParent"));
    assert_eq!(text(annotation, b"Contents"), "the site");
}

#[test]
fn images_are_figures_with_alt_text() {
    let pdf = tagged_pdf(
        "<p><img alt='A chart' src='missing.png' width=10 height=10></p>",
        &tagged(),
    );
    let figure = elements(&pdf, b"Figure")[0];
    assert_eq!(text(figure, b"Alt"), "A chart");
}

#[test]
fn repeated_table_headers_are_tagged_once() {
    let rows: String = (0..40)
        .map(|i| format!("<tr><td>Row {i}</td></tr>"))
        .collect();
    let pdf = tagged_pdf(
        &format!(
            "<style>@page {{ size: 300px 200px; margin: 10px }} body {{ margin: 0 }}</style>\
             <table><thead><tr><th>Head</th></tr></thead><tbody>{rows}</tbody></table>"
        ),
        &tagged(),
    );
    assert!(pdf.get_pages().len() > 1);
    let header = elements(&pdf, b"TH");
    assert_eq!(header.len(), 1);
    let mut marks = String::new();
    kids(&pdf, header[0].get(b"K").unwrap(), &mut marks);
    assert_eq!(marks, "#");
    let second = pdf.get_page_content(pdf.get_pages()[&2]);
    assert!(
        String::from_utf8_lossy(&second).contains("/Artifact"),
        "the repeated header is an artifact on later pages"
    );
}

#[test]
fn fixed_boxes_are_tagged_on_their_first_page_only() {
    let pdf = tagged_pdf(
        "<style>@page { size: 300px 200px; margin: 10px } body { margin: 0 }</style>\
         <div style='position: fixed; top: 0; left: 0'><span>Fixed</span></div>\
         <p style='height: 150px'>One</p><p style='height: 150px'>Two</p>",
        &tagged(),
    );
    assert!(pdf.get_pages().len() > 1);
    let spans = elements(&pdf, b"Span");
    assert_eq!(spans.len(), 1);
    let mut marks = String::new();
    kids(&pdf, spans[0].get(b"K").unwrap(), &mut marks);
    assert_eq!(marks, "#");
}

#[test]
fn multicolumn_text_is_tagged() {
    let pdf = tagged_pdf(
        "<div style='column-count: 2; column-rule: 1px solid black'><p>Left</p><p>Right</p></div>",
        &tagged(),
    );
    assert_eq!(outline(&pdf), "Document[Div[P[#] P[#]]]");
}

#[test]
fn box_decorations_and_margin_boxes_are_artifacts() {
    let pdf = tagged_pdf(
        "<style>@page { @bottom-center { content: 'Footer' } } p { background: red }</style><p>Body</p>",
        &tagged(),
    );
    let content = String::from_utf8_lossy(&pdf.get_page_content(pdf.get_pages()[&1])).into_owned();
    assert!(content.contains("/Artifact"), "{content}");
    assert!(content.contains("/Footer"), "{content}");
    assert_eq!(outline(&pdf), "Document[P[#]]");
}

#[test]
fn text_shadows_are_artifacts() {
    let pdf = tagged_pdf(
        "<p style='text-shadow: 2px 2px red, 4px 4px blue'>Body</p>",
        &tagged(),
    );
    let content = String::from_utf8_lossy(&pdf.get_page_content(pdf.get_pages()[&1])).into_owned();
    assert!(content.contains("/Artifact"), "{content}");
    assert_eq!(outline(&pdf), "Document[P[#]]");
}

#[test]
fn pdf_ua_output_validates() {
    let config = Config::builder()
        .pdf_ua(true)
        .title("Report")
        .lang("en")
        .build();
    let pdf = tagged_pdf(
        "<style>p::before{content:'* ' / 'Note: '} p::after{content:' ~' / ''}</style>\
         <h1>Report</h1><p>Body with <a href='https://example.com/'>a link</a>.</p>\
         <ul><li>Item</li></ul>",
        &config,
    );
    let catalog = pdf.catalog().unwrap();
    assert!(catalog.has(b"StructTreeRoot"));
    assert_eq!(text(elements(&pdf, b"Span")[0], b"Alt"), "Note: ");
    assert!(catalog.has(b"Outlines"), "PDF/UA implies bookmarks");
}

fn pdf_ua() -> Config {
    Config::builder().pdf_ua(true).lang("en").build()
}

#[test]
fn pdf_ua_takes_the_title_from_the_html() {
    let pdf = tagged_pdf(
        "<html><head><title> From HTML </title></head><body><p>Body</p></body></html>",
        &pdf_ua(),
    );
    let info = resolve(&pdf, pdf.trailer.get(b"Info").unwrap())
        .as_dict()
        .unwrap();
    assert_eq!(text(info, b"Title"), "From HTML");
}

#[test]
fn pdf_ua_omits_invisible_figures_and_headings() {
    let config = Config::builder().pdf_ua(true).title("T").lang("en").build();
    let pdf = tagged_pdf(
        "<h1 style='visibility: hidden'>Gone</h1>\
         <p><img style='visibility: hidden' alt='gone' src='missing.png' width=10 height=10></p>",
        &config,
    );
    assert!(elements(&pdf, b"Figure").is_empty());
    assert!(elements(&pdf, b"H1").is_empty());
}

#[test]
fn image_links_are_named_by_their_alt_text() {
    let config = Config::builder().pdf_ua(true).title("T").lang("en").build();
    let pdf = tagged_pdf(
        "<p><a href='https://example.com/'><img alt='Home' src='missing.png' width=10 height=10></a></p>",
        &config,
    );
    let page = pdf.get_dictionary(pdf.get_pages()[&1]).unwrap();
    let annotation = resolve(&pdf, &page.get(b"Annots").unwrap().as_array().unwrap()[0])
        .as_dict()
        .unwrap();
    assert_eq!(text(annotation, b"Contents"), "Home");
}

#[test]
fn repeated_links_stay_in_the_structure_tree() {
    let pdf = tagged_pdf(
        "<style>@page { size: 300px 200px; margin: 10px } body { margin: 0 }</style>\
         <div style='position: fixed; top: 0; left: 0'><a href='https://example.com/'>Home</a></div>\
         <p style='height: 150px'>One</p><p style='height: 150px'>Two</p>",
        &tagged(),
    );
    let pages = pdf.get_pages();
    assert!(pages.len() > 1);
    for page in pages.values() {
        let page = pdf.get_dictionary(*page).unwrap();
        for annotation in page.get(b"Annots").unwrap().as_array().unwrap() {
            assert!(
                resolve(&pdf, annotation)
                    .as_dict()
                    .unwrap()
                    .has(b"StructParent")
            );
        }
    }
    let links = elements(&pdf, b"Link");
    assert_eq!(links.len(), 1);
    let mut marks = String::new();
    kids(&pdf, links[0].get(b"K").unwrap(), &mut marks);
    assert_eq!(marks, format!("#{}", " @".repeat(pages.len())));
}

#[test]
fn margin_boxes_map_to_header_footer_or_page_artifacts() {
    use krilla::tagging::ArtifactType;
    use raikiri_html::PageMarginBoxSlot::*;
    for slot in [TopLeftCorner, TopLeft, TopCenter, TopRight, TopRightCorner] {
        assert!(matches!(
            super::margin_box_artifact(slot),
            ArtifactType::Header
        ));
    }
    for slot in [
        BottomLeftCorner,
        BottomLeft,
        BottomCenter,
        BottomRight,
        BottomRightCorner,
    ] {
        assert!(matches!(
            super::margin_box_artifact(slot),
            ArtifactType::Footer
        ));
    }
    for slot in [
        LeftTop,
        LeftMiddle,
        LeftBottom,
        RightTop,
        RightMiddle,
        RightBottom,
    ] {
        assert!(matches!(
            super::margin_box_artifact(slot),
            ArtifactType::Page
        ));
    }
}

#[test]
fn list_style_types_map_to_list_numbering() {
    use fulgur_core::tagging::ListNumbering;
    use raikiri_html::computed::ComputedListStyleType as Style;
    let cases = [
        ("disc", ListNumbering::Disc),
        ("circle", ListNumbering::Circle),
        ("square", ListNumbering::Square),
        ("decimal", ListNumbering::Decimal),
        ("lower-alpha", ListNumbering::LowerAlpha),
        ("lower-latin", ListNumbering::LowerAlpha),
        ("upper-alpha", ListNumbering::UpperAlpha),
        ("upper-latin", ListNumbering::UpperAlpha),
        ("lower-roman", ListNumbering::LowerRoman),
        ("upper-roman", ListNumbering::UpperRoman),
        ("hiragana", ListNumbering::None),
    ];
    for (name, expected) in cases {
        assert_eq!(
            super::list_numbering(&Style::Named(name.into())),
            expected,
            "{name}"
        );
    }
    assert_eq!(super::list_numbering(&Style::Disc), ListNumbering::Disc);
    assert_eq!(super::list_numbering(&Style::None), ListNumbering::None);
    assert_eq!(
        super::list_numbering(&Style::String("-".into())),
        ListNumbering::None
    );
}

#[test]
fn every_tag_converts_to_krilla() {
    use fulgur_core::tagging::{ListNumbering, PdfTag, TableHeaderScope};
    use krilla::tagging::TagKind;
    let mut tags = vec![
        PdfTag::P,
        PdfTag::H { level: 0 },
        PdfTag::H { level: 9 },
        PdfTag::Div,
        PdfTag::Span,
        PdfTag::Figure,
        PdfTag::Lbl,
        PdfTag::LBody,
        PdfTag::Li,
        PdfTag::Table,
        PdfTag::THead,
        PdfTag::TBody,
        PdfTag::TFoot,
        PdfTag::Tr,
        PdfTag::Td,
        PdfTag::Link,
    ];
    tags.extend(
        [
            ListNumbering::None,
            ListNumbering::Disc,
            ListNumbering::Circle,
            ListNumbering::Square,
            ListNumbering::Decimal,
            ListNumbering::LowerRoman,
            ListNumbering::UpperRoman,
            ListNumbering::LowerAlpha,
            ListNumbering::UpperAlpha,
        ]
        .map(|numbering| PdfTag::L { numbering }),
    );
    tags.extend(
        [
            TableHeaderScope::Row,
            TableHeaderScope::Column,
            TableHeaderScope::Both,
        ]
        .map(|scope| PdfTag::Th { scope }),
    );
    for tag in &tags {
        let kind = super::krilla_tag(tag, None, None);
        match tag {
            PdfTag::H { .. } => assert!(matches!(kind, TagKind::Hn(_))),
            PdfTag::L { .. } => assert!(matches!(kind, TagKind::L(_))),
            PdfTag::Th { .. } => assert!(matches!(kind, TagKind::TH(_))),
            _ => {}
        }
    }
}

#[test]
fn pdf_ua_skips_undisplayed_and_decorative_images() {
    let config = Config::builder().pdf_ua(true).title("T").lang("en").build();
    let pdf = tagged_pdf(
        "<p>Text<img style='display: none' src='missing.png' width=10 height=10>\
         <img alt='' src='missing.png' width=10 height=10></p>",
        &config,
    );
    assert!(elements(&pdf, b"Figure").is_empty());
    assert_eq!(outline(&pdf), "Document[P[#]]");
}

#[test]
fn headings_without_text_take_their_accessible_name() {
    let config = Config::builder().pdf_ua(true).title("T").lang("en").build();
    let pdf = tagged_pdf(
        "<h1 aria-label='Named'></h1><h2><img alt='Logo' src='missing.png' width=10 height=10></h2>",
        &config,
    );
    assert_eq!(text(elements(&pdf, b"H1")[0], b"T"), "Named");
    assert_eq!(text(elements(&pdf, b"H2")[0], b"T"), "Logo");
}

#[test]
fn aria_labelledby_names_links_and_headings() {
    let config = Config::builder().pdf_ua(true).title("T").lang("en").build();
    let pdf = tagged_pdf(
        "<p id='a'>Chapter</p><p id='b'>One</p><h1 aria-labelledby='a missing b'></h1>\
         <p><a href='https://example.com/' aria-labelledby='b' style='display:inline-block;width:20px;height:10px'></a></p>",
        &config,
    );
    assert_eq!(text(elements(&pdf, b"H1")[0], b"T"), "Chapter One");
    let page = pdf.get_dictionary(pdf.get_pages()[&1]).unwrap();
    let annotation = resolve(&pdf, &page.get(b"Annots").unwrap().as_array().unwrap()[0])
        .as_dict()
        .unwrap();
    assert_eq!(text(annotation, b"Contents"), "One");
}

#[test]
fn pdf_ua_takes_the_language_from_the_html() {
    let config = Config::builder().pdf_ua(true).title("T").build();
    let pdf = tagged_pdf("<html lang='ja'><body><p>本文</p></body></html>", &config);
    let catalog = pdf.catalog().unwrap();
    assert_eq!(text(catalog, b"Lang"), "ja");
}

#[test]
fn pdf_ua_without_a_language_is_rejected() {
    let config = Config::builder().pdf_ua(true).title("T").build();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.html");
    std::fs::write(&path, "<p>Body</p>").unwrap();
    let error = render(&path, &config).unwrap_err();
    assert!(error.to_string().contains("document language"), "{error}");
}

#[test]
fn pdf_ua_treats_a_blank_language_as_unset() {
    let config = Config::builder().pdf_ua(true).title("T").lang(" ").build();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.html");
    std::fs::write(&path, "<p>Body</p>").unwrap();
    let error = render(&path, &config).unwrap_err();
    assert!(error.to_string().contains("document language"), "{error}");
    std::fs::write(&path, "<html lang='de'><p>Body</p></html>").unwrap();
    let bytes = render(&path, &config).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("/Lang (de)"));
}

#[test]
fn running_elements_are_footer_and_header_artifacts() {
    let config = Config::builder().pdf_ua(true).title("T").lang("en").build();
    let pdf = tagged_pdf(
        "<style>@page {size:300px 200px; margin:40px; @top-center {content: element(hdr)}} \
         body {margin:0} .hdr {position: running(hdr)}</style>\
         <div class='hdr'><a href='https://example.com/'>Home</a></div><p>Body</p>",
        &config,
    );
    let content = String::from_utf8_lossy(&pdf.get_page_content(pdf.get_pages()[&1])).into_owned();
    assert!(content.contains("/Header"), "{content}");
    let page = pdf.get_dictionary(pdf.get_pages()[&1]).unwrap();
    let annotation = resolve(&pdf, &page.get(b"Annots").unwrap().as_array().unwrap()[0])
        .as_dict()
        .unwrap();
    assert_eq!(text(annotation, b"Contents"), "Home");
}

#[test]
fn generated_content_and_marker_images_are_tagged() {
    let pdf = tagged_pdf(
        "<style>p::before{content:'Pre '} p::after{content:' Post'} \
         ul{list-style-image:url(\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' \
         width='8' height='8'><rect width='8' height='8'/></svg>\")}</style>\
         <p>Body</p><ul><li>Item</li></ul>",
        &tagged(),
    );
    assert_eq!(outline(&pdf), "Document[P[# # #] L[LI[Lbl[#] LBody[#]]]]");
}

#[test]
fn generated_content_images_are_tagged_in_reading_order() {
    let image = "url(\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' \
                 width='8' height='8'><rect width='8' height='8'/></svg>\")";
    let pdf = tagged_pdf(
        &format!(
            "<style>p::before{{content:{image}}} p::after{{content:{image}}}</style>\
             <p>Body</p>"
        ),
        &tagged(),
    );
    assert_eq!(outline(&pdf), "Document[P[# # #]]");
}

/// What the marked content of each MCID on the first page draws: `T` for
/// text, `I` for an image, whether an XObject or filled vector paths.
fn mcid_kinds(pdf: &Document) -> std::collections::HashMap<i64, char> {
    let page_id = *pdf.get_pages().values().next().unwrap();
    let content = lopdf::content::Content::decode(&pdf.get_page_content(page_id)).unwrap();
    let mut kinds = std::collections::HashMap::new();
    let mut open: Vec<Option<i64>> = Vec::new();
    for operation in &content.operations {
        match operation.operator.as_str() {
            "BDC" => open.push(
                operation
                    .operands
                    .get(1)
                    .and_then(|properties| match properties {
                        Object::Dictionary(dict) => dict.get(b"MCID").ok(),
                        _ => None,
                    })
                    .and_then(|mcid| mcid.as_i64().ok()),
            ),
            "BMC" => open.push(None),
            "EMC" => {
                open.pop();
            }
            operator @ ("Do" | "f" | "f*" | "Tj" | "TJ") => {
                if let Some(&Some(mcid)) = open.iter().rev().find(|mcid| mcid.is_some()) {
                    let kind = if operator.starts_with('T') { 'T' } else { 'I' };
                    // Text wins over the paths of its own decorations.
                    if kinds.get(&mcid) != Some(&'T') {
                        kinds.insert(mcid, kind);
                    }
                }
            }
            _ => {}
        }
    }
    kinds
}

/// The content of the first `P` element in reading order, as `mcid_kinds`.
fn paragraph_leaves(pdf: &Document) -> String {
    let kinds = mcid_kinds(pdf);
    let paragraph = elements(pdf, b"P")[0];
    let mcid = |object: &Object| match resolve(pdf, object) {
        Object::Integer(mcid) => *mcid,
        Object::Dictionary(dict) => dict.get(b"MCID").unwrap().as_i64().unwrap(),
        other => panic!("unexpected paragraph kid {other:?}"),
    };
    let leaves: Vec<i64> = match resolve(pdf, paragraph.get(b"K").unwrap()) {
        Object::Array(items) => items.iter().map(mcid).collect(),
        single => vec![mcid(single)],
    };
    leaves.iter().map(|mcid| kinds[mcid]).collect()
}

#[test]
fn after_images_follow_the_element_text() {
    let image = "url(\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' \
                 width='8' height='8'><rect width='8' height='8'/></svg>\")";
    let pdf = tagged_pdf(
        &format!(
            "<style>p::before{{content:{image}}} p::after{{content:{image}}}</style>\
             <p>Body</p>"
        ),
        &tagged(),
    );
    assert_eq!(paragraph_leaves(&pdf), "ITI");
}

#[test]
fn generated_text_and_images_keep_their_content_list_order() {
    let image = "url(\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' \
                 width='8' height='8'><rect width='8' height='8'/></svg>\")";
    // Paint draws the line's image before its text; the structure follows
    // the `content` list instead.
    let pdf = tagged_pdf(
        &format!("<style>p::before{{content:'Note: ' {image} ' end '}}</style><p>Body</p>"),
        &tagged(),
    );
    assert_eq!(paragraph_leaves(&pdf), "TITT");
}

#[test]
fn generated_images_on_mixed_direction_lines_keep_paint_order() {
    let image = "url(\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' \
                 width='8' height='8'><rect width='8' height='8'/></svg>\")";
    // A line that mixes bidi directions gives the image no offset, so it
    // keeps its place before the line's text.
    let pdf = tagged_pdf(
        &format!(
            "<style>p::before{{content:'Note: ' {image} ' \u{5e9}\u{5dc}\u{5d5}\u{5dd} '}}</style>\
             <p>Body</p>"
        ),
        &tagged(),
    );
    // The number of text runs depends on the fonts that cover Hebrew.
    let leaves = paragraph_leaves(&pdf);
    assert!(leaves.starts_with('I'), "{leaves}");
    assert!(leaves[1..].len() >= 2 && leaves[1..].chars().all(|kind| kind == 'T'));
}

#[test]
fn generated_content_with_alternative_text_is_a_span_with_alt() {
    let image = "url(\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' \
                 width='8' height='8'><rect width='8' height='8'/></svg>\")";
    let pdf = tagged_pdf(
        &format!(
            "<style>p::before{{content:{image} ' Note: ' / attr(data-kind) ' note'}}</style>\
             <p data-kind=Warning>Body</p>"
        ),
        &tagged(),
    );
    // The image and the text of the pseudo-element share the Span, read in
    // their content-list order, and the element's own text follows it.
    assert_eq!(outline(&pdf), "Document[P[Span[# #] #]]");
    let span = elements(&pdf, b"Span")[0];
    assert_eq!(text(span, b"Alt"), "Warning note");
}

#[test]
fn generated_content_with_empty_alternative_text_is_an_artifact() {
    let image = "url(\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' \
                 width='8' height='8'><rect width='8' height='8'/></svg>\")";
    let pdf = tagged_pdf(
        &format!(
            "<style>p::before{{content:{image} ' * ' / ''}} \
             p::after{{content:' end'}}</style><p>Body</p>"
        ),
        &tagged(),
    );
    // The decorative `::before` leaves only the text and the `::after`.
    assert_eq!(paragraph_leaves(&pdf), "TT");
    assert!(elements(&pdf, b"Span").is_empty());
}

#[test]
fn marker_alternative_text_names_the_label() {
    let pdf = tagged_pdf(
        "<style>li::marker{content:'* ' / 'Important'}</style><ul><li>First</li></ul>",
        &tagged(),
    );
    assert_eq!(outline(&pdf), "Document[L[LI[Lbl[#] LBody[#]]]]");
    let label = elements(&pdf, b"Lbl")[0];
    assert_eq!(text(label, b"Alt"), "Important");
}

#[test]
fn decorative_markers_are_artifacts() {
    // The label stays, but the marker it held is drawn as an artifact.
    let pdf = tagged_pdf(
        "<style>li::marker{content:'* ' / ''}</style><ul><li>First</li></ul>",
        &tagged(),
    );
    assert_eq!(outline(&pdf), "Document[L[LI[Lbl[] LBody[#]]]]");
}

#[test]
fn markers_outside_list_items_belong_to_their_element() {
    // A list item that is not an `<li>` has no label, so its marker is
    // content of the element, before its text.
    let pdf = tagged_pdf(
        "<p style='display:list-item;list-style:inside decimal'>Body</p>",
        &tagged(),
    );
    assert!(elements(&pdf, b"Lbl").is_empty());
    let leaves = paragraph_leaves(&pdf);
    assert!(leaves.len() >= 2 && leaves.chars().all(|kind| kind == 'T'));
}

#[test]
fn links_are_named_by_nested_image_alt_text() {
    let pdf = tagged_pdf(
        "<p><a href='https://example.com/'><span> \
         <img alt='Home' src='missing.png' width=10 height=10></span></a></p>",
        &tagged(),
    );
    let page = pdf.get_dictionary(pdf.get_pages()[&1]).unwrap();
    let annotation = resolve(&pdf, &page.get(b"Annots").unwrap().as_array().unwrap()[0])
        .as_dict()
        .unwrap();
    assert_eq!(text(annotation, b"Contents"), "Home");
}

#[test]
fn fixed_generated_content_and_marker_images_are_tagged_once() {
    let pdf = tagged_pdf(
        "<style>@page { size: 300px 200px; margin: 10px } body { margin: 0 } \
         span::before { content: 'Pre ' } \
         ul { list-style-image: url(\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' \
         width='8' height='8'><rect width='8' height='8'/></svg>\") }</style>\
         <div style='position: fixed; top: 0; left: 0'><span>Fixed</span>\
         <ul><li>Item</li></ul></div>\
         <p style='height: 150px'>One</p><p style='height: 150px'>Two</p>",
        &tagged(),
    );
    assert!(pdf.get_pages().len() > 1);
    assert_eq!(
        outline(&pdf),
        "Document[Div[Span[# #] L[LI[Lbl[#] LBody[#]]]] P[#] P[#]]"
    );
}
