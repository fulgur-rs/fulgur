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

fn count_red_ops_in_content(bytes: &[u8]) -> usize {
    // Returning 0 here would let "no red operators on this page" hold for a
    // stream we simply failed to parse, hiding the very regression the callers
    // assert against.
    let content =
        lopdf::content::Content::decode(bytes).expect("page content stream should decode");
    content
        .operations
        .iter()
        .filter(|op| matches!(op.operator.as_str(), "rg" | "RG") && op.operands.len() == 3)
        .filter(|op| {
            let channels: Vec<f32> = op
                .operands
                .iter()
                .filter_map(|o| match o {
                    lopdf::Object::Integer(i) => Some(*i as f32),
                    lopdf::Object::Real(f) => Some(*f),
                    _ => None,
                })
                .collect();
            channels.len() == 3 && channels[0] > 0.9 && channels[1] < 0.1 && channels[2] < 0.1
        })
        .count()
}

fn count_red_ops_on_page(pdf: &[u8], page_number: u32) -> usize {
    let document = lopdf::Document::load_mem(pdf).expect("PDF should parse");
    let page_id = *document
        .get_pages()
        .get(&page_number)
        .expect("requested page should exist");
    let bytes = document
        .get_page_content(page_id)
        .expect("page content stream should be readable");
    count_red_ops_in_content(&bytes)
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
    assert!(
        document
            .catalog()
            .expect("PDF must have a catalog")
            .has(b"StructTreeRoot"),
        "tagged PDF catalog must contain /StructTreeRoot"
    );
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

/// Nested table whose header does not fit the remaining outer strip
/// records a zero-height fragment on page 0. Without a paint guard,
/// `table_box_size` falls back to the full layout box and the inner
/// table's red frame leaks as a sliver on that page.
#[test]
fn nested_oversized_inner_table_does_not_paint_zero_height_frame() {
    let html = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  #lead { height: 60px; }
  table { margin: 0; border-spacing: 0; width: 100px; }
  th, td { box-sizing: border-box; padding: 0; }
  #outer-header { height: 20px; }
  #inner { background: rgb(255, 0, 0); }
  #inner-header { height: 80px; }
  #inner-body { height: 30px; }
</style></head><body>
  <div id="lead"></div>
  <table><thead><tr><th id="outer-header">Outer</th></tr></thead><tbody><tr><td>
    <table id="inner"><thead><tr><th id="inner-header">Inner</th></tr></thead>
      <tbody><tr><td id="inner-body">Body</td></tr></tbody></table>
  </td></tr></tbody></table>
</body></html>"#;

    // 200×100 CSS px content box (150×75 pt) matches the geometry fixture
    // `nested_oversized_header_fallback_honors_occupied_outer_strip`.
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 150.0,
            height: 75.0,
        })
        .margin(fulgur::Margin::uniform(0.0))
        .build();
    let pdf = engine.render(html).expect("render should succeed");
    let document = lopdf::Document::load_mem(&pdf).expect("PDF should parse");
    let pages = document.get_pages();
    assert!(pages.len() >= 2, "inner table must continue past page 1");

    let layout = engine.layout(html).expect("layout should succeed");
    let inner_id = layout
        .drawables
        .tables
        .iter()
        .find(|(_, table)| table.id.as_deref().is_some_and(|id| id == "inner"))
        .map(|(id, _)| *id)
        .expect("inner table drawable");
    let inner_geom = layout
        .geometry
        .get(&inner_id)
        .expect("inner table geometry");
    assert!(
        inner_geom.is_split(),
        "inner table must be split so the zero-height page-0 slice hits the leak path"
    );
    let page0: Vec<_> = inner_geom
        .fragments
        .iter()
        .filter(|fragment| fragment.page_index == 0)
        .collect();
    assert!(
        !page0.is_empty(),
        "inner table must record a page-0 fragment: {:?}",
        inner_geom.fragments
    );
    assert!(
        page0
            .iter()
            .all(|fragment| fragment.height <= fulgur::units::Px::ZERO),
        "page-0 inner fragments must be zero-height: {page0:?}"
    );

    assert_eq!(
        count_red_ops_on_page(&pdf, 1),
        0,
        "zero-height nested-table fragment must not paint its outer frame on page 1"
    );
    let later_red: usize = pages
        .keys()
        .filter(|&&page| page > 1)
        .map(|&page| count_red_ops_on_page(&pdf, page))
        .sum();
    assert!(
        later_red > 0,
        "inner table red frame must still paint on a later page"
    );
}
