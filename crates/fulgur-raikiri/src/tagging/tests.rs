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
    assert_eq!(outline(&pdf), "Document[P[#] P[#] P[#]]");
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
    let second = pdf.get_page_content(pdf.get_pages()[&2]).unwrap();
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
        "<div style='column-count: 2'><p>Left</p><p>Right</p></div>",
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
    let content =
        String::from_utf8_lossy(&pdf.get_page_content(pdf.get_pages()[&1]).unwrap()).into_owned();
    assert!(content.contains("/Artifact"), "{content}");
    assert!(content.contains("/Footer"), "{content}");
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
        "<h1>Report</h1><p>Body with <a href='https://example.com/'>a link</a>.</p>\
         <ul><li>Item</li></ul>",
        &config,
    );
    let catalog = pdf.catalog().unwrap();
    assert!(catalog.has(b"StructTreeRoot"));
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
