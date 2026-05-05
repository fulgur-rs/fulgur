//! Integration tests: end-to-end rendering with bookmarks enabled.

use fulgur::asset::AssetBundle;
use fulgur::{Engine, PageSize};

fn render_with_bookmarks(html: &str, bookmarks: bool) -> Vec<u8> {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .bookmarks(bookmarks)
        .build();
    engine.render_html(html).expect("render ok")
}

/// Permissive substring check for an outline title in a PDF byte stream.
///
/// Krilla emits outline titles as PDF strings wrapped in `(..)`; we accept
/// either the parenthesized form or a bare substring so the check stays
/// robust against future encoding changes (hex strings, escaped chars, …).
fn pdf_contains_outline_entry(pdf: &[u8], needle: &str) -> bool {
    let s = String::from_utf8_lossy(pdf);
    s.contains(&format!("({needle})")) || s.contains(needle)
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

/// End-to-end confirmation that the GCPM-driven bookmark path (UA CSS +
/// `BookmarkPass` + `ConvertContext::bookmark_by_node`) produces the same
/// outline entries the legacy hardcoded `h1`-`h6` walk did. When Phase 5
/// removes the hardcoded fallback, this test guards against regression.
#[test]
fn end_to_end_h1_gets_bookmark_via_ua_css() {
    let html = r#"<html><body><h1>Title</h1></body></html>"#;
    let pdf = render_with_bookmarks(html, true);
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/Outlines"),
        "expected /Outlines object in PDF emitted via UA-CSS bookmark path"
    );
    assert!(
        s.contains("(Title)") || s.contains("Title"),
        "expected 'Title' in PDF outline (UA CSS should auto-bookmark h1)"
    );
}

/// Helper: build an Engine with author CSS delivered via AssetBundle.
///
/// Inline `<style>` blocks are also supported since `fulgur-mq5` wired
/// [`extract_gcpm_from_inline_styles`] into `engine.rs`. These tests use
/// `AssetBundle::add_css` as the canonical caller channel; see
/// `bookmarks_via_inline_style_block` for the inline-`<style>` variant.
fn engine_with_css(css: &str) -> Engine {
    let mut assets = AssetBundle::new();
    assets.add_css(css);
    Engine::builder()
        .page_size(PageSize::A4)
        .bookmarks(true)
        .assets(assets)
        .build()
}

/// Author CSS with `bookmark-level: none` on `h1` should override the UA
/// stylesheet's automatic h1 bookmark and suppress the entry entirely.
#[test]
fn author_css_can_suppress_h1_bookmark() {
    let engine = engine_with_css("h1 { bookmark-level: none; }");
    let html = r#"<html><body><h1>Hidden</h1></body></html>"#;
    let pdf = engine.render_html(html).expect("render ok");
    assert!(
        !pdf_contains_outline_entry(&pdf, "Hidden"),
        "expected 'Hidden' to be suppressed by bookmark-level: none"
    );
}

/// Author CSS can bookmark arbitrary elements (not just headings) via a
/// class selector, composing `bookmark-level`, literal strings, and
/// `attr()` in `bookmark-label`.
#[test]
fn custom_css_bookmarks_arbitrary_element() {
    let engine =
        engine_with_css(r#".ch { bookmark-level: 1; bookmark-label: "Ch. " attr(data-num); }"#);
    let html = r#"<html><body>
        <div class="ch" data-num="1">first</div>
        <div class="ch" data-num="2">second</div>
    </body></html>"#;
    let pdf = engine.render_html(html).expect("render ok");
    assert!(
        pdf_contains_outline_entry(&pdf, "Ch. 1"),
        "expected outline entry 'Ch. 1' resolved from attr(data-num)"
    );
    assert!(
        pdf_contains_outline_entry(&pdf, "Ch. 2"),
        "expected outline entry 'Ch. 2' resolved from attr(data-num)"
    );
}

/// UA-driven `h1` and author-driven custom class should coexist in the
/// same outline. Exact nesting is covered by `outline.rs` unit tests —
/// here we just confirm both labels survive end-to-end.
#[test]
fn mixed_h1_and_custom_aside_produce_nested_outline() {
    let engine = engine_with_css(".aside { bookmark-level: 2; bookmark-label: attr(data-title); }");
    let html = r#"<html><body>
        <h1>Chapter</h1>
        <div class="aside" data-title="Note">body</div>
    </body></html>"#;
    let pdf = engine.render_html(html).expect("render ok");
    assert!(
        pdf_contains_outline_entry(&pdf, "Chapter"),
        "expected UA-driven h1 'Chapter' in outline"
    );
    assert!(
        pdf_contains_outline_entry(&pdf, "Note"),
        "expected author-driven .aside 'Note' in outline"
    );
}

/// `counter()` in `bookmark-label` is not yet wired through the counter
/// pass; it must degrade to an empty string rather than panicking, and
/// the rest of the label components must still render.
#[test]
fn counter_in_bookmark_label_does_not_panic() {
    let engine = engine_with_css(r#"h1 { bookmark-label: counter(chapter) " " content(); }"#);
    let html = r#"<html><body><h1>Intro</h1></body></html>"#;
    let pdf = engine.render_html(html).expect("render ok");
    // counter(chapter) resolves to empty; " " content() yields " Intro".
    // The substring helper still matches on the trailing "Intro" portion.
    assert!(
        pdf_contains_outline_entry(&pdf, "Intro"),
        "h1 should still produce an outline entry; counter just becomes empty"
    );
}

/// When a rule declares `bookmark-level` without `bookmark-label`, the
/// label must fall back to the element's text content (equivalent to
/// `content()`). This guards the Phase 3 fallback path end-to-end.
#[test]
fn level_only_falls_back_to_element_text() {
    let engine = engine_with_css(".aside { bookmark-level: 2; }");
    let html = r#"<html><body>
        <div class="aside">Text Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).expect("render ok");
    assert!(
        pdf_contains_outline_entry(&pdf, "Text Content"),
        "label-less rule should fall back to element's text content"
    );
}

/// Inline `<style>` blocks are supported since fulgur-mq5 — verify that
/// `bookmark-level` delivered via `<style>` produces an outline entry.
#[test]
fn bookmarks_via_inline_style_block() {
    let html = r#"<!doctype html><html><head>
        <style>
            aside { bookmark-level: 1; bookmark-label: "Sidebar"; }
        </style>
    </head><body>
        <aside>My sidebar</aside>
        <p>Some body text.</p>
    </body></html>"#;

    let engine = Engine::builder().bookmarks(true).build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        pdf_contains_outline_entry(&pdf, "Sidebar"),
        "expected 'Sidebar' bookmark entry — bookmark-label via inline <style> not working"
    );
}
