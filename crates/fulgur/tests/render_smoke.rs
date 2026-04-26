//! End-to-end render smoke tests for `Engine::render_html`.
//!
//! Visual / pixel-level checks live in `crates/fulgur-vrt`; that crate is
//! excluded from the codecov measurement (`cargo llvm-cov nextest --workspace
//! --exclude fulgur-vrt`). These tests therefore exist purely to drive draw /
//! convert / pageable paths through `Engine::render_html` so coverage
//! attribution is recorded for new code added to those paths.
//!
//! When you add a new draw path (e.g. a `draw_background_layer` match arm),
//! also add a smoke test here — see CLAUDE.md "Coverage scope" Gotcha.

use fulgur::{AssetBundle, Engine};
use tempfile::tempdir;

#[test]
fn test_render_html_resolves_link_stylesheet() {
    let dir = tempdir().unwrap();
    let css_path = dir.path().join("test.css");
    std::fs::write(&css_path, "p { color: red; }").unwrap();

    let html = r#"<html><head><link rel="stylesheet" href="test.css"></head><body><p>Hello</p></body></html>"#;

    let engine = Engine::builder().base_path(dir.path()).build();
    let result = engine.render_html(html);
    assert!(result.is_ok());
}

#[test]
fn test_render_html_link_stylesheet_with_gcpm() {
    // <link>-loaded CSS that contains @page / running / counter rules
    // must produce a PDF identical in structure to the same CSS passed
    // via --css. Specifically the running header div should NOT appear
    // as body content.
    let dir = tempdir().unwrap();
    let css_path = dir.path().join("style.css");
    std::fs::write(
        &css_path,
        r#"
        .pageHeader { position: running(pageHeader); }
        @page { @top-center { content: element(pageHeader); } }
        body { font-family: sans-serif; }
        "#,
    )
    .unwrap();

    let html = r#"<!DOCTYPE html>
<html><head><link rel="stylesheet" href="style.css"></head>
<body>
<div class="pageHeader">RUNNING HEADER TEXT</div>
<h1>Body Heading</h1>
<p>Body paragraph.</p>
</body></html>"#;

    let engine = Engine::builder().base_path(dir.path()).build();
    let pdf = engine.render_html(html).expect("render");

    // Crude check: the PDF should have at least one page and not be
    // empty. A more thorough comparison would require pdf parsing in
    // tests, which we skip; the PR's verification step renders the
    // header-footer example and visually compares against the
    // --css output.
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_render_html_link_stylesheet_with_import() {
    // @import within a <link>-loaded stylesheet should also be
    // resolved by FulgurNetProvider via Blitz/stylo's StylesheetLoader.
    // The imported file is also fed through the GCPM parser, so
    // running elements declared inside an @import target are honoured.
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("base.css"),
        r#"@import "header.css"; body { font-family: serif; }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("header.css"),
        r#"
        .pageHeader { position: running(pageHeader); }
        @page { @top-center { content: element(pageHeader); } }
        "#,
    )
    .unwrap();

    let html = r#"<!DOCTYPE html>
<html><head><link rel="stylesheet" href="base.css"></head>
<body>
<div class="pageHeader">FROM IMPORT</div>
<p>Body.</p>
</body></html>"#;

    let engine = Engine::builder().base_path(dir.path()).build();
    let pdf = engine.render_html(html).expect("render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_render_html_link_stylesheet_rejects_path_traversal() {
    // A <link href="../secret.css"> outside the base_path must be
    // ignored even if the file exists on disk. We can't easily verify
    // "no styles applied" without parsing the PDF, but we can verify
    // the engine doesn't error out and produces output.
    let parent = tempdir().unwrap();
    let base = parent.path().join("base");
    std::fs::create_dir(&base).unwrap();
    std::fs::write(parent.path().join("secret.css"), "body { color: red; }").unwrap();

    let html = r#"<!DOCTYPE html>
<html><head><link rel="stylesheet" href="../secret.css"></head>
<body><p>Hi</p></body></html>"#;

    let engine = Engine::builder().base_path(&base).build();
    let pdf = engine.render_html(html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_marker_content_url_does_not_panic() {
    let html = r#"<!doctype html>
<html><head><style>
li::marker { content: url("bullet.png"); }
</style></head>
<body><ul><li>Item</li></ul></body></html>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("render should not panic");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_marker_content_url_with_image() {
    // 1x1 red PNG (valid, generated with correct CRC checksums)
    let png_data: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    let mut bundle = AssetBundle::default();
    bundle.add_css(r#"li::marker { content: url("bullet.png"); }"#);
    bundle.add_image("bullet.png", png_data);

    let html = r#"<!doctype html>
<html><body><ul><li>Item 1</li><li>Item 2</li></ul></body></html>"#;

    let engine = Engine::builder().assets(bundle).build();
    let pdf = engine
        .render_html(html)
        .expect("render should succeed with marker image");
    assert!(!pdf.is_empty(), "PDF should be non-empty");
}

/// `repeating-linear-gradient` を end-to-end で render し、`draw_background_layer`
/// の `LinearGradient { repeating: true }` 経路 (uniform-grid → tiling pattern) を
/// coverage 上カバーする。VRT 側で同等の reftest はあるが、CI が `--exclude fulgur-vrt`
/// で coverage 計測しているため lib 側にも smoke test が必要。
#[test]
fn test_render_repeating_linear_gradient_smoke() {
    let html = r#"<!doctype html>
<html><body>
<div style="width:200px;height:100px;background:repeating-linear-gradient(to right, red 0%, blue 25%);"></div>
</body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render repeating-linear-gradient");
    assert!(!pdf.is_empty());
}

/// `repeating-radial-gradient` の end-to-end smoke test。`RadialGradient { repeating: true }`
/// 経路をカバーする。
#[test]
fn test_render_repeating_radial_gradient_smoke() {
    let html = r#"<!doctype html>
<html><body>
<div style="width:200px;height:200px;background:repeating-radial-gradient(circle 100px at center, red 0px, blue 25px);"></div>
</body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render repeating-radial-gradient");
    assert!(!pdf.is_empty());
}

/// `linear-gradient(to top right, ...)` (Corner direction) の smoke test。
/// `draw_background_layer` の `LinearGradientDirection::Corner` 経路は既存だが
/// `repeating` 追加に伴い destructure を含む match arm を再書きしたため、
/// patch coverage を満たすために lib 側にも end-to-end カバーを置いておく。
#[test]
fn test_render_linear_gradient_corner_direction_smoke() {
    let html = r#"<!doctype html>
<html><body>
<div style="width:200px;height:100px;background:linear-gradient(to top right, red, blue);"></div>
</body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render corner-direction linear gradient");
    assert!(!pdf.is_empty());
}

/// `background-size` で複数タイルを生成して `try_uniform_grid` Some パスを
/// 通す smoke test。これで linear gradient の uniform-grid → tiling pattern
/// 経路が coverage に乗る。
#[test]
fn test_render_linear_gradient_tiled_smoke() {
    let html = r#"<!doctype html>
<html><body>
<div style="width:200px;height:100px;background:linear-gradient(red, blue);background-size:50px 50px;"></div>
</body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render tiled linear gradient");
    assert!(!pdf.is_empty());
}
