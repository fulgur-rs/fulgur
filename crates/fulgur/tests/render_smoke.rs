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

#[test]
fn test_render_html_conic_gradient_pie_chart() {
    // 4 セクター pie chart。draw_conic_gradient が path wedge を発行し、
    // 同色 wedge は merge されて step transition を表現する。
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:200px;height:200px;
            background:conic-gradient(
                red 0deg, red 90deg,
                yellow 90deg, yellow 180deg,
                green 180deg, green 270deg,
                blue 270deg, blue 360deg);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render conic pie chart");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_conic_gradient_smooth() {
    // 滑らか conic (auto-positioned stops)。fixup と sample_conic_color が
    // 360 wedge ぶん補間色を計算する経路を通す。
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:200px;height:200px;
            background:conic-gradient(red, yellow, green, blue, red);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render smooth conic");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_repeating_conic_gradient() {
    // repeating-conic-gradient: period = (last - first) で fraction を周期化する経路。
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:200px;height:200px;
            background:repeating-conic-gradient(
                red 0deg, red 15deg, blue 15deg, blue 30deg);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render repeating conic");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_conic_gradient_from_angle() {
    // from <angle> で sweep 開始位置をシフトする経路。
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:200px;height:200px;
            background:conic-gradient(from 90deg,
                red 0deg, red 90deg,
                blue 90deg, blue 360deg);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render conic with from angle");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_conic_gradient_at_position() {
    // at <position> で中心オフセットする経路。box_edge_at_angle が中心 ≠ box 中央
    // のケースを扱うことを確認。
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:200px;height:200px;
            background:conic-gradient(at 25% 75%,
                red 0deg, red 90deg,
                yellow 90deg, yellow 180deg,
                green 180deg, green 270deg,
                blue 270deg, blue 360deg);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render conic with offset center");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_shadow_inset_logged_and_skipped() {
    // box-shadow: inset paths the inset-warn skip arm in convert/style/shadow.rs.
    // The shadow must not be drawn (inset is unsupported), but the render must
    // still succeed.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;background:#fff;
                    box-shadow:inset 0 0 0 5px red;"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render inset shadow");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_shadow_blur_warning_path() {
    // Non-zero blur radius hits the blur-warn arm in shadow::apply_to.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;background:#fff;
                    box-shadow:0 0 8px red;"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render blurred shadow");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_shadow_fully_transparent_skipped() {
    // rgba(0,0,0,0) shadows hit the transparent-skip arm in shadow::apply_to.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;background:#fff;
                    box-shadow:5px 5px 0 rgba(0,0,0,0);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render transparent shadow");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_bg_image_unknown_asset() {
    // background-image: url(...) with a non-image asset (or one that
    // AssetKind::detect cannot classify) traverses the AssetKind::Unknown
    // arm in background::apply_to.
    let dir = tempdir().unwrap();
    let bogus = dir.path().join("bogus.dat");
    std::fs::write(&bogus, b"NOT_AN_IMAGE_OR_SVG").unwrap();

    let mut bundle = AssetBundle::default();
    bundle.add_image("bogus.dat", std::fs::read(&bogus).unwrap());

    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;
                    background-image:url(bogus.dat);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render_html(html)
        .expect("render unknown-asset bg");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_bg_image_invalid_svg_logs_and_falls_back() {
    // background-image: url(broken.svg) where the bytes look like SVG (XML)
    // but fail to parse triggers the SVG parse-error arm in
    // background::apply_to (logs warn, returns None).
    let dir = tempdir().unwrap();
    let broken = dir.path().join("broken.svg");
    std::fs::write(&broken, b"<svg<<<not valid xml>>>").unwrap();

    let mut bundle = AssetBundle::default();
    bundle.add_image("broken.svg", std::fs::read(&broken).unwrap());

    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;
                    background-image:url(broken.svg);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render_html(html)
        .expect("render broken-svg bg");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_linear_gradient_keyword_directions() {
    // linear-gradient(to top/bottom/left/right) — Vertical / Horizontal arms in
    // background::resolve_linear_gradient. Default (red, blue) = Angle(180deg)
    // does NOT hit these.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:80px;height:80px;background:linear-gradient(to top, red, blue);"></div>
        <div style="width:80px;height:80px;background:linear-gradient(to bottom, red, blue);"></div>
        <div style="width:80px;height:80px;background:linear-gradient(to left, red, blue);"></div>
        <div style="width:80px;height:80px;background:linear-gradient(to right, red, blue);"></div>
    </body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_linear_gradient_corner_directions() {
    // to top-left / bottom-left / bottom-right Corner arms (top-right is
    // already covered by the existing corner-direction smoke test).
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:80px;height:80px;background:linear-gradient(to top left, red, blue);"></div>
        <div style="width:80px;height:80px;background:linear-gradient(to bottom left, red, blue);"></div>
        <div style="width:80px;height:80px;background:linear-gradient(to bottom right, red, blue);"></div>
    </body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_radial_gradient_shape_variants() {
    // Cover Circle::Radius (single radius), Circle::Extent (closest-side etc.),
    // Ellipse::Radii arms in resolve_radial_gradient.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:120px;height:80px;background:radial-gradient(closest-side, red, blue);"></div>
        <div style="width:120px;height:80px;background:radial-gradient(farthest-side, red, blue);"></div>
        <div style="width:120px;height:80px;background:radial-gradient(closest-corner, red, blue);"></div>
        <div style="width:120px;height:80px;background:radial-gradient(farthest-corner, red, blue);"></div>
        <div style="width:120px;height:80px;background:radial-gradient(circle 30px, red, blue);"></div>
        <div style="width:120px;height:80px;background:radial-gradient(ellipse 40px 30px, red, blue);"></div>
    </body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_bg_repeat_origin_clip_variants() {
    // Cover non-default convert_bg_repeat / convert_bg_origin / convert_bg_clip
    // arms (NoRepeat, Space, Round, PaddingBox, ContentBox).
    // 1x1 red PNG (valid CRCs — same fixture as marker-image test above).
    let png_data: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", png_data);

    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:80px;height:80px;background:url(dot.png) no-repeat;"></div>
        <div style="width:80px;height:80px;background:url(dot.png) space;"></div>
        <div style="width:80px;height:80px;background:url(dot.png) round;"></div>
        <div style="width:80px;height:80px;padding:10px;background:url(dot.png);background-origin:padding-box;background-clip:padding-box;"></div>
        <div style="width:80px;height:80px;padding:10px;background:url(dot.png);background-origin:content-box;background-clip:content-box;"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render_html(html)
        .expect("render bg repeat/origin/clip variants");
    assert!(!pdf.is_empty());
}
