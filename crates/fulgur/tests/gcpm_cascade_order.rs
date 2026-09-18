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

/// Critical regression guard (found in code review of the document-order
/// fold): a top-level `<style>@import url(...);</style>` — no `<link>`
/// anywhere — must still surface the imported file's GCPM content.
///
/// Before the fulgur-smlr document-order fold existed, this content
/// reached `gcpm.bookmark_mappings` via the OLD flat
/// `gcpm.extend_from(link_gcpm)` path, built from `net.rs`'s
/// `drain_gcpm_contexts()` (which captures every fetched stylesheet's GCPM
/// content, `@import`-reached or not, just without a node id). The
/// document-order fold's two sources — `link_gcpm_by_node` (keyed by
/// `Resource::Css`'s node id, never populated for a `<style>` tag's own
/// `@import`) and a plain `parse_gcpm` of the `<style>` tag's own literal
/// text (which contains no GCPM declarations of its own — they're in the
/// *imported* file) — cannot see this content at all, so replacing the old
/// flat computation wholesale would otherwise silently drop it. See
/// `blitz_adapter::parse_gcpm_with_style_imports`'s doc comment for the
/// fix (independent filesystem-based `@import` resolution, since Blitz
/// gives `FulgurNetProvider` no way to attribute this fetch back to the
/// declaring `<style>` node).
#[test]
fn style_tag_top_level_import_content_survives() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("chapters.css"),
        r#".target { bookmark-level: 1; bookmark-label: "FromStyleImport"; }"#,
    )
    .unwrap();

    let html = r#"<!doctype html><html><head>
        <style>@import url("chapters.css");</style>
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
        vec!["FromStyleImport".to_string()],
        "a top-level <style>@import(...)> target's GCPM content must survive \
         end-to-end, got {titles:?}"
    );
}

/// Same regression, but for the AssetBundle-injected `<style>` node rather
/// than an author-written one — `InjectCssPass` puts AssetBundle's
/// `combined_css` into a `<style>` tag too, so it is equally exposed to
/// this bug if AssetBundle CSS itself declares a top-level `@import`.
#[test]
fn assetbundle_css_top_level_import_content_survives() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("chapters.css"),
        r#".target { bookmark-level: 1; bookmark-label: "FromAssetBundleImport"; }"#,
    )
    .unwrap();

    let mut assets = AssetBundle::new();
    assets.add_css(r#"@import url("chapters.css");"#);

    let html = r#"<!doctype html><html><body>
        <p class="target">Content</p>
    </body></html>"#;

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
        vec!["FromAssetBundleImport".to_string()],
        "AssetBundle CSS's own top-level @import target's GCPM content must \
         survive end-to-end, got {titles:?}"
    );
}

/// The `@import` resolution must recurse: a `<style>` tag's top-level
/// `@import` target can itself `@import` a further file, and that file's
/// GCPM content must also survive.
///
/// NOTE on what this test does and doesn't prove: the outline order here
/// (`["Child", "Parent"]`) is consistent with genuine child-before-parent
/// FOLD ordering, but it's equally consistent with plain DOM-order-driven
/// placement independent of fold order entirely — `<p class="child-rule">`
/// simply appears before `<p class="parent-rule">` in the DOM, and
/// `BookmarkPass`'s tree walk emits one outline entry per matching element
/// strictly in DOM order regardless of which CSS source contributed the
/// winning mapping. What this test DOES isolate: recursion actually
/// happened (drop the recursive `resolve_style_imports` call and you'd get
/// `["Parent"]` only, since child.css's content would never be reached at
/// all) and both files' content survives without being dropped or
/// corrupted. For a test that isolates true fold order (which mapping
/// wins a same-element, equal-specificity tie — the one thing DOM order
/// can't explain), see
/// `style_tag_nested_import_fold_order_parent_wins_tie_over_child` below.
#[test]
fn style_tag_nested_import_recursion_reaches_both_files() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("parent.css"),
        r#"@import "child.css"; .parent-rule { bookmark-level: 1; bookmark-label: "Parent"; }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("child.css"),
        r#".child-rule { bookmark-level: 1; bookmark-label: "Child"; }"#,
    )
    .unwrap();

    let html = r#"<!doctype html><html><head>
        <style>@import url("parent.css");</style>
    </head><body>
        <p class="child-rule">Child content</p>
        <p class="parent-rule">Parent content</p>
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
        vec!["Child".to_string(), "Parent".to_string()],
        "recursion must reach child.css through parent.css's own @import — \
         both files' content must survive, got {titles:?}"
    );
}

/// Isolates true fold order specifically (not DOM order, which
/// `style_tag_nested_import_recursion_reaches_both_files` above cannot
/// rule out): `child.css` and `parent.css` both declare a rule for the
/// SAME selector — matching the SAME single element, so there is only one
/// outline entry and no DOM-order confound — at EQUAL specificity, with
/// different labels. Per CSS's "`@import` is equivalent to inlining at the
/// top of the importing stylesheet" semantics, `parent.css`'s own rule
/// (which, being valid CSS, can only appear textually AFTER its own
/// `@import "child.css"`) must win the tie over `child.css`'s rule. If the
/// fold instead placed imported content AFTER the importer's own direct
/// declarations (the reverse, incorrect order), `child.css`'s rule would
/// win instead — this test would then observe `"Child"`, not `"Parent"`.
#[test]
fn style_tag_nested_import_fold_order_parent_wins_tie_over_child() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("parent.css"),
        r#"@import "child.css"; .shared { bookmark-level: 1; bookmark-label: "Parent"; }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("child.css"),
        r#".shared { bookmark-level: 1; bookmark-label: "Child"; }"#,
    )
    .unwrap();

    let html = r#"<!doctype html><html><head>
        <style>@import url("parent.css");</style>
    </head><body>
        <p class="shared">Content</p>
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
        vec!["Parent".to_string()],
        "parent.css's own rule (later in cascade than its own @import) \
         must win the equal-specificity tie over child.css's rule — proves \
         genuine child-before-parent fold order, got {titles:?}"
    );
}

/// Isolates a DIFFERENT instance of the same fold-order requirement: a
/// `<style>` tag's OWN direct declaration versus content pulled in by that
/// SAME tag's OWN top-level `@import` (not a nested file's declaration vs.
/// a further nested import, which
/// `style_tag_nested_import_fold_order_parent_wins_tie_over_child` above
/// covers — this is `blitz_adapter::parse_gcpm_with_style_imports`'s own
/// top-level merge order specifically, not
/// `resolve_style_imports`'s recursive merge order). Since a valid
/// `@import` must appear before any other rule in the same stylesheet, the
/// `<style>` tag's own rule (textually after its own `@import`) must win
/// an equal-specificity tie against the imported rule.
#[test]
fn style_tag_own_declaration_wins_tie_over_its_own_import() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("child.css"),
        r#".shared { bookmark-level: 1; bookmark-label: "Child"; }"#,
    )
    .unwrap();

    let html = r#"<!doctype html><html><head>
        <style>@import url("child.css"); .shared { bookmark-level: 1; bookmark-label: "Own"; }</style>
    </head><body>
        <p class="shared">Content</p>
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
        vec!["Own".to_string()],
        "the <style> tag's own direct rule (textually after its own \
         @import) must win the equal-specificity tie over the imported \
         rule, got {titles:?}"
    );
}
