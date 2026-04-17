//! Integration tests pinning down unit semantics across the Blitz boundary.
//!
//! Added for beads fulgur-9ul. These are smoke tests — real geometric
//! assertions live alongside convert.rs where we can walk the Pageable
//! tree directly.

use fulgur::Engine;

#[test]
fn width_percent_renders() {
    let html = r#"<html><body><div style="width:100%;height:10pt;background:red"></div></body></html>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("render");
    assert!(pdf.len() > 100);
}

#[test]
fn width_cm_renders() {
    let html = r#"<html><body><div style="width:10cm;height:1cm;background:red"></div></body></html>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("render");
    assert!(pdf.len() > 100);
}

#[test]
fn width_px_renders() {
    let html = r#"<html><body><div style="width:360px;height:10px;background:red"></div></body></html>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("render");
    assert!(pdf.len() > 100);
}
