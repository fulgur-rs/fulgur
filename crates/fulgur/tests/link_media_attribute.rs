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
