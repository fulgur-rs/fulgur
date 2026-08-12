use fulgur::{Engine, PageSize};

fn render(html: &str) -> Vec<u8> {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    engine.render(html).expect("render should succeed")
}

fn render_small(html: &str) -> Vec<u8> {
    Engine::builder()
        .page_size(PageSize::custom(80.0, 80.0))
        .margin(fulgur::Margin::uniform(5.0))
        .tagged(true)
        .build()
        .render(html)
        .expect("render should succeed")
}

fn annotation_quad_counts(document: &lopdf::Document, page_id: lopdf::ObjectId) -> Vec<usize> {
    let page = document
        .get_object(page_id)
        .expect("page object should exist")
        .as_dict()
        .expect("page object should be a dictionary");
    let Ok(annots) = page.get_deref(b"Annots", document) else {
        return Vec::new();
    };
    annots
        .as_array()
        .expect("Annots should resolve to an array")
        .iter()
        .map(|annotation| {
            let annotation = document
                .dereference(annotation)
                .expect("annotation reference should resolve")
                .1
                .as_dict()
                .expect("annotation should be a dictionary");
            assert_eq!(
                annotation
                    .get_deref(b"Subtype", document)
                    .expect("annotation should have a subtype")
                    .as_name()
                    .expect("annotation subtype should be a name"),
                b"Link"
            );
            assert_eq!(
                annotation
                    .get_deref(b"Rect", document)
                    .expect("link annotation should have a rectangle")
                    .as_array()
                    .expect("annotation rectangle should be an array")
                    .len(),
                4
            );
            let quad_points = annotation
                .get_deref(b"QuadPoints", document)
                .expect("link annotation should have quad points")
                .as_array()
                .expect("annotation quad points should be an array");
            assert_eq!(quad_points.len() % 8, 0);
            quad_points.len() / 8
        })
        .collect()
}

#[test]
fn table_simple_renders() {
    let html = r#"
    <table border="1">
        <thead><tr><th>Name</th><th>Value</th></tr></thead>
        <tbody><tr><td>A</td><td>1</td></tr><tr><td>B</td><td>2</td></tr></tbody>
    </table>"#;
    let pdf = render(html);
    assert!(!pdf.is_empty());
}

#[test]
fn table_no_thead_renders() {
    let html = r#"
    <table border="1">
        <tr><td>A</td><td>1</td></tr>
        <tr><td>B</td><td>2</td></tr>
    </table>"#;
    let pdf = render(html);
    assert!(!pdf.is_empty());
}

#[test]
fn table_long_with_thead_renders() {
    let mut rows = String::new();
    for i in 0..50 {
        rows.push_str(&format!(
            "<tr><td>Row {i}</td><td>Value {i}</td><td>Data {i}</td></tr>"
        ));
    }
    let html = format!(
        r#"
    <table border="1">
        <thead><tr><th>Name</th><th>Value</th><th>Data</th></tr></thead>
        <tbody>{rows}</tbody>
    </table>"#
    );
    let pdf = render(&html);
    assert!(!pdf.is_empty());
}

#[test]
fn repeated_table_header_renders_with_clip_link_and_tags() {
    let rows = (1..=30)
        .map(|row| format!("<tr><td>Row {row:02}</td></tr>"))
        .collect::<String>();
    let html = format!(
        r#"<!doctype html>
<style>
  html, body {{ margin: 0; }}
  table {{ width: 100%; overflow: hidden; border-collapse: collapse; }}
  th, td {{ height: 12px; padding: 0; }}
</style>
<table>
  <thead><tr><th><a href="https://example.com">H</a></th></tr></thead>
  <tbody>{rows}</tbody>
</table>"#
    );

    let pdf = render_small(&html);
    let document = lopdf::Document::load_mem(&pdf).expect("PDF should parse");
    let pages = document.get_pages();
    assert!(pages.len() >= 2, "table should span at least two pages");
    for (page_number, page_id) in pages {
        assert_eq!(
            annotation_quad_counts(&document, page_id),
            vec![1],
            "page {page_number} should contain one single-quad header link annotation"
        );
    }
}
