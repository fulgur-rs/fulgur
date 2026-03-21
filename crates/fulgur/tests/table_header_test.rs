use fulgur::{Engine, PageSize};

fn render(html: &str) -> Vec<u8> {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    engine.render_html(html).expect("render should succeed")
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
