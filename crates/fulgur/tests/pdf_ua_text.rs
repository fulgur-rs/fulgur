//! The text of a tagged PDF as assistive technology reads it: in reading
//! order on right-to-left lines, in the language each element is in, with
//! preserved tabs that PDF/UA and PDF/A accept, with links that keep their
//! letters when a ligature joins them to the text around, and with heading
//! titles that hold the heading's text once.

use fulgur::{AssetBundle, Engine};
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object};
use std::collections::BTreeMap;
use std::path::PathBuf;

const PAGE: &str = r#"<!DOCTYPE html><html lang="en"><title>Test</title><body>"#;

fn fonts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/.fonts")
}

/// `font` (a file in `examples/.fonts`) as the font of the body and of
/// `<pre>`, at 16px.
fn font_assets(font: &str) -> Result<AssetBundle, String> {
    let mut assets = AssetBundle::default();
    assets
        .add_font_file(fonts_dir().join(font))
        .map_err(|e| format!("failed to load {font}: {e}"))?;
    let family = if font.contains("Mono") {
        "Noto Sans Mono"
    } else {
        "Noto Sans"
    };
    assets.add_css(format!(
        "body, pre {{ font-family: '{family}'; font-size: 16px; }}"
    ));
    Ok(assets)
}

/// A tagged render with `font` (a file in `examples/.fonts`) as the body
/// font, so the output does not depend on the fonts of the machine.
fn render_with_font(html: &str, font: &str, ua: bool) -> Result<Vec<u8>, String> {
    let builder = Engine::builder()
        .assets(font_assets(font)?)
        .lang("en")
        .title("Test");
    let builder = if ua {
        builder.pdf_ua(true)
    } else {
        builder.tagged(true)
    };
    builder.build().render(html).map_err(|e| e.to_string())
}

fn render_plain(html: &str) -> Vec<u8> {
    Engine::builder()
        .lang("en")
        .title("Test")
        .build()
        .render(html)
        .expect("render")
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

/// The structure elements of type `kind`, in the order of the tree.
fn elements_of_type<'a>(doc: &'a Document, kind: &str) -> Vec<&'a Dictionary> {
    fn walk<'a>(doc: &'a Document, obj: &'a Object, kind: &str, out: &mut Vec<&'a Dictionary>) {
        let Ok(dict) = resolve(doc, obj).as_dict() else {
            return;
        };
        if dict.get(b"MCID").is_ok() || name_of(dict, b"Type").as_deref() == Some("OBJR") {
            return;
        }
        if name_of(dict, b"S").as_deref() == Some(kind) {
            out.push(dict);
        }
        match dict.get(b"K") {
            Ok(Object::Array(items)) => {
                for item in items {
                    walk(doc, item, kind, out);
                }
            }
            Ok(other) => walk(doc, other, kind, out),
            Err(_) => {}
        }
    }
    let root = doc
        .catalog()
        .expect("catalog")
        .get(b"StructTreeRoot")
        .expect("a structure tree");
    let mut found = Vec::new();
    walk(doc, root, kind, &mut found);
    found
}

fn lang_of(dict: &Dictionary) -> Option<String> {
    lopdf::decode_text_string(dict.get(b"Lang").ok()?).ok()
}

/// The MCIDs under a structure element, in the order of the tree.
fn mcids_in_order(doc: &Document, dict: &Dictionary) -> Vec<i64> {
    let mut out = Vec::new();
    let kids = match dict.get(b"K") {
        Ok(Object::Array(items)) => items.clone(),
        Ok(other) => vec![other.clone()],
        Err(_) => Vec::new(),
    };
    for kid in kids {
        match resolve(doc, &kid) {
            Object::Integer(mcid) => out.push(*mcid),
            Object::Dictionary(d) => {
                if let Ok(mcid) = d.get(b"MCID").and_then(Object::as_i64) {
                    out.push(mcid);
                } else if d.get(b"S").is_ok() {
                    out.extend(mcids_in_order(doc, d));
                }
            }
            _ => {}
        }
    }
    out
}

fn page_content(doc: &Document) -> Content {
    let page_id = *doc.get_pages().get(&1).expect("page 1");
    Content::decode(&doc.get_page_content(page_id)).expect("content")
}

fn number(o: &Object) -> Option<f32> {
    match o {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

/// The x of each text line start (`Tm`) on page 1, by the MCID it is in.
fn text_x_by_mcid(doc: &Document) -> BTreeMap<i64, Vec<f32>> {
    let mut found: BTreeMap<i64, Vec<f32>> = BTreeMap::new();
    let mut open: Vec<Option<i64>> = Vec::new();
    for op in page_content(doc).operations {
        match op.operator.as_str() {
            "BDC" => open.push(op.operands.get(1).and_then(|p| {
                let dict = match p {
                    Object::Dictionary(d) => d,
                    _ => return None,
                };
                dict.get(b"MCID").and_then(Object::as_i64).ok()
            })),
            "BMC" => open.push(None),
            "EMC" => {
                open.pop();
            }
            "Tm" => {
                let x = op.operands.get(4).and_then(number);
                if let (Some(x), Some(mcid)) = (x, open.iter().rev().flatten().next()) {
                    found.entry(*mcid).or_default().push(x);
                }
            }
            _ => {}
        }
    }
    found
}

// ── reading order on right-to-left lines ────────────────────────────────

/// Hebrew words, right to left: `ONE` is read first and drawn rightmost.
const ONE: &str = "אחת";
const TWO: &str = "שתיים";
const THREE: &str = "שלוש";

#[test]
fn the_runs_of_a_right_to_left_line_are_tagged_in_reading_order() {
    // Each run of the line has its own marked content: the first word read
    // is the rightmost, so the x of the marked content in the paragraph's
    // order must fall.
    let html = format!(
        r#"{PAGE}<p dir="rtl" lang="he">{ONE} <a href="https://example.com">{TWO}</a> {THREE}</p>"#
    );
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    let paragraph = elements_of_type(&doc, "P")[0];
    let positions = text_x_by_mcid(&doc);
    let xs: Vec<f32> = mcids_in_order(&doc, paragraph)
        .iter()
        .map(|mcid| positions[mcid][0])
        .collect();
    assert!(xs.len() >= 3, "the words and the spaces between: {xs:?}");
    assert!(
        xs.windows(2).all(|w| w[0] > w[1]),
        "the runs read right to left: {xs:?}"
    );
}

#[test]
fn the_runs_of_a_right_to_left_line_without_links_are_tagged_in_reading_order() {
    let html = format!(r#"{PAGE}<p dir="rtl" lang="he">{ONE} <b>{TWO}</b> <i>{THREE}</i></p>"#);
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    let paragraph = elements_of_type(&doc, "P")[0];
    let positions = text_x_by_mcid(&doc);
    let xs: Vec<f32> = mcids_in_order(&doc, paragraph)
        .iter()
        .map(|mcid| positions[mcid][0])
        .collect();
    assert!(xs.len() >= 3, "a run per style: {xs:?}");
    assert!(
        xs.windows(2).all(|w| w[0] > w[1]),
        "the runs read right to left: {xs:?}"
    );
}

#[test]
fn a_right_to_left_line_is_drawn_left_to_right() {
    // The content stream keeps the visual order, as Chromium writes it:
    // only the structure tree reads the line in order, so the page and
    // text extraction by content order do not change.
    let html = format!(
        r#"{PAGE}<p dir="rtl" lang="he">{ONE} <b>{TWO}</b> <a href="https://example.com">{THREE}</a></p>"#
    );
    for pdf in [render_tagged(&html), render_plain(&html)] {
        let doc = load(&pdf);
        let xs: Vec<f32> = page_content(&doc)
            .operations
            .iter()
            .filter(|op| op.operator == "Tm")
            .filter_map(|op| op.operands.get(4).and_then(number))
            .collect();
        assert!(xs.len() >= 3, "{xs:?}");
        assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
    }
}

#[test]
fn a_right_to_left_line_whose_only_right_to_left_item_is_a_box_is_read_in_order() {
    // The figure comes first in the text and is drawn on the right; the
    // number after it is left-to-right text.
    let html = format!(
        r#"{PAGE}<p dir="rtl" lang="he"><svg role="img" aria-label="טלפון" width="10" height="10"><rect width="10" height="10"/></svg>050-1234567</p>"#
    );
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    let paragraph = elements_of_type(&doc, "P")[0];
    let first = match paragraph.get(b"K") {
        Ok(Object::Array(items)) => items.first().cloned(),
        _ => None,
    }
    .expect("the paragraph has kids");
    let first = resolve(&doc, &first).as_dict().expect("an element first");
    assert_eq!(name_of(first, b"S").as_deref(), Some("Figure"));
}

#[test]
fn a_left_to_right_line_keeps_its_order() {
    let html = format!(r#"{PAGE}<p>one <a href="https://example.com">two</a> three</p>"#);
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    let paragraph = elements_of_type(&doc, "P")[0];
    let positions = text_x_by_mcid(&doc);
    let xs: Vec<f32> = mcids_in_order(&doc, paragraph)
        .iter()
        .map(|mcid| positions[mcid][0])
        .collect();
    assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
}

#[test]
fn an_inside_list_marker_is_read_before_a_box_that_starts_the_item() {
    let html = format!(
        r#"{PAGE}<ul style="list-style-position: inside"><li><p><svg role="img" aria-label="Icon" width="10" height="10"><rect width="10" height="10"/></svg> Text</p></li></ul>"#
    );
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    let holder = ["P", "LBody", "LI"]
        .iter()
        .flat_map(|kind| elements_of_type(&doc, kind))
        .find(|e| {
            let kids = match e.get(b"K") {
                Ok(Object::Array(items)) => items.clone(),
                _ => Vec::new(),
            };
            kids.iter().any(|k| {
                resolve(&doc, k)
                    .as_dict()
                    .is_ok_and(|d| name_of(d, b"S").as_deref() == Some("Figure"))
            })
        })
        .expect("an element holds the figure");
    let first = match holder.get(b"K") {
        Ok(Object::Array(items)) => items[0].clone(),
        _ => unreachable!(),
    };
    let first_is_figure = resolve(&doc, &first)
        .as_dict()
        .is_ok_and(|d| name_of(d, b"S").as_deref() == Some("Figure"));
    assert!(!first_is_figure, "the marker is read first");
}

// ── languages ───────────────────────────────────────────────────────────

#[test]
fn an_element_in_another_language_has_its_lang() {
    let html = format!(
        r#"{PAGE}<p lang="fr">Bonjour</p><p>Hello</p><div lang="es"><p>Hola</p></div>
        <ul lang="de"><li>Eins</li></ul>"#
    );
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    let paragraphs: Vec<Option<String>> = elements_of_type(&doc, "P")
        .into_iter()
        .map(lang_of)
        .collect();
    assert_eq!(
        paragraphs,
        [Some("fr".to_string()), None, None],
        "the French paragraph says so; the others are in the language of the \
         document and of their Div"
    );
    assert_eq!(
        lang_of(elements_of_type(&doc, "Div")[0]).as_deref(),
        Some("es")
    );
    assert_eq!(
        lang_of(elements_of_type(&doc, "L")[0]).as_deref(),
        Some("de")
    );
    assert_eq!(
        lang_of(elements_of_type(&doc, "LI")[0]),
        None,
        "inherits from L"
    );
}

#[test]
fn an_element_takes_the_lang_of_an_ancestor_that_is_not_in_the_tree() {
    // A custom element is no structure element: the paragraph inside it
    // is the one that says it is Italian.
    let html = format!(r#"{PAGE}<x-quote lang="it"><p>Ciao</p></x-quote><p>Hello</p>"#);
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    let paragraphs: Vec<Option<String>> = elements_of_type(&doc, "P")
        .into_iter()
        .map(lang_of)
        .collect();
    assert_eq!(paragraphs, [Some("it".to_string()), None]);
}

#[test]
fn a_lang_that_is_not_a_language_tag_is_mended_or_left_out() {
    // PDF/UA and PDF/A take only language tags: `en_US` is a common slip
    // for `en-US`, and `fr)` is no tag at all.
    let html = r#"<!DOCTYPE html><html lang="de_AT"><title>Test</title><body>
        <p lang="en_US">Color</p><p lang="fr)">Bonjour</p>"#;
    let pdf = Engine::builder()
        .tagged(true)
        .title("Test")
        .build()
        .render(html)
        .expect("render");
    let doc = load(&pdf);
    let catalog = doc.catalog().expect("catalog");
    assert_eq!(lang_of(catalog).as_deref(), Some("de-AT"));
    let paragraphs: Vec<Option<String>> = elements_of_type(&doc, "P")
        .into_iter()
        .map(lang_of)
        .collect();
    assert_eq!(paragraphs, [Some("en-US".to_string()), None]);
}

#[test]
fn a_root_lang_that_is_not_a_language_tag_says_so() {
    let html = r#"<!DOCTYPE html><html lang="english!"><title>Test</title><body><p>Hello</p>"#;
    let err = Engine::builder()
        .pdf_ua(true)
        .build()
        .render(html)
        .expect_err("PDF/UA needs a language");
    let message = err.to_string();
    assert!(
        message.contains("english!") && message.contains("not a language tag"),
        "{message}"
    );
}

#[test]
fn a_table_caption_is_in_the_language_of_its_table() {
    let html = format!(
        r#"{PAGE}<table lang="de"><caption>Beschriftung</caption><tr><td>Zelle</td></tr></table>"#
    );
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    // The caption is drawn above the table, in a Div with the table.
    let langs: Vec<Option<String>> = elements_of_type(&doc, "Div")
        .into_iter()
        .map(lang_of)
        .collect();
    assert_eq!(langs, [Some("de".to_string())]);
}

/// The `/Lang` of each marked-content sequence on page 1.
fn content_langs(doc: &Document) -> Vec<String> {
    page_content(doc)
        .operations
        .iter()
        .filter(|op| op.operator == "BDC")
        .filter_map(|op| match op.operands.get(1)? {
            Object::Dictionary(d) => lang_of(d),
            _ => None,
        })
        .collect()
}

#[test]
fn text_in_another_language_has_its_lang() {
    let html = format!(
        r#"{PAGE}<p>English <span lang="de">Guten Tag</span> and
        <a href="https://example.com" lang="tr">Merhaba</a></p><p>Only English</p>"#
    );
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    assert_eq!(content_langs(&doc), ["de", "tr"]);
}

#[test]
fn text_in_another_language_renders_as_pdf_ua() {
    let html = format!(r#"{PAGE}<h1>Title</h1><p>English <span lang="de">Guten Tag</span></p>"#);
    render_with_font(&html, "NotoSans-Regular.ttf", true).expect("PDF/UA render");
}

// ── tabs ────────────────────────────────────────────────────────────────

/// Whether page 1 draws a `.notdef` glyph (code 0 of a two-byte font).
fn draws_notdef(doc: &Document) -> bool {
    let strings = |o: &Object| -> Vec<Vec<u8>> {
        match o {
            Object::String(s, _) => vec![s.clone()],
            Object::Array(items) => items
                .iter()
                .filter_map(|i| match i {
                    Object::String(s, _) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    };
    page_content(doc)
        .operations
        .iter()
        .filter(|op| op.operator == "TJ" || op.operator == "Tj")
        .flat_map(|op| op.operands.iter().flat_map(strings))
        .any(|s| s.chunks(2).any(|code| code == [0, 0]))
}

#[test]
fn a_preserved_tab_is_not_drawn_as_notdef() {
    for body in [
        "<pre>a\tb</pre>",
        "<p style=\"white-space: pre-wrap\">a\tb</p>",
    ] {
        let html = format!("{PAGE}{body}");
        let pdf = render_with_font(&html, "NotoSansMono-Regular.ttf", false).expect("render");
        assert!(!draws_notdef(&load(&pdf)), "{body} draws .notdef");
    }
}

#[test]
fn a_preserved_tab_renders_as_pdf_ua_and_pdf_a() {
    let html = format!("{PAGE}<h1>Code</h1><pre>if x {{\n\treturn\n}}</pre>");
    render_with_font(&html, "NotoSansMono-Regular.ttf", true).expect("PDF/UA render");
    for level in [fulgur::PdfA::A2b, fulgur::PdfA::A2u, fulgur::PdfA::A2a] {
        Engine::builder()
            .assets(font_assets("NotoSansMono-Regular.ttf").expect("font"))
            .lang("en")
            .title("Test")
            .pdf_a(level)
            .creation_date("2026-11-22T00:00:00Z")
            .build()
            .render(&html)
            .unwrap_or_else(|e| panic!("PDF/A {level:?}: {e}"));
    }
}

/// Whether a ToUnicode map of the file maps a glyph to U+0009.
fn maps_a_glyph_to_tab(doc: &Document) -> bool {
    doc.objects.values().any(|obj| {
        let Ok(stream) = obj.as_stream() else {
            return false;
        };
        let content = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        let text = String::from_utf8_lossy(&content);
        text.contains("beginbfchar") && text.contains("<0009>")
    })
}

#[test]
fn a_tab_leaves_the_spaces_of_its_font_alone() {
    // The tab is a gap, not a glyph: the space glyph keeps mapping to a
    // space, whichever comes first.
    let html = format!("{PAGE}<pre>\tone two</pre>");
    let pdf = render_with_font(&html, "NotoSansMono-Regular.ttf", false).expect("render");
    assert!(!maps_a_glyph_to_tab(&load(&pdf)));
}

/// The left edge of each word on page 1, as pdftotext reads it, or `None`
/// when pdftotext is not installed.
fn word_lefts(pdf: &[u8]) -> Option<Vec<(String, f32)>> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tabs.pdf");
    std::fs::write(&path, pdf).expect("write pdf");
    let output = match std::process::Command::new("pdftotext")
        .arg("-bbox")
        .arg(&path)
        .arg("-")
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => panic!("pdftotext: {err}"),
    };
    assert!(output.status.success(), "pdftotext failed: {output:?}");
    let html = String::from_utf8_lossy(&output.stdout);
    let mut words = Vec::new();
    for line in html.lines().filter(|l| l.contains("<word")) {
        let x = line
            .split("xMin=\"")
            .nth(1)?
            .split('"')
            .next()?
            .parse()
            .ok()?;
        let word = line.split('>').nth(1)?.split('<').next()?.to_string();
        words.push((word, x));
    }
    Some(words)
}

#[test]
fn a_tab_moves_the_text_after_it_to_the_next_tab_stop() {
    // Noto Sans Mono advances 0.6 em; at 16px a space is 7.2pt, and the
    // default tab-size of 8 puts the stops 57.6pt apart.
    let html =
        format!("{PAGE}<pre>a\tb\n12345678\tc\n\td</pre><pre style=\"tab-size: 20px\">e\tf</pre>");
    let pdf = render_with_font(&html, "NotoSansMono-Regular.ttf", false).expect("render");
    let Some(words) = word_lefts(&pdf) else {
        eprintln!("pdftotext not installed; skipping");
        return;
    };
    let x = |w: &str| {
        words
            .iter()
            .find(|(word, _)| word == w)
            .unwrap_or_else(|| panic!("{w:?} not in {words:?}"))
            .1
    };
    let start = x("a");
    let near = |a: f32, b: f32| (a - b).abs() < 0.05;
    assert!(near(x("b") - start, 57.6), "b at {}", x("b") - start);
    // A tab right at a stop goes on to the next one.
    assert!(near(x("c") - start, 115.2), "c at {}", x("c") - start);
    assert!(near(x("d") - start, 57.6), "d at {}", x("d") - start);
    // `tab-size: 20px` puts the stops 15pt apart.
    assert!(near(x("f") - x("e"), 15.0), "f at {}", x("f") - x("e"));
}

/// The right edge of each word on page 1, as pdftotext reads it.
fn word_rights(pdf: &[u8]) -> Option<Vec<(String, f32)>> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tabs.pdf");
    std::fs::write(&path, pdf).expect("write pdf");
    let output = match std::process::Command::new("pdftotext")
        .arg("-bbox")
        .arg(&path)
        .arg("-")
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => panic!("pdftotext: {err}"),
    };
    let html = String::from_utf8_lossy(&output.stdout);
    let mut words = Vec::new();
    for line in html.lines().filter(|l| l.contains("<word")) {
        let x = line
            .split("xMax=\"")
            .nth(1)?
            .split('"')
            .next()?
            .parse()
            .ok()?;
        let word = line.split('>').nth(1)?.split('<').next()?.to_string();
        words.push((word, x));
    }
    Some(words)
}

#[test]
fn a_tab_in_a_box_sized_to_its_text_stays_in_the_box() {
    // The cell is as wide as Parley measured its text, with the tab as wide
    // as one glyph: tab stops would carry `b` into the next cell.
    let html = format!("{PAGE}<table><tr><td><pre>a\tb</pre></td><td>next</td></tr></table>");
    let pdf = render_with_font(&html, "NotoSansMono-Regular.ttf", false).expect("render");
    let (Some(lefts), Some(rights)) = (word_lefts(&pdf), word_rights(&pdf)) else {
        eprintln!("pdftotext not installed; skipping");
        return;
    };
    let right_of_b = rights.iter().find(|(w, _)| w == "b").expect("b").1;
    let left_of_next = lefts.iter().find(|(w, _)| w == "next").expect("next").1;
    assert!(
        right_of_b < left_of_next,
        "b ends at {right_of_b}, next starts at {left_of_next}"
    );
}

#[test]
fn a_tab_beside_a_float_stays_clear_of_it() {
    // The line box ends at the float, 150px (112.5pt) from the left: tab
    // stops would carry the text over it.
    let html = format!(
        "{PAGE}<div style=\"width: 300px\"><div style=\"float: right; width: 150px; height: 60px\"></div>\
         <p style=\"white-space: pre-wrap; margin: 0\">a\tb\tc\td</p></div>"
    );
    let pdf = render_with_font(&html, "NotoSansMono-Regular.ttf", false).expect("render");
    let (Some(lefts), Some(rights)) = (word_lefts(&pdf), word_rights(&pdf)) else {
        eprintln!("pdftotext not installed; skipping");
        return;
    };
    let start = lefts.iter().find(|(w, _)| w == "a").expect("a").1;
    let end = rights.iter().map(|(_, x)| *x).fold(0.0, f32::max);
    assert!(
        end - start <= 112.5 + 0.05,
        "the text ends {} pt in",
        end - start
    );
}

#[test]
fn a_tab_in_pre_keeps_its_stops_under_text_align_justify() {
    // `white-space: pre` lines are never stretched, so a justified page
    // still has its tab stops.
    let html = format!("{PAGE}<div style=\"text-align: justify\"><pre>a\tb</pre></div>");
    let pdf = render_with_font(&html, "NotoSansMono-Regular.ttf", false).expect("render");
    let Some(words) = word_lefts(&pdf) else {
        eprintln!("pdftotext not installed; skipping");
        return;
    };
    let x = |w: &str| words.iter().find(|(word, _)| word == w).expect(w).1;
    assert!(
        (x("b") - x("a") - 57.6).abs() < 0.05,
        "b at {}",
        x("b") - x("a")
    );
}

// ── links inside ligatures ──────────────────────────────────────────────

fn link_rects(doc: &Document) -> Vec<Vec<f32>> {
    let mut rects = Vec::new();
    for page_id in doc.get_pages().values() {
        let page = doc.get_dictionary(*page_id).expect("page");
        let Ok(annots) = page.get(b"Annots") else {
            continue;
        };
        for annot in resolve(doc, annots).as_array().expect("Annots") {
            let dict = resolve(doc, annot).as_dict().expect("annotation");
            if name_of(dict, b"Subtype").as_deref() != Some("Link") {
                continue;
            }
            let rect = dict.get(b"Rect").and_then(Object::as_array).expect("Rect");
            rects.push(rect.iter().map(|v| number(v).expect("number")).collect());
        }
    }
    rects
}

#[test]
fn a_link_inside_a_ligature_keeps_its_annotation() {
    // Noto Sans joins `ffi` into one glyph, whose first letter is outside
    // the link: the link has no glyph of its own.
    let html = format!(r#"{PAGE}<p>of<a href="https://example.com">f</a>ice</p>"#);
    for ua in [false, true] {
        let pdf = render_with_font(&html, "NotoSans-Regular.ttf", ua).expect("render");
        let doc = load(&pdf);
        assert_eq!(link_rects(&doc).len(), 1, "one annotation (ua: {ua})");
        if ua {
            assert_eq!(elements_of_type(&doc, "Link").len(), 1, "one Link element");
        }
    }
}

#[test]
fn a_link_that_starts_inside_a_ligature_covers_the_ligature() {
    // The `ffi` of `of<a>fice</a>` holds two of the link's letters: the
    // link covers it, as it does when the link starts at its first letter.
    let inside = format!(r#"{PAGE}<p>of<a href="https://example.com">fice</a></p>"#);
    let before = format!(r#"{PAGE}<p>o<a href="https://example.com">ffice</a></p>"#);
    let rect = |html: &str| {
        let pdf = render_with_font(html, "NotoSans-Regular.ttf", false).expect("render");
        link_rects(&load(&pdf)).remove(0)
    };
    let (a, b) = (rect(&inside), rect(&before));
    assert!(
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 0.01),
        "{a:?} vs {b:?}"
    );
}

#[test]
fn a_link_made_of_a_tab_keeps_its_area() {
    // The tab draws nothing, but a link on it, or starting with it, still
    // covers its width.
    let only_tab = format!("{PAGE}<pre>a<a href=\"https://example.com\">\t</a>b</pre>");
    let starts_with_tab = format!("{PAGE}<pre>a<a href=\"https://example.com\">\tb</a></pre>");
    for (html, kind) in [(only_tab, "only a tab"), (starts_with_tab, "a tab first")] {
        let pdf = render_with_font(&html, "NotoSansMono-Regular.ttf", false).expect("render");
        let rects = link_rects(&load(&pdf));
        assert_eq!(rects.len(), 1, "{kind}: one annotation");
        let width = rects[0][2] - rects[0][0];
        // From after `a` to the first tab stop, 57.6pt from the start.
        assert!(width > 45.0, "{kind}: the link is {width} pt wide");
    }
}

// ── heading titles ──────────────────────────────────────────────────────

#[test]
fn a_heading_title_leaves_out_its_inline_boxes() {
    let html = format!(
        r#"{PAGE}<h1>Logo <svg role="img" aria-label="Star" width="10" height="10"><rect width="10" height="10"/></svg> Title</h1>"#
    );
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    let title = elements_of_type(&doc, "H1")
        .into_iter()
        .find_map(|e| lopdf::decode_text_string(e.get(b"T").ok()?).ok())
        .expect("a title");
    assert!(!title.contains('\u{FFFC}'), "{title:?}");
}

#[test]
fn a_heading_title_holds_its_text_once() {
    let html =
        format!(r#"{PAGE}<h1>Title <a href="https://example.com">link</a> <em>end</em></h1>"#);
    let pdf = render_tagged(&html);
    let doc = load(&pdf);
    let titles: Vec<String> = elements_of_type(&doc, "H1")
        .into_iter()
        .filter_map(|e| lopdf::decode_text_string(e.get(b"T").ok()?).ok())
        .collect();
    assert_eq!(titles, ["Title link end"]);
}
