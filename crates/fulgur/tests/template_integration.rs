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
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_html_mode_still_works() {
    let html = "<html><body><p>Hello</p></body></html>";
    let pdf = Engine::builder().build().render_html(html).unwrap();
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_template_syntax_error_propagates() {
    let result = Engine::builder()
        .template("bad.html", "{% if %}")
        .data(json!({}))
        .build()
        .render();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Template error"));
}

#[test]
fn test_render_without_template_errors() {
    let result = Engine::builder().build().render();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no template set"));
}

#[test]
fn test_invalid_filter_propagates() {
    let result = Engine::builder()
        .template("test.html", "{{ x | bogus }}")
        .data(json!({"x": 1}))
        .build()
        .render();
    assert!(result.is_err());
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
    assert!(pdf.starts_with(b"%PDF-"));
}
