//! Integration tests for the fulgur-smlr document-order GCPM cascade fold
//! (`blitz_adapter::document_ordered_gcpm_mappings`, wired into
//! `Engine::layout_to_drawables`).
//!
//! These scenarios all need a real `<link rel="stylesheet">` pointing at a
//! file on disk (`base_path`), which is why they live here rather than in
//! `crates/fulgur/src/engine.rs`'s `#[cfg(test)] mod tests` — the plan's
//! Task 8 explicitly allows either location; real files on disk push this
//! one into an integration test.
//!
//! See `docs/plans/2026-09-17-fulgur-smlr-gcpm-cascade-design.md` and
//! `docs/plans/2026-09-17-fulgur-smlr-cascade-implementation.md` (Task 8).

use std::fs;

use fulgur::{AssetBundle, Engine};
use tempfile::tempdir;

/// Read the PDF outline's top-level entry titles, in document order.
///
/// Mirrors `crates/fulgur/tests/render_smoke.rs::outline_titles`; kept as a
/// separate local copy because it reads krilla's `/Title` text-string
/// encoding directly via `lopdf` and the equivalent decoder in
/// `crates/fulgur/src/inspect.rs` is crate-private (not visible from an
/// integration test crate — same reasoning `render_smoke.rs`'s copy notes).
fn outline_titles(pdf_bytes: &[u8]) -> Vec<String> {
    let doc = lopdf::Document::load_mem(pdf_bytes).expect("load_mem");
    let catalog_id = doc
        .trailer
        .get(b"Root")
        .expect("Root")
        .as_reference()
        .expect("Root ref");
    let catalog = doc
        .get_object(catalog_id)
        .expect("catalog")
        .as_dict()
        .expect("catalog dict");
    let outlines_id = match catalog.get(b"Outlines") {
        Ok(v) => v.as_reference().expect("Outlines ref"),
        Err(_) => return Vec::new(),
    };
    let outlines = doc
        .get_object(outlines_id)
        .expect("outlines")
        .as_dict()
        .expect("outlines dict");

    fn decode_title(s: &[u8]) -> String {
        // PDF text strings: UTF-16BE with BOM, or fall back to UTF-8 lossy.
        if s.starts_with(&[0xFE, 0xFF]) {
            let chars: Vec<u16> = s[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_be_bytes(c))
                .collect();
            String::from_utf16_lossy(&chars)
        } else {
            String::from_utf8_lossy(s).into_owned()
        }
    }

    let mut out = Vec::new();
    let mut cur = outlines
        .get(b"First")
        .ok()
        .and_then(|v| v.as_reference().ok());
    while let Some(id) = cur {
        let dict = doc
            .get_object(id)
            .expect("outline node")
            .as_dict()
            .expect("outline dict");
        if let Ok(title) = dict.get(b"Title")
            && let Ok(s) = title.as_str()
        {
            out.push(decode_title(s));
        }
        cur = dict.get(b"Next").ok().and_then(|v| v.as_reference().ok());
    }
    out
}

/// Scenario 1: `<style>` appears before `<link>` in markup, both declaring
/// `bookmark-label` for the same element via the SAME selector kind (equal
/// specificity — both `Class`). The `<link>` rule is LATER in true DOM
/// document order, so it must win the tie.
#[test]
fn style_before_link_equal_specificity_link_wins() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("link.css"),
        r#".target { bookmark-level: 1; bookmark-label: "FROM_LINK"; }"#,
    )
    .unwrap();

    let html = r#"<!doctype html><html><head>
        <style>.target { bookmark-level: 1; bookmark-label: "FROM_STYLE"; }</style>
        <link rel="stylesheet" href="link.css">
    </head><body><p class="target">Content</p></body></html>"#;

    let pdf = Engine::builder()
        .bookmarks(true)
        .base_path(dir.path())
        .build()
        .render(html)
        .expect("render");
    let titles = outline_titles(&pdf);
    assert_eq!(
        titles,
        vec!["FROM_LINK".to_string()],
        "the <link> rule (later in document order) must win the equal-specificity tie, got {titles:?}"
    );
}

/// Scenario 2: reverse markup order — `<link>` appears before `<style>`.
/// Now `<style>` is later in document order, so it must win instead.
#[test]
fn link_before_style_equal_specificity_style_wins() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("link.css"),
        r#".target { bookmark-level: 1; bookmark-label: "FROM_LINK"; }"#,
    )
    .unwrap();

    let html = r#"<!doctype html><html><head>
        <link rel="stylesheet" href="link.css">
        <style>.target { bookmark-level: 1; bookmark-label: "FROM_STYLE"; }</style>
    </head><body><p class="target">Content</p></body></html>"#;

    let pdf = Engine::builder()
        .bookmarks(true)
        .base_path(dir.path())
        .build()
        .render(html)
        .expect("render");
    let titles = outline_titles(&pdf);
    assert_eq!(
        titles,
        vec!["FROM_STYLE".to_string()],
        "the <style> rule (later in document order) must win the equal-specificity tie, got {titles:?}"
    );
}

/// Scenario 3 (the non-obvious case): AssetBundle CSS vs. a `<link>` rule,
/// equal specificity. AssetBundle wins — NOT because it behaves like a
/// "base/default" (it doesn't; ordinary CSS properties from AssetBundle
/// cascade the same way), but because `InjectCssPass` appends AssetBundle's
/// cleaned CSS as `<head>`'s LAST child (`insert_before: None`), landing
/// AFTER any author `<link>`/`<style>` already in the document — i.e. it is
/// LATEST in true DOM document order, which is what actually wins ties.
/// This looks backwards at a glance ("shouldn't the bundled library CSS
/// lose to page-authored CSS?") but matches how fulgur's real cascade
/// already treats ordinary (non-GCPM) AssetBundle CSS today; this test
/// pins that the GCPM cascade fold stays consistent with it rather than
/// "fixing" it to feel more intuitive.
#[test]
fn assetbundle_vs_link_equal_specificity_assetbundle_wins() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("link.css"),
        r#".target { bookmark-level: 1; bookmark-label: "FROM_LINK"; }"#,
    )
    .unwrap();

    let mut assets = AssetBundle::new();
    assets.add_css(r#".target { bookmark-level: 1; bookmark-label: "FROM_ASSETBUNDLE"; }"#);

    let html = r#"<!doctype html><html><head>
        <link rel="stylesheet" href="link.css">
    </head><body><p class="target">Content</p></body></html>"#;

    let pdf = Engine::builder()
        .bookmarks(true)
        .assets(assets)
        .base_path(dir.path())
        .build()
        .render(html)
        .expect("render");
    let titles = outline_titles(&pdf);
    assert_eq!(
        titles,
        vec!["FROM_ASSETBUNDLE".to_string()],
        "AssetBundle's injected <style> lands LAST in <head>, so it must win \
         the equal-specificity tie over an author <link>, got {titles:?}"
    );
}

/// Scenario 4 (fulgur-smlr Part 0, end-to-end): two `<link media="print">`
/// tags in one document, each media-rewritten to a synthetic
/// `<style>@import ...>` by `apply_link_media_rewrites`. Without the Part 0
/// node-id remap, `doc`'s `slab::Slab` node arena can reuse the first
/// rewrite's freed `<link>` slot for the second rewrite's replacement
/// `<style>` node, causing `link_gcpm_by_node`'s pre-rewrite keys to
/// silently drop or cross-attribute content once
/// `document_ordered_gcpm_mappings` walks the post-rewrite `doc` (see
/// `blitz_adapter.rs`'s
/// `parse_html_with_local_resources_media_restricted_links_do_not_cross_attribute_after_remap`
/// for the lower-level version of this same regression, and its sibling
/// `..._remaps_key_to_live_style_node` for the single-link case).
///
/// This test asserts BOTH links' bookmark content survives, attributed to
/// the correct (distinct) matching elements, in the correct document
/// order — an exact `Vec` equality catches drop, cross-attribution, AND
/// duplication in one assertion.
#[test]
fn two_media_restricted_links_both_survive_the_node_id_remap() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("one.css"),
        r#".item-one { bookmark-level: 1; bookmark-label: "One"; }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("two.css"),
        r#".item-two { bookmark-level: 1; bookmark-label: "Two"; }"#,
    )
    .unwrap();

    let html = r#"<!doctype html><html><head>
        <link rel="stylesheet" href="one.css" media="print">
        <link rel="stylesheet" href="two.css" media="print">
    </head><body>
        <p class="item-one">First</p>
        <p class="item-two">Second</p>
    </body></html>"#;

    let pdf = Engine::builder()
        .bookmarks(true)
        .base_path(dir.path())
        .build()
        .render(html)
        .expect("render");
    let titles = outline_titles(&pdf);
    assert_eq!(
        titles,
        vec!["One".to_string(), "Two".to_string()],
        "both media-restricted <link>s' bookmark content must survive, in \
         document order, with neither dropped nor cross-attributed \
         (fulgur-smlr Part 0 regression guard), got {titles:?}"
    );
}
