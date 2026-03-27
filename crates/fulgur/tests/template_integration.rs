use fulgur::engine::Engine;
use serde_json::json;

#[test]
fn test_template_to_pdf() {
    let template = r#"<html><body><h1>{{ title }}</h1>
{% for item in items %}<p>{{ item }}</p>{% endfor %}
</body></html>"#;
    let data = json!({
        "title": "Invoice",
        "items": ["Item A", "Item B"]
    });

    let pdf = Engine::builder()
        .template("invoice.html", template)
        .data(data)
        .build()
        .render()
        .unwrap();

    assert!(!pdf.is_empty());
    // PDF magic bytes
    assert_eq!(&pdf[..5], b"%PDF-");
}

#[test]
fn test_html_mode_still_works() {
    let html = "<html><body><p>Hello</p></body></html>";
    let pdf = Engine::builder().build().render_html(html).unwrap();
    assert!(!pdf.is_empty());
    assert_eq!(&pdf[..5], b"%PDF-");
}

#[test]
fn test_template_with_assets() {
    let mut assets = fulgur::asset::AssetBundle::new();
    assets.add_css("p { color: red; }");

    let template = "<html><body><p>{{ text }}</p></body></html>";
    let data = json!({"text": "styled"});

    let pdf = Engine::builder()
        .template("test.html", template)
        .data(data)
        .assets(assets)
        .build()
        .render()
        .unwrap();

    assert!(!pdf.is_empty());
    assert_eq!(&pdf[..5], b"%PDF-");
}
