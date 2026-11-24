//! PDF/UA requires every piece of page content to be either tagged or an
//! artifact. krilla draws a fractional opacity as a group (a form XObject),
//! and marked content opened inside a group is not linked into the
//! structure tree, so its content is lost to assistive technology. These
//! tests read back every page content stream and every form XObject and
//! check that nothing is painted outside marked content, that no tagged
//! content sits inside a group, and that every group is drawn inside
//! marked content.

use fulgur::Engine;
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object};

fn render(html: &str) -> Vec<u8> {
    Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .title("Test")
        .build()
        .render(html)
        .expect("render")
}

const PAGE: &str = r#"<!DOCTYPE html><html lang="en"><title>Test</title><body><h1>Test</h1>"#;

// A 1×1 PNG. (A GIF without a color table is refused by krilla with or
// without tagging.)
const IMG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

/// The marked content a painting operator is drawn in.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mark {
    /// Tagged content: a sequence with an MCID, linked into the structure
    /// tree.
    Tagged,
    Artifact,
    /// Marked content that is neither (a plain `BMC`).
    Other,
}

#[derive(Default, Debug)]
struct Report {
    /// Painting operators in a page stream outside any marked content.
    untagged: Vec<String>,
    /// Tagged sequences (with an MCID, or any non-artifact tag) opened
    /// inside a form XObject, where krilla does not link them.
    tagged_in_forms: Vec<String>,
    /// Text drawn as tagged content, and as an artifact.
    tagged_text: usize,
    artifact_text: usize,
    /// Images and forms drawn as tagged content.
    tagged_do: usize,
}

fn is_painting(op: &str) -> bool {
    matches!(
        op,
        "TJ" | "Tj"
            | "'"
            | "\""
            | "f"
            | "f*"
            | "F"
            | "S"
            | "s"
            | "B"
            | "B*"
            | "b"
            | "b*"
            | "Do"
            | "sh"
            | "BI"
    )
}

fn is_text(op: &str) -> bool {
    matches!(op, "TJ" | "Tj" | "'" | "\"")
}

struct Walker<'a> {
    document: &'a Document,
    report: Report,
}

impl Walker<'_> {
    fn classify(&self, op: &lopdf::content::Operation, resources: Option<&Dictionary>) -> Mark {
        let tag = op.operands.first().and_then(|o| o.as_name().ok());
        if tag == Some(b"Artifact".as_slice()) {
            return Mark::Artifact;
        }
        let properties = match op.operands.get(1) {
            Some(Object::Dictionary(d)) => Some(d.clone()),
            Some(Object::Name(name)) => resources
                .and_then(|r| r.get(b"Properties").ok())
                .and_then(|p| self.resolve_dict(p))
                .and_then(|p| p.get(name).ok().and_then(|o| self.resolve_dict(o))),
            _ => None,
        };
        match properties {
            Some(d) if d.get(b"MCID").is_ok() => Mark::Tagged,
            _ => Mark::Other,
        }
    }

    fn resolve_dict(&self, object: &Object) -> Option<Dictionary> {
        match object {
            Object::Dictionary(d) => Some(d.clone()),
            Object::Reference(id) => self.document.get_dictionary(*id).ok().cloned(),
            _ => None,
        }
    }

    /// Walk one content stream. `outer` is the marked content the stream
    /// is drawn in (for a form XObject, where its `Do` was); `in_form` is
    /// set inside form XObjects.
    fn walk(
        &mut self,
        where_: &str,
        bytes: &[u8],
        resources: Option<Dictionary>,
        outer: Option<Mark>,
        in_form: bool,
        depth: usize,
    ) {
        let content = Content::decode(bytes).expect("content stream should decode");
        let mut stack: Vec<Mark> = Vec::new();
        for op in &content.operations {
            let name = op.operator.as_str();
            match name {
                "BDC" | "BMC" => {
                    let mark = self.classify(op, resources.as_ref());
                    if in_form && mark != Mark::Artifact {
                        self.report
                            .tagged_in_forms
                            .push(format!("{where_}: {mark:?} {:?}", op.operands));
                    }
                    stack.push(mark);
                }
                "EMC" => {
                    stack.pop();
                }
                _ if is_painting(name) => {
                    let mark = stack.last().copied().or(outer);
                    if mark.is_none() {
                        self.report.untagged.push(format!("{where_}: {name}"));
                    }
                    if is_text(name) {
                        match mark {
                            Some(Mark::Tagged) => self.report.tagged_text += 1,
                            Some(Mark::Artifact) => self.report.artifact_text += 1,
                            _ => {}
                        }
                    }
                    if name == "Do" {
                        if mark == Some(Mark::Tagged) {
                            self.report.tagged_do += 1;
                        }
                        self.walk_xobject(where_, op, resources.as_ref(), mark, depth);
                    }
                }
                _ => {}
            }
        }
    }

    fn walk_xobject(
        &mut self,
        where_: &str,
        op: &lopdf::content::Operation,
        resources: Option<&Dictionary>,
        mark: Option<Mark>,
        depth: usize,
    ) {
        if depth > 16 {
            return;
        }
        let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
            return;
        };
        let Some(xobjects) = resources
            .and_then(|r| r.get(b"XObject").ok())
            .and_then(|x| self.resolve_dict(x))
        else {
            return;
        };
        let Ok(Object::Reference(id)) = xobjects.get(name) else {
            return;
        };
        let Ok(stream) = self.document.get_object(*id).and_then(Object::as_stream) else {
            return;
        };
        let is_form = stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .is_ok_and(|s| s == b"Form");
        if !is_form {
            return;
        }
        let bytes = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        let form_resources = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|r| self.resolve_dict(r));
        let label = format!("{where_} > form {} {}", id.0, id.1);
        self.walk(&label, &bytes, form_resources, mark, true, depth + 1);
    }
}

/// Read back every page of `pdf`, following each `Do` into its form
/// XObject (so a form drawn several times is checked each time).
fn report(pdf: &[u8]) -> Report {
    let document = Document::load_mem(pdf).expect("PDF should parse");
    let mut walker = Walker {
        document: &document,
        report: Report::default(),
    };
    for (number, page_id) in document.get_pages() {
        let bytes = document.get_page_content(page_id);
        let (inline, referenced) = document.get_page_resources(page_id).expect("resources");
        let resources = inline.cloned().or_else(|| {
            referenced
                .first()
                .and_then(|id| document.get_dictionary(*id).ok().cloned())
        });
        walker.walk(&format!("page {number}"), &bytes, resources, None, false, 0);
    }
    walker.report
}

/// Every form XObject in the file, whether or not a page draws it: none
/// may hold tagged content.
fn tagged_content_in_any_form(pdf: &[u8]) -> Vec<String> {
    let document = Document::load_mem(pdf).expect("PDF should parse");
    let mut found = Vec::new();
    for (id, object) in &document.objects {
        let Ok(stream) = object.as_stream() else {
            continue;
        };
        let is_form = stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .is_ok_and(|s| s == b"Form");
        if !is_form {
            continue;
        }
        let bytes = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        let Ok(content) = Content::decode(&bytes) else {
            continue;
        };
        for op in &content.operations {
            if op.operator != "BDC" && op.operator != "BMC" {
                continue;
            }
            let artifact = op
                .operands
                .first()
                .and_then(|o| o.as_name().ok())
                .is_some_and(|n| n == b"Artifact");
            let has_mcid =
                matches!(op.operands.get(1), Some(Object::Dictionary(d)) if d.get(b"MCID").is_ok());
            if !artifact || has_mcid {
                found.push(format!("form {} {}: {:?}", id.0, id.1, op.operands));
            }
        }
    }
    found
}

/// Link annotations that are not in the structure tree (no
/// `/StructParent`), on every page.
fn untagged_links(pdf: &[u8]) -> usize {
    let document = Document::load_mem(pdf).expect("PDF should parse");
    let mut untagged = 0;
    for page_id in document.get_pages().into_values() {
        for annotation in document.get_page_annotations(page_id).unwrap_or_default() {
            let is_link = annotation
                .get(b"Subtype")
                .and_then(Object::as_name)
                .is_ok_and(|s| s == b"Link");
            if is_link && annotation.get(b"StructParent").is_err() {
                untagged += 1;
            }
        }
    }
    untagged
}

/// Assert that everything is tagged or an artifact and that no tagged
/// content is inside a group, and return the report for further checks.
#[track_caller]
fn assert_all_marked(html: &str) -> Report {
    assert_all_marked_pdf(&render(html))
}

#[track_caller]
fn assert_all_marked_pdf(pdf: &[u8]) -> Report {
    let report = report(pdf);
    assert_eq!(report.untagged, Vec::<String>::new(), "untagged painting");
    assert_eq!(
        report.tagged_in_forms,
        Vec::<String>::new(),
        "tagged content inside a group"
    );
    assert_eq!(
        tagged_content_in_any_form(pdf),
        Vec::<String>::new(),
        "tagged content inside a form XObject"
    );
    report
}

// ─── Item 1: groups open inside the element's tagged sequence ──────────

#[test]
fn a_faded_paragraph_with_a_link() {
    // Reproducers from here on: each used to leave the group's `Do` on
    // the page outside marked content, with the text tagged inside the
    // group, where krilla does not link it.
    let report = assert_all_marked(&format!(
        r#"{PAGE}<p style="opacity:.6">See <a href="https://example.com">the docs</a></p>"#
    ));
    assert!(
        report.tagged_do > 0,
        "the group is tagged content: {report:?}"
    );
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn a_faded_paragraph_with_a_link_tags_the_link() {
    // PDF/UA requires every link annotation in the structure tree: the
    // link is noted for the paragraph whose group holds its text.
    let pdf = render(&format!(
        r#"{PAGE}<p style="opacity:.6">See <a href="https://example.com">the docs</a></p>"#
    ));
    assert_eq!(untagged_links(&pdf), 0);
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/S /Link"),
        "a Link element holds the annotation"
    );
}

#[test]
fn a_faded_paragraph_with_an_inline_image() {
    let report = assert_all_marked(&format!(
        r#"{PAGE}<p style="opacity:.5">Logo <img alt="ACME" src="{IMG}" width="10" height="10"> Inc.</p>"#
    ));
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn a_faded_inline_svg() {
    let report = assert_all_marked(&format!(
        r#"{PAGE}<p>Logo <svg role="img" aria-label="Logo" style="opacity:.5" width="20" height="20"><rect width="20" height="20"/></svg> text</p>"#
    ));
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn a_faded_inline_block_with_text() {
    let report = assert_all_marked(&format!(
        r#"{PAGE}<p>Before <span style="display:inline-block;opacity:.5;background:#eee">boxed</span> after</p>"#
    ));
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn a_faded_list_item() {
    let report = assert_all_marked(&format!(
        r#"{PAGE}<ul><li style="opacity:.5">Faded item</li></ul>"#
    ));
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn a_faded_list_item_with_a_link() {
    let pdf = render(&format!(
        r#"{PAGE}<ul><li style="opacity:.5">See <a href="https://example.com">docs</a></li></ul>"#
    ));
    let report = assert_all_marked_pdf(&pdf);
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(untagged_links(&pdf), 0);
}

#[test]
fn a_faded_block_with_a_child_paragraph_and_a_link() {
    let pdf = render(&format!(
        r#"{PAGE}<div style="opacity:.5"><p>Inner <a href="https://example.com">link</a></p><p>Second</p></div>"#
    ));
    let report = assert_all_marked_pdf(&pdf);
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(report.artifact_text, 0, "{report:?}");
    assert_eq!(untagged_links(&pdf), 0);
}

#[test]
fn a_faded_clipped_block() {
    let report = assert_all_marked(&format!(
        r#"{PAGE}<div style="overflow:hidden;opacity:.5;height:40px">Clipped text</div>"#
    ));
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn a_faded_clipped_block_with_a_child_paragraph() {
    let pdf = render(&format!(
        r#"{PAGE}<div style="overflow:hidden;opacity:.5;height:40px"><p>Clipped <a href="https://example.com">link</a></p></div>"#
    ));
    let report = assert_all_marked_pdf(&pdf);
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(untagged_links(&pdf), 0);
}

#[test]
fn a_faded_clipped_table() {
    let report = assert_all_marked(&format!(
        r#"{PAGE}<table style="overflow:hidden;opacity:.5"><tr><td>A</td></tr></table>"#
    ));
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn a_faded_block_with_a_background() {
    // The block and its text are one group: the group opens inside the
    // paragraph's sequence, the background inside it.
    let report = assert_all_marked(&format!(
        r#"{PAGE}<p style="opacity:.5;background:#eee">Faded with a background</p>"#
    ));
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn a_faded_block_with_a_background_and_a_link() {
    let pdf = render(&format!(
        r#"{PAGE}<p style="opacity:.5;background:#eee">See <a href="https://example.com">the docs</a></p>"#
    ));
    let report = assert_all_marked_pdf(&pdf);
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(untagged_links(&pdf), 0);
}

/// Enough text to wrap onto several lines, so that a two-column container
/// splits it between the columns.
const LONG: &str = "One two three four five six seven eight nine ten eleven twelve. \
    One two three four five six seven eight nine ten eleven twelve. \
    One two three four five six seven eight nine ten eleven twelve. \
    One two three four five six seven eight nine ten eleven twelve.";

#[test]
fn faded_text_split_across_columns() {
    // The container is the paragraph, painted one slice per column, all
    // in one group.
    let report = assert_all_marked(&format!(
        r#"{PAGE}<div style="column-count:2;opacity:.5">{LONG}</div>"#
    ));
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn faded_text_with_a_link_split_across_columns() {
    let pdf = render(&format!(
        r#"{PAGE}<div style="column-count:2;opacity:.5">{LONG} <a href="https://example.com">four</a> {LONG}</div>"#
    ));
    let report = assert_all_marked_pdf(&pdf);
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(untagged_links(&pdf), 0);
}

#[test]
fn a_faded_paragraph_in_columns() {
    let report = assert_all_marked(&format!(
        r#"{PAGE}<div style="column-count:2"><p style="opacity:.5">{LONG}</p></div>"#
    ));
    assert!(report.tagged_do > 0, "{report:?}");
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn a_faded_decorative_svg_is_an_artifact() {
    assert_all_marked(&format!(
        r#"{PAGE}<p>Before</p><svg aria-hidden="true" width="10" height="10" style="opacity:.5;display:block"><rect width="10" height="10"/></svg>"#
    ));
}

#[test]
fn a_faded_box_with_no_content_is_an_artifact() {
    assert_all_marked(&format!(
        r#"{PAGE}<div style="opacity:.5;background:#ccc;height:20px"></div><p>After</p>"#
    ));
}

#[test]
fn a_faded_image() {
    let report = assert_all_marked(&format!(
        r#"{PAGE}<div><img alt="Pixel" style="opacity:.5" src="{IMG}" width="10" height="10"></div>"#
    ));
    assert!(report.tagged_do > 0, "{report:?}");
}

// ─── Item 2: fallbacks in `try_start_tagged` ───────────────────────────

#[test]
fn an_inline_list_is_tagged() {
    let report = assert_all_marked(&format!(
        r#"{PAGE}<ul><li style="display:inline">Home</li> › <li style="display:inline">Docs</li></ul>"#
    ));
    assert!(report.tagged_text > 1, "{report:?}");
    assert_eq!(report.artifact_text, 0, "{report:?}");
}

#[test]
fn an_aria_hidden_subtree_is_tagged_or_an_artifact() {
    // A node left out of the structure tree is drawn as an artifact
    // (`try_start_tagged`); either way nothing is left unmarked.
    assert_all_marked(&format!(
        r#"{PAGE}<div aria-hidden="true"><p>Hidden from readers</p></div><p>Shown</p>"#
    ));
}

// ─── Item 3: decorations after an inline box ───────────────────────────

#[test]
fn a_line_through_before_an_inline_svg() {
    assert_all_marked(&format!(
        r#"{PAGE}<p>Price: <del>$20</del> <svg role="img" aria-label="Sale" width="12" height="12"><rect width="12" height="12"/></svg></p>"#
    ));
}

#[test]
fn an_underline_before_an_inline_image() {
    assert_all_marked(&format!(
        r#"{PAGE}<p><u>Signed</u> <img alt="signature" src="{IMG}" width="40" height="12"></p>"#
    ));
}

#[test]
fn an_overline_before_an_inline_image() {
    assert_all_marked(&format!(
        r#"{PAGE}<p><span style="text-decoration:overline">Over</span> <img alt="mark" src="{IMG}" width="12" height="12"></p>"#
    ));
}

// ─── Item 4: markers of non-`<li>` list items ───────────────────────────

#[test]
fn the_marker_of_a_div_list_item() {
    assert_all_marked(&format!(
        r#"{PAGE}<div style="display:list-item;margin-left:2em">Item</div>"#
    ));
}

// ─── Item 5: without tagging nothing is marked ──────────────────────────

#[test]
fn without_tagging_nothing_is_marked() {
    let pdf = Engine::builder()
        .build()
        .render(&format!(
            r#"{PAGE}<p style="opacity:.6">See <a href="https://example.com">the docs</a></p>
            <p>Logo <svg role="img" aria-label="Logo" style="opacity:.5" width="20" height="20"><rect width="20" height="20"/></svg> text</p>
            <ul><li style="opacity:.5">Faded item</li></ul>
            <div style="overflow:hidden;opacity:.5;height:40px">Clipped text</div>
            <div style="display:list-item;margin-left:2em"><u>Item</u> <img alt="x" src="{IMG}" width="10" height="10"></div>"#
        ))
        .expect("render");
    let document = Document::load_mem(&pdf).expect("PDF should parse");
    let mut streams: Vec<(String, Vec<u8>)> = document
        .get_pages()
        .into_iter()
        .map(|(number, id)| {
            let bytes = document.get_page_content(id);
            (format!("page {number}"), bytes)
        })
        .collect();
    let mut forms = 0;
    for (id, object) in &document.objects {
        let Ok(stream) = object.as_stream() else {
            continue;
        };
        let is_form = stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .is_ok_and(|s| s == b"Form");
        if is_form {
            forms += 1;
            let bytes = stream
                .decompressed_content()
                .unwrap_or_else(|_| stream.content.clone());
            streams.push((format!("form {id:?}"), bytes));
        }
    }
    assert!(forms > 0, "the opacity groups are form XObjects");
    for (label, bytes) in streams {
        let content = Content::decode(&bytes).expect("content stream should decode");
        for op in &content.operations {
            assert!(
                !matches!(op.operator.as_str(), "BDC" | "BMC" | "EMC"),
                "{label} has marked content without tagging"
            );
        }
    }
    let s = String::from_utf8_lossy(&pdf);
    assert!(!s.contains("/StructTreeRoot"), "no structure tree");
}
