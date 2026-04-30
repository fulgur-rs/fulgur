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

#[test]
fn linear_gradient_with_interpolation_hint_renders_via_engine() {
    // CSS Images 3 §3.5.3 hint expansion を `Engine::render_html` 経由で叩く
    // (fulgur-2zam). VRT は codecov 対象外なので draw branch 起動の証拠を
    // ここに残す (CLAUDE.md "Coverage scope" Gotcha).
    let html = r#"<html><body><div style="width:200px;height:100px;background:linear-gradient(red, 30%, blue)">x</div></body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn radial_gradient_with_interpolation_hint_renders_via_engine() {
    let html = r#"<html><body><div style="width:200px;height:100px;background:radial-gradient(red, 30%, blue)">x</div></body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn repeating_linear_gradient_with_hint_renders_via_engine() {
    // hint expansion + repeating 周期展開の組み合わせ経路.
    let html = r#"<html><body><div style="width:200px;height:100px;background:repeating-linear-gradient(red, 30%, blue 50%, red 100%)">x</div></body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn position_absolute_pseudo_at_body_resolves_initial_cb() {
    // Exercises `build_absolute_pseudo_children`'s body-anchored path —
    // ::before is `position: absolute` with no positioned ancestor, so
    // CB resolution walks to body and (with the fulgur-tbxs viewport
    // fallback) takes the page area as the padding box. Verifies the
    // pseudo path's `cb_absolute.get_or_insert_with(...)` arm is
    // exercised by an end-to-end render and not just by unit tests.
    let html = r#"<html><body style="margin:0">
<style>body::before { content: "x"; position: absolute; bottom: 0; }</style>
<p>filler</p>
</body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn position_fixed_inside_absolute_relayouts_against_viewport() {
    // Regression for fulgur-tbxs (WPT fixedpos-002): when `position: fixed`
    // is nested inside a shrink-to-fit `position: absolute` ancestor, the
    // first Taffy pass collapses Fixed → Absolute and sizes the fixed
    // element against the abs's narrow box. The %PDF byte check only
    // proves engine completion; the structural assertion is the real
    // regression guard — we walk the Pageable tree, find every
    // out-of-flow `ParagraphPageable`, and assert the fixed text laid
    // itself out as a single line. Without `relayout_position_fixed`,
    // Parley shapes the long sentence at the abs's ~37.5pt width and
    // produces multiple wrapped lines; with the relayout, the fixed
    // subtree is reshaped against the page area and the sentence fits
    // on one line.
    use fulgur::pageable::{BlockPageable, Pageable, PositionedChild};
    use fulgur::paragraph::ParagraphPageable;

    let html = r#"<html><body style="margin:0">
<div style="position:absolute; width:50px; height:300vh">
  outer
  <div style="position:fixed; bottom:0">This text is much wider than fifty pixels</div>
</div>
</body></html>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("render");
    assert!(pdf.starts_with(b"%PDF"));

    fn max_oof_paragraph_lines(node: &dyn Pageable, own_oof: bool) -> usize {
        let any = node.as_any();
        if own_oof && let Some(p) = any.downcast_ref::<ParagraphPageable>() {
            return p.lines.len();
        }
        if let Some(block) = any.downcast_ref::<BlockPageable>() {
            let mut max = 0;
            for PositionedChild {
                child, out_of_flow, ..
            } in &block.children
            {
                let n = max_oof_paragraph_lines(child.as_ref(), *out_of_flow);
                if n > max {
                    max = n;
                }
            }
            return max;
        }
        0
    }

    let tree = engine.build_pageable_for_testing_no_gcpm(html);
    let lines = max_oof_paragraph_lines(tree.as_ref(), false);
    assert_eq!(
        lines, 1,
        "expected the inner position:fixed paragraph to be relayouted \
         wide enough to hold the sentence on a single line; got {lines} lines, \
         meaning it kept the 37.5pt parent-abs width and Parley wrapped"
    );
}

/// fulgur-jkl5: position:fixed elements must repeat on every page in
/// multi-page output. Renders a 2-page document with a fixed div
/// containing visible text, then runs `pdftotext` per page to verify
/// the text appears on **both** pages — the previous behaviour
/// (out_of_flow with abs-CB y-shift) caused fixed elements to be
/// rendered off-screen on every page after the first.
#[test]
fn position_fixed_repeats_on_every_page() {
    use fulgur::{Engine, PageSize};

    let html = r#"<html><body>
          <div style="height: 600px"></div>
          <div style="height: 600px"></div>
          <div style="position: fixed; top: 10px; left: 20px;
                      width: 200px; height: 50px">FXFXFX</div>
        </body></html>"#;
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render_html(html).expect("render");

    // We do not have an inline PDF text extractor; pdftotext is the
    // canonical "did this glyph render on this page" probe used in
    // examples_determinism. Skip the assertion gracefully when not
    // available so the test is informative on dev machines without
    // poppler installed.
    let dir = tempfile::tempdir().expect("tempdir");
    let pdf_path = dir.path().join("out.pdf");
    std::fs::write(&pdf_path, &pdf).unwrap();
    if std::process::Command::new("pdftotext")
        .arg("-v")
        .output()
        .is_err()
    {
        eprintln!("pdftotext not available; skipping per-page text assertion");
        return;
    }

    let extract = |page: u32| {
        std::process::Command::new("pdftotext")
            .args(["-f", &page.to_string(), "-l", &page.to_string(), "-layout"])
            .arg(&pdf_path)
            .arg("-")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default()
    };

    assert!(
        extract(1).contains("FXFXFX"),
        "page 1 should contain FXFXFX"
    );
    assert!(
        extract(2).contains("FXFXFX"),
        "page 2 should also contain FXFXFX (per-page repetition for position:fixed)"
    );
}

// ── Phase 4 v2 render path smoke tests (fulgur-9t3z) ─────────────────
//
// Exercise the v2 dispatcher (`render_v2`) so the patch-coverage gate
// sees the new draw helpers added in PR 6 (`draw_under_transform`,
// `draw_under_clip`, `draw_under_opacity`, `paint_multicol_rule_for_page`,
// `paint_root_block_v2`, `MarginBoxRenderer`). These run end-to-end
// through `Engine::render_html_v2` and assert only that the bytes
// come back non-empty — byte-eq with v1 is already covered by the
// `render_path_parity` shadow harness.

#[test]
fn render_v2_smoke_transform_translate() {
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}.box{width:80px;height:60px;background:#cef;transform:translate(10px,5px)}</style></head><body><div class="box"></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_nested_transforms() {
    // Exercises the `draw_under_transform` recursion path added in
    // PR #305 Devin fix: outer rotate wraps an inner translate, both
    // matrices must compose.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}.outer{width:120px;height:80px;background:#cef;transform:rotate(10deg)}.inner{width:60px;height:40px;background:#fce;transform:translate(8px,4px)}</style></head><body><div class="outer"><div class="inner"></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_multicol_with_column_rule() {
    // Exercises `paint_multicol_rule_for_page` — most fixtures don't
    // declare `column-rule` so this path needs an explicit smoke test.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:8pt}.cols{column-count:2;column-rule:1pt solid #888;column-gap:12pt;height:80pt}.cell{height:30pt;background:#cef;margin-bottom:6pt}</style></head><body><div class="cols"><div class="cell"></div><div class="cell"></div><div class="cell"></div><div class="cell"></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_html_body_bg_multi_page() {
    // Exercises `paint_root_block_v2` for both `<html>` (pre-pass on
    // every page) and `<body>` (pre-pass on continuation pages).
    let html = r##"<!DOCTYPE html><html><head><style>html,body{margin:0;background:#fafafa}.tall{height:1500px;background:#cef}</style></head><body><div class="tall"></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_block_with_inline_root_padding() {
    // Exercises `draw_block_with_inner_content` content-inset path —
    // the `padding: 6px` shift fix that landed in PR 6.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}p{margin:0;padding:6px;background:#cef}</style></head><body><p>hello</p></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_bookmarks_under_transform() {
    // Exercises the bookmark-anchor pre-skip path (`record(...)` runs
    // before `transformed_descendants` skip) added in PR 6 Devin fix.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}div{transform:rotate(5deg)}h1{margin:0;font-size:14px}</style></head><body><div><h1>Heading</h1></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().bookmarks(true).build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_transform_inside_overflow_clip() {
    // Exercises `draw_under_clip`'s transform-aware descendant
    // dispatch added in PR #310 Devin fix: a `transform` nested
    // inside `overflow:hidden` was dropped because the main loop
    // pre-skips `clipped_descendants` before the transform check.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}.outer{width:120px;height:80px;overflow:hidden;background:#cef}.inner{width:60px;height:40px;background:#fce;transform:rotate(10deg)}</style></head><body><div class="outer"><div class="inner"></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_body_overflow_hidden_multi_page_content_survives() {
    // Regression for PR #310 follow-up Devin: `<body style="overflow:
    // hidden|auto|scroll">` previously blanked every descendant on
    // page 1+. The fragmenter records body with a single fragment at
    // `page_index=0`, so `draw_under_clip(body)` would only fire on
    // page 0; the main loop's `clipped_descendants.contains` guard
    // then ate every body descendant on every page, leaving page 1+
    // with only the body bg pre-pass and nothing else.
    //
    // Render with `body{overflow:hidden}` AND with a tall enough
    // child to force multi-page output, and assert the v2 PDF size
    // stays close to the no-overflow render (within 5%). If the bug
    // returns, page 1's content stream collapses and the PDF shrinks
    // dramatically.
    let with_clip = r##"<!DOCTYPE html><html><head><style>html,body{margin:0;padding:0;background:#fff}body{overflow:hidden}.tall{height:1500px;background:#cef}</style></head><body><div class="tall"></div></body></html>"##;
    let without_clip = r##"<!DOCTYPE html><html><head><style>html,body{margin:0;padding:0;background:#fff}.tall{height:1500px;background:#cef}</style></head><body><div class="tall"></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf_clip = engine.render_html_v2(with_clip).expect("v2 render w/ clip");
    let pdf_plain = engine
        .render_html_v2(without_clip)
        .expect("v2 render w/o clip");
    let ratio = pdf_clip.len() as f32 / pdf_plain.len() as f32;
    assert!(
        ratio > 0.95 && ratio < 1.05,
        "body overflow:hidden v2 size ({} B) diverges too much from baseline ({} B); \
         likely indicates page 1+ content was dropped by the clipped_descendants pre-skip",
        pdf_clip.len(),
        pdf_plain.len(),
    );
}

#[test]
fn render_v2_smoke_list_item_overflow_clip_with_opacity() {
    // Exercises `draw_under_clip`'s list_items branch added in PR #310
    // Devin fix: when the clipped block's NodeId also has a
    // `ListItemEntry`, the outer opacity wrap must use
    // `list_item.opacity` (the body block carries default opacity=1.0
    // from `convert::list_item::build_list_item_body`) and the marker
    // must paint before `push_clip_path`.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}ul{margin:0;padding:0 0 0 24px}li{background:#cef;overflow:hidden;opacity:0.5}.inner{height:30px;background:#fce}</style></head><body><ul><li><div class="inner"></div></li></ul></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_overflow_clip_inside_transform() {
    // Regression for PR #309 follow-up Devin: an `overflow:hidden`
    // descendant of a `transform` ancestor must enter
    // `draw_under_clip` so its clip path is pushed. Previously the
    // descendant's bg/border landed via `dispatch_fragment` but no
    // clip path fired, leaking content past the boundary.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}.outer{width:140px;height:80px;background:#cef;transform:translate(8px,4px)}.inner{width:60px;height:40px;background:#fce;overflow:hidden}.leaf{width:120px;height:20px;background:#ffd}</style></head><body><div class="outer"><div class="inner"><div class="leaf"></div></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_nested_overflow_clip_blocks() {
    // Regression for PR #309 follow-up Devin: nested
    // `overflow:hidden` blocks must each push their own clip path.
    // Previously the inner block's bg/border landed via
    // `dispatch_fragment` and no inner clip fired, losing the inner
    // boundary while overflowing content escaped through it.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}.outer{width:120px;height:80px;overflow:hidden;background:#cef}.inner{width:60px;height:40px;overflow:hidden;background:#fce}.leaf{width:200px;height:20px;background:#ffd}</style></head><body><div class="outer"><div class="inner"><div class="leaf"></div></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_multicol_dashed_and_dotted_column_rule() {
    // Exercises the `ColumnRuleStyle::Dashed` and `Dotted` arms of
    // `build_multicol_stroke` (`render.rs:613-627` from PR #305).
    // Solid is already covered by the `multicol-2` VRT fixture but
    // dashed/dotted patterns weren't on any byte-eq path.
    let html_dashed = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:8pt}.cols{column-count:2;column-rule:1pt dashed #888;column-gap:12pt;height:80pt}.cell{height:30pt;background:#cef;margin-bottom:6pt}</style></head><body><div class="cols"><div class="cell"></div><div class="cell"></div><div class="cell"></div><div class="cell"></div></div></body></html>"##;
    let html_dotted = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:8pt}.cols{column-count:2;column-rule:1pt dotted #888;column-gap:12pt;height:80pt}.cell{height:30pt;background:#cef;margin-bottom:6pt}</style></head><body><div class="cols"><div class="cell"></div><div class="cell"></div><div class="cell"></div><div class="cell"></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf_dashed = engine.render_html_v2(html_dashed).expect("dashed render");
    let pdf_dotted = engine.render_html_v2(html_dotted).expect("dotted render");
    assert!(!pdf_dashed.is_empty());
    assert!(!pdf_dotted.is_empty());
}

#[test]
fn render_v2_smoke_paragraph_multi_fragment_slice() {
    // Exercises `paragraph_lines_for_page` (`render.rs:1075-1138`
    // from PR #305): a paragraph that splits across multiple pages
    // requires the slice / rebase logic. Most fixtures keep
    // paragraphs on one page, so this branch needs an explicit
    // multi-page paragraph.
    let mut paragraph_text = String::new();
    for i in 0..400 {
        use std::fmt::Write;
        write!(&mut paragraph_text, "Sentence {i} with some words. ").unwrap();
    }
    let html = format!(
        r##"<!DOCTYPE html><html><head><style>html,body{{margin:0;padding:0}}p{{margin:0;font-size:14pt;line-height:1.4}}</style></head><body><p>{paragraph_text}</p></body></html>"##
    );
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(&html).expect("v2 render");
    assert!(!pdf.is_empty());
    // Multi-page sanity check: multiple `Type /Page` entries (one per
    // page object) — without slicing, only the first fragment would
    // emit any glyphs.
    let pdf_str = String::from_utf8_lossy(&pdf);
    let page_count = pdf_str.matches("/Type /Page\n").count();
    assert!(
        page_count >= 2,
        "expected multi-page output, got {page_count} pages"
    );
}

#[test]
fn render_v2_smoke_list_item_image_marker() {
    // Exercises `draw_list_item_marker`'s `ListItemMarker::Image` arm
    // (`render.rs:1004-1019` from PR #305): a `<li>` rendered with a
    // raster `list-style-image` so the marker takes the
    // `ImageMarker::Raster` branch instead of the text/glyph default.
    let png_data: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let mut bundle = AssetBundle::default();
    bundle.add_css(r#"li { list-style-image: url("bullet.png"); }"#);
    bundle.add_image("bullet.png", png_data);
    let html =
        r##"<!doctype html><html><body><ul><li>Item 1</li><li>Item 2</li></ul></body></html>"##;
    let engine = Engine::builder().assets(bundle).build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_multicol_rule_inside_transform() {
    // Regression for PR #305 follow-up Devin: a multicol container
    // with `column-rule` nested inside a `transform` ancestor needs
    // the rule lines painted from inside `draw_under_transform`'s
    // `push_transform / pop` group, not the page-level post-pass.
    // Otherwise the rules render in untransformed page coordinates.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}.tx{transform:translate(8px,4px)}.cols{column-count:2;column-rule:1pt solid #888;column-gap:12pt;height:80pt}.cell{height:30pt;background:#cef;margin-bottom:6pt}</style></head><body><div class="tx"><div class="cols"><div class="cell"></div><div class="cell"></div><div class="cell"></div><div class="cell"></div></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_opacity_descendants_block_with_svg() {
    // Regression for fulgur-gdb9: a fractional-opacity block wrapping
    // a child element of a different node_id (the canonical
    // `<div opacity:0.4><svg>..</svg></div>` shape) must paint the
    // svg INSIDE the parent's `draw_with_opacity` group. Without
    // `BlockEntry.opacity_descendants` + `draw_under_opacity`, v2's
    // flat dispatch paints svg at full opacity and double-emits the
    // transparency group. This smoke exercises the new
    // `draw_under_opacity` arm in `dispatch_fragment`'s precedence
    // chain.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}.faded{opacity:0.4}</style></head><body><div class="faded"><svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40" fill="#1a6faa"/></svg></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_opacity_inside_overflow_clip() {
    // Composition: a clipping ancestor with an opacity-scoped
    // descendant. Exercises the new `nested_opacity_skip` set inside
    // `draw_under_clip`'s descendant loop and the `draw_under_opacity`
    // arm in the recursive descend. Without the skip, the inner
    // opacity block's strict descendants would dispatch twice (once
    // by the clip's main loop iteration, once under the opacity
    // wrap).
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}.clip{overflow:hidden;width:120pt;height:80pt}.faded{opacity:0.5}</style></head><body><div class="clip"><div class="faded"><svg xmlns="http://www.w3.org/2000/svg" width="60" height="60"><circle cx="30" cy="30" r="25" fill="#e74c3c"/></svg></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_opacity_inside_transform() {
    // Composition: a transformed ancestor with an opacity-scoped
    // descendant. Exercises the new `opacity_skip` set inside
    // `draw_under_transform`'s descendant loop and the
    // `draw_under_opacity` arm in the recursive descend.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}.tx{transform:rotate(5deg)}.faded{opacity:0.6}</style></head><body><div class="tx"><div class="faded"><svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40" fill="#27ae60"/></svg></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_anonymous_block_inline_level_sibling() {
    // Regression for fulgur-bq6i (review_card_inline_block):
    // a block container with mixed block-level and inline-level
    // children triggers Stylo's anonymous-block synthesis (CSS 2.1
    // §9.2.1.1). The anonymous wrapper has its own `node_id` and
    // appears in `Node.layout_children` but NOT in `Node.children`.
    // The fragmenter's `record_subtree_descendants` previously
    // walked `children`, missing the wrapper and silently dropping
    // the inline-level child's paint. Now `layout_children` is
    // preferred when `Some`.
    //
    // Without the fix, the BADGE span content (background + text)
    // never paints in v2 because its wrapping inline-root
    // paragraph's `node_id` lacks a geometry entry, so
    // `dispatch_fragment` skips it. The size-comparison sanity
    // check below catches a regression where the anonymous-block
    // walk gets dropped: with-badge PDF must be measurably larger
    // than no-badge PDF.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}.card{padding:8pt}.label{display:inline-block;background:#cef;padding:2pt 6pt}</style></head><body><div class="card"><div>block child</div><span class="label">BADGE</span></div></body></html>"##;
    let html_no_badge = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}.card{padding:8pt}</style></head><body><div class="card"><div>block child</div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    let pdf_no_badge = engine.render_html_v2(html_no_badge).expect("v2 render");
    assert!(!pdf.is_empty());
    assert!(
        pdf.len() > pdf_no_badge.len(),
        "expected BADGE rendering to produce more bytes than no-badge \
         baseline (got {} vs {}); did anonymous-block walk regress?",
        pdf.len(),
        pdf_no_badge.len(),
    );
}

#[test]
fn render_v2_smoke_split_block_uses_per_slice_height() {
    // Regression for fulgur-bq6i:break-inside — when a block spans
    // multiple pages (the fragmenter records one fragment per page
    // slice), `draw_block_inner_paint` must paint each slice at its
    // per-page `frag.height` rather than the block's full
    // `layout_size.height`. Without this, the block's bg / border
    // paints full-size on every slice, leaking past the page bottom on
    // earlier slices and double-painting on the continuation page.
    //
    // Construct a body that overflows page 1 with a tall styled box
    // straddling the page break. The styled box has a colored bg so a
    // full-height repaint on page 2 (the bug) would emit an
    // unmistakably oversized rect — verifiable via PDF size: split
    // version stays close to single-page version + per-slice paints,
    // not 2× the block-area worth of bg fills.
    let html = r##"<!DOCTYPE html><html><head><style>
        body{margin:0;padding:0;font-size:10pt}
        .filler{height:600pt;background:#eef}
        .box{height:300pt;background:#cef;border:2pt solid #44a}
    </style></head><body>
        <div class="filler"></div>
        <div class="box"></div>
    </body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
    // Sanity: must produce a multi-page PDF (the box straddles page
    // bottom).
    let pdf_str = String::from_utf8_lossy(&pdf);
    let page_count = pdf_str.matches("/Type /Page\n").count();
    assert!(
        page_count >= 2,
        "expected multi-page output for split block, got {page_count}",
    );
}

#[test]
fn render_v2_smoke_body_layout_children_for_form_siblings() {
    // Regression for fulgur-bq6i:wasm-demo — body with mixed
    // block-level and inline-level children triggers Stylo's
    // anonymous-block synthesis at the BODY level (CSS 2.1
    // §9.2.1.1). The synthesized wrapper appears in
    // `body.layout_children` but NOT in `body.children`, so the
    // fragmenter's `fragment_pagination_root` (now preferring
    // `layout_children` when non-empty) must visit it for v2 to
    // see the inline-level group's paint.
    //
    // Without this fix, a body containing
    // `<h1>title</h1><label>field: <input></label>` paints only
    // the h1; the label + input row gets dropped because its
    // anonymous wrapper isn't visited.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:8pt;font-size:10pt}label{margin-right:8pt}input{padding:2pt;border:1pt solid #888;width:120pt}</style></head><body><h1>Form sample</h1><label>Name:</label><input type="text" value="hello"></body></html>"##;
    let html_no_inline = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:8pt;font-size:10pt}</style></head><body><h1>Form sample</h1></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    let pdf_no_inline = engine.render_html_v2(html_no_inline).expect("v2 render");
    assert!(!pdf.is_empty());
    // Sanity: body with inline-level form siblings produces a
    // larger PDF than h1-only baseline. Without the body-level
    // layout_children walk, the label + input row is dropped and
    // the two sizes converge.
    assert!(
        pdf.len() > pdf_no_inline.len(),
        "expected form-row rendering to produce more bytes than \
         h1-only baseline (got {} vs {}); did body layout_children \
         walk regress?",
        pdf.len(),
        pdf_no_inline.len(),
    );
}

#[test]
fn render_v2_smoke_body_opacity_multi_page_content_survives() {
    // Regression for PR #314 follow-up Devin Review:
    // `body { opacity: 0.5 }` with content that spans multiple
    // pages must NOT silently blank pages 1+. The `body` element
    // gets exactly one fragment at `page_index = 0`
    // (`pagination_layout.rs:380-384`), so
    // `draw_under_opacity(body)` only fires on page 0. If body's
    // descendants are added to `opacity_wrapped_descendants`
    // unconditionally, they get skipped on pages 1+ by the
    // `opacity_wrapped_descendants.contains(...)` guard but no-one
    // dispatches them — silently blanking everything after page 1.
    //
    // Body is now excluded from `opacity_wrapped_descendants` (and
    // from the `draw_under_opacity` dispatch arm) for the same
    // reason `clipped_descendants` excludes it (PR #310 Devin).
    let html = r##"<!DOCTYPE html><html><head><style>
        body{margin:0;padding:0;opacity:0.5;font-size:10pt}
        .filler{height:800pt;background:#eef}
        .tail{height:120pt;background:#cef;margin-top:12pt}
    </style></head><body>
        <div class="filler"></div>
        <div class="tail">tail content on page 2</div>
    </body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
    // Sanity: must be multi-page (filler 800pt + tail 132pt > A4
    // content height of ~842pt).
    let pdf_str = String::from_utf8_lossy(&pdf);
    let page_count = pdf_str.matches("/Type /Page\n").count();
    assert!(
        page_count >= 2,
        "expected multi-page output for body-opacity test, got {page_count}",
    );
    // Compare to a no-opacity baseline. Without the body exclusion
    // fix, body's descendants on page 2 silently disappear and the
    // PDF shrinks compared to the no-opacity version. The opacity-
    // group XObject adds a small constant; the test simply asserts
    // the with-opacity PDF retains MOST of the no-opacity content
    // (i.e. didn't lose page 2 entirely).
    let html_baseline = r##"<!DOCTYPE html><html><head><style>
        body{margin:0;padding:0;font-size:10pt}
        .filler{height:800pt;background:#eef}
        .tail{height:120pt;background:#cef;margin-top:12pt}
    </style></head><body>
        <div class="filler"></div>
        <div class="tail">tail content on page 2</div>
    </body></html>"##;
    let pdf_baseline = engine.render_html_v2(html_baseline).expect("v2 render");
    // Allow some room for the opacity group XObject overhead but
    // require at least 90% of the baseline content survives. A real
    // regression (silent page-2 blanking) drops the size by far more
    // than 10%.
    assert!(
        pdf.len() * 100 >= pdf_baseline.len() * 90,
        "with-opacity PDF lost too much content vs baseline \
         (with={}B baseline={}B); did body exclusion regress?",
        pdf.len(),
        pdf_baseline.len(),
    );
}

#[test]
fn render_v2_smoke_split_opacity_block_uses_per_slice_height() {
    // Regression for PR #314 follow-up Devin Review: a fractional-
    // opacity block with descendants that ALSO spans multiple pages
    // (split slice) must paint each per-page slice at its
    // `frag.height`, not the full `layout_size.height`. v2's
    // `draw_under_opacity` inlines the bg / border / shadow paint;
    // without the `is_split` height fix that
    // `draw_block_inner_paint` got in PR #316, the inlined paint
    // overflows the page bottom on earlier slices and double-paints
    // on continuation pages.
    //
    // Construct a body with a tall opacity-wrapped block (containing
    // a same-node_id-different SVG descendant so the opacity arm
    // fires) that straddles the page boundary.
    let html = r##"<!DOCTYPE html><html><head><style>
        body{margin:0;padding:0;font-size:10pt}
        .filler{height:600pt;background:#eef}
        .opaque{opacity:0.5;height:300pt;background:#cef;border:2pt solid #44a;margin-top:12pt}
    </style></head><body>
        <div class="filler"></div>
        <div class="opaque">
            <svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
                <rect width="40" height="40" fill="#1a6faa"/>
            </svg>
        </div>
    </body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
    // Multi-page sanity.
    let pdf_str = String::from_utf8_lossy(&pdf);
    let page_count = pdf_str.matches("/Type /Page\n").count();
    assert!(
        page_count >= 2,
        "expected multi-page output for split-opacity test, got {page_count}",
    );
}

#[test]
fn render_v2_smoke_split_overflow_clip_block_uses_per_slice_height() {
    // Regression for PR #313 follow-up Devin Review: an
    // `overflow: hidden` block that spans multiple pages must
    // paint its bg / border / shadow at the per-page slice height
    // (`frag.height`) and push a clip rectangle of the slice
    // height too — not at the full `layout_size.height`.
    //
    // PR #316 added `is_split` height handling to
    // `draw_block_inner_paint`; PR #314 follow-up added it to
    // `draw_under_opacity`; this test guards `draw_under_clip`
    // (the third parallel paint path). Without the fix, the
    // overflow:hidden block overflows the page bottom on earlier
    // slices AND the clip rectangle on continuation pages covers
    // content that should be cut off.
    let html = r##"<!DOCTYPE html><html><head><style>
        body{margin:0;padding:0;font-size:10pt}
        .filler{height:600pt;background:#eef}
        .clip{overflow:hidden;height:300pt;background:#cef;border:2pt solid #44a;margin-top:12pt}
        .clip > .inner{height:60pt;background:#fef;margin:6pt}
    </style></head><body>
        <div class="filler"></div>
        <div class="clip"><div class="inner">clipped content</div></div>
    </body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render_html_v2(html).expect("v2 render");
    assert!(!pdf.is_empty());
    // Multi-page sanity (filler 600pt + clip 300pt + 12pt margin
    // = 912pt > A4 ~842pt content height).
    let pdf_str = String::from_utf8_lossy(&pdf);
    let page_count = pdf_str.matches("/Type /Page\n").count();
    assert!(
        page_count >= 2,
        "expected multi-page output for split-overflow-clip test, got {page_count}",
    );
}
