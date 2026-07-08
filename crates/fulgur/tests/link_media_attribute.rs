//! Media-query handling after the blitz 0.3 upgrade (epic fulgur-trnn).
//!
//! fulgur now renders as **print** media: `parse_inner` sets
//! `DocumentConfig::media_type = MediaType::print()`, the blitz 0.3 official
//! API. blitz evaluates `@media` rules natively against that device, so
//! `@media print { … }` applies and `@media screen { … }` is excluded — this
//! retired fulgur's old CSS-text rewrite hack and resolves the long-standing
//! "print vs screen device" question (was tracked as fulgur-801).
//!
//! Caveat: blitz 0.3's stylesheet handler still hardcodes `MediaList::empty()`
//! for `<link rel=stylesheet media=…>` elements, so the `<link>` *media
//! attribute* itself is still ignored (only `@media` blocks inside the CSS are
//! gated). fulgur's previous workaround rewrote `<link media=X>` into
//! `<style>@import url(...) X;</style>`, but that relied on intercepting the
//! resources blitz pre-fetched — a hook the 0.3 net architecture removed
//! (resources now flow through the document's own event channel). Re-supporting
//! the `<link>` media attribute is tracked as a follow-up.

use std::fs;
use std::path::Path;
use std::process::Command;

use fulgur::{Engine, PageSize};
use tempfile::tempdir;

/// Render `html` and report whether the page contains a strong red pixel
/// (our marker colour). Returns `None` when `pdftocairo` is unavailable so
/// the test degrades to a harmless skip in minimal CI images.
fn render_contains_red(html: &str, base: &Path) -> Option<bool> {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .base_path(base.to_path_buf())
        .build();
    let pdf = engine.render(html).expect("render must succeed");

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

    let Some(result) = render_contains_red(html, root) else {
        return; // pdftocairo unavailable; harmless skip
    };
    assert!(result, "unqualified <link> must apply");
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

    let Some(result) = render_contains_red(html, root) else {
        return;
    };
    assert!(result, "media=all is the identity and must apply");
}

/// The headline win of the blitz 0.3 upgrade: `@media print` is evaluated
/// natively against fulgur's print device, so print-only rules apply.
#[test]
fn at_media_print_block_applies() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(
        root.join("sheet.css"),
        "@media print { body { background: red; } }\n",
    )
    .unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="sheet.css">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    let Some(result) = render_contains_red(html, root) else {
        return;
    };
    assert!(
        result,
        "@media print rules must apply — fulgur renders for print media"
    );
}

/// Mirror of the above: `@media screen` must be excluded, because fulgur's
/// device is print, not screen.
#[test]
fn at_media_screen_block_excluded() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(
        root.join("sheet.css"),
        "@media screen { body { background: red; } }\n",
    )
    .unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="sheet.css">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    let Some(result) = render_contains_red(html, root) else {
        return;
    };
    assert!(
        !result,
        "@media screen rules must be excluded under fulgur's print device"
    );
}

/// IDEAL behaviour, currently unmet: a `<link media=screen>` stylesheet should
/// be excluded under the print device. blitz 0.3 still hardcodes
/// `MediaList::empty()` for `<link>` stylesheets, so the media attribute is
/// ignored and the sheet applies unconditionally. Ignored until the `<link>`
/// media attribute is re-supported (follow-up to the blitz 0.3 upgrade).
#[test]
#[ignore = "blitz 0.3 ignores the <link media> attribute (MediaList::empty hardcode); \
            fulgur's rewrite hack was removed with the 0.3 net redesign — follow-up pending"]
fn link_media_screen_should_be_excluded_under_print_device() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("screen.css"), "body { background: red; }\n").unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="screen.css" media="screen">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    let Some(result) = render_contains_red(html, root) else {
        return;
    };
    assert!(
        !result,
        "a screen-only <link> must not apply under fulgur's print device"
    );
}
