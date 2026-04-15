//! Integration tests: `<a href>` is emitted as PDF `/Link` annotation.

use fulgur::Engine;

fn engine() -> Engine {
    Engine::builder().build()
}

#[test]
fn external_link_produces_uri_action_in_pdf() {
    let html = r#"<html><body><p><a href="https://example.com">click</a></p></body></html>"#;
    let bytes = engine().render_html(html).expect("render");
    assert!(bytes.starts_with(b"%PDF"));
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Link"), "missing /Link annotation subtype");
    assert!(text.contains("/URI"), "missing /URI action type");
    assert!(
        text.contains("https://example.com"),
        "missing URI string in PDF body"
    );
}

#[test]
fn internal_anchor_produces_destination() {
    let html = r##"<html><body>
        <p><a href="#section">jump</a></p>
        <div style="height:1500px"></div>
        <h2 id="section">Target</h2>
    </body></html>"##;
    let bytes = engine().render_html(html).expect("render");
    assert!(bytes.starts_with(b"%PDF"));
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Link"), "missing /Link annotation");
    // krilla serializes XYZ destinations; the annotation dict references
    // it via /Dest (indirect object reference) and the destination itself
    // carries /XYZ.
    assert!(
        text.contains("/XYZ") || text.contains("/Dest"),
        "missing internal destination marker (/XYZ or /Dest)"
    );
}

#[test]
fn unresolved_internal_anchor_is_ignored_not_error() {
    let html = r##"<html><body><p><a href="#nope">dangling</a></p></body></html>"##;
    let bytes = engine().render_html(html).expect("render should not fail");
    assert!(bytes.starts_with(b"%PDF"));
}
