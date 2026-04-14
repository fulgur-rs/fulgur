//! FAILING tests (pre-implementation): `<link rel="stylesheet" media="...">`
//! must honour the media query. Until `LinkMediaRewritePass` lands,
//! blitz-dom 0.2.4's CssHandler hardcodes `MediaList::empty()` and media-restricted
//! linked stylesheets are applied unconditionally — so
//! `link_media_print_does_not_apply_on_screen` fails today and passes
//! after Task 6 wires the rewrite in.

use std::fs;
use std::path::Path;
use std::process::Command;

use fulgur::{Engine, PageSize};
use tempfile::tempdir;

fn render_contains_red(html: &str, base: &Path) -> Option<bool> {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .base_path(base.to_path_buf())
        .build();
    let pdf = engine.render_html(html).expect("render must succeed");

    let work = tempdir().unwrap();
    let pdf_path = work.path().join("fixture.pdf");
    fs::write(&pdf_path, &pdf).unwrap();

    let prefix = work.path().join("page");
    let status = match Command::new("pdftocairo")
        .args(["-png", "-r", "100", "-f", "1", "-l", "1"])
        .arg(&pdf_path)
        .arg(&prefix)
        .status()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "skipping: pdftocairo not available ({e}); \
                 install poppler-utils (apt install poppler-utils) to run this test"
            );
            return None;
        }
    };
    assert!(status.success(), "pdftocairo failed");

    let png_path = work.path().join("page-1.png");
    let img = image::open(&png_path).expect("decode PNG").to_rgba8();
    Some(
        img.pixels()
            .any(|p| p[0] > 200 && p[1] < 60 && p[2] < 60 && p[3] > 0),
    )
}

#[test]
fn link_media_print_does_not_apply_on_screen() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("print.css"), "body { background: red; }\n").unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="print.css" media="print">
        </head><body>
            <p style="color:black">hello</p>
        </body></html>
    "#;

    let result = match render_contains_red(html, root) {
        Some(v) => v,
        None => return, // pdftocairo unavailable; harmless skip
    };
    assert!(
        !result,
        "print.css must not be applied during screen rendering"
    );
}

#[test]
fn link_without_media_still_applies() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("base.css"), "body { background: red; }\n").unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="base.css">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    let result = match render_contains_red(html, root) {
        Some(v) => v,
        None => return, // pdftocairo unavailable; harmless skip
    };
    assert!(
        result,
        "unqualified <link> must apply; regression guard for the media rewrite"
    );
}

/// Rewrite-path liveness: fulgur renders with `media=screen`, so a
/// `<link media=screen>` triggers the LinkMediaRewrite pipeline AND
/// should still end up applying its rules. If the synthetic
/// `<style>@import url() screen;</style>` is never registered with
/// Stylo, the red background would be silently dropped even though
/// the media query matches. This test catches that regression.
#[test]
fn link_media_matching_screen_still_loads_via_rewrite() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("matching.css"), "body { background: red; }\n").unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="matching.css" media="screen">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    let result = match render_contains_red(html, root) {
        Some(v) => v,
        None => return,
    };
    assert!(
        result,
        "media=screen matches fulgur's screen device; rewritten stylesheet must still apply"
    );
}

#[test]
fn link_media_all_still_applies() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("base.css"), "body { background: red; }\n").unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="base.css" media="all">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    let result = match render_contains_red(html, root) {
        Some(v) => v,
        None => return, // pdftocairo unavailable; harmless skip
    };
    assert!(
        result,
        "media=all is the identity; must not be stripped by the rewrite"
    );
}

/// Pin for beads fulgur-owa: when a `<link media=print>` CSS contains
/// GCPM constructs (`@page`, running elements, string-set, counters),
/// the margin-box rule is currently added twice to the effective GCPM
/// context — once from the wasted first fetch, once from the rewrite's
/// `@import` re-fetch. This test asserts the CORRECT behaviour and is
/// ignored until `FulgurNetProvider` tracks fetch ancestry.
#[test]
#[ignore = "fulgur-owa: GCPM contexts double-counted by <link media> rewrite"]
fn link_media_print_does_not_duplicate_gcpm_context() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    // A print-only CSS that contains a @page margin box. If the rewrite
    // double-counts, the margin box rule will be registered twice.
    fs::write(
        root.join("print.css"),
        r#"
        @page { @top-center { content: "HDR"; } }
        body { color: black; }
        "#,
    )
    .unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="print.css" media="print">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    // We measure by peeking at the engine's merged GCPM context. For this
    // pin we render and assume the observable side-effect is a duplicated
    // margin-box string "HDR" in the resulting PDF page margin. A simple
    // substring count is not reliable against PDF encoding, so this test
    // is kept ignored until a proper assertion path exists (likely via
    // exposing the GCPM context from Engine or from a rendered PDF VRT).
    //
    // For now this test just documents the expectation and must be wired
    // up in the fulgur-owa follow-up issue.
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .base_path(root.to_path_buf())
        .build();
    let pdf = engine.render_html(html).expect("render must succeed");
    assert!(!pdf.is_empty());
    // TODO (fulgur-owa): assert margin-box "HDR" appears exactly once in
    // the rendered page margin, not twice.
}

#[test]
fn link_media_print_nested_import_also_excluded_on_screen() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("leaf.css"), "body { background: red; }\n").unwrap();
    fs::write(root.join("print.css"), "@import url(\"leaf.css\");\n").unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="print.css" media="print">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    let result = match render_contains_red(html, root) {
        Some(v) => v,
        None => return, // pdftocairo unavailable; skip
    };
    assert!(
        !result,
        "nested @import under a print-only <link> must also be excluded on screen"
    );
}
