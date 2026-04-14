//! Integration tests: end-to-end rendering with bookmarks enabled.

use fulgur::{Engine, PageSize};

fn render_with_bookmarks(html: &str, bookmarks: bool) -> Vec<u8> {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .bookmarks(bookmarks)
        .build();
    engine.render_html(html).expect("render ok")
}

#[test]
fn bookmarks_disabled_produces_no_outline_marker() {
    let html = r#"<html><body><h1>A</h1><h2>B</h2></body></html>"#;
    let pdf = render_with_bookmarks(html, false);
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        !s.contains("/Outlines"),
        "PDF should not contain /Outlines when bookmarks disabled"
    );
}

#[test]
fn bookmarks_enabled_emits_outline_with_heading_titles() {
    let html = r#"<html><body><h1>Chapter One</h1><p>Body</p><h2>Section</h2></body></html>"#;
    let pdf = render_with_bookmarks(html, true);
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/Outlines"), "PDF must contain /Outlines");
    assert!(
        s.contains("(Chapter One)") || s.contains("Chapter One"),
        "PDF must reference `Chapter One` title"
    );
    assert!(
        s.contains("(Section)") || s.contains("Section"),
        "PDF must reference `Section` title"
    );
}
