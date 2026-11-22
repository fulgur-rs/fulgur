//! PDF/UA requires every piece of page content to be either tagged (part
//! of the structure tree, so assistive technology reads it) or marked as an
//! artifact (decoration, running headers and footers). These tests read the
//! page content streams back and check both, element by element.

use fulgur::Engine;
use lopdf::content::Content;
use lopdf::{Document, Object};

fn render(html: &str) -> Vec<u8> {
    Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .title("Test")
        .build()
        .render(html)
        .expect("render")
}

/// Painting operators on page 1 that are outside any marked content:
/// neither tagged nor an artifact.
fn untagged_painting(pdf: &[u8]) -> Vec<String> {
    let document = Document::load_mem(pdf).expect("PDF should parse");
    let page_id = *document.get_pages().get(&1).expect("page 1");
    let content = Content::decode(&document.get_page_content(page_id)).expect("content");
    let mut depth = 0usize;
    let mut untagged = Vec::new();
    for op in &content.operations {
        match op.operator.as_str() {
            "BDC" | "BMC" => depth += 1,
            "EMC" => depth = depth.saturating_sub(1),
            "TJ" | "Tj" | "'" | "\"" | "f" | "f*" | "F" | "S" | "s" | "B" | "B*" | "b" | "b*"
            | "Do" | "sh"
                if depth == 0 =>
            {
                untagged.push(op.operator.clone())
            }
            _ => {}
        }
    }
    untagged
}

/// The artifact types (`/Pagination`, `/Layout`, …) and subtypes
/// (`/Header`, `/Footer`) used on page 1, and how many artifacts have no
/// properties.
fn artifacts(pdf: &[u8]) -> Vec<String> {
    let document = Document::load_mem(pdf).expect("PDF should parse");
    let page_id = *document.get_pages().get(&1).expect("page 1");
    let content = Content::decode(&document.get_page_content(page_id)).expect("content");
    let mut found = Vec::new();
    for op in &content.operations {
        let is_artifact = op
            .operands
            .first()
            .and_then(|o| o.as_name().ok())
            .is_some_and(|n| n == b"Artifact");
        if !is_artifact {
            continue;
        }
        match op.operands.get(1) {
            Some(Object::Dictionary(d)) => {
                for key in [b"Type".as_slice(), b"Subtype".as_slice()] {
                    if let Ok(name) = d.get(key).and_then(Object::as_name) {
                        found.push(String::from_utf8_lossy(name).into_owned());
                    }
                }
            }
            _ => found.push("Artifact".into()),
        }
    }
    found
}

/// The structure types (`TD`, `P`, …) of the elements that directly hold
/// marked content, once per element.
fn elements_with_content(pdf: &[u8]) -> Vec<String> {
    let document = Document::load_mem(pdf).expect("PDF should parse");
    let holds_content = |k: &Object| match k {
        Object::Integer(_) => true,
        Object::Dictionary(d) => d.get(b"MCID").is_ok(),
        _ => false,
    };
    let mut types = Vec::new();
    for object in document.objects.values() {
        let Ok(dict) = object.as_dict() else { continue };
        let (Ok(s), Ok(k)) = (dict.get(b"S"), dict.get(b"K")) else {
            continue;
        };
        let Ok(s) = s.as_name() else { continue };
        let has = match k {
            Object::Array(items) => items.iter().any(holds_content),
            other => holds_content(other),
        };
        if has {
            types.push(String::from_utf8_lossy(s).into_owned());
        }
    }
    types.sort();
    types
}

fn count(types: &[String], name: &str) -> usize {
    types.iter().filter(|t| *t == name).count()
}

const PAGE: &str = r#"<!DOCTYPE html><html lang="en"><title>Test</title><body>"#;

#[test]
fn table_cell_text_is_tagged() {
    let pdf = render(&format!(
        "{PAGE}<h1>Rates</h1><table><thead><tr><th>Item</th><th>Price</th></tr></thead>\
         <tbody><tr><td>Design</td><td>100</td></tr></tbody></table>"
    ));
    assert_eq!(untagged_painting(&pdf), Vec::<String>::new());
    let types = elements_with_content(&pdf);
    assert_eq!(count(&types, "TH"), 2, "{types:?}");
    assert_eq!(count(&types, "TD"), 2, "{types:?}");
}

#[test]
fn text_in_a_div_or_an_unmapped_element_is_tagged() {
    let pdf = render(&format!(
        "{PAGE}<h1>Notes</h1><div>Loose text</div><blockquote>Quoted</blockquote>\
         <dl><dt>Term</dt><dd>Definition</dd></dl>"
    ));
    assert_eq!(untagged_painting(&pdf), Vec::<String>::new());
    let types = elements_with_content(&pdf);
    assert_eq!(count(&types, "Div"), 1, "{types:?}");
    // blockquote, dt and dd become paragraphs.
    assert_eq!(count(&types, "P"), 3, "{types:?}");
}

#[test]
fn text_next_to_a_block_in_a_cell_is_tagged() {
    // "Design" sits in an anonymous block beside the block <small>.
    let pdf = render(&format!(
        "{PAGE}<h1>Rates</h1><table><tr>\
         <td>Design<small style=\"display:block\">Two days</small></td></tr></table>"
    ));
    assert_eq!(untagged_painting(&pdf), Vec::<String>::new());
    assert_eq!(count(&elements_with_content(&pdf), "P"), 2);
}

#[test]
fn decoration_is_an_artifact() {
    let pdf = render(&format!(
        "{PAGE}<h1>Box</h1><div style=\"background:#eee;border:2px solid red;\
         box-shadow:2px 2px #999;padding:8px\">Inside</div>\
         <table style=\"border-collapse:collapse\"><tr><td style=\"border:1px solid\">Cell</td></tr></table>"
    ));
    assert_eq!(untagged_painting(&pdf), Vec::<String>::new());
    assert!(!artifacts(&pdf).is_empty());
}

#[test]
fn margin_boxes_are_header_and_footer_artifacts() {
    let pdf = render(&format!(
        "<style>@page {{ @top-center {{ content: \"Report\" }} \
         @bottom-right {{ content: \"Page \" counter(page) }} }}</style>{PAGE}<h1>Body</h1>"
    ));
    assert_eq!(untagged_painting(&pdf), Vec::<String>::new());
    let found = artifacts(&pdf);
    assert!(found.iter().any(|a| a == "Header"), "{found:?}");
    assert!(found.iter().any(|a| a == "Footer"), "{found:?}");
}

#[test]
fn content_under_opacity_is_tagged() {
    // A fractional opacity is drawn as a group; the content of the group
    // is tagged as content of the element that has the opacity.
    let pdf = render(&format!(
        "{PAGE}<h1>Faded</h1><div style=\"opacity:.5\"><p>Inner</p></div>"
    ));
    assert_eq!(untagged_painting(&pdf), Vec::<String>::new());
    assert!(elements_with_content(&pdf).contains(&"Div".to_string()));
}

#[test]
fn multicol_text_is_tagged() {
    let pdf = render(&format!(
        "{PAGE}<h1>News</h1><div style=\"columns:2\">One two three four five six \
         seven eight nine ten eleven twelve</div>"
    ));
    assert_eq!(untagged_painting(&pdf), Vec::<String>::new());
}

#[test]
fn an_inline_svg_is_a_figure_or_an_artifact() {
    let square = "<rect width=\"20\" height=\"20\"/>";
    let pdf = render(&format!(
        "{PAGE}<h1>Images</h1>\
         <div><svg role=\"img\" width=\"20\" height=\"20\"><title>Black square</title>{square}</svg></div>\
         <div><svg aria-hidden=\"true\" width=\"20\" height=\"20\">{square}</svg></div>"
    ));
    assert_eq!(untagged_painting(&pdf), Vec::<String>::new());
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("Black square"),
        "the title is the figure's alt text"
    );
    assert_eq!(count(&elements_with_content(&pdf), "Figure"), 1);
}

#[test]
fn an_svg_without_a_text_alternative_is_refused_and_says_how_to_fix_it() {
    let err = Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .title("Test")
        .build()
        .render(&format!(
            "{PAGE}<h1>Chart</h1><div><svg width=\"20\" height=\"20\"><rect width=\"20\" height=\"20\"/></svg></div>"
        ))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("cannot write PDF/UA-1") && err.contains("aria-label"),
        "{err}"
    );
}

#[test]
fn a_document_without_headings_gets_an_outline() {
    let pdf = render(&format!("{PAGE}<p>Just a paragraph.</p>"));
    let document = Document::load_mem(&pdf).expect("PDF should parse");
    let catalog = document.catalog().expect("catalog");
    assert!(catalog.get(b"Outlines").is_ok(), "PDF/UA needs an outline");
}

#[test]
fn empty_elements_are_left_out_of_the_structure_tree() {
    // The <p> inside the opacity group draws nothing of its own: its text
    // is tagged as the div's content.
    let pdf = render(&format!(
        "{PAGE}<h1>Faded</h1><div style=\"opacity:.5\"><p>Inner</p></div>"
    ));
    let s = String::from_utf8_lossy(&pdf);
    assert!(!s.contains("/S /P"), "no empty P element");
}
