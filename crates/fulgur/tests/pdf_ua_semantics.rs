//! Which elements become structure elements under PDF/UA: what is hidden
//! or paints nothing stays out of the structure tree, a picture is one
//! figure, a layout table is not a table, and elements read in the order
//! they are drawn. These tests render with PDF/UA on and read the
//! structure tree back.

use fulgur::Engine;
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object};
use std::collections::BTreeMap;

fn try_render(html: &str) -> Result<Vec<u8>, String> {
    Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .title("T")
        .build()
        .render(html)
        .map_err(|e| e.to_string())
}

fn render(html: &str) -> Vec<u8> {
    try_render(html).expect("render")
}

const PAGE: &str = r#"<!DOCTYPE html><html lang="en"><title>T</title><body>"#;

fn resolve<'a>(document: &'a Document, object: &'a Object) -> &'a Object {
    match object {
        Object::Reference(id) => document.get_object(*id).expect("referenced object"),
        other => other,
    }
}

fn kids<'a>(document: &'a Document, dict: &'a Dictionary) -> Vec<&'a Object> {
    match dict.get(b"K").map(|k| resolve(document, k)) {
        Ok(Object::Array(items)) => items.iter().map(|k| resolve(document, k)).collect(),
        Ok(other) => vec![other],
        Err(_) => Vec::new(),
    }
}

fn name(dict: &Dictionary, key: &[u8]) -> Option<String> {
    let n = dict.get(key).ok()?.as_name().ok()?;
    Some(String::from_utf8_lossy(n).into_owned())
}

/// The structure tree as text, `Type[kids]`, with marked-content ids as
/// numbers: `Document[H1[0] Div[P[2] P[1]]]`. Also returns the ids in
/// reading order.
fn structure(pdf: &[u8]) -> (String, Vec<i64>) {
    fn walk(document: &Document, elem: &Dictionary, text: &mut String, mcids: &mut Vec<i64>) {
        text.push_str(&name(elem, b"S").unwrap_or_default());
        text.push('[');
        for (i, kid) in kids(document, elem).into_iter().enumerate() {
            if i > 0 {
                text.push(' ');
            }
            match kid {
                Object::Integer(mcid) => {
                    mcids.push(*mcid);
                    text.push_str(&mcid.to_string());
                }
                Object::Dictionary(d) if d.get(b"MCID").is_ok() => {
                    let mcid = d.get(b"MCID").unwrap().as_i64().unwrap();
                    mcids.push(mcid);
                    text.push_str(&mcid.to_string());
                }
                Object::Dictionary(d) if d.get(b"S").is_ok() => walk(document, d, text, mcids),
                Object::Dictionary(_) => text.push_str("OBJR"),
                other => text.push_str(&format!("{other:?}")),
            }
        }
        text.push(']');
    }
    let document = Document::load_mem(pdf).expect("PDF should parse");
    let root = document
        .catalog()
        .expect("catalog")
        .get(b"StructTreeRoot")
        .map(|r| resolve(&document, r))
        .expect("a structure tree")
        .as_dict()
        .expect("StructTreeRoot dictionary");
    let mut text = String::new();
    let mut mcids = Vec::new();
    for kid in kids(&document, root) {
        if let Object::Dictionary(d) = kid {
            walk(&document, d, &mut text, &mut mcids);
        }
    }
    (text, mcids)
}

/// Every structure element: its type and its `/Alt`.
fn elements(pdf: &[u8]) -> Vec<(String, Option<String>)> {
    let document = Document::load_mem(pdf).expect("PDF should parse");
    let mut found = Vec::new();
    for object in document.objects.values() {
        let Ok(dict) = object.as_dict() else { continue };
        if name(dict, b"Type").as_deref() != Some("StructElem") {
            continue;
        }
        let alt = dict
            .get(b"Alt")
            .ok()
            .and_then(|a| a.as_str().ok())
            .map(|a| String::from_utf8_lossy(a).into_owned());
        found.push((name(dict, b"S").unwrap_or_default(), alt));
    }
    found.sort();
    found
}

/// The kinds of the kids of each structure element of type `tag`: the
/// type of a child element, or `content` for marked content.
fn kids_of(pdf: &[u8], tag: &str) -> Vec<Vec<String>> {
    let document = Document::load_mem(pdf).expect("PDF should parse");
    let mut found = Vec::new();
    for object in document.objects.values() {
        let Ok(dict) = object.as_dict() else { continue };
        if name(dict, b"Type").as_deref() != Some("StructElem")
            || name(dict, b"S").as_deref() != Some(tag)
        {
            continue;
        }
        let kinds = kids(&document, dict)
            .into_iter()
            .map(|kid| match kid {
                Object::Dictionary(d) if d.get(b"S").is_ok() => name(d, b"S").unwrap_or_default(),
                Object::Dictionary(d) if d.get(b"MCID").is_err() => "OBJR".to_string(),
                _ => "content".to_string(),
            })
            .collect();
        found.push(kinds);
    }
    found
}

fn count(elements: &[(String, Option<String>)], tag: &str) -> usize {
    elements.iter().filter(|(t, _)| t == tag).count()
}

/// Where each marked-content sequence on page 1 starts drawing text: the
/// page-space height (PDF y, up) of its first text, keyed by its id.
fn text_heights(pdf: &[u8]) -> BTreeMap<i64, f32> {
    fn numbers(operands: &[Object]) -> Vec<f32> {
        operands
            .iter()
            .map(|o| o.as_float().unwrap_or(0.0))
            .collect()
    }
    let document = Document::load_mem(pdf).expect("PDF should parse");
    let page_id = *document.get_pages().get(&1).expect("page 1");
    let content = Content::decode(&document.get_page_content(page_id)).expect("content");
    let mut ctm = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut saved = Vec::new();
    let mut text_origin = (0.0f32, 0.0f32);
    let mut open: Vec<Option<i64>> = Vec::new();
    let mut heights = BTreeMap::new();
    for op in &content.operations {
        match op.operator.as_str() {
            "q" => saved.push(ctm),
            "Q" => ctm = saved.pop().unwrap_or(ctm),
            "cm" => {
                let m = numbers(&op.operands);
                ctm = [
                    m[0] * ctm[0] + m[1] * ctm[2],
                    m[0] * ctm[1] + m[1] * ctm[3],
                    m[2] * ctm[0] + m[3] * ctm[2],
                    m[2] * ctm[1] + m[3] * ctm[3],
                    m[4] * ctm[0] + m[5] * ctm[2] + ctm[4],
                    m[4] * ctm[1] + m[5] * ctm[3] + ctm[5],
                ];
            }
            "Tm" => {
                let m = numbers(&op.operands);
                text_origin = (m[4], m[5]);
            }
            "BDC" => open.push(
                op.operands
                    .get(1)
                    .and_then(|o| o.as_dict().ok())
                    .and_then(|d| d.get(b"MCID").ok())
                    .and_then(|m| m.as_i64().ok()),
            ),
            "BMC" => open.push(None),
            "EMC" => {
                open.pop();
            }
            "TJ" | "Tj" => {
                if let Some(mcid) = open.iter().rev().find_map(|m| *m) {
                    let (x, y) = text_origin;
                    heights
                        .entry(mcid)
                        .or_insert(ctm[1] * x + ctm[3] * y + ctm[5]);
                }
            }
            _ => {}
        }
    }
    heights
}

/// Painting operators on page 1 outside any marked content.
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

const SQUARE: &str = r#"<rect width="20" height="20"/>"#;

#[test]
fn a_hidden_sprite_sheet_or_an_svg_of_defs_needs_no_alt_text() {
    let pdf = try_render(&format!(
        "{PAGE}<h1>Icons</h1>\
         <svg style=\"display:none\"><symbol id=\"i\" viewBox=\"0 0 20 20\">{SQUARE}</symbol></svg>\
         <svg width=\"0\" height=\"0\" style=\"position:absolute\">\
         <defs><linearGradient id=\"g\"><stop offset=\"0\"/></linearGradient></defs></svg>\
         <svg width=\"20\" height=\"20\"><symbol id=\"s\">{SQUARE}</symbol></svg>\
         <p><svg aria-label=\"Square icon\" width=\"20\" height=\"20\">{SQUARE}</svg> Save</p>"
    ))
    .expect("only the svg that paints something is a figure");
    let found = elements(&pdf);
    assert_eq!(count(&found, "Figure"), 1, "{found:?}");
    assert!(found.contains(&("Figure".into(), Some("Square icon".into()))));
}

#[test]
fn a_nested_svg_is_part_of_its_figure() {
    let pdf = try_render(&format!(
        "{PAGE}<h1>Chart</h1>\
         <div><svg aria-label=\"Two squares\" width=\"40\" height=\"20\">{SQUARE}\
         <svg x=\"20\" width=\"20\" height=\"20\">{SQUARE}</svg></svg></div>"
    ))
    .expect("the nested svg needs no alt text of its own");
    let found = elements(&pdf);
    assert_eq!(count(&found, "Figure"), 1, "{found:?}");
}

#[test]
fn an_invisible_svg_is_not_a_figure() {
    let pdf = try_render(&format!(
        "{PAGE}<h1>Chart</h1>\
         <div><svg aria-label=\"Secret chart\" width=\"20\" height=\"20\" \
         style=\"visibility:hidden\">{SQUARE}</svg></div>"
    ))
    .expect("an svg that is not drawn needs no alt text");
    assert_eq!(count(&elements(&pdf), "Figure"), 0);
    assert!(!String::from_utf8_lossy(&pdf).contains("Secret chart"));
}

#[test]
fn an_aria_hidden_subtree_has_no_structure_elements() {
    let pdf = render(&format!(
        "{PAGE}<h1>Report</h1>\
         <div aria-hidden=\"true\"><p>Decoration</p>\
         <table><tr><td>Cell</td></tr></table></div>\
         <p>Body</p>"
    ));
    let found = elements(&pdf);
    for tag in ["Table", "TR", "TD"] {
        assert_eq!(count(&found, tag), 0, "{tag}: {found:?}");
    }
    assert_eq!(
        count(&found, "P"),
        1,
        "only the visible paragraph: {found:?}"
    );
}

#[test]
fn a_layout_table_is_not_a_table() {
    let pdf = render(&format!(
        "{PAGE}<h1>Letter</h1>\
         <table role=\"presentation\"><tr><td>Sender</td><td>Date</td></tr></table>"
    ));
    let found = elements(&pdf);
    for tag in ["Table", "TBody", "TR", "TD"] {
        assert_eq!(count(&found, tag), 0, "{tag}: {found:?}");
    }
    assert_eq!(count(&found, "P"), 2, "each cell is a paragraph: {found:?}");
}

#[test]
fn an_element_with_role_img_is_one_figure() {
    let cell = "<td style=\"width:4px;height:4px;background:#000\"></td>";
    let pdf = render(&format!(
        "{PAGE}<h1>Pay</h1>\
         <div role=\"img\" aria-label=\"QR code\" style=\"width:20px\">\
         <table><tr>{cell}{cell}</tr><tr>{cell}{cell}</tr></table></div>"
    ));
    let found = elements(&pdf);
    assert_eq!(
        found
            .iter()
            .filter(|(t, _)| t == "Figure")
            .collect::<Vec<_>>(),
        vec![&("Figure".to_string(), Some("QR code".to_string()))],
        "{found:?}"
    );
    assert_eq!(count(&found, "Table"), 0, "{found:?}");
}

#[test]
fn generated_content_reads_where_it_is_drawn() {
    // The block around "Tip: " and "Save often." is drawn above
    // "Details.", and is read first.
    let pdf = render(&format!(
        "<style>.tip::before {{ content: \"Tip: \" }}</style>{PAGE}<h1>Notes</h1>\
         <div class=\"tip\">Save often.<p>Details.</p></div>"
    ));
    let (tree, order) = structure(&pdf);
    let heights = text_heights(&pdf);
    let read: Vec<f32> = order.iter().map(|m| heights[m]).collect();
    assert!(read.len() >= 3, "{tree}");
    assert!(
        read.windows(2).all(|w| w[0] > w[1]),
        "read top to bottom: {tree}, heights {read:?}"
    );
}

#[test]
fn an_element_drawn_as_a_group_is_tagged() {
    // A fractional opacity draws the blockquote as one group, tagged as
    // the content of its structure element.
    let pdf = render(&format!(
        "{PAGE}<h1>Quote</h1><blockquote style=\"opacity:.5\"><p>Quoted</p></blockquote>"
    ));
    assert_eq!(untagged_painting(&pdf), Vec::<String>::new());
    assert_eq!(
        count(&elements(&pdf), "Div"),
        1,
        "the blockquote: {}",
        structure(&pdf).0
    );
}

#[test]
fn text_beside_list_items_is_not_a_child_of_the_list() {
    let pdf = render(&format!(
        "{PAGE}<h1>Steps</h1><ul>Before you start:<li>Read</li></ul>"
    ));
    // A list holds only list items; the text goes beside the list.
    assert_eq!(
        kids_of(&pdf, "L"),
        vec![vec!["LI".to_string()]],
        "{}",
        structure(&pdf).0
    );
    assert_eq!(count(&elements(&pdf), "P"), 1);
}
