//! `@media print` ルールが fulgur 生成 PDF に適用されることを確認する
//! 統合テスト。fulgur は PDF 生成専用であり、常に print media として
//! レンダリングされる。

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
        Err(_) => return None,
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
fn inline_media_print_applies() {
    let dir = tempdir().unwrap();
    let html = r#"
        <!DOCTYPE html>
        <html><head><style>
            body { background: white; }
            @media print { body { background: red; } }
        </style></head><body><p>hi</p></body></html>
    "#;

    let result = match render_contains_red(html, dir.path()) {
        Some(v) => v,
        None => return,
    };
    assert!(
        result,
        "@media print rules must apply to fulgur's PDF output"
    );
}

#[test]
fn inline_media_screen_does_not_apply() {
    let dir = tempdir().unwrap();
    let html = r#"
        <!DOCTYPE html>
        <html><head><style>
            body { background: white; }
            @media screen { body { background: red; } }
        </style></head><body><p>hi</p></body></html>
    "#;

    let result = match render_contains_red(html, dir.path()) {
        Some(v) => v,
        None => return,
    };
    assert!(
        !result,
        "@media screen rules must NOT apply to fulgur's print-mode PDF output"
    );
}
