use fulgur::config::PageSize;
use fulgur::engine::Engine;

fn make_engine() -> Engine {
    Engine::builder().page_size(PageSize::A4).build()
}

#[test]
fn border_radius_uniform_renders() {
    let engine = make_engine();
    let html = r#"<div style="border: 2px solid black; border-radius: 10px; padding: 10px;">Rounded box</div>"#;
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(!pdf.is_empty(), "PDF should not be empty");
}

#[test]
fn border_radius_individual_corners_renders() {
    let engine = make_engine();
    let html = r#"<div style="border: 2px solid black; border-top-left-radius: 20px; border-bottom-right-radius: 20px; padding: 10px;">Diagonal rounded</div>"#;
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(!pdf.is_empty(), "PDF should not be empty");
}

#[test]
fn border_radius_with_background_renders() {
    let engine = make_engine();
    let html = r#"<div style="background-color: #3498db; border-radius: 15px; padding: 20px; color: white;">Rounded background</div>"#;
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(!pdf.is_empty(), "PDF should not be empty");
}

#[test]
fn border_radius_elliptical_renders() {
    let engine = make_engine();
    let html = r#"<div style="border: 1px solid gray; border-radius: 20px / 10px; padding: 10px;">Elliptical radius</div>"#;
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(!pdf.is_empty(), "PDF should not be empty");
}

#[test]
fn border_radius_circle_renders() {
    let engine = make_engine();
    let html = r#"<div style="width: 50px; height: 50px; border-radius: 50%; background-color: red;"></div>"#;
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(!pdf.is_empty(), "PDF should not be empty");
}
