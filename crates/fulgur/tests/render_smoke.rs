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

use std::path::PathBuf;

use fulgur::{AssetBundle, Engine};
use tempfile::tempdir;

/// Count `/Type /Page` objects (excluding the `/Type /Pages` page-tree
/// root) directly from raw PDF bytes. Robust to whatever delimiter
/// krilla emits after `/Type /Page` — unlike a fixed `"/Type /Page\n"`
/// string match, which silently miscounts if the trailing byte ever
/// changes. Mirrors the `page_count` helper used by the other
/// pagination integration tests.
fn page_count(pdf: &[u8]) -> usize {
    let prefix = b"/Type /Page";
    let mut count = 0;
    let mut i = 0;
    while i + prefix.len() < pdf.len() {
        if &pdf[i..i + prefix.len()] == prefix {
            // `/Type /Pages` (the tree root) has an alphanumeric next
            // byte (`s`); a real page object is followed by a delimiter.
            if !pdf[i + prefix.len()].is_ascii_alphanumeric() {
                count += 1;
            }
            i += prefix.len();
        } else {
            i += 1;
        }
    }
    count
}

/// Concatenate every `ShapedGlyphRun` string across a paragraph's shaped
/// lines — enough to identify a paragraph by its text content in the flat
/// `Drawables.paragraphs` map, or to assert resolved pseudo-content text.
/// Reads the pre-raster run text (`ShapedGlyphRun::text`), so it is
/// font-independent (no glyph inspection).
fn paragraph_run_text(entry: &fulgur::drawables::ParagraphEntry) -> String {
    entry
        .lines
        .iter()
        .flat_map(|line| line.items.iter())
        .filter_map(|item| match item {
            fulgur::paragraph::LineItem::Text(run) => Some(run.text.as_ref()),
            _ => None,
        })
        .collect()
}

/// Render `html` through the v2 pipeline and assert it produced a valid
/// PDF that paginated (`>= 2` pages). `what` labels the failure case.
fn assert_paginates(html: &str, what: &str) {
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(pdf.starts_with(b"%PDF"));
    let pages = page_count(&pdf);
    assert!(pages >= 2, "{what}, got {pages}");
}

fn check_pdf_snapshot(name: &str, pdf: &[u8]) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.pdf"));

    if std::env::var("FULGUR_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, pdf).unwrap();
        return;
    }

    if !path.exists() {
        std::fs::write(&path, pdf).unwrap();
        panic!("new snapshot created: {name}.pdf — review the file, then re-run the test");
    }

    let expected = std::fs::read(&path).unwrap();
    if pdf != expected.as_slice() {
        panic!("PDF snapshot mismatch: {name}\nRun with FULGUR_UPDATE_SNAPSHOTS=1 to update.");
    }
}

fn noto_engine() -> Engine {
    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets
        .add_font_file(&font_path)
        .unwrap_or_else(|e| panic!("failed to load Noto Sans from {}: {e}", font_path.display()));
    assets.add_css("body { font-family: 'Noto Sans', sans-serif; }");
    Engine::builder().assets(assets).build()
}

fn tagged_render_with_noto(html: &str) -> Vec<u8> {
    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets
        .add_font_file(&font_path)
        .unwrap_or_else(|e| panic!("failed to load Noto Sans from {}: {e}", font_path.display()));
    assets.add_css("body { font-family: 'Noto Sans', sans-serif; }");
    Engine::builder()
        .tagged(true)
        .lang("en")
        .assets(assets)
        .build()
        .render(html)
        .expect("tagged render")
}

/// Extract text from a PDF via `pdftotext -raw`. `-raw` flattens tagged
/// PDFs to reading-order content stream text and avoids the column wrap
/// pdftotext applies in default mode, which would split sentinels like
/// `[APP: ]` across line breaks. Returns `None` only when the pdftotext
/// binary is not installed (spawn fails with `ErrorKind::NotFound`) —
/// callers should gracefully skip in that case (CI has poppler-utils
/// preinstalled; local dev on macOS needs `brew install poppler`). Any
/// other spawn error (permission denied, corrupt executable, resource
/// exhaustion, …) and any non-zero exit both return
/// `Some("[pdftotext ...]")` with the underlying diagnostic embedded,
/// so the caller's `assert!(text.contains(...))` fires with the error
/// visible in the panic message instead of silently skipping.
/// pdftotext walks the font's ToUnicode CMap to recover Unicode from
/// CID-encoded TJ strings, which lopdf 0.40 cannot do — this is the
/// replacement for the old ActualText hex grep, which was only viable
/// while the now-fixed `text_range = 0..text_len` bug triggered
/// Krilla's whole-paragraph ActualText path.
fn extract_pdf_text(pdf: &[u8]) -> Option<String> {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("extract.pdf");
    std::fs::write(&path, pdf).expect("write pdf");
    let output = match std::process::Command::new("pdftotext")
        .arg("-raw")
        .arg(&path)
        .arg("-")
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            return Some(format!(
                "[pdftotext spawn failed: {err} (kind={:?})]",
                err.kind()
            ));
        }
    };
    if !output.status.success() {
        return Some(format!(
            "[pdftotext failed status={} stderr={:?}]",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

#[test]
fn test_render_html_resolves_link_stylesheet() {
    let dir = tempdir().unwrap();
    let css_path = dir.path().join("test.css");
    std::fs::write(&css_path, "p { color: red; }").unwrap();

    let html = r#"<html><head><link rel="stylesheet" href="test.css"></head><body><p>Hello</p></body></html>"#;

    let engine = Engine::builder().base_path(dir.path()).build();
    let result = engine.render(html);
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
    let pdf = engine.render(html).expect("render");

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
    let pdf = engine.render(html).expect("render");
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
    let pdf = engine.render(html).expect("render");
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
    let pdf = engine.render(html).expect("render should not panic");
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
        .render(html)
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
        .render(html)
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
        .render(html)
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
        .render(html)
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
        .render(html)
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
        .render(html)
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
        .render(html)
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
        .render(html)
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
        .render(html)
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
        .render(html)
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
        .render(html)
        .expect("render inset shadow");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_shadow_blur_warning_path() {
    // Non-zero blur radius now routes through the gradient 9-slice path.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;background:#fff;
                    box-shadow:0 0 8px red;"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("render blurred shadow");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_shadow_blur_gradient_path() {
    // blur > 0 with spread and offset → exercises draw_blur_box_shadow.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;background:#fff;
                    box-shadow:4px 4px 8px 2px rgba(0,0,0,0.6);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("render blurred shadow with offset and spread");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_shadow_blur_rounded() {
    // blur > 0 with border-radius → exercises RadialGradient corner slices.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;background:#fff;border-radius:12px;
                    box-shadow:0 0 10px 0 black;"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("render blurred shadow with border-radius");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_shadow_blur_non_white_background() {
    // A dark page background verifies the soft-mask approach is background-colour-independent.
    let html = r#"<!DOCTYPE html><html style="background-color:#1a1a2e"><body>
        <div style="width:100px;height:100px;background:#e94560;
                    box-shadow:0 0 12px 4px rgba(233,69,96,0.8);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("render blurred shadow on dark background");
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
        .render(html)
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
        .render(html)
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
        .render(html)
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
    let pdf = Engine::builder().build().render(html).expect("render");
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
    let pdf = Engine::builder().build().render(html).expect("render");
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
    let pdf = Engine::builder().build().render(html).expect("render");
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
        .render(html)
        .expect("render bg repeat/origin/clip variants");
    assert!(!pdf.is_empty());
}

#[test]
fn linear_gradient_with_interpolation_hint_renders_via_engine() {
    // CSS Images 3 §3.5.3 hint expansion を `Engine::render_html` 経由で叩く
    // (fulgur-2zam). VRT は codecov 対象外なので draw branch 起動の証拠を
    // ここに残す (CLAUDE.md "Coverage scope" Gotcha).
    let html = r#"<html><body><div style="width:200px;height:100px;background:linear-gradient(red, 30%, blue)">x</div></body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn radial_gradient_with_interpolation_hint_renders_via_engine() {
    let html = r#"<html><body><div style="width:200px;height:100px;background:radial-gradient(red, 30%, blue)">x</div></body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn repeating_linear_gradient_with_hint_renders_via_engine() {
    // hint expansion + repeating 周期展開の組み合わせ経路.
    let html = r#"<html><body><div style="width:200px;height:100px;background:repeating-linear-gradient(red, 30%, blue 50%, red 100%)">x</div></body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("render");
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
    let pdf = Engine::builder().build().render(html).expect("render");
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
    // regression guard — we walk the drawable tree, find every
    // out-of-flow `ParagraphRender`, and assert the fixed text laid
    // itself out as a single line. Without `relayout_position_fixed`,
    // Parley shapes the long sentence at the abs's ~37.5pt width and
    // produces multiple wrapped lines; with the relayout, the fixed
    // subtree is reshaped against the page area and the sentence fits
    // on one line.

    // The fixed paragraph carries a unique sentence so we can identify
    // it in the flat `Drawables.paragraphs` map by text content. The
    // outer abs box's "outer" text is short enough not to wrap so it
    // never inflates the line count we want to pin.
    const FIXED_TEXT: &str = "This text is much wider than fifty pixels";

    let html = r#"<html><body style="margin:0">
<div style="position:absolute; width:50px; height:300vh">
  outer
  <div style="position:fixed; bottom:0">This text is much wider than fifty pixels</div>
</div>
</body></html>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render(html).expect("render");
    assert!(pdf.starts_with(b"%PDF"));

    let drawables = engine.build_drawables_for_testing_no_gcpm(html);

    // Find the paragraph whose shaped text matches the fixed sentence.
    // Concatenating every glyph-run text within the paragraph is enough
    // to distinguish it (the abs box's "outer" text lives in a
    // different paragraph entry).
    let fixed_para = drawables
        .paragraphs
        .values()
        .find(|&p| paragraph_run_text(p).contains(FIXED_TEXT))
        .expect("fixed paragraph must appear in Drawables.paragraphs");

    assert_eq!(
        fixed_para.lines.len(),
        1,
        "expected the inner position:fixed paragraph to be relayouted \
         wide enough to hold the sentence on a single line; got {} lines, \
         meaning it kept the 37.5pt parent-abs width and Parley wrapped",
        fixed_para.lines.len(),
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
    let pdf = engine.render(html).expect("render");

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

#[test]
fn page_counter_footer_paints_above_body_background() {
    let html = r#"<!doctype html><html><head><style>
        @page {
          size: A4;
          margin: 20mm;
          @bottom-center {
            content: "888";
            font-size: 24pt;
            color: #000;
          }
        }
        html, body { margin: 0; padding: 0; background: #fff; }
        .page { height: 100vh; background: #fff; }
      </style></head><body><div class="page">body</div></body></html>"#;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render(html).expect("render");

    let dir = tempfile::tempdir().expect("tempdir");
    let pdf_path = dir.path().join("out.pdf");
    std::fs::write(&pdf_path, &pdf).unwrap();
    if std::process::Command::new("pdftoppm")
        .arg("-v")
        .output()
        .is_err()
    {
        eprintln!("pdftoppm not available; skipping visual footer assertion");
        return;
    }

    let prefix = dir.path().join("page");
    let output = std::process::Command::new("pdftoppm")
        .args(["-f", "1", "-l", "1", "-r", "72"])
        .arg(&pdf_path)
        .arg(&prefix)
        .output()
        .expect("run pdftoppm");
    assert!(output.status.success(), "pdftoppm failed: {output:?}");

    let ppm = std::fs::read(dir.path().join("page-1.ppm")).expect("ppm output");
    let mut idx = 0;
    let next_token = |bytes: &[u8], idx: &mut usize| -> String {
        while *idx < bytes.len() && bytes[*idx].is_ascii_whitespace() {
            *idx += 1;
        }
        let start = *idx;
        while *idx < bytes.len() && !bytes[*idx].is_ascii_whitespace() {
            *idx += 1;
        }
        String::from_utf8(bytes[start..*idx].to_vec()).unwrap()
    };
    assert_eq!(next_token(&ppm, &mut idx), "P6");
    let width: usize = next_token(&ppm, &mut idx).parse().unwrap();
    let height: usize = next_token(&ppm, &mut idx).parse().unwrap();
    assert_eq!(next_token(&ppm, &mut idx), "255");
    while idx < ppm.len() && ppm[idx].is_ascii_whitespace() {
        idx += 1;
    }
    let pixels = &ppm[idx..];
    let y_start = height * 91 / 100;
    let y_end = height * 98 / 100;
    let x_start = 0;
    let x_end = width;
    let dark_pixels = (y_start..y_end)
        .flat_map(|y| (x_start..x_end).map(move |x| (y, x)))
        .filter(|(y, x)| {
            let offset = (y * width + x) * 3;
            let r = pixels[offset];
            let g = pixels[offset + 1];
            let b = pixels[offset + 2];
            r < 80 && g < 80 && b < 80
        })
        .count();
    assert!(
        dark_pixels > 10,
        "expected visible black footer glyph pixels in the bottom margin, found {dark_pixels}"
    );
}

// ── Phase 4 v2 render path smoke tests (fulgur-9t3z) ─────────────────
//
// Exercise the v2 render path (`render_v2`) so the patch-coverage
// gate sees the new draw helpers added in PR 6 (`draw_under_transform`,
// `draw_under_clip`, `draw_under_opacity`, `paint_multicol_rule_for_page`,
// `paint_root_block_v2`, `MarginBoxRenderer`). These run end-to-end
// through `Engine::render_html` (defaulted to v2 in PR 7) and assert
// only that the bytes come back non-empty.

#[test]
fn render_v2_smoke_transform_translate() {
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}.box{width:80px;height:60px;background:#cef;transform:translate(10px,5px)}</style></head><body><div class="box"></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_nested_transforms() {
    // Exercises the `draw_under_transform` recursion path added in
    // PR #305 Devin fix: outer rotate wraps an inner translate, both
    // matrices must compose.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}.outer{width:120px;height:80px;background:#cef;transform:rotate(10deg)}.inner{width:60px;height:40px;background:#fce;transform:translate(8px,4px)}</style></head><body><div class="outer"><div class="inner"></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_multicol_with_column_rule() {
    // Exercises `paint_multicol_rule_for_page` — most fixtures don't
    // declare `column-rule` so this path needs an explicit smoke test.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:8pt}.cols{column-count:2;column-rule:1pt solid #888;column-gap:12pt;height:80pt}.cell{height:30pt;background:#cef;margin-bottom:6pt}</style></head><body><div class="cols"><div class="cell"></div><div class="cell"></div><div class="cell"></div><div class="cell"></div></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_html_body_bg_multi_page() {
    // Exercises `paint_root_block_v2` for both `<html>` (pre-pass on
    // every page) and `<body>` (pre-pass on continuation pages).
    let html = r##"<!DOCTYPE html><html><head><style>html,body{margin:0;background:#fafafa}.tall{height:1500px;background:#cef}</style></head><body><div class="tall"></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_rtl_page_left_right_selectors() {
    // Exercises `extract_root_dir_rtl` (direction:rtl on :root) and the
    // first_page_is_left RTL branch in resolve_page_settings / selector_matches.
    // Also exercises the Y-origin fix for continuation pages in an RTL doc.
    let html = r##"<!DOCTYPE html><html><head><style>
        :root { direction: rtl; }
        @page :left  { margin: 30pt; }
        @page :right { margin: 60pt; }
        .tall { height: 900px; background: #cef; }
    </style></head><body>
        <div class="tall"></div>
        <div class="tall"></div>
    </body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_block_with_inline_root_padding() {
    // Exercises `draw_block_with_inner_content` content-inset path —
    // the `padding: 6px` shift fix that landed in PR 6.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}p{margin:0;padding:6px;background:#cef}</style></head><body><p>hello</p></body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_bookmarks_under_transform() {
    // Exercises the bookmark-anchor pre-skip path (`record(...)` runs
    // before `transformed_descendants` skip) added in PR 6 Devin fix.
    let html = r##"<!DOCTYPE html><html><head><style>body{margin:0}div{transform:rotate(5deg)}h1{margin:0;font-size:14px}</style></head><body><div><h1>Heading</h1></div></body></html>"##;
    let engine = fulgur::engine::Engine::builder().bookmarks(true).build();
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf_clip = engine.render(with_clip).expect("v2 render w/ clip");
    let pdf_plain = engine.render(without_clip).expect("v2 render w/o clip");
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
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_many_opacity_clip_transform_scopes_are_bounded() {
    // fulgur-vrkv hardening: every clip / opacity / transform / inline-box
    // scope used to snapshot-diff *all* accumulated drawables to compute its
    // descendant list, making a document of N such sibling scopes O(N²).
    // `TrackedMap`'s insertion log makes each scope O(its own subtree).
    // This drives all six migrated sinks at once (block opacity, block clip,
    // table clip, list-item opacity, transform, inline-box) with many
    // siblings so the linear path is exercised end-to-end; correctness of the
    // descendant sets themselves is pinned byte-for-byte by fulgur-vrt.
    let n = 40;
    let mut body = String::from("<ul>");
    for _ in 0..n {
        body.push_str(r#"<li style="opacity:.99">x</li>"#);
    }
    body.push_str("</ul>");
    for _ in 0..n {
        body.push_str(r#"<div style="opacity:.99">o</div>"#);
        body.push_str(r#"<div style="overflow:hidden"><span>c</span></div>"#);
        body.push_str(r#"<div style="transform:translateX(1px)">t</div>"#);
        body.push_str(r#"<p><span style="display:inline-block;opacity:.9">i</span></p>"#);
        body.push_str(r#"<table style="overflow:hidden"><tr><td>y</td></tr></table>"#);
    }
    let html = format!("<!DOCTYPE html><html><body>{body}</body></html>");
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render(&html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf_dashed = engine.render(html_dashed).expect("dashed render");
    let pdf_dotted = engine.render(html_dotted).expect("dotted render");
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
    let pdf = engine.render(&html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
    let pdf_no_badge = engine.render(html_no_badge).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
    let pdf_no_inline = engine.render(html_no_inline).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf_baseline = engine.render(html_baseline).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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
    let pdf = engine.render(html).expect("v2 render");
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

#[test]
fn render_v2_smoke_html_opacity_multi_page_content_survives() {
    // Regression for PR #312 follow-up Devin Review: the
    // `<html>` root element with fractional opacity must NOT
    // silently blank the entire document.
    //
    // Root is never recorded in `geometry` (it's painted via
    // `paint_root_block_v2` as a pre-pass). When `html { opacity:
    // 0.5 }` is set, `extract_drawables_from_pageable` populates
    // root's `BlockEntry.opacity_descendants` with body + all body
    // descendants. Without the root exclusion in the
    // `opacity_wrapped_descendants` collection, those descendants
    // were added to the skip set and dropped from the main loop —
    // but `draw_under_opacity(root)` never fires either (root is
    // skipped at line 348), so the entire page content silently
    // blanked.
    //
    // Same defense as the body exclusion (PR #314 follow-up).
    let html = r##"<!DOCTYPE html><html style="opacity:0.5"><head><style>
        body{margin:0;padding:0;font-size:10pt}
        .filler{height:800pt;background:#eef}
        .tail{height:120pt;background:#cef;margin-top:12pt}
    </style></head><body>
        <div class="filler"></div>
        <div class="tail">tail content on page 2</div>
    </body></html>"##;
    let engine = fulgur::engine::Engine::builder().build();
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
    // Multi-page sanity.
    let pdf_str = String::from_utf8_lossy(&pdf);
    let page_count = pdf_str.matches("/Type /Page\n").count();
    assert!(
        page_count >= 2,
        "expected multi-page output for html-opacity test, got {page_count}",
    );
    // Compare against no-opacity baseline. Without the root
    // exclusion fix, `html { opacity: 0.5 }` silently dropped all
    // body content and the PDF shrunk to roughly the empty-page
    // size. Require ≥85% of baseline (small overhead for the
    // root-level transparency-group XObject).
    let html_baseline = r##"<!DOCTYPE html><html><head><style>
        body{margin:0;padding:0;font-size:10pt}
        .filler{height:800pt;background:#eef}
        .tail{height:120pt;background:#cef;margin-top:12pt}
    </style></head><body>
        <div class="filler"></div>
        <div class="tail">tail content on page 2</div>
    </body></html>"##;
    let pdf_baseline = engine.render(html_baseline).expect("v2 render");
    assert!(
        pdf.len() * 100 >= pdf_baseline.len() * 85,
        "with-html-opacity PDF lost too much content vs baseline \
         (with={}B baseline={}B); did root exclusion regress?",
        pdf.len(),
        pdf_baseline.len(),
    );
}

#[test]
fn render_v2_smoke_positioned_child_height_field_paths() {
    // PR 8f added `PositionedChild.height` populated from Taffy at every
    // production construction site (convert/{positioned, pseudo, replaced,
    // list_item, inline_root, table, mod}.rs). Most existing smoke tests
    // exercise individual paths, but the `let p_h = paragraph.cached_height;`
    // and `let pseudo_h_pt = ...` assignments need coverage to satisfy
    // codecov patch threshold. This consolidated render exercises:
    //
    // - inline_root paragraph wrapped in a block drawable (inline_root.rs:122 / 187)
    // - list_item paragraph + inline marker (list_item.rs:204, 268, 378, 433)
    // - block pseudo `::before` / `::after` images (pseudo.rs:253, 263)
    // - abs-positioned pseudo (positioned.rs:631)
    // - replaced element with visual style (replaced.rs:75)
    // - orphan string-set / counter-op / bookmark markers (mod.rs:911, 934, 970)
    let png_data: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", png_data);

    let html = r##"<!DOCTYPE html><html><head><style>
        body { margin: 0; padding: 8pt; counter-reset: section; }
        .styled { background: #cef; padding: 4pt; }
        .styled::before { content: ""; display: block; width: 8pt; height: 8pt; background: #fce; }
        .styled::after { content: ""; display: block; width: 8pt; height: 8pt; background: #fef; }
        .abs-pseudo::before { content: ""; position: absolute; top: 0; right: 0; width: 6pt; height: 6pt; background: red; }
        h1 { counter-increment: section; font-size: 14pt; }
        h1::before { content: counter(section) ". "; }
        ul { list-style-type: disc; }
        li.tagged::before { content: "[" attr(data-tag) "] "; }
    </style></head><body>
        <h1>Heading One</h1>
        <p class="styled">Paragraph with styled background and before/after pseudos.</p>
        <div class="abs-pseudo" style="position: relative; padding: 8pt; background: #eef;">
            Container with absolutely-positioned pseudo.
        </div>
        <ul>
            <li>Plain item</li>
            <li class="tagged" data-tag="A">Tagged item</li>
            <li><p>Block-content list item.</p></li>
        </ul>
        <img src="dot.png" style="width: 32pt; height: 32pt; border: 2pt solid #888; padding: 4pt;">
    </body></html>"##;
    let engine = Engine::builder().assets(bundle).bookmarks(true).build();
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

// ── Phase 4 PR 8g: dispatch_inline_box_content branches ──────────────────────

#[test]
fn render_v2_smoke_inline_block_css_transform_branch() {
    // Exercises dispatch_inline_box_content → draw_under_transform path.
    let html = r#"<!DOCTYPE html><html><body>
        <p>text <span style="display:inline-block;width:60px;height:30px;background:red;
                             transform:rotate(15deg)">rotated</span> text</p>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn render_v2_smoke_inline_block_overflow_hidden_clip_branch() {
    // Exercises dispatch_inline_box_content → draw_under_clip path.
    let html = r#"<!DOCTYPE html><html><body>
        <p>text <span style="display:inline-block;width:40px;height:20px;overflow:hidden;">
            <span style="margin-left:100px">clipped</span>
        </span> text</p>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn render_v2_smoke_inline_block_opacity_descendant_branch() {
    // Exercises dispatch_inline_box_content → draw_under_opacity path.
    let html = r#"<!DOCTYPE html><html><body>
        <p>text <span style="display:inline-block;width:60px;height:30px;background:blue;">
            <span style="opacity:0.4;background:red;width:100%;height:100%;display:block;">
                faded
            </span>
        </span> text</p>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

/// Build `depth` nested `inline-block` spans wrapping a coloured leaf.
/// `scope` is the extra declaration applied to every wrapper — the
/// `overflow:hidden` / `opacity` variants are the ones that used to
/// re-dispatch their inline-box subtree.
fn nested_inline_blocks(depth: usize, scope: &str) -> String {
    let mut s = String::from("<!DOCTYPE html><html><body>");
    for _ in 0..depth {
        s.push_str(&format!(
            r#"<span style="display:inline-block;width:60px;height:40px;{scope}">"#
        ));
    }
    s.push_str(
        r#"<span style="display:inline-block;width:20px;height:20px;background:#ff0000"></span>"#,
    );
    for _ in 0..depth {
        s.push_str("</span>");
    }
    s.push_str("</body></html>");
    s
}

/// Regression: an inline root that needs an `overflow` clip, an opacity
/// wrapper, or a `transform` must not paint its inline-box subtree twice.
///
/// `paragraph::draw_shaped_lines` dispatches `LineItem::InlineBox` content
/// itself, so every descendant walk in `render` must filter
/// `inline_box_subtree_skip` — exactly like the top-level dispatch loop
/// does. Without that filter the subtree paints once per scope, and where
/// the duplicate re-enters the *same* helper for the next nested wrapper,
/// the cost doubles per level: depth 22 produced a 10.7 MB PDF in ~4.8 s
/// (clip) and a 28.6 MB PDF in ~4.4 s (transform) from a ~2 KB input.
///
/// The assertion compares growth *rate* across two equal-width depth
/// windows rather than absolute size, because some nesting genuinely costs
/// more per level (each nested opacity scope needs its own transparency
/// group — a measured, and legitimate, ~432 bytes/level). Linear growth
/// keeps the two windows comparable; doubling-per-level makes the upper
/// window ~2^8 times the lower one.
///
/// The clip and transform scopes are the ones that compound, because
/// `draw_under_clip` / `draw_under_transform` recurse into themselves for
/// a nested clipped / transformed block. The opacity duplicate stays a
/// constant factor (measured linear both before and after the fix); it is
/// asserted here anyway so that if `draw_under_opacity` ever grows an
/// equivalent recursive arm, it cannot silently inherit the same blowup.
#[test]
fn nested_clipped_inline_blocks_do_not_amplify_output() {
    let render_at = |depth: usize, scope: &str| {
        Engine::builder()
            .build()
            .render(&nested_inline_blocks(depth, scope))
            .unwrap_or_else(|e| panic!("render at depth {depth} failed: {e}"))
            .len()
    };

    for scope in [
        "overflow:hidden;",
        "opacity:0.5;",
        "transform:rotate(1deg);",
    ] {
        let (d8, d12, d16, d20) = (
            render_at(8, scope),
            render_at(12, scope),
            render_at(16, scope),
            render_at(20, scope),
        );
        let lower = d12.saturating_sub(d8);
        let upper = d20.saturating_sub(d16);
        // Slack term keeps tiny absolute deltas (the clip case grows by a
        // couple of bytes per window) from tripping a pure ratio test.
        let bound = 4 * lower + 1024;
        assert!(
            upper <= bound,
            "nested `{scope}` inline-blocks amplified output super-linearly: \
             sizes 8/12/16/20 = {d8}/{d12}/{d16}/{d20} bytes; growth over \
             depths 16→20 was {upper} bytes vs {lower} bytes over 8→12 \
             (allowed <= {bound}). The inline-box subtree is being dispatched \
             both by the paragraph draw and by the clip/opacity descendant walk.",
        );
    }
}

/// Count the `rg` (non-stroking RGB) operators selecting pure red in one
/// decoded content stream.
fn count_red_in_content(bytes: &[u8]) -> usize {
    let Ok(content) = lopdf::content::Content::decode(bytes) else {
        return 0;
    };
    content
        .operations
        .iter()
        .filter(|op| op.operator == "rg" && op.operands.len() == 3)
        .filter(|op| {
            let channels: Vec<f32> = op
                .operands
                .iter()
                .filter_map(|o| match o {
                    lopdf::Object::Integer(i) => Some(*i as f32),
                    lopdf::Object::Real(f) => Some(*f),
                    _ => None,
                })
                .collect();
            channels.len() == 3 && channels[0] > 0.9 && channels[1] < 0.1 && channels[2] < 0.1
        })
        .count()
}

/// Count pure-red fills across page content streams *and* form XObjects.
///
/// The XObject sweep is required for the opacity scope: `draw_with_opacity`
/// paints into a transparency group, which krilla emits as a separate
/// `/Subtype /Form` stream rather than inline in the page content.
fn count_red_fill_ops(pdf: &[u8]) -> usize {
    let doc = lopdf::Document::load_mem(pdf).expect("PDF must parse");
    let mut count = 0;
    for (_, page_id) in doc.get_pages() {
        let bytes = doc
            .get_page_content(page_id)
            .expect("page content stream readable");
        count += count_red_in_content(&bytes);
    }
    for obj in doc.objects.values() {
        let lopdf::Object::Stream(stream) = obj else {
            continue;
        };
        let is_form = matches!(
            stream.dict.get(b"Subtype").and_then(lopdf::Object::as_name),
            Ok(b"Form")
        );
        if !is_form {
            continue;
        }
        if let Ok(bytes) = stream.decompressed_content() {
            count += count_red_in_content(&bytes);
        }
    }
    count
}

/// Regression: the inline-box subtree paints exactly once under *every*
/// wrapping scope, not just the ones that amplify.
///
/// `nested_clipped_inline_blocks_do_not_amplify_output` catches the
/// self-recursive helpers (clip, transform) by their growth rate, but a
/// duplicate that stays a constant factor — the opacity wrapper, and a
/// `<table>` with `overflow:hidden` around a cell's inline-block — is
/// invisible to a rate test. Counting the red fill operator pins the
/// invariant directly: one leaf, one `1 0 0 rg`.
///
/// Before the fix the `overflow:hidden` table emitted the leaf twice
/// (once from the cell's paragraph draw, once from
/// `draw_under_clip_table`'s `clip_descendants` walk).
#[test]
fn inline_box_subtree_paints_once_under_every_scope() {
    for scope in [
        "",
        "overflow:hidden;",
        "opacity:0.5;",
        "transform:rotate(1deg);",
    ] {
        let pdf = Engine::builder()
            .build()
            .render(&nested_inline_blocks(1, scope))
            .expect("render");
        let reds = count_red_fill_ops(&pdf);
        assert_eq!(
            reds, 1,
            "inline-block leaf under `{scope}` painted {reds} times, expected 1 \
             — the inline-box subtree is dispatched both by the paragraph draw \
             and by a descendant walk",
        );
    }
}

/// The other shape the skip has to get right: the scope sits on a *block*
/// wrapper, and the inline box is inside that block's paragraph.
///
/// Here the descendant walk dispatches the `<p>`, and the paragraph draw is
/// what paints the inline box — so skipping the subtree in the walk must
/// still leave exactly one paint. This variant guards the opposite failure
/// mode from the amplification test: an over-eager skip makes the leaf paint
/// *zero* times, and neither a growth-rate assertion nor a byte-identical
/// VRT golden would notice content that simply vanished.
#[test]
fn inline_box_under_block_scope_paints_exactly_once() {
    for scope in [
        "",
        "overflow:hidden;",
        "opacity:0.5;",
        "transform:rotate(1deg);",
    ] {
        let html = format!(
            "<!DOCTYPE html><html><body>\
             <div style=\"width:300px;height:200px;{scope}\">\
             <p>text {} tail</p></div></body></html>",
            r#"<span style="display:inline-block;width:20px;height:20px;background:#ff0000"></span>"#,
        );
        let pdf = Engine::builder().build().render(&html).expect("render");
        let reds = count_red_fill_ops(&pdf);
        assert_eq!(
            reds, 1,
            "inline box inside a paragraph under block scope `{scope}` painted \
             {reds} times, expected 1 (0 = the descendant-walk skip swallowed \
             content the paragraph draw was supposed to paint; 2 = it is \
             painted by both)",
        );
    }
}

/// Regression: a scoped inline-block whose child is a *block* must still
/// paint that child.
///
/// This is the shape that makes `Drawables::inline_box_subtree_skip` the
/// wrong predicate for a scope walk. The global set contains every drawable
/// under every inline box, so it contains this red child — but only because
/// the wrapper is an inline box in the *body's* paragraph. The wrapper has
/// no paragraph carrying the child, so nothing else paints it, and the
/// scope walk skipping it drops the content outright.
///
/// PR #677 shipped exactly that regression on the clip and opacity paths
/// (verified against `c36efebf`: the red child was emitted 0 times under
/// `overflow:hidden` and `opacity`, and 1 time before #677). The fix is
/// `paragraph_owned_inline_boxes`, which skips only what a paragraph drawn
/// inside the scope actually paints.
#[test]
fn block_child_of_scoped_inline_block_still_paints() {
    for scope in [
        "",
        "overflow:hidden;",
        "opacity:0.5;",
        "transform:rotate(1deg);",
    ] {
        for wrapper_content in ["", "text"] {
            let html = format!(
                "<!DOCTYPE html><html><body>\
                 <div style=\"display:inline-block;width:100px;height:100px;{scope}\">\
                 {wrapper_content}\
                 <div style=\"background:#ff0000;width:50px;height:50px\"></div>\
                 </div></body></html>"
            );
            let pdf = Engine::builder().build().render(&html).expect("render");
            let reds = count_red_fill_ops(&pdf);
            assert_eq!(
                reds, 1,
                "block child of an inline-block under scope `{scope}` \
                 (wrapper text: {wrapper_content:?}) painted {reds} times, \
                 expected 1 — 0 means the scope walk's skip predicate \
                 swallowed a child no paragraph paints",
            );
        }
    }
}

/// Regression: `draw_under_clip_table`'s `clip_descendants` walk must not
/// re-paint a cell's inline box, like the three block-level walks.
///
/// `convert::table` fills `TableEntry::clip_descendants` from the same
/// `drawn_since` diff the block paths use, so a cell's inline-box subtree
/// lands in it. Before the fix the leaf painted twice: once from the cell's
/// paragraph draw, once from the table's descendant walk. Unlike the clip
/// and transform paths this duplicate does not compound with nesting, so
/// the growth-rate assertion above cannot see it.
#[test]
fn inline_box_in_clipped_table_cell_paints_once() {
    // Both cell shapes: a bare inline-block (the cell's inline root is the
    // box itself) and text around it (the box is one item in a real
    // paragraph, so the walk dispatches the paragraph instead).
    for cell in [
        r#"<span style="display:inline-block;width:20px;height:20px;background:#ff0000"></span>"#,
        r#"cell text <span style="display:inline-block;width:20px;height:20px;background:#ff0000"></span> tail"#,
    ] {
        let html = format!(
            "<!DOCTYPE html><html><body>\
             <table style=\"overflow:hidden;width:300px;height:200px\"><tr><td>{cell}</td></tr></table>\
             </body></html>",
        );
        let pdf = Engine::builder().build().render(&html).expect("render");
        let reds = count_red_fill_ops(&pdf);
        assert_eq!(
            reds, 1,
            "inline-block leaf inside an `overflow:hidden` table painted {reds} times, \
             expected 1 — `draw_under_clip_table` must skip the inline boxes the \
             cell paragraphs already paint (cell markup: {cell})",
        );
    }
}

#[test]
fn render_v2_smoke_inline_block_image_child_via_dispatch_fragment() {
    // Exercises dispatch_fragment image branch when called from the inline-box
    // subtree descendant walk inside dispatch_inline_box_content.
    const MINIMAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let mut bundle = AssetBundle::new();
    bundle.add_image("dot.png", MINIMAL_PNG.to_vec());
    let html = r#"<!DOCTYPE html><html><body>
        <p>text <span style="display:inline-block;width:60px;height:40px;background:#eee;">
            <img src="dot.png" style="width:20px;height:20px;">
        </span> text</p>
    </body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn render_v2_smoke_list_item_svg_marker() {
    // Exercises `draw_list_item_marker`'s `ImageMarker::Svg` branch
    // (render.rs: svg.draw call) — a `<li>` with an SVG list-style-image.
    let svg_data = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><circle cx="5" cy="5" r="4" fill="blue"/></svg>"#;
    let mut bundle = AssetBundle::default();
    bundle.add_css(r#"li { list-style-image: url("bullet.svg"); }"#);
    bundle.add_image("bullet.svg", svg_data.to_vec());
    let html = r##"<!doctype html><html><body><ul><li>Alpha</li><li>Beta</li></ul></body></html>"##;
    let engine = Engine::builder().assets(bundle).build();
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_margin_box_renderer() {
    // Exercises `MarginBoxRenderer`'s Stage 3 draw path
    // (render.rs: pageable.draw call) — a simple @top-center counter.
    let html = r##"<!DOCTYPE html><html><head><style>
        @page { margin: 36pt; @top-center { content: counter(page); } }
        body { margin: 0; }
        p { height: 500pt; background: #eee; }
    </style></head><body><p>Page 1</p><p>Page 2</p></body></html>"##;
    let engine = Engine::builder().build();
    let pdf = engine.render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

// ── PR #290 codecov patch coverage uplift ─────────────────────────────────

#[test]
fn render_v2_smoke_border_groove_and_ridge() {
    // Exercises `border_3d_colors` + `draw_border_line` Groove/Ridge arm
    // (`draw_primitives.rs:1245-1274`). Width must be ≥ 3pt because the
    // groove/ridge codepath subdivides the stroke into half-width strips.
    let html = r#"<!DOCTYPE html><html><head><style>
        .grv { border: 8pt groove #888; padding: 4pt; }
        .rdg { border: 8pt ridge #888; padding: 4pt; }
    </style></head><body>
        <div class="grv">grooved box</div>
        <div class="rdg">ridge box</div>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn render_v2_smoke_border_inset_and_outset() {
    // Exercises `border_3d_colors` + `draw_border_line` Inset/Outset arm
    // (`draw_primitives.rs:1276-1286`).
    let html = r#"<!DOCTYPE html><html><head><style>
        .ins { border: 6pt inset #888; padding: 4pt; }
        .ots { border: 6pt outset #888; padding: 4pt; }
    </style></head><body>
        <div class="ins">inset box</div>
        <div class="ots">outset box</div>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_border_radius_with_style_rounded_path() {
    // Exercises the rounded-path branch in `draw_block_border`
    // (`draw_primitives.rs:~1337`): when a non-uniform radius forces the
    // border to be drawn as a stroked path rather than 4 line segments.
    let html = r#"<!DOCTYPE html><html><head><style>
        .rnd { border: 4pt solid #444; border-radius: 12pt; padding: 8pt; }
    </style></head><body><div class="rnd">rounded</div></body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_overflow_hidden_with_border_radius() {
    // Exercises `compute_overflow_clip_path` both-axes + has_radius branch
    // through Engine rendering (covers `draw_primitives.rs:999-1000` plus
    // the overflow-clip dispatch in `render.rs`).
    let html = r#"<!DOCTYPE html><html><head><style>
        .clipped { overflow: hidden; border-radius: 16pt; width: 100pt; height: 60pt; background: #cef; }
        .inner { width: 200pt; height: 80pt; background: #fce; }
    </style></head><body>
        <div class="clipped"><div class="inner">overflow content</div></div>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_inline_block_with_background_and_text() {
    // Targets `convert/inline_root.rs:185-212` — the `if needs_block`
    // branch for an inline root with `needs_block_wrapper()` true and a
    // populated paragraph. An inline-block span with a background plus
    // direct text content satisfies both conditions.
    let html = r#"<!DOCTYPE html><html><body>
        <p>before <span style="display:inline-block; background:#fce; padding:4pt; border:1pt solid #888;">inline-block with background</span> after</p>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_inline_block_with_overflow_clip_and_text() {
    // Same `if needs_block` branch as above but exercising the
    // `clipping = true` arm (`inline_root.rs:207-209`) so
    // `clip_descendants` records the inline-block subtree.
    let html = r#"<!DOCTYPE html><html><body>
        <p>x <span style="display:inline-block; overflow:hidden; width:60pt; height:20pt; background:#cef;">overflow inline-block text</span> y</p>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_empty_li_inside_position_marker_paragraph() {
    // Targets `convert/list_item.rs:172-202` — the `children.is_empty()`
    // branch with an inside-positioned marker that synthesises a marker-only
    // paragraph for the empty `<li>`.
    let html = r#"<!DOCTYPE html><html><head><style>
        ul { list-style-position: inside; list-style-type: disc; }
    </style></head><body>
        <ul><li></li><li>second</li></ul>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_li_inside_with_block_child_synthesizes_marker_paragraph() {
    // Targets `convert/list_item.rs:229-242` — the non-empty <li> path
    // where `inject_marker_into_first_paragraph` returns false because
    // the only descendant is a block (no paragraph in `out` to inject
    // into), so the caller synthesises a marker-only paragraph.
    let html = r#"<!DOCTYPE html><html><head><style>
        ul { list-style-position: inside; list-style-type: disc; }
        .blk { display: block; height: 12pt; background: #cef; }
    </style></head><body>
        <ul><li><div class="blk"></div></li></ul>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_content_url_on_non_replaced_element() {
    // Targets `convert/replaced.rs:135-159` — `convert_content_url` on a
    // non-`<img>` / non-`<svg>` element with `content: url(...)` resolved
    // against the asset bundle.
    let png_data: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", png_data);
    bundle.add_css(r#".replaced { content: url("dot.png"); width: 24pt; height: 24pt; }"#);
    let html = r#"<!DOCTYPE html><html><body><div class="replaced"></div></body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
}

// PNG bytes used by the pseudo / list-marker tests below.
const PR290_PSEUDO_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

#[test]
fn render_v2_smoke_inline_root_after_pseudo_image_with_link() {
    // Targets `convert/inline_root.rs:73-75` — the `after_inline` map
    // closure that calls `attach_link_to_inline_image`. Existing tests
    // exercise the `before` arm but not `after`. An `<a>` ancestor wraps
    // the inline so `attach_link_to_inline_image` has a link to attach.
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", PR290_PSEUDO_PNG.to_vec());
    let html = r#"<!DOCTYPE html><html><head><style>
        .with-after::after { content: url("dot.png"); }
    </style></head><body>
        <a href="https://example.com"><span class="with-after">linked</span></a>
    </body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_inline_root_pseudo_only_with_background() {
    // Targets `convert/inline_root.rs:185-213` — the second `if needs_block`
    // block, reached when `paragraph_opt` is None (no direct text) but the
    // element has an inline pseudo image and visual styling that requires
    // a block wrapper. Covers both clipping and opacity-scope arms by
    // including variants below.
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", PR290_PSEUDO_PNG.to_vec());
    let html = r#"<!DOCTYPE html><html><head><style>
        .pseudo-bg::before { content: url("dot.png"); }
        .pseudo-bg { background: #fce; padding: 4pt; border: 1pt solid #888; }
        .pseudo-clip::after { content: url("dot.png"); }
        .pseudo-clip { overflow: hidden; width: 30pt; }
    </style></head><body>
        <p>x<span class="pseudo-bg"></span>y<span class="pseudo-clip"></span>z</p>
    </body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_li_pseudo_image_outside_marker_no_text() {
    // Targets `convert/list_item.rs:387-396` — `build_list_item_body`'s
    // inline-root `<li>` with no text content but a `::before` pseudo
    // image.  The marker stays in `out.list_items`; this branch
    // synthesizes a marker-less paragraph from the pseudo image.
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", PR290_PSEUDO_PNG.to_vec());
    let html = r#"<!DOCTYPE html><html><head><style>
        li::before { content: url("dot.png"); }
    </style></head><body>
        <ul><li></li></ul>
    </body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_abs_positioned_pseudo_image() {
    // Targets `convert/positioned.rs::try_build_absolute_pseudo_image`
    // (Path A of pseudo image sizing). Guards the Px-basis rewrite in
    // fulgur-m0xl -- which removed the Px→Pt→Px round-trip through
    // `AbsCb.padding_box_size` -- so a future refactor cannot silently
    // re-introduce the conversion pair. VRT / examples do not cover
    // `position: absolute` × `content: url()` today.
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", PR290_PSEUDO_PNG.to_vec());
    let html = r#"<!DOCTYPE html><html><head><style>
        .parent { position: relative; width: 100pt; height: 40pt; }
        .parent::before {
            content: url("dot.png");
            position: absolute;
            top: 4pt;
            left: 4pt;
            width: 8pt;
            height: 8pt;
        }
    </style></head><body>
        <div class="parent"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_li_with_opacity_records_opacity_descendants() {
    // Targets `convert/list_item.rs:434-435` — the `opacity_descendants`
    // arm of `record_li_clip_opacity_descendants`. The `<li>` has opacity
    // < 1.0 (so opacity_scope is true) but no overflow clip, so the
    // descendants are recorded into `opacity_descendants` rather than
    // `clip_descendants`.
    let html = r#"<!DOCTYPE html><html><head><style>
        ul li { opacity: 0.5; }
        .blk { display: block; height: 12pt; background: #cef; }
    </style></head><body>
        <ul><li><div class="blk">child</div></li></ul>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_inside_list_style_image_with_image_child() {
    // Targets `inject_marker_into_first_paragraph` shift arms for
    // `LineItem::Image` and `LineItem::InlineBox`
    // (`convert/list_item.rs:287-288, 293-294`).
    // The list uses `list-style-image` with `inside` positioning so the
    // marker is injected as a `LineItem::Image` and the child contents
    // contain an image and an inline-block to exercise both shift arms.
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", PR290_PSEUDO_PNG.to_vec());
    bundle.add_image("dot2.png", PR290_PSEUDO_PNG.to_vec());
    let html = r#"<!DOCTYPE html><html><head><style>
        ul { list-style-position: inside; list-style-image: url("dot.png"); }
        .ib { display: inline-block; width: 12pt; height: 8pt; background: #cef; }
    </style></head><body>
        <ul>
            <li>txt <img src="dot2.png" width="8" height="8"> end</li>
            <li>before<span class="ib"></span>after</li>
        </ul>
    </body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_li_inside_marker_with_inline_box_in_line() {
    // Targets `convert/inline_root.rs:97` — the `LineItem::InlineBox`
    // shift arm in the inside list-style-image marker injection inside
    // `try_convert` (the inline-root path, not the inject helper).
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", PR290_PSEUDO_PNG.to_vec());
    let html = r#"<!DOCTYPE html><html><head><style>
        ul { list-style-position: inside; list-style-image: url("dot.png"); }
        .ib { display: inline-block; width: 16pt; height: 10pt; background: #cef; }
    </style></head><body>
        <ul><li><span class="ib"></span> text</li></ul>
    </body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn render_v2_smoke_content_url_on_replaced_with_visual_style() {
    // Same `convert_content_url` path, but with a styled wrapper so
    // `maybe_insert_block_for_replaced` inserts a BlockEntry too.
    let png_data: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", png_data);
    bundle.add_css(
        r#".replaced { content: url("dot.png"); width: 24pt; height: 24pt;
                       padding: 4pt; background: #fce; border: 1pt solid #888; }"#,
    );
    let html = r#"<!DOCTYPE html><html><body><div class="replaced"></div></body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn page_property_induces_implicit_break_between_named_siblings() {
    // fulgur-uebl: end-to-end coverage for the `page` property's
    // implicit forced-break behaviour. Two block-level siblings with
    // differing named pages should produce a non-empty PDF without
    // any author `break-before:page` declaration. Page-count semantics
    // are validated separately by the `fulgur-wpt` reftests
    // (`page-name-siblings-001` and friends).
    let html = r#"<!DOCTYPE html><html><body>
        <div style="page: a">a</div>
        <div style="page: b">b</div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("page-named siblings render");
    assert!(!pdf.is_empty());
}

#[test]
fn page_property_propagates_through_block_subtree() {
    // fulgur-uebl: when a container has no own `page` declaration but
    // its first block descendant does, the propagated name reaches the
    // sibling-comparison level. Drives `compute_used_page_names`
    // propagation walk and the (start, end) table lookups. WPT
    // `page-name-propagated-001` covers the visual semantics; this is
    // the lib-coverage entry.
    // Outer container has no own `page` — the used page-name must be
    // sourced from the deepest first descendant. Adding an outer page
    // here would short-circuit the propagation walk before it reaches
    // the nested `page: a`, so the test would no longer cover that
    // path (coderabbit PR #336 review).
    let html = r#"<!DOCTYPE html><html><body>
        <div style="page: a">a</div>
        <div>
            <div style="page: c">
                <div style="page: a">b</div>
            </div>
        </div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("page-propagation render");
    assert!(!pdf.is_empty());
}

#[test]
fn page_property_on_orthogonal_block_with_own_page_drives_outer_break() {
    // fulgur-uebl: an orthogonal-flow child is atomic w.r.t. the
    // outer flow, but its **own** `page` declaration must still drive
    // a forced break around the orthogonal box itself — only the
    // child's *internal* propagation is hidden. Regression for the
    // over-collapse coderabbit flagged on PR #336: prior to the fix
    // every orthogonal child was treated as if it inherited the
    // parent's page name, silently suppressing the surrounding break.
    let html = r#"<!DOCTYPE html><html style="writing-mode: vertical-rl"><body>
        <div style="page: a">a</div>
        <div style="writing-mode: horizontal-tb; page: chapter">
            <div>nested</div>
        </div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("orthogonal-with-own-page render");
    assert!(!pdf.is_empty());
}

#[test]
fn page_property_on_inline_canvas_is_ignored() {
    // fulgur-uebl: `page` only applies to block-level boxes (CSS Page 3
    // §5.3). An inline `<canvas>` with `page: b` inherits its parent's
    // `page: a` — covers the `is_block_level_outside` false branch.
    let html = r#"<!DOCTYPE html><html><body style="page: a">
        <canvas height="1" style="page: b"></canvas>
        <div style="page: b">b</div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("inline-canvas page render");
    assert!(!pdf.is_empty());
}

#[test]
fn vh_resolves_against_at_page_content_area() {
    // fulgur-lv0a regression net: viewport-relative units (`vh`, `vw`,
    // `vmin`, `vmax`) must bind to the @page-resolved content area, not
    // the initial page size. Before the fix, `height: 100vh` resolved to
    // the full page height (842pt for A4) so a cover element overflowed
    // into the @page bottom margin. After the fix, the viewport is
    // updated via `blitz_adapter::set_viewport_size_px` *before* the
    // first `resolve()` pass, so Stylo cascades vh/vw against the
    // resolved content area (page_size − @page margins).
    //
    // The VRT golden `bugs/cover-page-break-after.pdf` is the
    // visual-level regression net; this smoke test drives the new
    // `set_viewport_size_px` hook through `Engine::render_html`.
    let html = r#"<!DOCTYPE html><html><head><style>
        @page { size: A4; margin: 20mm; }
        .cover { height: 100vh; background: #123; page-break-after: always; }
    </style></head><body>
        <div class="cover"></div>
        <p>after</p>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("vh-with-page-margin render");
    assert!(!pdf.is_empty());
}

#[test]
fn page_property_inside_flex_container_does_not_propagate_outward() {
    // fulgur-uebl: flex containers establish a flex formatting context
    // where children are not class A break points. `page` on flex items
    // must not surface as forced breaks. Drives the
    // `is_flex_or_grid_container_node` suppression branch.
    let html = r#"<!DOCTYPE html><html><body>
        <div>a</div>
        <div style="display: flex; flex-direction: column">
            <div style="page: b">b</div>
            <div style="page: c">c</div>
        </div>
        <div>d</div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("flex page render");
    assert!(!pdf.is_empty());
}

/// fulgur-6q5 Task 7: convert pass populates `Drawables.paragraph_slices`
/// from `MulticolGeometry::paragraph_splits`. Case B fixture — inline-root
/// `<p>` child whose paragraph parley layout was rebroken to `col_w` by
/// Blitz during `compute_child_layout`. The convert pass reads the
/// per-column line ranges from the multicol geometry side-table and
/// builds one `ParagraphSlice` per non-empty column.
#[test]
fn multicol_inline_root_split_emits_paragraph_slices_case_b() {
    use fulgur::PageSize;

    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0;">
          <p style="font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p>
        </div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 400.0,
            height: 600.0,
        })
        .build();
    let drawables = engine.build_drawables_for_testing_no_gcpm(html);
    assert!(
        !drawables.paragraph_slices.is_empty(),
        "Case B split paragraph must register paragraph_slices entry"
    );
    let entry = drawables.paragraph_slices.values().next().unwrap();
    assert_eq!(
        entry.slices.len(),
        2,
        "expected 2 non-empty column slices, got {}",
        entry.slices.len()
    );
    for slice in &entry.slices {
        assert!(!slice.lines.is_empty(), "slice lines must not be empty");
        assert!(
            slice.size_pt.1 > fulgur::units::Pt::ZERO,
            "slice height (pt) must be > 0"
        );
        // First-line baseline must be slice-relative (line-local). For
        // a 16px (12pt) font, ascent is ~10pt — the baseline must be
        // strictly less than the line height (otherwise the slice
        // wasn't rebased and is still parley-absolute).
        let first = &slice.lines[0];
        assert!(
            first.baseline > fulgur::units::Pt::ZERO && first.baseline <= first.height,
            "slice line[0].baseline must be slice-local: got baseline={}, height={}",
            first.baseline.to_f32(),
            first.height.to_f32(),
        );
    }
    // The two slices must land in different x columns.
    assert_ne!(
        entry.slices[0].origin_pt.0, entry.slices[1].origin_pt.0,
        "slices must occupy distinct columns"
    );
}

/// fulgur-6q5 Task 7: Case A fixture — bare text directly in the multicol
/// container (the container itself is the inline root). The Case A path
/// re-clones the container's parley layout, rebreaks at `col_w`, and
/// reads slice line ranges from the recorded geometry.
#[test]
fn multicol_inline_root_split_emits_paragraph_slices_case_a() {
    use fulgur::PageSize;

    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0; font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 400.0,
            height: 600.0,
        })
        .build();
    let drawables = engine.build_drawables_for_testing_no_gcpm(html);
    assert!(
        !drawables.paragraph_slices.is_empty(),
        "Case A split paragraph must register paragraph_slices entry"
    );
    let entry = drawables.paragraph_slices.values().next().unwrap();
    assert_eq!(
        entry.slices.len(),
        2,
        "expected 2 non-empty column slices, got {}",
        entry.slices.len()
    );
    for slice in &entry.slices {
        assert!(!slice.lines.is_empty(), "slice lines must not be empty");
        assert!(
            slice.size_pt.1 > fulgur::units::Pt::ZERO,
            "slice height (pt) must be > 0"
        );
        let first = &slice.lines[0];
        assert!(
            first.baseline > fulgur::units::Pt::ZERO && first.baseline <= first.height,
            "slice line[0].baseline must be slice-local: got baseline={}, height={}",
            first.baseline.to_f32(),
            first.height.to_f32(),
        );
    }
    assert_ne!(
        entry.slices[0].origin_pt.0, entry.slices[1].origin_pt.0,
        "slices must occupy distinct columns"
    );
}

/// fulgur-6q5 Task 8: `dispatch_fragment` consumes
/// `Drawables.paragraph_slices` and paints each slice at its column
/// origin. Acceptance check — confirm the rendered PDF carries text
/// runs at TWO distinct x positions corresponding to col 0 and col 1.
///
/// Implementation note: we use `fulgur::inspect()` (which already
/// implements CTM / text-matrix tracking via lopdf) on a tempfile-
/// written PDF, rather than re-implementing PDF text-stream parsing
/// inline. Asserting at least two distinct `text_items[i].x` cluster
/// values is sufficient to prove the slice override fired — before
/// this task the standard `draw_paragraph_v2` path painted every line
/// at the source's body-relative position, producing a single x
/// cluster regardless of `column-count`.
///
/// Case B fixture: the source paragraph is a `<p>` child of the
/// multicol container, so `paragraph_slices` is keyed by the `<p>`'s
/// NodeId. The standard `paragraphs.get(&node_id)` arm of
/// `dispatch_fragment` handles the override.
#[test]
fn multicol_inline_root_split_renders_both_columns_in_pdf_case_b() {
    use fulgur::PageSize;
    use fulgur::inspect::inspect;

    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0;">
          <p style="font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p>
        </div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 400.0,
            height: 600.0,
        })
        .build();
    let pdf = engine.render(html).expect("render must succeed");
    assert!(!pdf.is_empty());

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("multicol-case-b.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");

    let inspected = inspect(&path).expect("inspect pdf");
    assert!(
        !inspected.text_items.is_empty(),
        "rendered PDF must contain text items"
    );

    // Cluster x positions to the nearest 0.5 pt to absorb intra-column
    // glyph drift; we want the count of distinct *column* origins, not
    // the count of distinct text-show operators.
    let mut x_clusters = std::collections::BTreeSet::new();
    for item in &inspected.text_items {
        x_clusters.insert((item.x * 2.0).round() as i32);
    }
    assert!(
        x_clusters.len() >= 2,
        "expected text drawn at >=2 distinct x positions (col 0 + col 1), \
         got {} cluster(s): {:?}",
        x_clusters.len(),
        x_clusters,
    );
}

/// fulgur-6q5 Task 8: Case A render acceptance — bare text directly in
/// the multicol container (no `<p>` wrapper). The container is itself
/// the inline root, so `paragraph_slices` is keyed by the container's
/// own NodeId. The container also carries a `block_styles` entry, so
/// `dispatch_fragment` enters the `block_styles` arm and routes through
/// the new `has_paragraph_slices` branch that suppresses the inline
/// `draw_paragraph_inner_paint` call inside `draw_block_with_inner_content`
/// and paints the slices afterward via `paint_multicol_paragraph_slices`.
#[test]
fn multicol_inline_root_split_renders_both_columns_in_pdf_case_a() {
    use fulgur::PageSize;
    use fulgur::inspect::inspect;

    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0; font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 400.0,
            height: 600.0,
        })
        .build();
    let pdf = engine.render(html).expect("render must succeed");
    assert!(!pdf.is_empty());

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("multicol-case-a.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");

    let inspected = inspect(&path).expect("inspect pdf");
    assert!(
        !inspected.text_items.is_empty(),
        "rendered PDF must contain text items"
    );

    let mut x_clusters = std::collections::BTreeSet::new();
    for item in &inspected.text_items {
        x_clusters.insert((item.x * 2.0).round() as i32);
    }
    assert!(
        x_clusters.len() >= 2,
        "Case A: expected text drawn at >=2 distinct x positions (col 0 + col 1), \
         got {} cluster(s): {:?}",
        x_clusters.len(),
        x_clusters,
    );
}

/// fulgur-6q5 Fix 1: Case A's `cloned.align(...)` previously hard-coded
/// `Alignment::default()` (= `Start`), so `text-align: center` (or any
/// non-Start) on a self-inline-root multicol container rendered the
/// split slices as start-aligned. Read the container's resolved
/// `text-align` and feed the matching `parley::Alignment`.
///
/// The check inspects the materialised `Drawables.paragraph_slices`
/// directly: a centre-aligned line in a column whose width far exceeds
/// the glyph-run width must produce a `ShapedGlyphRun.x_offset > 0`
/// (parley records the per-line horizontal shift on each run's offset
/// after `Layout::align`).
#[test]
fn multicol_inline_root_split_honours_text_align_center() {
    use fulgur::PageSize;
    use fulgur::drawables::Drawables;
    use fulgur::paragraph::LineItem;

    // Same content, only the `text-align` value differs. Comparing the
    // measured x_offset of the leading text run between the two cases
    // makes the test relative — wrapping drift across font / parley
    // versions cancels out, and we still catch a regression to
    // start-alignment because the two values must be strictly ordered.
    let center_html = r#"<!doctype html><html><body>
        <div style="column-count: 2; column-gap: 0; text-align: center; font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</div>
    </body></html>"#;
    let start_html = r#"<!doctype html><html><body>
        <div style="column-count: 2; column-gap: 0; text-align: start; font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 400.0,
            height: 600.0,
        })
        .build();
    let center_drawables = engine.build_drawables_for_testing_no_gcpm(center_html);
    let start_drawables = engine.build_drawables_for_testing_no_gcpm(start_html);
    assert!(
        !center_drawables.paragraph_slices.is_empty(),
        "Case A multicol (center) must produce paragraph_slices for the test fixture",
    );
    assert!(
        !start_drawables.paragraph_slices.is_empty(),
        "Case A multicol (start) must produce paragraph_slices for the test fixture",
    );

    // The single source paragraph (the container itself) splits across
    // the two columns — exactly two non-empty slices in both fixtures.
    // The slice counts must match for the comparison to be apples-to-apples.
    let center_slice_count = center_drawables
        .paragraph_slices
        .values()
        .next()
        .expect("center: at least one paragraph_slices entry")
        .slices
        .len();
    let start_slice_count = start_drawables
        .paragraph_slices
        .values()
        .next()
        .expect("start: at least one paragraph_slices entry")
        .slices
        .len();
    assert_eq!(
        center_slice_count, 2,
        "Case A long text in a two-column container must yield two slices (center), \
         got {center_slice_count}",
    );
    assert_eq!(
        start_slice_count, 2,
        "Case A long text in a two-column container must yield two slices (start), \
         got {start_slice_count}",
    );

    fn x_offset_of_first_run(drawables: &Drawables, slice_idx: usize) -> f32 {
        let entry = drawables
            .paragraph_slices
            .values()
            .next()
            .expect("at least one paragraph_slices entry");
        let line = &entry.slices[slice_idx].lines[0];
        line.items
            .iter()
            .find_map(|item| match item {
                LineItem::Text(t) => Some(t.x_offset.to_f32()),
                _ => None,
            })
            .expect("first line of slice must have a text item")
    }

    // For each slice, compare the first text run's `x_offset` between
    // the center- and start-aligned fixtures. With `text-align: start`,
    // the leading run sits at x=0 in current fonts; with `text-align:
    // center`, parley shifts it right by the per-line trailing space.
    // The relative comparison survives wrapping drift because both
    // fixtures share the same content and width.
    for slice_idx in 0..2 {
        let center_x = x_offset_of_first_run(&center_drawables, slice_idx);
        let start_x = x_offset_of_first_run(&start_drawables, slice_idx);
        assert!(
            center_x > start_x + 1e-3,
            "slice {slice_idx}: text-align: center should produce a larger \
             x_offset than text-align: start, but got center={center_x:.3} \
             vs start={start_x:.3}",
        );
    }
}

/// fulgur-6q5 Fix 2: a multicol container whose inline-root paragraph
/// contains inline-box content (`display: inline-block`, inline images,
/// inline-flex …) must NOT generate `paragraph_slices`.
/// `convert_multicol_paragraph_slices` only reconstructs `GlyphRun`
/// items from the parley layout, so an inline-box would be silently
/// dropped on render. The layout pass falls back to atomic placement
/// (whole paragraph in column 0) instead.
///
/// Case A trigger — the multicol container is itself the inline root,
/// hosting bare text with an `inline-block` span.
#[test]
fn multicol_with_inline_box_paragraph_falls_back_to_atomic() {
    use fulgur::PageSize;

    let html = r#"<!doctype html><html><body>
        <div style="column-count: 2; column-gap: 0; font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha <span style="display: inline-block; width: 30px; height: 16px; background: red"></span> alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 400.0,
            height: 600.0,
        })
        .build();
    let drawables = engine.build_drawables_for_testing_no_gcpm(html);
    assert!(
        drawables.paragraph_slices.is_empty(),
        "paragraph_slices must be empty when the inline-root paragraph contains \
         inline-box content (would otherwise drop the inline-box on render); \
         got slices for source ids {:?}",
        drawables.paragraph_slices.keys().collect::<Vec<_>>(),
    );
    // Sanity: rendering still succeeds (the container falls back to
    // atomic placement and renders the text at full container width via
    // the standard inline-root path).
    let pdf = engine.render(html).expect("render must succeed");
    assert!(pdf.starts_with(b"%PDF"));
}

/// fulgur-6q5 Fix 4: when a multicol container straddles a page
/// boundary, `paint_multicol_paragraph_slices` must partition slices
/// per fragment. A slice's `origin_pt.y` is measured from the
/// container's border-box top (= the start of the FIRST fragment), so
/// on a page that owns the container's second fragment we have to
/// subtract the consumed height of prior fragments before placing the
/// slice. Without this, slices on page 2+ would render at impossibly
/// large y positions (the un-rebased pre-fix value could be many times
/// the page height).
///
/// Pre-fix, `paint_multicol_paragraph_slices` painted **every** slice
/// against the **current** page's container fragment origin — meaning
/// page 1 slices were also replayed on every subsequent page (at
/// off-page coordinates) and the page-2 fragment's slices were placed
/// at their original (page-1-relative) y values, far below the page
/// bottom on page 2.
///
/// The regression signal is the per-page bounds sweep below: if any
/// `Tm` operand lands outside the page's vertical visible area on any
/// page, Fix 4 has regressed. Reverting Fix 4 (forcing `consumed = 0`
/// and `needs_partition = false` in `paint_multicol_paragraph_slices`)
/// reproduces the pre-fix bug — page 2's `Tm` y reaches ~126pt on a
/// 100pt-tall page (verified locally on Linux 2026-05-03).
///
/// We deliberately do **not** assert that page 2 carries any `Tm`
/// content. Fix 4 conservatively skips slices that straddle a page
/// boundary, and the post-fix continuation slice on page 2 sits flush
/// against the page-2 fragment's `cutoff` (slice_bottom == cutoff in
/// Linux measurements). That gives ~zero slack against font-driven
/// layout drift: macOS system fonts can shift slice heights / fragment
/// heights by up to ~10%, which would push the slice into the straddle
/// skip and produce zero `Tm` operators on page 2 — without that being
/// a regression of Fix 4 (it's the conservative skip working as
/// designed). Asserting `page 2 has Tm` would therefore be a false
/// negative on macOS while Linux happens to land inside the strip.
/// The bounds sweep is platform-stable: it fires when (and only when)
/// Fix 4 regresses, regardless of whether the continuation slice is
/// painted or skipped on a given page.
///
/// The acceptance test exercises the per-fragment partition by
/// rendering a tight page that forces a multi-fragment multicol
/// container, then asserting:
///
/// 1. Render does not panic.
/// 2. The output PDF has more than one page (confirming the multi-page
///    case is exercised — without this, the test is vacuous because
///    `paint_multicol_paragraph_slices`'s split branch is never
///    reached).
/// 3. The PDF parses cleanly via `lopdf`.
/// 4. No `Tm` operator on any page lands outside the vertical visible
///    area (the bounds sweep — primary regression signal).
#[test]
fn multicol_inline_root_split_skips_slices_outside_current_page() {
    use fulgur::PageSize;

    // Multicol containers only paginate across pages when they include
    // a `column-span: all` child (see `pagination_layout.rs:818`).
    // Otherwise the container is atomic and stays on one page. Combine
    // an inline-root paragraph that splits across columns (Fix 4's
    // target) with a column-span block tall enough to force the
    // *post-span* group onto page 2 — that produces a 2-fragment
    // multicol container.
    let html = r#"<!doctype html><html><body style="margin: 0">
        <div style="column-count: 2; column-gap: 0;">
          <p style="font-size: 16px; margin: 0;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p>
          <h1 style="column-span: all; font-size: 16px; margin: 0;">SPAN</h1>
          <p style="font-size: 16px; margin: 0;">beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta beta</p>
        </div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 400.0,
            height: 100.0,
        })
        .margin(fulgur::config::Margin::uniform(0.0))
        .build();
    let pdf = engine.render(html).expect("render must succeed");
    assert!(!pdf.is_empty(), "render produced empty PDF");
    assert!(pdf.starts_with(b"%PDF"));

    // Confirm the fixture actually produces paragraph_slices entries
    // (without that, the test exercises nothing). We re-run the
    // engine via the Drawables-only helper so we can inspect the
    // intermediate state directly.
    let drawables = engine.build_drawables_for_testing_no_gcpm(html);
    assert!(
        !drawables.paragraph_slices.is_empty(),
        "fixture must produce paragraph_slices to exercise Fix 4",
    );

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("multicol-multipage.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");

    let doc = lopdf::Document::load(&path).expect("PDF must parse");
    let pages = doc.get_pages();
    assert!(
        pages.len() >= 2,
        "fixture must produce a multi-page document to exercise Fix 4 \
         (pre-fix would mis-position slices on pages > 1); got {} page(s)",
        pages.len(),
    );

    // Walk every page's content stream and check that no text-matrix
    // (Tm) operator places text at a y coordinate outside the visible
    // page area. PDF coordinate space is Y-up with origin at the
    // bottom-left (ISO 32000), so on a 100pt-tall page a Tm y operand
    // sitting at y=0 is the bottom of the page and y=100 is the top —
    // any value below 0 or above ~110 (slack for font ascent above
    // baseline) means the text is invisible / off page.
    //
    // Pre-fix `paint_multicol_paragraph_slices` painted slices on
    // page-2+ at their original (page-1-relative) origins because it
    // never subtracted `consumed = sum of prior fragment heights`. On
    // a 100pt page, that produced Tm y operands far above the page
    // top (e.g. y > 100 = above the page). The strict bound below
    // catches that drift.
    const PAGE_HEIGHT_PT: f32 = 100.0;
    // Slack above page top: a glyph baseline sits up to ~font_size
    // above the line top in Y-up space; 16pt font + safety = 25pt.
    const Y_TOP_SLACK: f32 = 25.0;
    let y_max = PAGE_HEIGHT_PT + Y_TOP_SLACK;
    let y_min = -Y_TOP_SLACK;

    // Bounds sweep across all pages — primary regression signal.
    //
    // We deliberately don't assert "page 2 has at least N Tm operators":
    // Fix 4 conservatively skips slices that straddle a page boundary,
    // and on this fixture the page-2 continuation slice can land flush
    // against the fragment cutoff (slice_bottom == cutoff). macOS
    // system fonts drift slice / fragment heights by up to ~10% from
    // Linux's bundled Noto Sans, which would push the slice into the
    // straddle skip and produce zero Tm operators on page 2 — without
    // that being a regression of Fix 4. The bounds sweep, by contrast,
    // is platform-stable: it asserts only that **whatever** lands on
    // each page lands inside the page rect.
    //
    // Verified by reverting Fix 4 (`consumed = 0` and
    // `needs_partition = false`) on Linux 2026-05-03: the sweep fails
    // with "page 2: Tm at y=126.1, outside [-25, 125]".
    for (&page_num, &page_id) in &pages {
        let bytes = doc
            .get_page_content(page_id)
            .expect("page content stream readable");
        let content =
            lopdf::content::Content::decode(&bytes).expect("page content stream decodable");
        for op in &content.operations {
            if op.operator == "Tm" && op.operands.len() >= 6 {
                let y = match &op.operands[5] {
                    lopdf::Object::Integer(i) => *i as f32,
                    lopdf::Object::Real(f) => *f,
                    _ => continue,
                };
                assert!(
                    y >= y_min && y <= y_max,
                    "page {page_num}: Tm operator placed text at y={y:.1}, \
                     outside the visible page area [{y_min:.0}, {y_max:.0}] \
                     (page height {PAGE_HEIGHT_PT}pt) — Fix 4 regressed",
                );
            }
        }
    }
}

#[test]
fn tagged_render_produces_pdf() {
    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render("<html><body><p>hello tagged</p></body></html>")
        .expect("render tagged");
    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/StructTreeRoot"),
        "tagged PDF must have StructTreeRoot"
    );
}

#[test]
fn tagged_pdf_headings_and_paragraphs_produce_struct_tree() {
    let html = r#"<!DOCTYPE html><html lang="en"><body>
        <h1>Heading One</h1>
        <p>First paragraph.</p>
        <h2>Heading Two</h2>
        <p>Second paragraph.</p>
    </body></html>"#;

    let pdf = Engine::builder()
        .tagged(true)
        .lang("en")
        .build()
        .render(html)
        .expect("render tagged headings");

    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/StructTreeRoot"),
        "tagged PDF must contain /StructTreeRoot"
    );
}

#[test]
fn tagged_pdf_multipage_does_not_panic() {
    let mut html = String::from("<!DOCTYPE html><html><body>");
    for i in 0..40 {
        html.push_str(&format!("<h2>Section {i}</h2><p>Content line for section {i}. This is a longer paragraph to ensure we get multi-page output from the renderer.</p>"));
    }
    html.push_str("</body></html>");

    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(&html)
        .expect("render multi-page tagged");

    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/StructTreeRoot"));

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("tagged-multipage.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");
    let doc = lopdf::Document::load(&path).expect("PDF must parse");
    assert!(
        doc.get_pages().len() >= 2,
        "fixture must produce a multi-page document; got {} page(s)",
        doc.get_pages().len()
    );
}

#[test]
fn untagged_pdf_has_no_struct_tree_root() {
    let pdf = Engine::builder()
        .build()
        .render("<html><body><h1>Hello</h1><p>World</p></body></html>")
        .expect("render untagged");

    let s = String::from_utf8_lossy(&pdf);
    assert!(
        !s.contains("/StructTreeRoot"),
        "untagged PDF must not contain /StructTreeRoot"
    );
}

#[test]
fn pdf_ua_without_title_returns_error() {
    // pdf_ua=true requires a document title (PDF/UA-1 §7.1).
    // Neither config.title nor HTML <title> is provided → krilla
    // emits ValidationError::NoDocumentTitle → Err.
    let result = Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .build()
        .render("<html><body><h1>Hello</h1><p>World</p></body></html>");
    assert!(
        result.is_err(),
        "pdf_ua without title must return Err (NoDocumentTitle)"
    );
}

#[test]
fn pdf_ua_with_html_title_succeeds() {
    // PDF/UA-1 smoke: <title> in HTML head provides the document title,
    // satisfying krilla's UA1 requirement without explicit config.title.
    // lang + outline (h1 → bookmark) complete the required metadata.
    //
    // Manual full validation: veraPDF (https://verapdf.org):
    //   java -jar verapdf.jar --flavour ua1 output.pdf
    // CI relies on krilla's own UA1 validator (build-time check).
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head><title>Test Document</title></head>
<body><h1>Hello</h1><p>World</p></body>
</html>"#;

    let pdf = Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .build()
        .render(html)
        .expect("pdf_ua with <title> must succeed");

    assert!(!pdf.is_empty(), "pdf must be non-empty");
    let text = String::from_utf8_lossy(&pdf);
    assert!(
        text.contains("pdfuaid"),
        "pdf must contain pdfuaid XMP namespace"
    );
    assert!(
        text.contains("/StructTreeRoot"),
        "pdf must contain /StructTreeRoot"
    );
    assert!(
        text.contains("/Lang"),
        "pdf must contain /Lang when lang is set"
    );
}

#[test]
fn pdf_ua_with_explicit_title_succeeds() {
    // config.title takes priority over HTML <title>.
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head><title>HTML Title</title></head>
<body><h1>Hello</h1><p>World</p></body>
</html>"#;

    let pdf = Engine::builder()
        .pdf_ua(true)
        .title("Explicit Title")
        .lang("en")
        .build()
        .render(html)
        .expect("pdf_ua with explicit title must succeed");

    assert!(!pdf.is_empty());
}

#[test]
fn pdf_ua_without_lang_succeeds() {
    // PDF/UA-1 strongly recommends lang but does NOT hard-fail
    // when absent (krilla UA1 prohibits(NoDocumentLanguage) = false).
    // Without lang, /Lang is absent from the catalog — semantically
    // incomplete but valid per krilla's enforcement.
    let html = r#"<!DOCTYPE html>
<html>
<head><title>No Lang</title></head>
<body><h1>Hello</h1><p>World</p></body>
</html>"#;

    let pdf = Engine::builder()
        .pdf_ua(true)
        .build()
        .render(html)
        .expect("pdf_ua without lang must succeed");

    assert!(!pdf.is_empty());
}

#[test]
fn html_title_appears_in_untagged_pdf_metadata() {
    // HTML <title> is used as PDF metadata title even for non-tagged PDFs.
    let html = r#"<!DOCTYPE html>
<html><head><title>Untagged Title</title></head>
<body><p>Hello</p></body></html>"#;

    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("untagged render with title");

    let text = String::from_utf8_lossy(&pdf);
    assert!(
        text.contains("Untagged Title"),
        "HTML <title> must appear in PDF metadata for non-tagged PDFs"
    );
}

#[test]
fn tagged_struct_tree_reflects_dom_nesting() {
    // Smoke test: /Div appears in the PDF StructTree bytes (font-agnostic).
    // Deep structural verification (that /Div nests /Hn and /P as children
    // rather than siblings) is tracked in fulgur-izp.5 follow-up.
    let html = r#"<!DOCTYPE html><html lang="en">
<head><style>body{margin:0}</style></head>
<body><section><h1>Title</h1><p>Body.</p></section></body></html>"#;

    let pdf = Engine::builder()
        .tagged(true)
        .lang("en")
        .build()
        .render(html)
        .expect("render");

    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/Div"),
        "StructTree must contain /Div for <section>"
    );
}

#[test]
fn snapshot_tagged_struct_tree_nested() {
    let html = r#"<!DOCTYPE html><html lang="en">
<head><style>body{font-family:'Noto Sans',sans-serif;margin:0}</style></head>
<body><section><h1>Title</h1><p>Body text.</p></section></body></html>"#;
    let pdf = tagged_render_with_noto(html);
    check_pdf_snapshot("tagged_struct_tree_nested", &pdf);
}

#[test]
fn tagged_pdf_is_deterministic() {
    let html = r#"<!DOCTYPE html><html lang="en">
<head><style>body{font-family:'Noto Sans',sans-serif;margin:0}</style></head>
<body><section><h1>Title</h1><p>Body text.</p></section></body></html>"#;
    let pdf1 = tagged_render_with_noto(html);
    let pdf2 = tagged_render_with_noto(html);
    assert_eq!(
        pdf1, pdf2,
        "tagged PDF must be byte-identical across renders"
    );
}

#[test]
fn tagged_figure_alt_text_appears_in_pdf() {
    // 1x1 GIF の data URI（外部ファイル不要）
    let html = r#"<!DOCTYPE html><html lang="en">
<head><style>body{margin:0}</style></head>
<body><img src="data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==" alt="fulgur logo"></body>
</html>"#;

    let pdf = Engine::builder()
        .tagged(true)
        .lang("en")
        .build()
        .render(html)
        .expect("render");

    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/Alt"),
        "PDF StructTree must contain /Alt for <img alt=...>"
    );
    assert!(
        s.contains("fulgur logo"),
        "PDF must embed the alt text value in the /Alt entry"
    );
}

#[test]
fn tagged_table_basic_structure() {
    let html = r#"<!DOCTYPE html><html><body>
        <table>
            <thead><tr><th>Name</th><th>Score</th></tr></thead>
            <tbody>
                <tr><td>Alice</td><td>95</td></tr>
                <tr><td>Bob</td><td>87</td></tr>
            </tbody>
        </table>
    </body></html>"#;
    let pdf = tagged_render_with_noto(html);
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/StructTreeRoot"), "must have StructTreeRoot");
    assert!(s.contains("/Table"), "must have /Table tag");
    assert!(s.contains("/THead"), "must have /THead tag");
    assert!(s.contains("/TBody"), "must have /TBody tag");
    assert!(s.contains("/TH"), "must have /TH tag");
    assert!(s.contains("/TD"), "must have /TD tag");
    assert!(s.contains("/TR"), "must have /TR tag");
}

#[test]
fn tagged_table_thead_tbody_tfoot_distinction() {
    let html = r#"<!DOCTYPE html><html><body>
        <table>
            <thead><tr><th>Header</th></tr></thead>
            <tbody><tr><td>Body</td></tr></tbody>
            <tfoot><tr><td>Footer</td></tr></tfoot>
        </table>
    </body></html>"#;
    let pdf = tagged_render_with_noto(html);
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/THead"), "must have /THead");
    assert!(s.contains("/TBody"), "must have /TBody");
    assert!(s.contains("/TFoot"), "must have /TFoot");
}

#[test]
fn tagged_th_scope_attribute_preserved() {
    let html = r#"<!DOCTYPE html><html><body>
        <table>
            <tr>
                <th scope="col">Column Header</th>
                <th scope="row">Row Header</th>
            </tr>
        </table>
    </body></html>"#;
    let pdf = tagged_render_with_noto(html);
    let s = String::from_utf8_lossy(&pdf);
    // Krilla writes /Scope /Column and /Scope /Row in the PDF stream
    assert!(
        s.contains("/Column"),
        "must have Column scope for col-scoped TH"
    );
    assert!(s.contains("/Row"), "must have Row scope for row-scoped TH");
}

#[test]
fn tagged_pdf_external_link_produces_link_struct_element() {
    let html = r#"<!DOCTYPE html><html lang="en"><body>
        <p>Visit <a href="https://example.com">Example Site</a> for more info.</p>
    </body></html>"#;

    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(html)
        .expect("tagged render with link");

    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/StructTreeRoot"), "must have struct tree root");
    assert!(
        s.contains("/S /Link") || s.contains("/S/Link"),
        "must have /Link structure element"
    );
    assert!(s.contains("/Annots"), "must have link annotation on page");
}

#[test]
fn tagged_pdf_internal_anchor_link_produces_link_struct_element() {
    let html = r##"<!DOCTYPE html><html lang="en"><body>
        <h2 id="section1">Section 1</h2>
        <p>Go to <a href="#section1">Section 1</a> above.</p>
    </body></html>"##;

    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(html)
        .expect("tagged render with internal link");

    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/S /Link") || s.contains("/S/Link"),
        "must have /Link structure element"
    );
    assert!(s.contains("/Annots"), "must have link annotation on page");
}

#[test]
fn tagged_pdf_image_link_does_not_panic() {
    // 1x1 transparent GIF
    let html = r#"<!DOCTYPE html><html lang="en"><body>
        <p><a href="https://example.com"><img src="data:image/gif;base64,R0lGODlhAQABAIAAAP///wAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw==" alt="logo" width="10" height="10"></a></p>
    </body></html>"#;

    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(html)
        .expect("image link tagged render must not panic");

    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/Annots"), "image link must produce annotation");
}

#[test]
fn tagged_pdf_inline_box_after_link_does_not_panic() {
    // Regression: InlineBox was not closing the per-run tag region before
    // dispatching, causing a non-nestable start_tagged panic in Krilla.
    let html = r##"<!DOCTYPE html><html lang="en"><body>
        <p><a href="#x">link text</a><span style="display:inline-block">box</span></p>
    </body></html>"##;

    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(html)
        .expect("inline box after link must not panic");

    assert!(!pdf.is_empty());
}

#[test]
fn untagged_pdf_with_link_preserves_annotation_no_struct_tree() {
    let html = r#"<!DOCTYPE html><html><body>
        <p>Visit <a href="https://example.com">Example</a>.</p>
    </body></html>"#;

    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("untagged render with link");

    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        !s.contains("/StructTreeRoot"),
        "untagged PDF must not have struct tree"
    );
    assert!(
        s.contains("/Annots"),
        "link annotation must be present in untagged PDF"
    );
}

#[test]
fn tagged_pdf_link_in_overflow_hidden_does_not_panic() {
    // Regression: draw_under_clip used try_start_tagged for para with link runs,
    // causing a non-nestable start_tagged panic. Should use use_run_tagging instead.
    let html = r#"<!DOCTYPE html><html lang="en"><body>
        <div style="overflow:hidden;width:200px;height:50px">
            Visit <a href="https://example.com">clipped link</a> here.
        </div>
    </body></html>"#;

    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(html)
        .expect("overflow:hidden link in tagged PDF must not panic");

    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/S /Link") || s.contains("/S/Link"),
        "must have /Link structure element"
    );
    assert!(s.contains("/Annots"), "must have link annotation on page");
}

#[test]
fn tagged_pdf_link_in_list_item_produces_link_struct_element() {
    // Regression: draw_list_item_with_block used try_start_tagged for para with
    // link runs, causing a non-nestable start_tagged panic.
    let html = r#"<!DOCTYPE html><html lang="en"><body>
        <ul>
            <li>Visit <a href="https://example.com">list item link</a>.</li>
        </ul>
    </body></html>"#;

    let pdf = tagged_render_with_noto(html);
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/S /Link") || s.contains("/S/Link"),
        "must have /Link structure element"
    );
    assert!(s.contains("/Annots"), "must have link annotation on page");
}

#[test]
fn tagged_pdf_link_in_multicol_produces_link_struct_element() {
    // Regression: paint_multicol_paragraph_slices never set link_run_node_id,
    // so links in multicol blocks had no /Link StructTree entries.
    let html = r#"<!DOCTYPE html><html lang="en"><body>
        <div style="columns:2;column-gap:20px;width:400px">
            <p>Visit <a href="https://example.com">multicol link</a> here.</p>
        </div>
    </body></html>"#;

    let pdf = tagged_render_with_noto(html);
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/S /Link") || s.contains("/S/Link"),
        "must have /Link structure element"
    );
    assert!(s.contains("/Annots"), "must have link annotation on page");
}

#[test]
fn tagged_pdf_link_in_opacity_block_does_not_panic() {
    // Regression: draw_under_opacity had no tagging at all — para with link runs
    // would invoke draw_shaped_lines while link_run_node_id was not set, meaning
    // no /Link entry in StructTree. The fix adds the full use_run_tagging chain.
    let html = r#"<!DOCTYPE html><html lang="en"><body>
        <div style="opacity:0.8">
            Visit <a href="https://example.com">opacity link</a> here.
        </div>
    </body></html>"#;

    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(html)
        .expect("opacity block with link in tagged PDF must not panic");

    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/S /Link") || s.contains("/S/Link"),
        "must have /Link structure element"
    );
    assert!(s.contains("/Annots"), "must have link annotation on page");
}

#[test]
fn tagged_pdf_link_in_opacity_block_with_inline_box_child() {
    // Exercises the `use_run_tagging` branch inside `draw_under_opacity`
    // (render.rs L2035/2036/2055) which is reached only when the opacity-
    // scoped node is ALSO an inline root (has a ParagraphDraw at the same
    // node_id) AND has `opacity_descendants` non-empty (i.e. it contains
    // inline-box children that register their own drawable entries during
    // `extract_paragraph`). The combination: visual-style + opacity +
    // inline-box child + link run.
    let html = r#"<!DOCTYPE html><html lang="en"><body>
        <div style="opacity:0.8;background:#fff;padding:4px">
            Visit <a href="https://example.com">opacity link</a>
            <span style="display:inline-block;width:10px;height:10px;background:#aaa;"></span>
            here.
        </div>
    </body></html>"#;

    let pdf =
        Engine::builder().tagged(true).build().render(html).expect(
            "opacity inline-root with inline-box child and link in tagged PDF must not panic",
        );

    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/S /Link") || s.contains("/S/Link"),
        "must have /Link structure element"
    );
    assert!(s.contains("/Annots"), "must have link annotation on page");
}

#[test]
fn render_v2_smoke_gcpm_leader_dotted_in_margin_box() {
    let html = r#"<!DOCTYPE html>
<html><head><style>
@page {
  size: A5;
  margin: 40pt;
  @top-right {
    content: "Introduction" leader(dotted) counter(page);
    font-size: 9pt;
    font-family: sans-serif;
  }
}
body { margin: 0; padding: 0; font-size: 11pt; font-family: sans-serif; }
p { margin: 8pt 0; }
</style></head>
<body>
<p>Leader smoke test. This page should have a dot leader in the top-right margin box.</p>
</body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn render_v2_smoke_gcpm_leader_custom_string() {
    let html = r#"<!DOCTYPE html>
<html><head><style>
@page {
  size: A5; margin: 40pt;
  @bottom-center {
    content: "Left" leader(" - ") "Right";
    font-size: 9pt;
    font-family: sans-serif;
  }
}
body { font-family: sans-serif; }
</style></head><body><p>Custom leader test.</p></body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn content_url_resolves_image_when_base_path_set() {
    // Regression: before the AssetBundle base_url fix, Stylo resolved
    // url("dot.png") to an absolute file:// path, but get_image only
    // accepted relative names, so the image was silently dropped.
    let dir = tempfile::tempdir().unwrap();
    // Minimal 1x1 red PNG. Supplied to the engine via AssetBundle, so we
    // intentionally do NOT write it to disk — the bundle short-circuits
    // the base_path lookup that Stylo would otherwise attempt, which is
    // exactly the regression path this test exercises.
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    // Write an external CSS file that uses content: url() for a pseudo
    let css_path = dir.path().join("style.css");
    std::fs::write(
        &css_path,
        r#"p::before { content: url("dot.png"); width: 8pt; height: 8pt; display: block; }"#,
    )
    .unwrap();

    // Write the HTML with a <link> to the external stylesheet
    let html_path = dir.path().join("index.html");
    std::fs::write(
        &html_path,
        r#"<!DOCTYPE html><html><head><link rel="stylesheet" href="style.css"></head>
<body><p>Hello</p></body></html>"#,
    )
    .unwrap();

    let html = std::fs::read_to_string(&html_path).unwrap();
    let mut bundle = fulgur::asset::AssetBundle::new();
    bundle.add_image("dot.png", PNG_1X1.to_vec());

    let pdf = fulgur::Engine::builder()
        .base_path(dir.path())
        .assets(bundle)
        .build()
        .render(&html)
        .unwrap();

    assert!(!pdf.is_empty(), "PDF must be generated");
    // Verify at least one image object appears in the PDF byte stream.
    // XObject images are referenced via "/Subtype /Image" in the PDF.
    assert!(
        pdf.windows(b"/Subtype /Image".len())
            .any(|w| w == b"/Subtype /Image"),
        "PDF must contain at least one image XObject (content: url() not resolved)"
    );
}

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
        // (krilla always emits BOM-prefixed UTF-16BE for outline titles, so
        // the else branch is defensive — never hit in practice.)
        // Mirrors `crates/fulgur/src/inspect.rs::decode_pdf_string`; kept
        // local because that helper is crate-private and not visible from
        // an integration test crate.
        if s.starts_with(&[0xFE, 0xFF]) {
            let chars: Vec<u16> = s[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .copied()
                .map(u16::from_be_bytes)
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

#[test]
fn bookmark_label_counter_appears_in_outline() {
    let html = r#"<!doctype html><html><head><style>
        h1 { counter-increment: chapter; bookmark-level: 1; bookmark-label: counter(chapter) ". " content(text); }
    </style></head><body>
        <h1>Intro</h1><h1>Method</h1>
    </body></html>"#;
    let pdf = Engine::builder()
        .bookmarks(true)
        .build()
        .render(html)
        .expect("render");
    let titles = outline_titles(&pdf);
    assert!(
        titles.iter().any(|t| t == "1. Intro"),
        "outline should contain '1. Intro', got: {titles:?}"
    );
    assert!(
        titles.iter().any(|t| t == "2. Method"),
        "outline should contain '2. Method', got: {titles:?}"
    );
}

#[test]
fn bookmark_label_string_appears_in_outline() {
    let html = r#"<!doctype html><html><head><style>
        h1 { string-set: title content(text); bookmark-level: 1; bookmark-label: string(title); }
    </style></head><body>
        <h1>Alpha</h1><h1>Beta</h1>
    </body></html>"#;
    let pdf = Engine::builder()
        .bookmarks(true)
        .build()
        .render(html)
        .expect("render");
    let titles = outline_titles(&pdf);
    assert_eq!(titles, vec!["Alpha".to_string(), "Beta".to_string()]);
}

/// End-to-end coverage-scope companion (CLAUDE.md "Coverage scope") for the
/// `StringSetPass` per-node snapshot budget (`MAX_STRING_SNAPSHOT_BYTES`,
/// unit-tested in `string_set_pass_snapshot_bounds_total_stored_bytes`). The
/// lib test drives the pass in isolation via `.with_snapshot_recording()`; this
/// drives the *engine* gate → pass → budget path on the amplification shape:
/// one large `string-set` value followed by many elements that each snapshot it.
/// Asserts the render stays bounded (completes, non-empty PDF) and that the
/// heading's `string(title)` label still resolves — the budget clips only the
/// per-node snapshot storage, never the first-seen value.
#[test]
fn string_set_snapshot_bounded_render_does_not_blow_up() {
    let big = "A".repeat(48 * 1024); // one large named-string source value
    let mut html = String::from(
        "<!doctype html><html><head><style>\
         h1 { string-set: title content(text); bookmark-level: 1; bookmark-label: string(title); }\
         </style></head><body>",
    );
    html.push_str(&format!("<h1>{big}</h1>"));
    // Many following siblings each trigger a per-node snapshot clone of `title`
    // through the engine's `record_string_snapshots` gate; without the budget
    // this is O(N × value_size) retained memory.
    for _ in 0..400 {
        html.push_str("<p>x</p>");
    }
    html.push_str("</body></html>");

    let pdf = Engine::builder()
        .bookmarks(true)
        .build()
        .render(&html)
        .expect("render");
    assert!(
        !pdf.is_empty(),
        "adversarial string-set doc should still render"
    );
    let titles = outline_titles(&pdf);
    assert!(
        titles.iter().any(|t| t == &big),
        "the heading's string(title) label must still resolve"
    );
}

/// Regression test for fulgur-v1cm + fulgur-vrkv: render time must not
/// blow up quadratically with section/table count.
///
/// Pre-fulgur-v1cm a 100-section doc rendered ~410ms while a 10-section
/// doc was ~10ms — a 41x ratio, well past the 10x linear baseline
/// (textbook O(N²) signature). v1cm moved the page-independent skip
/// sets and per-page geometry buckets out of the per-page loop and
/// gated `convert_node`'s document-wide drawables snapshot on
/// `node_has_transform`. fulgur-vrkv then replaced the remaining
/// unconditional `BTreeSet<usize>` snapshot in
/// `pseudo::register_pseudo_content` with an O(1) length-sum probe,
/// dropping the t100/t10 ratio from ~4.5x to ~2.6x.
///
/// We assert ratio (not absolute time) to stay robust against CI
/// variance. 15x is tight enough to fail loudly if a future change
/// reintroduces a per-node document-wide snapshot, while still
/// tolerating sub-linear startup amortisation at small N.
#[test]
fn render_table_pagebreak_does_not_scale_quadratically() {
    fn build(n: usize) -> String {
        let mut html = String::from("<!DOCTYPE html><html><body>");
        for i in 0..n {
            html.push_str(&format!(
                "<div style=\"page-break-after:always\"><h2>S{i}</h2><p>Lorem ipsum.</p>\
<table><thead><tr><th>A</th><th>B</th></tr></thead>\
<tbody><tr><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></tbody></table></div>",
            ));
        }
        html.push_str("</body></html>");
        html
    }

    fn time_render(n: usize) -> std::time::Duration {
        let html = build(n);
        // Warm up so the first call doesn't pay font / GCPM init costs.
        let _ = fulgur::Engine::builder().build().render(&html).unwrap();
        let start = std::time::Instant::now();
        let _ = fulgur::Engine::builder().build().render(&html).unwrap();
        start.elapsed()
    }

    let t10 = time_render(10);
    let t100 = time_render(100);

    let ratio = t100.as_secs_f64() / t10.as_secs_f64();
    assert!(
        ratio < 15.0,
        "render time scaling regressed: t10={:?} t100={:?} ratio={:.1}x \
         (expected < 15x — see fulgur-v1cm + fulgur-vrkv)",
        t10,
        t100,
        ratio,
    );
}

/// End-to-end smoke for the 2-pass `target-counter` orchestration. A
/// simple TOC declares `a::after { content: ...
/// target-counter(attr(href), page) ... }`. After pass 1 paginates the
/// chapter headings (each on a fresh page via `page-break-before`),
/// pass 2 renders the TOC links with their resolved page numbers.
///
/// Krilla emits CID-encoded TJ strings, so neither raw byte search for
/// "(p.2)" nor `fulgur::inspect::inspect` (which lacks ToUnicode CMap
/// parsing on the lopdf 0.40 stack — see MEMORY) recovers Unicode text
/// from the content stream. What Krilla DOES write in human-readable
/// form is the `/Span << /ActualText <FEFF…> >>` marker (PDF tagging
/// for accessibility): the bracketed hex is UTF-16BE of the
/// post-resolution text. We assert against that — pass 2 must produce
/// "(p.2)" and "(p.3)" in some ActualText payload, otherwise the
/// orchestration didn't fire.
#[test]
fn target_counter_in_toc_renders_page_number() {
    let html = r##"
<!doctype html>
<html><head><style>
  body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; }
  a::after { content: " (p." target-counter(attr(href), page) ")"; }
  h2 { page-break-before: always; }
</style></head>
<body>
  <nav class="toc">
    <a href="#a">Chapter A</a><br>
    <a href="#b">Chapter B</a>
  </nav>
  <h2 id="a">Chapter A</h2>
  <p>aaa</p>
  <h2 id="b">Chapter B</h2>
  <p>bbb</p>
</body></html>"##;

    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets
        .add_font_file(&font_path)
        .unwrap_or_else(|e| panic!("failed to load Noto Sans from {}: {e}", font_path.display()));
    assets.add_css("body { font-family: 'Noto Sans', sans-serif; }");

    let pdf = Engine::builder()
        .tagged(true)
        .assets(assets)
        .build()
        .render(html)
        .expect("render");
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    assert!(
        text.contains("(p.2)"),
        "extracted PDF text missing `(p.2)` — pass 2 likely did not fire \
         and target-counter was not resolved to the actual page number. \
         Got: {text:?}"
    );
    assert!(
        text.contains("(p.3)"),
        "extracted PDF text missing `(p.3)` — pass 2 likely did not fire. \
         Got: {text:?}"
    );
}

/// Regression for `target-*` declared **only** in a
/// `<link rel="stylesheet">`-loaded CSS file: the 2-pass orchestration
/// must still fire. The previous pre-parse probe scanned
/// `assets.combined_css()` plus a literal substring sweep of the HTML;
/// neither captures `<link>`-loaded files (resolved later by
/// `parse_html_with_local_resources`), so the literal `target-*` never
/// appeared in either signal and pass 2 was silently skipped —
/// resolved page numbers degraded to `"00"` placeholders. The
/// post-parse gate inside `layout_to_drawables` runs after
/// `extend_from(link_gcpm)`, so the decision is made against the merged
/// GCPM context.
#[test]
fn target_counter_in_link_loaded_css_triggers_pass_two() {
    let dir = tempdir().expect("tempdir");
    let css_path = dir.path().join("toc.css");
    // `target-counter(...)` lives ONLY in this file. The HTML below
    // never contains the substring, so the old pre-parse probe (which
    // scanned `assets.combined_css()` plus a literal sweep of the HTML)
    // could not detect it. The new post-parse gate sees it via
    // `extend_from(link_gcpm)`. `page-break-before` stays in the inline
    // `<style>` so cascade ordering is uncoupled from this test's
    // detection-of-target-* assertion.
    std::fs::write(
        &css_path,
        "a::after { content: \" (p.\" target-counter(attr(href), page) \")\"; }\n",
    )
    .expect("write css");

    // Note: no `target-counter(` token anywhere in this HTML. The only
    // GCPM-relevant thing the engine sees inline is the <link> ref.
    let html = r##"
<!doctype html>
<html><head>
  <link rel="stylesheet" href="toc.css">
  <style>
    body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; }
    h2 { page-break-before: always; }
  </style>
</head>
<body>
  <nav class="toc">
    <a href="#a">Chapter A</a><br>
    <a href="#b">Chapter B</a>
  </nav>
  <h2 id="a">Chapter A</h2>
  <p>aaa</p>
  <h2 id="b">Chapter B</h2>
  <p>bbb</p>
</body></html>"##;

    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets
        .add_font_file(&font_path)
        .unwrap_or_else(|e| panic!("failed to load Noto Sans from {}: {e}", font_path.display()));
    assets.add_css("body { font-family: 'Noto Sans', sans-serif; }");

    let pdf = Engine::builder()
        .tagged(true)
        .assets(assets)
        .base_path(dir.path())
        .build()
        .render(html)
        .expect("render");
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    assert!(
        text.contains("(p.2)"),
        "extracted PDF text missing `(p.2)` — pass 2 did not fire for \
         <link>-loaded target-counter CSS. Got: {text:?}"
    );
    assert!(
        text.contains("(p.3)"),
        "extracted PDF text missing `(p.3)` — pass 2 did not fire for \
         <link>-loaded target-counter CSS. Got: {text:?}"
    );
}

/// fulgur-qgy7: `target-text(attr(href))` inside an `@page` margin box
/// has no link element to read `href` from. The renderer supplies an
/// implicit reference — the first `<a href="#...">` landing on the
/// current page — so `@top-center { content: ... target-text(...) }`
/// resolves to the section title that page 1's link points at. The
/// margin box's `"Header: "` prefix is unique to the margin-box
/// payload, so a substring search distinguishes it from the body's
/// `<h2>` copy.
#[test]
fn target_text_in_top_center_resolves_via_implicit_href() {
    let html = r##"
<!doctype html>
<html><head><style>
  body { counter-reset: chapter; font-family: 'Noto Sans', sans-serif; font-size: 12pt; }
  @page { margin: 1in; @top-center { content: "Header: " target-text(attr(href)); } }
  h2 { page-break-before: always; }
</style></head>
<body>
  <p><a href="#sec1">Jump to section</a></p>
  <h2 id="sec1">My Section Title</h2>
  <p>section body</p>
</body></html>"##;

    let pdf = tagged_render_with_noto(html);
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    // The literal `Header: ` prefix is unique to the margin-box payload
    // (the body's `<h2>` contains only the bare title), so this contiguous
    // match proves the margin box rendered the resolved `target-text` —
    // not just that `My Section Title` exists somewhere on the page.
    assert!(
        text.contains("Header: My Section Title"),
        "extracted PDF text missing `Header: My Section Title` — \
         margin-box `target-text(attr(href))` did not pick up the implicit \
         href from `<a href=\"#sec1\">`. Got: {text:?}"
    );
}

#[test]
fn target_text_second_arg_resolves_target_fragments() {
    let html = r##"
<!doctype html>
<html><head><style>
  body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; }
  @page {
    margin: 1in;
    @top-center {
      content: "Before: " target-text(attr(href), before)
               " After: " target-text(attr(href), after)
               " First: " target-text(attr(href), first-letter);
    }
  }
  h2 { counter-increment: chapter; }
  h2::before { content: "BeforeFrag" counter(chapter); }
  h2::after { content: "AfterFrag" counter(chapter); }
</style></head>
<body>
  <p><a href="#sec1">Jump</a></p>
  <h2 id="sec1">My Section Title</h2>
</body></html>"##;

    let pdf = tagged_render_with_noto(html);
    assert!(!pdf.is_empty());

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("target-text-second-arg.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");

    let output = std::process::Command::new("pdftotext")
        .arg(&path)
        .arg("-")
        .output();
    let Ok(output) = output else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("Before: BeforeFrag1 After: AfterFrag1 First: M"),
        "target-text second-argument payload missing from extracted PDF text: {text:?}"
    );
}

/// fulgur-r73p Task 4.1: `target-text(attr(href), before)` must reflect a
/// target element's *cascade-only* `::before` content. `#sec::before`
/// here uses `attr(data-tag)` + a literal string — content with no
/// `counter()` / `target-*`, which the GCPM parser deliberately routes to
/// Blitz's normal cascade and which the old plumbing never captured into
/// `AnchorMap`. The read-only `collect_pseudo_text` capture added in
/// Task 2 must pick `APP: ` up so the referencing `.ref::after` renders
/// it into pass 2. The `<a>` carries visible text so the inline host is
/// not culled before its generated `::after` is drawn.
#[test]
fn target_text_before_resolves_attr_pseudo() {
    let html = r##"<!doctype html><html><head><style>
      body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; }
      #sec::before { content: attr(data-tag) ": "; }
      .ref::after  { content: "[" target-text(attr(href), before) "]"; }
    </style></head><body>
      <p>See <a class="ref" href="#sec">the appendix</a> for details.</p>
      <h2 id="sec" data-tag="APP">Appendix</h2>
    </body></html>"##;
    let pdf = tagged_render_with_noto(html);
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    // Sentinel-wrapped: the trailing `]` immediately follows the captured
    // text, so a hit proves the trailing separator space of `APP: ` is part
    // of the captured pseudo content — not borrowed from following page
    // text. A trim-everything normalization would yield `[APP:]` and fail.
    assert!(
        text.contains("[APP: ]"),
        "extracted PDF text missing contiguous `[APP: ]` — \
         collect_pseudo_text did not capture the cascade-only attr() \
         `::before` of #sec (with its trailing separator space) into the \
         AnchorMap, so `.ref::after {{ content: \"[\" \
         target-text(attr(href), before) \"]\" }}` rendered the wrong \
         text. Got: {text:?}"
    );
}

/// fulgur-r73p Task 4.2 / fulgur-da3u: a counter-bearing `::after`
/// content list selected by an **ID selector inside an inline `<style>`**
/// must render its resolved value to the PDF text layer.
///
/// `#s::after` uses `counter(c)`, so `CounterPass` + `InjectCssPass`
/// overlay the resolved value into the cascade; `.r::before { content:
/// target-text(attr(href), after) }` makes `has_target_references()`
/// true so `collect_pseudo_text` reads that counter-overlaid computed
/// content for `#s::after` directly.
///
/// This was a no-panic-only guard until fulgur-da3u: the inline
/// `<style>` text stays in the DOM, so the author `#s::after` rule
/// (`1,0,1`) survived and out-specified `CounterPass`'s bare
/// `[data-fulgur-cid]::after` injection (`0,1,1`), leaving Blitz to
/// render the `items[0]`-truncated view (just the leading `[`).
/// fulgur-da3u reconstructs the element compound into the injected
/// selector so the injection wins, which is exactly what makes the
/// real `[2]` text-layer assertion below possible — it discharges
/// fulgur-2ykw acceptance criterion #4, deferred to fulgur-da3u.
#[test]
fn target_text_after_resolves_counter_via_counter_pass() {
    let html = r##"<!doctype html><html><head><style>
      body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; counter-reset: c; }
      h2 { counter-increment: c; }
      #s::after { content: " [" counter(c) "]"; }
      .r::before { content: target-text(attr(href), after); }
    </style></head><body>
      <h2>One</h2>
      <h2 id="s">Two</h2>
      <p>Jump to <a class="r" href="#s">section two</a> now.</p>
    </body></html>"##;
    let pdf = tagged_render_with_noto(html);
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    // `#s` is the second `h2`, so counter `c` == 2. All three items of
    // `" [" counter(c) "]"` must render — the contiguous run `[2]` is
    // present only when the CounterPass injection out-specifies the
    // surviving inline-`<style>` author `#s::after` rule.
    assert!(
        text.contains("[2]"),
        "extracted PDF text missing contiguous `[2]` — the \
         counter-resolved `#s::after` content (selected by an ID selector \
         in an inline `<style>`) must render ALL items. Got: {text:?}"
    );
}

/// fulgur-r73p Task 4.3: `target-text(url, first-letter)` must apply the
/// CSS Pseudo-Elements 4 §3.2 algorithm — leading typographic
/// punctuation is included with the first letter, so the result is
/// `「H`, not `「` or `H`.
///
/// `「` (U+300C) is not in `NotoSans-Regular.ttf`, so this test layers
/// in `NotoSansJP-Regular.otf` (also bundled under `examples/.fonts/`)
/// to give the CJK punctuation a glyph with a real ToUnicode CMap
/// entry — pdftotext can then recover the rendered text. Without the
/// JP font Stylo would fall back to a system font whose CID 0 lands in
/// the PDF unmapped and no extractor can recover it. The pure
/// `compute_first_letter` algorithm (including its CJK punctuation
/// handling) is covered by unit tests in
/// `crates/fulgur/src/gcpm/target_ref.rs` — this test exercises the
/// end-to-end render plumbing.
#[test]
fn target_text_first_letter_typographic() {
    let html = r##"<!doctype html><html><head><style>
      body { font-family: 'Noto Sans JP', 'Noto Sans', sans-serif; font-size: 12pt; }
      .r::after { content: target-text(attr(href), first-letter); }
    </style></head><body>
      <p>Open <a class="r" href="#h">the heading</a> reference.</p>
      <h2 id="h">「Hello」</h2>
    </body></html>"##;

    let fonts_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/.fonts");
    let mut assets = AssetBundle::default();
    for name in ["NotoSans-Regular.ttf", "NotoSansJP-Regular.otf"] {
        let path = fonts_dir.join(name);
        assets
            .add_font_file(&path)
            .unwrap_or_else(|e| panic!("failed to load {}: {e}", path.display()));
    }
    assets.add_css("body { font-family: 'Noto Sans JP', 'Noto Sans', sans-serif; }");
    let pdf = Engine::builder()
        .tagged(true)
        .lang("en")
        .assets(assets)
        .build()
        .render(html)
        .expect("tagged render");
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    // The pseudo-element content `「H` must appear contiguously. `H`
    // alone could come from the body's `<h2>` "Hello"; `「H` together
    // proves compute_first_letter folded the leading CJK typographic
    // punctuation into the first-letter result.
    //
    // On Windows, Parley falls back to system CJK fonts (Meiryo / Yu
    // Gothic) which lack recoverable ToUnicode entries, so pdftotext
    // cannot extract 「. Skip the assertion on that platform only.
    #[cfg(not(target_os = "windows"))]
    assert!(
        text.contains("「H"),
        "extracted PDF text missing contiguous `「H` — \
         compute_first_letter did not fold the leading typographic \
         punctuation into the first letter per CSS Pseudo-Elements 4 \
         §3.2. Got: {text:?}"
    );
}

/// fulgur-r73p Task 4.4: a `target-text(url, before)` whose target has
/// no `::before` must resolve to the empty string — no panic, render
/// still succeeds, and the surrounding literal `[` / `]` brackets still
/// render with nothing captured between them.
#[test]
fn target_text_empty_for_missing_pseudo() {
    let html = r##"<!doctype html><html><head><style>
      body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; }
      .r::after { content: "[" target-text(attr(href), before) "]"; }
    </style></head><body>
      <p>Check <a class="r" href="#h">this link</a> here.</p>
      <h2 id="h">No pseudo here</h2>
    </body></html>"##;
    let pdf = tagged_render_with_noto(html);
    assert!(!pdf.is_empty()); // resolves to "[]" — no panic, empty capture

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    // The brackets must be adjacent (`[]`, not `[X]`): a missing
    // `::before` resolves the inner target-text to EMPTY, so nothing is
    // captured between them. Asserting contiguous `[]` distinguishes
    // this from a regression that inserts spurious content.
    assert!(
        text.contains("[]"),
        "extracted PDF text missing contiguous `[]` — a missing \
         `::before` should resolve target-text to the empty string, \
         leaving the surrounding brackets adjacent (`[]`, not `[X]`). \
         Got: {text:?}"
    );
}

/// fulgur-2ykw: a multi-item element `::after` content list — pure
/// strings, NO `counter()` / `target-*` so the GCPM scan does not route
/// it through the `CounterPass` flattening workaround — must render ALL
/// items in order to the PDF text layer, not just the first. Before the
/// fix, blitz-dom 0.2.4's `flush_pseudo_elements` materialized only
/// `items[0]`, so only the leading `[` reached the text layer.
#[test]
fn pseudo_multi_item_content_renders_all_items() {
    let html = r##"<!doctype html><html><head><style>
      body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; }
      p::after { content: "[" "x" "]"; }
    </style></head><body>
      <p>Body text</p>
    </body></html>"##;
    let pdf = tagged_render_with_noto(html);
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    // Contiguous `[x]` proves all three items rendered in order.
    // Checking `[`, `x`, `]` individually would not — `x` is already in
    // "Body te**x**t", and `[`/`]` could each appear without the middle
    // item if only items[0] and items[2] survived.
    assert!(
        text.contains("[x]"),
        "extracted PDF text missing contiguous `[x]` — a multi-item \
         `::after` content list must render ALL items, not just the \
         first `[`. Got: {text:?}"
    );
}

/// Static multi-item pseudo content is injected once at the *selector*
/// level, not once per matching node — that per-node expansion is the
/// availability bug this hardening removed (a hostile document could turn a
/// small `p::after { content: "…20KB…" "x" }` plus many `<p>` into O(nodes ×
/// literal_len) generated CSS). This proves the single injected `p::after`
/// rule still reaches every matching element: all three paragraphs render
/// the full flattened `Q1Q2`.
#[test]
fn pseudo_multi_item_static_content_covers_all_matching_nodes() {
    let html = r##"<!doctype html><html><head><style>
      body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; }
      p::after { content: "Q1" "Q2"; }
    </style></head><body>
      <p>a</p><p>b</p><p>c</p>
    </body></html>"##;
    let pdf = tagged_render_with_noto(html);
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    // One `p::after` rule, three matching `<p>` → three contiguous `Q1Q2`.
    let count = text.matches("Q1Q2").count();
    assert!(
        count >= 3,
        "one selector-level `p::after` rule must reach all three \
         paragraphs (expected >= 3 contiguous `Q1Q2`, got {count}). \
         Text: {text:?}"
    );
}

/// Multi-item static pseudo content declared in **AssetBundle / `<link>`
/// CSS** (not inline `<style>`) — the finding's primary attack surface via
/// `render_html(combined_css)`. On this path `cleaned_css` is what Blitz sees,
/// so the parser strips the original declaration and the injected selector-
/// level rule is the only source. This asserts the full `[x]` renders (see
/// `verify_bundle_important_multi_item` for the `!important` variant that the
/// strip specifically rescues).
#[test]
fn pseudo_multi_item_static_content_via_bundle_css() {
    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets
        .add_font_file(&font_path)
        .unwrap_or_else(|e| panic!("failed to load Noto Sans: {e}"));
    // The GCPM content rule is in bundle CSS, not inline `<style>`.
    assets.add_css(
        "body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; } \
         p::after { content: \"[\" \"x\" \"]\"; }",
    );
    let pdf = Engine::builder()
        .tagged(true)
        .lang("en")
        .assets(assets)
        .build()
        .render("<!doctype html><html><body><p>Body text</p></body></html>")
        .expect("bundle render");
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    assert!(
        text.contains("[x]"),
        "multi-item static content declared in bundle CSS must render all \
         items (`[x]`), not just `[`. Got: {text:?}"
    );
}

/// Regression: a multi-item static `content` declared `!important` in
/// AssetBundle / `<link>` CSS. Because the injected selector-level rule is
/// normal priority, an un-stripped `!important` original would win in Stylo
/// and blitz-dom would keep truncating to `items[0]` (`[`). The parser
/// therefore strips the original from `cleaned_css` on the bundle/link path,
/// so the injection is the only source and the full `[x]` renders. (Inline
/// `<style>` `!important` is a separate, pre-existing limitation — its text is
/// never in `cleaned_css`, so the important original survives there.)
#[test]
fn pseudo_multi_item_important_static_content_via_bundle_css() {
    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets
        .add_font_file(&font_path)
        .unwrap_or_else(|e| panic!("failed to load Noto Sans: {e}"));
    assets.add_css(
        "body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; } \
         p::after { content: \"[\" \"x\" \"]\" !important; }",
    );
    let pdf = Engine::builder()
        .tagged(true)
        .lang("en")
        .assets(assets)
        .build()
        .render("<!doctype html><html><body><p>Body text</p></body></html>")
        .expect("bundle render");
    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    assert!(
        text.contains("[x]"),
        "`!important` multi-item static content in bundle CSS must render all \
         items (`[x]`); a surviving `!important` original would truncate to \
         `[`. Got: {text:?}"
    );
}

/// Known limitation (documented, not a bug to fix here): when the *same*
/// simple `selector::pseudo` is declared twice — first with `counter()` /
/// `target-*` (routed per-node through `CounterPass` with boosted specificity)
/// and then with a static multi-item string (injected at plain selector-level
/// specificity) — the later static rule does NOT override the earlier dynamic
/// one, so `[1]` wins instead of `Q1Q2`. Preserving source order here would
/// require cross-rule declaration-order tracking; the DoS-safe fix is not
/// worth the surface for a pattern no real document uses (same class as the
/// accepted GCPM cascade-collision limitation fulgur-da3u). This test pins the
/// current behavior so a future change is noticed.
#[test]
fn pseudo_counter_then_static_same_selector_is_known_limitation() {
    let html = r##"<!doctype html><html><head><style>
      body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; counter-reset: n; }
      p { counter-increment: n; }
      p::after { content: "[" counter(n) "]"; }
      p::after { content: "Q1" "Q2"; }
    </style></head><body><p>x</p></body></html>"##;
    let pdf = tagged_render_with_noto(html);
    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    // Documents the current (limitation) behavior: the counter content wins.
    assert!(
        text.contains("[1]") && !text.contains("Q1Q2"),
        "known limitation: counter content is expected to win over the later \
         static rule. Got: {text:?}"
    );
}

/// fulgur-2ykw: the canonical 3-item case — open-bracket string +
/// `counter()` + close-bracket string — on `::before` (the spec sibling
/// of the `::after` path; both go through blitz-dom's shared
/// `flush_pseudo_elements`). All three items must reach the text layer
/// contiguously as `[1]`, and single-item content elsewhere on the page
/// must stay unchanged.
#[test]
fn pseudo_three_item_counter_content_before_and_single_item_unchanged() {
    let html = r##"<!doctype html><html><head><style>
      body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; counter-reset: c; }
      h2 { counter-increment: c; }
      h2::before { content: "[" counter(c) "]"; }
      p::after { content: "."; }
    </style></head><body>
      <h2>Section</h2>
      <p>Body</p>
    </body></html>"##;
    let pdf = tagged_render_with_noto(html);
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    // Contiguous `[1]` proves all three items (string + counter() +
    // string) rendered together and the counter resolved to 1.
    assert!(
        text.contains("[1]"),
        "extracted PDF text missing contiguous `[1]` — a 3-item \
         `::before` content list (string + counter() + string) must \
         render ALL items in order. Got: {text:?}"
    );
    // Single-item plain `::after` content still renders via Blitz's
    // native path (the lone `.` after "Body").
    assert!(
        text.contains("Body."),
        "extracted PDF text missing `Body.` — single-item pseudo \
         content (`.`) must keep rendering via Blitz's native path. \
         Got: {text:?}"
    );
}

/// fulgur-da3u: a multi-item element `::before` content list selected by
/// an **ID selector inside an inline `<style>`** must render ALL items to
/// the PDF text layer.
///
/// The inline `<style>` text stays in the DOM verbatim, so the author
/// `#s::before` rule (specificity `1,0,1`) survives the cascade. Before
/// the fix, `CounterPass`'s injected rule used a bare
/// `[data-fulgur-cid]::before` selector (`0,1,1`) and lost — Blitz then
/// rendered its `items[0]`-truncated view of the surviving author rule,
/// so only the leading `[` reached the text layer. The fix reconstructs
/// the element's own compound (`h2#s…[data-fulgur-cid]::before`) so the
/// injection out-specifies the author rule.
#[test]
fn pseudo_multi_item_content_via_inline_style_id_selector() {
    let html = r##"<!doctype html><html><head><style>
      body { font-family: 'Noto Sans', sans-serif; font-size: 12pt; counter-reset: c; }
      h2 { counter-increment: c; }
      #s::before { content: "[" counter(c) "]"; }
    </style></head><body>
      <h2>One</h2>
      <h2 id="s">Two</h2>
    </body></html>"##;
    let pdf = tagged_render_with_noto(html);
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    // `#s` is the second `h2`, so counter `c` == 2. All three items must
    // render contiguously as `[2]`, not the truncated leading `[`.
    assert!(
        text.contains("[2]"),
        "extracted PDF text missing contiguous `[2]` — a multi-item \
         `::before` content list selected by an ID selector in an inline \
         `<style>` must render ALL items. The CounterPass injected rule \
         must out-specify the surviving author `#s::before` rule. \
         Got: {text:?}"
    );
}

// ── ShapedGlyph text_range invariant tests ────────────────────────────────
//
// The invariant under test: selecting a contiguous run of glyphs should copy
// exactly the characters those glyphs represent — nothing more, nothing less.
// A PDF reader determines what to copy by taking the union of selected glyphs'
// `text_range`s.
//
// The original bug assigned `0..text.len()` to every glyph, so selecting any
// single glyph would union to the entire paragraph string.  These tests
// reconstruct the copied text from the glyph array and compare it to the
// expected string, so they fail on that bug and pass on the fix.
//
// No PDF rendering needed — the invariants live on the `ShapedGlyph` array
// that the PDF encoder consumes, so `build_drawables_for_testing_no_gcpm`
// is sufficient.

/// Collect every `ShapedGlyphRun` from paragraphs and list markers.
fn collect_text_runs(
    drawables: &fulgur::drawables::Drawables,
) -> Vec<fulgur::paragraph::ShapedGlyphRun> {
    use fulgur::drawables::ListItemMarker;
    use fulgur::paragraph::{LineItem, ShapedLine};

    fn from_lines(lines: &[ShapedLine], out: &mut Vec<fulgur::paragraph::ShapedGlyphRun>) {
        for line in lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    out.push(run.clone());
                }
            }
        }
    }

    let mut runs = Vec::new();
    for entry in drawables.paragraphs.values() {
        from_lines(&entry.lines, &mut runs);
    }
    for entry in drawables.paragraph_slices.values() {
        for slice in &entry.slices {
            from_lines(&slice.lines, &mut runs);
        }
    }
    for entry in drawables.list_items.values() {
        if let ListItemMarker::Text { lines, .. } = &entry.marker {
            from_lines(lines, &mut runs);
        }
    }
    runs
}

/// Simulate "select all glyphs in this run and copy".
///
/// Consecutive glyphs that share a `text_range` belong to the same cluster
/// (e.g. a base letter + combining diacritic, or a ligature glyph); they are
/// deduplicated so each cluster contributes its text exactly once.
///
/// Panics immediately if any range is out of bounds or splits a UTF-8
/// character boundary — the panic message identifies the offending glyph.
fn copy_run(run: &fulgur::paragraph::ShapedGlyphRun) -> String {
    cluster_strings(run).join("")
}

/// Return the decoded string for each distinct cluster in the run, in order.
/// Glyphs sharing a `text_range` (same cluster) produce one entry.
/// Panics on any invalid range.
fn cluster_strings(run: &fulgur::paragraph::ShapedGlyphRun) -> Vec<String> {
    let text = &run.text;
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<std::ops::Range<usize>> = None;
    for (gi, glyph) in run.glyphs.iter().enumerate() {
        let r = &glyph.text_range;
        assert!(
            r.end <= text.len(),
            "glyph {gi}: range end {} > text.len() {} in {text:?}",
            r.end,
            text.len()
        );
        assert!(
            r.start < r.end,
            "glyph {gi}: empty range {}..{} in {text:?}",
            r.start,
            r.end
        );
        assert!(
            text.is_char_boundary(r.start),
            "glyph {gi}: start {} splits a UTF-8 char in {text:?}",
            r.start
        );
        assert!(
            text.is_char_boundary(r.end),
            "glyph {gi}: end {} splits a UTF-8 char in {text:?}",
            r.end
        );
        if prev.as_ref() != Some(r) {
            out.push(text[r.clone()].to_string());
            prev = Some(r.clone());
        }
    }
    out
}

/// Simulate selecting glyphs `glyph_range` in `run` and assert the copied
/// text equals `expected`.  Union of their `text_range`s gives the selection.
fn assert_selection(
    run: &fulgur::paragraph::ShapedGlyphRun,
    glyph_range: std::ops::Range<usize>,
    expected: &str,
) {
    let glyphs = &run.glyphs[glyph_range.clone()];
    let sel_start = glyphs.iter().map(|g| g.text_range.start).min().unwrap();
    let sel_end = glyphs.iter().map(|g| g.text_range.end).max().unwrap();
    let copied = &run.text[sel_start..sel_end];
    assert_eq!(
        copied, expected,
        "selecting glyphs {}..{} copied {copied:?} instead of {expected:?}",
        glyph_range.start, glyph_range.end,
    );
}

#[test]
fn selecting_first_word_copies_only_first_word() {
    // "Hello World" — select glyphs 0..5 ("Hello"), expect "Hello" back.
    // Old bug: every glyph has range 0..11, so the union = "Hello World".
    let d = noto_engine()
        .build_drawables_for_testing_no_gcpm("<html><body><p>Hello World</p></body></html>");
    let runs = collect_text_runs(&d);
    let run = runs
        .iter()
        .find(|r| r.text.contains("Hello World"))
        .expect("no run containing 'Hello World'");
    assert_selection(run, 0..5, "Hello");
    assert_selection(run, 6..11, "World");
}

#[test]
fn each_ascii_glyph_copies_its_own_character() {
    // One glyph per ASCII character.  Copying glyph i should yield text[i].
    // Old bug: every glyph copies "Hello World" (the whole paragraph).
    let d = noto_engine()
        .build_drawables_for_testing_no_gcpm("<html><body><p>Hello World</p></body></html>");
    let runs = collect_text_runs(&d);
    let run = runs
        .iter()
        .find(|r| r.text.contains("Hello World"))
        .expect("no run containing 'Hello World'");
    let expected_chars: Vec<char> = "Hello World".chars().collect();
    for (gi, glyph) in run.glyphs.iter().enumerate() {
        let copied = &run.text[glyph.text_range.clone()];
        assert_eq!(
            copied,
            expected_chars[gi].to_string(),
            "glyph {gi} copied {copied:?}, expected {:?}",
            expected_chars[gi],
        );
    }
}

#[test]
fn each_multibyte_glyph_copies_its_own_character() {
    // é = U+00E9 (2 bytes), € = U+20AC (3 bytes).
    // Each cluster must decode to exactly its own character.
    // Old bug: all glyphs claim 0..text.len(), so cluster_strings collapses
    // to a single entry ["café €42"] instead of one entry per character.
    //
    // We concatenate cluster_strings across all runs because run.text is the
    // full paragraph string for every run — searching by text.contains() would
    // match any run in the paragraph and might miss characters covered by a
    // later run (e.g. if a font fallback splits the paragraph mid-way).
    let d = noto_engine()
        .build_drawables_for_testing_no_gcpm("<html><body><p>café €42</p></body></html>");
    let all_clusters: Vec<String> = collect_text_runs(&d)
        .iter()
        .flat_map(cluster_strings)
        .collect();
    assert_eq!(all_clusters, vec!["c", "a", "f", "é", " ", "€", "4", "2"]);
}

#[test]
fn each_line_in_multiline_paragraph_copies_only_its_own_text() {
    // "First<br>Second" — the two lines share the same run.text but their
    // glyphs must not bleed into each other's text.
    // Old bug: every glyph has range 0..len("First\nSecond"), so copying
    // any run yields the entire string instead of just that line.
    let d = noto_engine()
        .build_drawables_for_testing_no_gcpm("<html><body><p>First<br>Second</p></body></html>");
    let copied: Vec<String> = collect_text_runs(&d).iter().map(copy_run).collect();
    assert!(
        copied.contains(&"First".to_string()),
        "no run copies exactly 'First'; got {copied:?}"
    );
    assert!(
        copied.contains(&"Second".to_string()),
        "no run copies exactly 'Second'; got {copied:?}"
    );
}

#[test]
fn bold_span_copies_only_bold_text() {
    // <strong> creates a style boundary.  The run covering "bold" must copy
    // only "bold", not the whole paragraph "Normal bold text".
    let d = noto_engine().build_drawables_for_testing_no_gcpm(
        "<html><body><p>Normal <strong>bold</strong> text</p></body></html>",
    );
    let copied: Vec<String> = collect_text_runs(&d).iter().map(copy_run).collect();
    assert!(
        copied.contains(&"bold".to_string()),
        "no run copies exactly 'bold'; got {copied:?}"
    );
}

#[test]
fn list_marker_copies_correct_text() {
    // List markers are shaped by a separate code path (shape_marker_with_skrifa).
    // The body text "item" must decode one character per cluster.
    // Old bug: all glyphs claim 0..4, cluster_strings returns ["item"] not
    // ["i","t","e","m"].
    let d = noto_engine()
        .build_drawables_for_testing_no_gcpm("<html><body><ul><li>item</li></ul></body></html>");
    // Find the run that actually covers "item" by checking what it decodes to,
    // not by run.text (which is the full paragraph string for every run).
    let runs = collect_text_runs(&d);
    let run = runs
        .iter()
        .find(|r| copy_run(r) == "item")
        .expect("no run that copies exactly 'item'");
    assert_eq!(cluster_strings(run), vec!["i", "t", "e", "m"]);
}

#[test]
fn cjk_glyphs_copy_correct_characters() {
    // 你好世界 — each character is 3 UTF-8 bytes.
    // cluster_strings must return one entry per character.
    // Old bug: all glyphs claim 0..12, collapsing to ["你好世界"] instead of
    // ["你","好","世","界"].  Any byte-level split of 3-byte chars would
    // also panic on the char-boundary assertion inside cluster_strings.
    let jp_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSansJP-Regular.otf");
    let latin_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets.add_font_file(&latin_path).expect("NotoSans");
    assets.add_font_file(&jp_path).expect("NotoSansJP");
    assets.add_css("body { font-family: 'Noto Sans JP', 'Noto Sans', sans-serif; }");
    let engine = Engine::builder().assets(assets).build();
    let d = engine.build_drawables_for_testing_no_gcpm("<html><body><p>你好世界</p></body></html>");
    let all_clusters: Vec<String> = collect_text_runs(&d)
        .iter()
        .flat_map(cluster_strings)
        .collect();
    assert_eq!(all_clusters, vec!["你", "好", "世", "界"]);
}

/// Regression for the multi-GlyphRun duplication bug (fulgur-tt91): when a
/// single Parley `Run` is split across multiple `GlyphRun`s (as the
/// `counters(item, ".")` generated `::before` content here produces — one
/// GlyphRun for the counter prefix and another for the body text, both in the
/// same shaping Run), iterating `glyph_run.run().visual_clusters()` once per
/// GlyphRun emits ALL clusters of the parent Run for EACH GlyphRun, so every
/// glyph is drawn once per GlyphRun rather than once total.
///
/// With `list-style: none` the prefix comes from `::before` generated content,
/// so this fixture exercises the inline-root paragraph path
/// (`convert/inline_root.rs::extract_paragraph`). The structurally identical
/// loop also lives in `convert/mod.rs`, `paragraph.rs`, and
/// `convert/list_marker.rs`; this fixture does NOT route through those, so a
/// regression reintroduced there would slip past this test — covering them
/// needs separate fixtures and is out of scope for fulgur-tt91.
///
/// Each marker must appear exactly once. We assert on whole `pdftotext -raw`
/// lines rather than `text.matches(marker).count()`: a bare substring count is
/// unsound because shorter markers are substrings of longer ones — e.g.
/// `"2.Beta"` occurs inside the `"2.2.Beta-two"` line, so
/// `matches("2.Beta").count()` is 2 even in correct output. Full-line equality
/// also catches the duplication whether pdftotext emits the doubled glyphs as a
/// second line (`"1.Alpha"` twice → count 2) or merges them on one line
/// (`"1.Alpha1.Alpha"` → count 0); both differ from the required 1.
#[test]
fn multi_glyph_run_marker_renders_without_duplication() {
    let html = r#"<!doctype html>
<html>
<head><style>
ol { counter-reset: item; padding: 0; margin: 0; list-style: none; }
li { counter-increment: item; }
li::before { content: counters(item, ".") ". "; }
body { font-family: 'Noto Sans', sans-serif; }
</style></head>
<body>
<ol>
  <li>Alpha</li>
  <li>Beta
    <ol>
      <li>Beta-one</li>
      <li>Beta-two</li>
    </ol>
  </li>
  <li>Gamma</li>
</ol>
</body>
</html>"#;

    let pdf = noto_engine().render(html).expect("render");
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping duplication assertion");
        return;
    };

    // pdftotext -raw flattens each marker + its body label onto its own line.
    // Strip all whitespace per line so the comparison is robust against
    // platform-specific spacing extraction: the marker content is `"N. "` with
    // a trailing space, and some Poppler versions reconstruct it as "1. Alpha"
    // rather than the "1.Alpha" seen here. Whole-(stripped-)line equality keeps
    // the substring-safety property ("2.Beta" never equals "2.2.Beta-two").
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.chars().filter(|c| !c.is_whitespace()).collect::<String>())
        .filter(|l| !l.is_empty())
        .collect();

    for marker in [
        "1.Alpha",
        "2.Beta",
        "2.1.Beta-one",
        "2.2.Beta-two",
        "3.Gamma",
    ] {
        let occurrences = lines.iter().filter(|l| l.as_str() == marker).count();
        assert_eq!(
            occurrences, 1,
            "marker {marker:?} appeared on {occurrences} line(s), expected 1 — \
             multi-GlyphRun duplication regression; extracted lines: {lines:?}"
        );
    }
}

/// Exercises `clear_subtree_cache` for codecov coverage.
///
/// The real regression guard for this bug (stale Taffy cache hits in multicol)
/// is the VRT golden at `crates/fulgur-vrt/fixtures/layout/multicol-table-width.html`.
/// The `build_drawables_for_testing_no_gcpm` path does not reproduce the width
/// overflow (the stale-hit requires the full blitz initial layout + GCPM path),
/// so this test only verifies that multicol+table rendering completes without
/// panic and produces drawables.
#[test]
fn multicol_table_with_text_content_renders() {
    // Matches the VRT fixture layout/multicol-table-width.html.
    let html = r#"<!DOCTYPE html>
<html><head><style>
  @page { size: A4; margin: 12mm; }
  body { font-family: sans-serif; font-size: 10pt; }
  .cols { column-count: 2; column-gap: 10mm; }
  .panel { break-inside: avoid; margin-bottom: 8px; }
  table { width: 100%; border-collapse: collapse; }
  th, td { border: 1px solid #333; padding: 2px 4px; }
  th.title { background: #cfe0f0; }
  td.name { width: 40%; }
  td.ref  { width: 22%; }
  td.val  { width: 20%; text-align: right; }
  td.judg { width: 18%; }
</style></head><body>
<div class="cols">
  <div class="panel"><table>
    <tr><th class="title" colspan="4">Panel A</th></tr>
    <tr><td class="name">height</td><td class="ref"></td><td class="val">169.2</td><td class="judg"></td></tr>
    <tr><td class="name">BMI</td><td class="ref">18.5-24.9</td><td class="val">23.1</td><td class="judg">A</td></tr>
  </table></div>
  <div class="panel"><table>
    <tr><th class="title" colspan="4">Panel B</th></tr>
    <tr><td class="name">WBC</td><td class="ref">39-98</td><td class="val">56.0</td><td class="judg"></td></tr>
    <tr><td class="name">RBC</td><td class="ref">427-570</td><td class="val">509</td><td class="judg"></td></tr>
  </table></div>
</div>
</body></html>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render(html).expect("render must not fail");
    assert!(!pdf.is_empty());
}

/// fulgur-78o: a `<table>` with a `<caption>` must render the caption's
/// text in the output PDF. Blitz drops `<caption>` during table box
/// construction (layout/table.rs "Probably a table caption: ignore"),
/// so without fulgur's caption restructure pass the text is silently
/// missing. See docs/plans/2026-06-17-table-caption-redesign.md.
#[test]
fn table_caption_text_renders_in_pdf() {
    let html = r#"<!doctype html><html><body>
        <table>
          <caption>CAPTIONWORD</caption>
          <tbody><tr><td>cellone</td><td>celltwo</td></tr></tbody>
        </table>
    </body></html>"#;
    let pdf = noto_engine().render(html).expect("render must succeed");
    assert!(!pdf.is_empty());

    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    assert!(
        text.contains("CAPTIONWORD"),
        "caption text must render in the PDF; got: {text:?}"
    );
}

/// fulgur-78o caption-side fixture. The caption uses a distinctly larger
/// font so its text item can be told apart from the cell text items in
/// `inspect` output (which reports glyph IDs, not Unicode, so we cannot
/// match on text). `inspect` y-coordinates are PDF user space (y-up:
/// larger y is higher on the page). Returns `(caption_y, min_cell_y,
/// max_cell_y)`.
fn caption_and_cell_ys(caption_side: &str) -> (f32, f32, f32) {
    use fulgur::inspect::inspect;

    let html = format!(
        r#"<!doctype html><html><head><style>
          caption {{ caption-side: {caption_side}; font-size: 28px; }}
          td {{ font-size: 12px; padding: 4px; }}
        </style></head><body>
          <table>
            <caption>CAP</caption>
            <tbody>
              <tr><td>rowone</td></tr>
              <tr><td>rowtwo</td></tr>
              <tr><td>rowthree</td></tr>
            </tbody>
          </table>
        </body></html>"#
    );
    let pdf = noto_engine().render(&html).expect("render must succeed");
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("caption-side.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");
    let inspected = inspect(&path).expect("inspect pdf");

    // The caption renders at 28px → 21pt; cells at 12px → 9pt. Split on a
    // threshold between the two.
    let caption_y = inspected
        .text_items
        .iter()
        .find(|t| t.font_size > 15.0)
        .map(|t| t.y)
        .expect("caption text item must render");
    let cell_ys: Vec<f32> = inspected
        .text_items
        .iter()
        .filter(|t| t.font_size <= 15.0)
        .map(|t| t.y)
        .collect();
    assert!(!cell_ys.is_empty(), "cell text items must render");
    let min_cell = cell_ys.iter().copied().fold(f32::INFINITY, f32::min);
    let max_cell = cell_ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    (caption_y, min_cell, max_cell)
}

/// fulgur-78o: `caption-side: top` (default) places the caption above the
/// table body — its y must be higher than every cell (y-up coordinates).
#[test]
fn table_caption_side_top_renders_above_table() {
    let (caption_y, _min_cell, max_cell) = caption_and_cell_ys("top");
    assert!(
        caption_y > max_cell,
        "caption-side:top — caption (y={caption_y}) must sit above all cells (max cell y={max_cell})"
    );
}

/// fulgur-78o: `caption-side: bottom` places the caption below the table
/// body — its y must be lower than every cell (y-up coordinates).
#[test]
fn table_caption_side_bottom_renders_below_table() {
    let (caption_y, min_cell, _max_cell) = caption_and_cell_ys("bottom");
    assert!(
        caption_y < min_cell,
        "caption-side:bottom — caption (y={caption_y}) must sit below all cells (min cell y={min_cell})"
    );
}

/// fulgur-78o: a caption with nested inline markup renders its text. The
/// restructured caption is an ordinary block flow root, so inline shaping
/// applies to its descendants just like any block.
#[test]
fn table_caption_with_nested_inline_renders() {
    let html = r#"<!doctype html><html><body>
        <table>
          <caption>pre <strong>BOLDCAPWORD</strong> post</caption>
          <tbody><tr><td>cellone</td></tr></tbody>
        </table>
    </body></html>"#;
    let pdf = noto_engine().render(html).expect("render must succeed");
    let Some(text) = extract_pdf_text(&pdf) else {
        eprintln!("pdftotext not available; skipping text assertion");
        return;
    };
    assert!(
        text.contains("BOLDCAPWORD"),
        "nested caption text must render; got: {text:?}"
    );
}

/// fulgur-78o: a table with more than one `<caption>` must not panic.
/// Only the first caption is restructured (per scope); the render must
/// still succeed.
#[test]
fn table_with_two_captions_does_not_panic() {
    let html = r#"<!doctype html><html><body>
        <table>
          <caption>FIRSTCAP</caption>
          <caption>SECONDCAP</caption>
          <tbody><tr><td>cellone</td></tr></tbody>
        </table>
    </body></html>"#;
    let pdf = noto_engine().render(html).expect("render must succeed");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

/// Helper: number of large-font (caption-sized) text items rendered for a
/// captioned table whose `<style>` block is `extra_css`. The caption uses a
/// 28px font so it is distinguishable from the 12px cells. fulgur-uoao.
fn caption_big_text_count(extra_css: &str) -> usize {
    use fulgur::inspect::inspect;
    let html = format!(
        r#"<!doctype html><html><head><style>
          caption {{ font-size: 28px; }}
          td {{ font-size: 12px; }}
          {extra_css}
        </style></head><body>
          <table><caption>CAP</caption>
            <tbody><tr><td>cellone</td></tr></tbody>
          </table>
        </body></html>"#
    );
    let pdf = noto_engine().render(&html).expect("render must succeed");
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("caption-display.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");
    let inspected = inspect(&path).expect("inspect pdf");
    inspected
        .text_items
        .iter()
        .filter(|t| t.font_size > 15.0)
        .count()
}

/// fulgur-uoao: an author `caption { display: none }` must keep the caption
/// out of the PDF. The restructure pass used to force inline `display: block`
/// unconditionally, overriding the cascade and leaking hidden caption text.
#[test]
fn table_caption_display_none_is_not_rendered() {
    assert_eq!(
        caption_big_text_count("caption { display: none; }"),
        0,
        "a display:none caption must not render"
    );
    // Sanity: the same fixture without the override does render the caption.
    assert_eq!(
        caption_big_text_count(""),
        1,
        "a visible caption must render exactly one large-font item"
    );
}

/// fulgur-uoao: a hidden table (`table { display: none }`) must not leak its
/// caption into the output. Restructuring would otherwise move the caption
/// into a visible wrapper while the table stayed suppressed.
#[test]
fn table_caption_hidden_table_does_not_leak_caption() {
    assert_eq!(
        caption_big_text_count("table { display: none; }"),
        0,
        "a caption under a display:none table must stay hidden"
    );
}

/// fulgur-uoao: a `visibility: hidden` table must not leak its caption.
/// fulgur already suppresses the table's cells; without skipping the
/// restructure, the moved caption re-inherits `visible` from the wrapper and
/// renders alone (an inconsistent half-hidden table). Codex review on #487.
#[test]
fn table_caption_visibility_hidden_table_does_not_leak_caption() {
    assert_eq!(
        caption_big_text_count("table { visibility: hidden; }"),
        0,
        "a caption under a visibility:hidden table must stay hidden"
    );
}

/// fulgur-uoao: a caption hidden via `visibility: hidden` must not render —
/// including a descendant selector like `table > caption` that only matches
/// before the restructure moves the caption out of the table. The pass reads
/// the cascade pre-move, so the rule is honoured. coderabbit review on #487.
#[test]
fn table_caption_visibility_hidden_caption_does_not_leak() {
    assert_eq!(
        caption_big_text_count("caption { visibility: hidden; }"),
        0,
        "a visibility:hidden caption (element selector) must stay hidden"
    );
    assert_eq!(
        caption_big_text_count("table > caption { visibility: hidden; }"),
        0,
        "a visibility:hidden caption (descendant selector, broken by the move) \
         must still stay hidden because the cascade is read pre-restructure"
    );
}

/// fulgur-uoao: `caption-side` only applies to `display: table-caption`.
/// When an author overrides the caption to `display: block`, `caption-side`
/// must not apply and the caption keeps source order (above the table). The
/// 28px caption sits above the 12px cells. Codex review on #487.
#[test]
fn table_caption_side_ignored_for_non_table_caption_display() {
    use fulgur::inspect::inspect;
    let html = r#"<!doctype html><html><head><style>
          caption { display: block; caption-side: bottom; font-size: 28px; }
          td { font-size: 12px; }
        </style></head><body>
          <table><caption>CAP</caption>
            <tbody><tr><td>cellone</td></tr><tr><td>celltwo</td></tr></tbody>
          </table>
        </body></html>"#;
    let pdf = noto_engine().render(html).expect("render must succeed");
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("caption-block-bottom.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");
    let inspected = inspect(&path).expect("inspect pdf");
    let caption_y = inspected
        .text_items
        .iter()
        .find(|t| t.font_size > 15.0)
        .map(|t| t.y)
        .expect("caption text item must render");
    let max_cell_y = inspected
        .text_items
        .iter()
        .filter(|t| t.font_size <= 15.0)
        .map(|t| t.y)
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(
        caption_y > max_cell_y,
        "display:block caption ignores caption-side:bottom and stays above the table \
         (caption y={caption_y}, max cell y={max_cell_y})"
    );
}

/// fulgur-78o: `caption-side` must be honored even when supplied through
/// engine-injected CSS (`AssetBundle::add_css`) rather than a document
/// `<style>`. The caption restructure pass's pre-resolve must run after
/// engine CSS is injected into the stylist, otherwise `caption-side`
/// silently defaults to top.
#[test]
fn table_caption_side_bottom_via_injected_css() {
    use fulgur::inspect::inspect;

    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets.add_font_file(&font_path).expect("load font");
    assets.add_css("body { font-family: 'Noto Sans', sans-serif; }");
    assets.add_css(
        "caption { caption-side: bottom; font-size: 28px; } td { font-size: 12px; padding: 4px; }",
    );
    let engine = Engine::builder().assets(assets).build();

    let html = r#"<!doctype html><html><body>
        <table>
          <caption>CAP</caption>
          <tbody>
            <tr><td>rowone</td></tr>
            <tr><td>rowtwo</td></tr>
          </tbody>
        </table>
    </body></html>"#;
    let pdf = engine.render(html).expect("render must succeed");
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("caption-injected.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");
    let inspected = inspect(&path).expect("inspect pdf");

    let caption_y = inspected
        .text_items
        .iter()
        .find(|t| t.font_size > 15.0)
        .map(|t| t.y)
        .expect("caption text item must render");
    let min_cell = inspected
        .text_items
        .iter()
        .filter(|t| t.font_size <= 15.0)
        .map(|t| t.y)
        .fold(f32::INFINITY, f32::min);
    assert!(
        caption_y < min_cell,
        "caption-side:bottom via injected CSS — caption (y={caption_y}) must sit below all cells (min cell y={min_cell})"
    );
}

/// Helper: max cell-text x position for a `width:100%` two-column table,
/// optionally with a caption. Used to prove the caption restructure
/// wrapper does not shrink the table away from full width (the primary
/// SaaS report-table use case). fulgur-78o.
fn full_width_table_max_cell_x(with_caption: bool) -> f32 {
    use fulgur::inspect::inspect;
    let caption = if with_caption {
        "<caption>CAP</caption>"
    } else {
        ""
    };
    let html = format!(
        r#"<!doctype html><html><head><style>
          td {{ font-size: 12px; padding: 4px; }}
        </style></head><body>
          <table style="width:100%">{caption}
            <tbody><tr><td>leftcell</td><td>rightcell</td></tr></tbody>
          </table>
        </body></html>"#
    );
    let pdf = noto_engine().render(&html).expect("render must succeed");
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("fullwidth.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");
    let inspected = inspect(&path).expect("inspect pdf");
    inspected
        .text_items
        .iter()
        .filter(|t| t.font_size <= 15.0)
        .map(|t| t.x)
        .fold(f32::NEG_INFINITY, f32::max)
}

/// fulgur-78o: a `width:100%` table keeps its full width when it has a
/// caption — the `fit-content` wrapper must not shrink-wrap it. We assert
/// the second column's x is essentially unchanged by adding a caption.
#[test]
fn table_caption_preserves_full_width_table() {
    let without = full_width_table_max_cell_x(false);
    let with = full_width_table_max_cell_x(true);
    assert!(
        (without - with).abs() < 1.0,
        "caption must not change table width: max cell x without={without}, with={with}"
    );
}

/// fulgur-2m6w (DoS guard): a tall CSS height that slices across many pages
/// must drive the multi-page slice render path end-to-end without hanging.
/// This exercises `render.rs`'s per-slice draw loop on a sliced body child;
/// a moderate height (~tens of pages) keeps the render fast — the page CAP
/// itself (the `99999999px` / `f32::MAX` amplification bound) is covered by
/// the fast pagination unit tests in `pagination_layout.rs`, which need no
/// render. Rendering the full 10k-page ceiling here would just be a slow
/// CI tax for no extra coverage.
#[test]
fn pathological_tall_block_render_is_bounded() {
    let html = r#"<!doctype html><html><body><div style="height:50000px"></div></body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("render must terminate and succeed");
    assert!(!pdf.is_empty(), "expected a non-empty bounded PDF");
}

/// fulgur-ezst: a childless block tall enough to exceed the page cap must
/// collapse end-to-end — a real `render` yields a tiny, single-page
/// PDF rather than a ~10k-page one. Byte size cleanly separates the two: a
/// collapsed render is a few KB (measured ~1.5 KB); the uncollapsed cap
/// render would be MBs. The `< 500_000` threshold sits far below the
/// uncollapsed size yet well above the collapsed one.
#[test]
fn childless_cap_exceeding_block_collapses_end_to_end() {
    let html = r#"<!doctype html><html><body><div style="height:99999999px"></div></body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("render must terminate and succeed");
    assert!(!pdf.is_empty(), "expected a non-empty PDF");
    assert!(
        pdf.len() < 500_000,
        "collapsed render must be small (single page); got {} bytes",
        pdf.len(),
    );
}

/// fulgur-c8re (security): a tall CHILDLESS *replaced* element that paints
/// nothing — an unresolved `src` (the common offline-first case, no matching
/// `AssetBundle` entry), a `visibility:hidden` image, or an empty `<svg>` —
/// must collapse end-to-end just like the blank `<div>` above, not amplify
/// into ~10k blank pages. Guards the full `render.rs` per-page-allocation
/// path the DoS finding exploited; the pagination unit test only checks
/// `implied_page_count`. A tag-only replaced-element exception previously
/// disabled the collapse here — uncollapsed, this 79-byte input rendered
/// ~10k pages / ~3 MB; collapsed it is a single ~KB page.
#[test]
fn replaced_childless_cap_exceeding_block_collapses_end_to_end() {
    let html = r#"<!doctype html><html><body><img src="missing.png" style="display:block;width:1px;height:99999999px"></body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("render must terminate and succeed");
    assert!(pdf.starts_with(b"%PDF"), "expected a valid PDF");
    assert_eq!(
        page_count(&pdf),
        1,
        "a non-painting tall replaced element must collapse to a single page",
    );
    assert!(
        pdf.len() < 500_000,
        "collapsed render must be small (single page); got {} bytes",
        pdf.len(),
    );
}

/// fulgur-2qpt: deprecated aliases `render_html` and `render_html_to_file`
/// must delegate to the renamed methods and produce valid PDFs.
#[test]
#[allow(deprecated)]
fn deprecated_render_html_alias_delegates() {
    let html = r#"<!doctype html><html><body><p>compat</p></body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render_html must succeed");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
#[allow(deprecated)]
fn deprecated_render_html_to_file_alias_delegates() {
    let html = r#"<!doctype html><html><body><p>compat</p></body></html>"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("compat.pdf");
    Engine::builder()
        .build()
        .render_html_to_file(html, &path)
        .expect("render_html_to_file must succeed");
    assert!(path.exists());
    assert!(std::fs::metadata(&path).unwrap().len() > 0);
}

/// fulgur-2map.2: the multicol column-rule pt carrier (units::Pt) renders
/// end-to-end through record_multicol_rule + paint_multicol_rule_for_page.
#[test]
fn multicol_column_rule_renders() {
    let html = r#"<!doctype html><html><body>
      <div style="column-count:3; column-gap:20px; column-rule:2px solid #333;">
        <p>alpha beta gamma delta epsilon zeta eta theta iota kappa
        lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega</p>
      </div>
    </body></html>"#;
    let pdf = fulgur::Engine::builder()
        .build()
        .render(html)
        .expect("render multicol column-rule");
    assert!(!pdf.is_empty());
}

// ===== P1a (fulgur-2map.3) byte-neutral units::Pt migration coverage =====
// These end-to-end smoke tests drive the three migration lines that only the
// VRT suite exercised (and so were missing from the non-VRT codecov patch
// measurement): `background.rs:554` (`compute_inner_radii` call in
// `draw_background_layer`), `convert/inline_root.rs:197-198` (the pseudo-only
// inline-root `Size { width: width.pt(), height: height.pt() }` producer), and
// `convert/list_item.rs:203-204` (the empty-`<li>` inside-marker `Size`
// producer). See CLAUDE.md "Coverage scope" Gotcha.

/// 1x1 raster PNG used as a `content: url(...)` / `list-style-image` asset so
/// the pseudo-image and inside-image-marker paths resolve deterministically
/// without depending on system fonts.
fn smoke_tiny_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

#[test]
fn background_image_layer_with_border_radius_inner_radii_renders() {
    // Covers `background.rs:554` — the `compute_inner_radii(&style.border_radii
    // .map(Pt::to_f32), ..)` call inside `draw_background_layer`'s
    // `style.has_radius()` arm. Only reached when a background *image* layer
    // (here a linear gradient) co-occurs with a non-zero `border-radius`;
    // existing gradient smoke tests lacked the radius. Border + padding give
    // the inner-radii insets a non-trivial value.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:120pt;height:60pt;background:linear-gradient(to right,#f00,#00f);border:4pt solid #333;border-radius:14pt;padding:6pt;">grad rounded</div>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn inline_root_block_wrapper_pseudo_only_size_renders() {
    // Covers `convert/inline_root.rs:196-199` — the second `Size { width:
    // width.pt(), height: height.pt() }` producer in the `else if before_inline
    // .is_some() || after_inline.is_some()` arm (no real text, so
    // `extract_paragraph` returns `None`). An `display:inline-block` span with
    // an inline `::before` image and a visual block style (background + border)
    // makes `needs_block` true while keeping the paragraph empty, hitting the
    // pseudo-only block-wrapper path. A plain `inline` span instead routes
    // through the first (already-covered) producer.
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", smoke_tiny_png());
    bundle.add_css(
        r#".px{display:inline-block;background:#eee;border:2pt solid #333;}
.px::before{content:url("dot.png");}"#,
    );
    let html = r#"<!DOCTYPE html><html><body><p>x <span class="px"></span> y</p></body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn list_item_inside_marker_empty_li_block_size_renders() {
    // Covers `convert/list_item.rs:202-205` — the `Size` producer in the
    // empty-`<li>` (`children.is_empty()`) arm of the inside-positioned-marker
    // branch. The marker must *resolve* for the producer to run: with a
    // bundled `list-style-image` the inside-image marker resolves, unlike the
    // disc-glyph path which yields `None` without a bundled font (that is why
    // `render_v2_smoke_empty_li_inside_position_marker_paragraph` does not
    // reach this line).
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", smoke_tiny_png());
    bundle.add_css(r#"ul{list-style-position:inside;list-style-image:url("dot.png");}"#);
    let html = r#"<!DOCTYPE html><html><body><ul><li></li><li>second</li></ul></body></html>"#;
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(html)
        .expect("v2 render");
    assert!(!pdf.is_empty());
}

#[test]
fn border_radius_px_and_percent_resolve_radius_renders() {
    // coderabbit follow-up for P1a: exercise `convert/style/border.rs`'s
    // `resolve_radius` closure (`.px().in_pt()`) through both a percentage
    // basis (`50%`, which depends on the border-box size) and an absolute
    // length (`8px`), guarding against a 4/3x px<->pt regression. Backgrounds
    // make the rounded fills actually paint.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:80pt;height:40pt;background:#cef;border-radius:50%;"></div>
        <div style="width:80pt;height:40pt;background:#fce;border-radius:8px;"></div>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("v2 render");
    assert!(!pdf.is_empty());
}

/// fulgur-5biq: Engine is Send + Sync — can be shared across threads.
///
/// If `Engine` ever stops being `Send + Sync` this test will fail to compile.
#[test]
fn engine_is_send_and_sync() {
    let engine = fulgur::Engine::builder().build();
    std::thread::scope(|s| {
        s.spawn(|| {
            let pdf = engine
                .render("<!doctype html><html><body><p>ok</p></body></html>")
                .expect("render in spawned thread");
            assert!(pdf.starts_with(b"%PDF"));
        });
    });
}

/// fulgur-5biq: render_batch returns one valid PDF per input.
///
/// Output length equals input length and each element is a valid PDF.
/// Ordering is guaranteed by rayon's `IndexedParallelIterator`: `par_iter()`
/// on a slice is indexed, so `.collect::<Vec<_>>()` preserves the original order.
#[test]
fn render_batch_returns_one_pdf_per_input() {
    let htmls = [
        r#"<!doctype html><html><body><p>first</p></body></html>"#,
        r#"<!doctype html><html><body><p>second</p></body></html>"#,
        r#"<!doctype html><html><body><p>third</p></body></html>"#,
    ];
    let results = Engine::builder().build().render_batch(&htmls);
    assert_eq!(results.len(), htmls.len());
    for (i, result) in results.iter().enumerate() {
        let pdf = result
            .as_ref()
            .unwrap_or_else(|e| panic!("item {i} failed: {e}"));
        assert!(!pdf.is_empty(), "item {i} produced empty PDF");
        assert!(pdf.starts_with(b"%PDF"), "item {i} not a PDF");
    }
}

/// fulgur-5biq: a failing item in render_batch does not abort its siblings.
/// pdf_ua(true) without a document title reliably triggers Err (NoDocumentTitle).
/// A batch mixing titled and untitled HTML must return Ok for the titled item
/// even when adjacent items fail.
#[test]
fn render_batch_failure_does_not_abort_siblings() {
    let engine = Engine::builder().pdf_ua(true).lang("en").build();
    // item 0: no <title> → Err (NoDocumentTitle)
    // item 1: has <title>  → Ok
    // item 2: no <title> → Err
    let htmls = [
        "<!doctype html><html lang=\"en\"><body><h1>no title</h1></body></html>",
        "<!doctype html><html lang=\"en\"><head><title>T</title></head><body><h1>Heading</h1><p>ok</p></body></html>",
        "<!doctype html><html lang=\"en\"><body><h1>no title either</h1></body></html>",
    ];
    let results = engine.render_batch(&htmls);
    assert_eq!(results.len(), 3);
    assert!(results[0].is_err(), "item 0 (no title) should fail");
    assert!(
        results[1].is_ok(),
        "item 1 (has title) should succeed, got: {:?}",
        results[1]
    );
    assert!(results[2].is_err(), "item 2 (no title) should fail");
}

// --- P1c (fulgur-2map.5) Fragment->units::Px coverage closure ---
//
// These four tests drive `fragment_block_subtree`'s break-after arms
// and `draw_under_opacity`'s split-height path through the full
// `Engine::render` pipeline. Those Fragment construction sites were
// previously exercised only by VRT goldens (excluded from the codecov
// measurement), so the `.px()` / `frag.height.in_pt()` lines showed as
// uncovered patch lines. See CLAUDE.md "Coverage scope". (The
// `depth >= MAX_DOM_DEPTH` bail-out is covered by an in-crate unit test
// in `pagination_layout.rs`, which can call the guard directly.)

/// fulgur-2map.5: an opacity-scoped block with block-level
/// descendants (non-empty `opacity_descendants`) that itself SPLITS
/// across a page boundary must reach `draw_under_opacity`'s
/// `is_split && frag.height > Px::ZERO` arm (render.rs ~2065). The
/// pre-existing split-opacity smoke test wraps an atomic `<svg>`
/// (shared-node content) which keeps the block a single fragment, so
/// `is_split` stays false there. Two tall block children guarantee a
/// real multi-fragment split.
#[test]
fn render_v2_smoke_opacity_block_split_uses_per_slice_height_block_children() {
    let html = r#"<!doctype html><html><body>
        <div style="opacity:0.5;background:#f0f4ff;border:2px solid #89c">
          <div style="height:600px;background:#eef">first block child</div>
          <div style="height:600px;background:#fce">second block child</div>
        </div>
    </body></html>"#;
    assert_paginates(html, "expected multi-page split");
}

/// fulgur-2map.5: a nested block parent whose child carries
/// `break-after: page` (a simple leaf child - no recursion) emits a
/// parent Fragment via `fragment_block_subtree`'s post-fragment
/// break-after arm (pagination_layout.rs ~2162). The parent is routed
/// through `fragment_block_subtree` because it has a forced break
/// below.
#[test]
fn render_v2_smoke_break_after_child_no_recursion_emits_parent_fragment() {
    let html = r#"<!doctype html><html><body>
        <div style="background:#eef">
          <div style="height:80px">before the forced break</div>
          <div style="break-after:page;height:80px">forces a page break after</div>
          <div style="height:80px">after the forced break</div>
        </div>
    </body></html>"#;
    assert_paginates(html, "break-after:page must paginate");
}

/// fulgur-2map.5: a zero-height child (explicit `height:0`, no
/// content) carrying `break-after: page` inside a nested block parent
/// hits `fragment_block_subtree`'s zero-height break-after arm
/// (pagination_layout.rs ~1835). The explicit `height:0` keeps the
/// child out of the positive-height path (~2162).
#[test]
fn render_v2_smoke_break_after_zero_height_child_emits_parent_fragment() {
    let html = r#"<!doctype html><html><body>
        <div style="background:#eef">
          <div style="height:80px">before</div>
          <div style="break-after:page;height:0"></div>
          <div style="height:80px">after</div>
        </div>
    </body></html>"#;
    assert_paginates(html, "zero-height break-after must paginate");
}

/// fulgur-2map.5: a child that itself recurses (two tall grandchildren
/// -> `would_split` true) AND carries `break-after: page` hits
/// `fragment_block_subtree`'s post-recursion break-after arm
/// (pagination_layout.rs ~2062).
#[test]
fn render_v2_smoke_break_after_recursing_child_emits_parent_fragment() {
    let html = r#"<!doctype html><html><body>
        <div style="background:#eef">
          <div style="height:80px">before</div>
          <div style="break-after:page;background:#dfe">
            <div style="height:600px">tall grandchild one</div>
            <div style="height:600px">tall grandchild two</div>
          </div>
          <div style="height:80px">after</div>
        </div>
    </body></html>"#;
    assert_paginates(html, "recursing break-after must paginate");
}

/// Smoke: public `Engine::layout()` single-pass path. A plain document
/// (no `target-*`) takes the `!needs_pass_two` early return. Asserts the
/// renderer-agnostic layout output carries drawables + geometry, so an
/// out-of-core consumer (image / OCR) has something to compose.
#[test]
fn layout_single_pass_returns_drawables_and_geometry() {
    let out = Engine::builder()
        .build()
        .layout("<html><body><p>hello layout</p></body></html>")
        .expect("layout single-pass");
    assert!(!out.geometry.is_empty(), "geometry should record fragments");
    assert!(
        !out.drawables.paragraphs.is_empty(),
        "the <p> should produce a paragraph draw payload"
    );
}

/// Smoke: public `Engine::layout()` 2-pass path. A `target-counter()` in
/// `::after` forces `needs_pass_two`, so `layout()` falls through the early
/// return and re-lays out with the pass-1 `AnchorMap`. Covers the else-arm
/// of the branch for codecov/patch (the single-pass test covers the `return`).
#[test]
fn layout_two_pass_target_counter_returns_drawables_and_geometry() {
    let html = r##"<!doctype html>
<html><head><style>
  a::after { content: " (p." target-counter(attr(href), page) ")"; }
  h2 { page-break-before: always; }
</style></head>
<body>
  <a href="#a">Chapter A</a>
  <h2 id="a">Chapter A</h2>
  <p>aaa</p>
</body></html>"##;
    let out = Engine::builder()
        .build()
        .layout(html)
        .expect("layout 2-pass");
    assert!(!out.geometry.is_empty(), "geometry should record fragments");
    assert!(
        !out.drawables.paragraphs.is_empty(),
        "the paragraphs should produce draw payloads after 2-pass layout"
    );

    // Pass-2-SPECIFIC guard: `target-counter(attr(href), page)` in the
    // `a::after` resolves against the pass-1 `AnchorMap`. The `#a` heading
    // lands on page 2 (`page-break-before: always`), so the resolved pseudo
    // content is " (p.2)". On a regression to single-pass this stays a
    // fixed-width placeholder (never the literal resolved page number), so
    // this assert fails — which the non-empty checks above cannot detect.
    // Reads the pre-raster run text (`ShapedGlyphRun::text`), so it is
    // font-independent (no glyph inspection).
    let resolved = out
        .drawables
        .paragraphs
        .values()
        .any(|p| paragraph_run_text(p).contains("(p.2)"));
    assert!(
        resolved,
        "pass-2 target-counter should resolve to the real page number \"(p.2)\"; \
         got paragraph texts: {:?}",
        out.drawables
            .paragraphs
            .values()
            .map(paragraph_run_text)
            .collect::<Vec<_>>()
    );
}

/// End-to-end coverage for the `counters(name, sep, style)` `::before`
/// resolution path (added in the GCPM counters() work) *and* a regression
/// guard for the DoS hardening around it: many sibling `counter-reset`
/// elements each emitting `counters()` with a non-trivial separator must
/// render to a bounded, non-empty PDF without hanging or exhausting memory.
///
/// Sibling resets share the parent scope, so the chain grows with the sibling
/// index and the generated pseudo content would blow up quadratically without
/// the `MAX_COUNTER_CHAIN_BYTES` / `MAX_GENERATED_CSS_BYTES` caps. The pure
/// boundedness invariants are unit-tested in the `fulgur` lib; this drives the
/// whole `Engine::render_html` pipeline (convert → paginate → draw → PDF) so
/// the counters() draw path is attributed in codecov (see CLAUDE.md
/// "Coverage scope").
#[test]
fn test_render_counters_sibling_reset_is_bounded_smoke() {
    const SIBLINGS: usize = 40;
    let separator = "-".repeat(64);
    let mut html = String::from(
        "<!doctype html><html><head><style>\
         div{counter-reset:x}\
         div::before{content:counters(x,\"",
    );
    html.push_str(&separator);
    html.push_str("\")}</style></head><body>");
    for _ in 0..SIBLINGS {
        html.push_str("<div></div>");
    }
    html.push_str("</body></html>");

    let pdf = Engine::builder()
        .build()
        .render(&html)
        .expect("counters() sibling-reset render should not panic or hang");
    assert!(!pdf.is_empty(), "PDF should be non-empty");
    // Sanity: input-proportional output, nowhere near the amplification a
    // quadratic blowup would produce.
    assert!(
        page_count(&pdf) < 1000,
        "page count {} unexpectedly large for {SIBLINGS} siblings",
        page_count(&pdf)
    );
}

/// Regression guard: a non-semantic CSS list item (`display:list-item` on a
/// non-`<li>` element) that contains a hyperlink used to panic under tagged
/// output. `drawables.list_items` is populated for CSS list-item behaviour,
/// including non-`<li>` elements, but `li_lbody_ids` is only populated for
/// semantic `<li>` (`PdfTag::Li`), so `draw_list_item_with_block`'s per-run
/// tagging path hit `.expect("list-item paragraph must have an LBody id")`
/// on the missing LBody entry. `<div style="display:list-item"><a href>`
/// with tagging enabled reproduced a hard panic (CLI exit 101); default
/// (untagged) output rendered fine. The fix falls back to the node's own id
/// (matching the plain block-paragraph link-run path) when there is no LBody
/// remap for the node.
#[test]
fn test_tagged_css_list_item_with_link_does_not_panic() {
    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(
            "<html><body>\
             <div style=\"display:list-item; margin-left:40px\">\
             <a href=\"https://example.com\">x</a></div>\
             </body></html>",
        )
        .expect("tagged CSS list-item containing a link must not panic");
    assert!(!pdf.is_empty(), "PDF should be non-empty");
}

/// Guard for every per-run tagging sink: a link run whose owning element has
/// no `SemanticEntry` must not be wired into the struct tree. `classify_element`
/// returns `None` for `<a>`, custom elements and `<body>`, so a link-bearing
/// element that is itself the paragraph / inline-root node (rather than a child
/// of a `<p>` / `<div>`) has neither an LBody id nor a semantic entry. Wiring
/// the link span under such a node marks it "wired" while its content never
/// enters the tag tree, so Krilla panics in `serialize` ("annotation identifier
/// ... doesn't appear in the tag tree"). Every rendering path — `dispatch_fragment`
/// block / paragraph, multicol slices, `draw_under_clip`, list item — routes the
/// per-run node through `run_tag_target`, which returns `None` for these and
/// keeps the link annotation untagged. Reported by Codex Review on #592 across
/// two rounds (list item, then the clipped / block paths).
#[test]
fn test_tagged_semanticless_link_runs_do_not_panic() {
    // Each body is a link-bearing element with no semantic entry that is itself
    // the paragraph node, exercising a distinct per-run tagging sink.
    let bodies = [
        // `<a>` that is itself a CSS list item (list-item path).
        r#"<a href="https://example.com" style="display:list-item; margin-left:40px">x</a>"#,
        // Custom element list item wrapping a link.
        r#"<x-foo style="display:list-item"><a href="https://example.com">x</a></x-foo>"#,
        // Clipped CSS list item (draw_under_clip).
        r#"<a href="https://example.com" style="display:list-item; overflow:hidden">x</a>"#,
        // Block-level `<a>` (block-with-inner-content path).
        r#"<a href="https://example.com" style="display:block">x</a>"#,
        // Clipped block-level `<a>` (draw_under_clip block path).
        r#"<a href="https://example.com" style="display:block; overflow:hidden">x</a>"#,
        // Plain inline `<a>` as the sole body content (paragraph path).
        r#"<a href="https://example.com">x</a>"#,
    ];
    for body in bodies {
        let pdf = Engine::builder()
            .tagged(true)
            .build()
            .render(&format!("<html><body>{body}</body></html>"))
            .unwrap_or_else(|e| panic!("tagged render must not fail for {body:?}: {e:?}"));
        assert!(!pdf.is_empty(), "PDF should be non-empty for {body:?}");
    }
}

/// Headings and `<img alt>` values with `visibility: hidden` (or
/// `collapse`) must not leak their text into the tagged PDF struct tree
/// via `Tag::Hn`'s `/T` (Title) attribute or `Tag::Figure`'s `/Alt`
/// attribute. CSS 2 §11.2 + WAI-ARIA specify that `visibility: hidden`
/// excludes the subtree from the accessibility tree (Chromium / Firefox
/// agreement); tagged PDF is the paged counterpart of that a11y tree so
/// the same subtree exclusion applies. `pdf_tag_to_krilla_tag` carries
/// exactly two free-text attributes (heading title, figure alt) — every
/// other `PdfTag` variant is an enum or nothing — so gating these two
/// closes the class completely.
///
/// Covers all three sinks:
/// - `try_start_tagged` H arm (heading `/T`, normal paragraph tagging)
/// - `build_struct_tree` per-run backfill (heading `/T` when an inner
///   link forces per-run tagging)
/// - `SemanticEntry.alt_text` extraction (`<img>` Figure `/Alt`)
#[test]
fn test_tagged_hidden_heading_omits_title_attribute() {
    // Sentinel appears in the input HTML only inside elements that are
    // `visibility: hidden` (or `collapse`). If it shows up in the rendered
    // PDF bytes, it can only have come from a tag-tree attribute, since
    // painting is already suppressed for these paragraphs / images.
    let sentinel = "FULGURHIDDENSECRETXYZ";
    // Tiny valid 1x1 PNG (raster ping-through so the Figure tag is emitted).
    let png_data_url = "data:image/png;base64,\
        iVBORw0KGgoAAAANSUhEUgAAAAEAAAABAQAAAAA3bvkkAAAAB0lEQVR42mNgAAAAAgABc3UBGAAAAABJRU5ErkJggg==";
    let cases = [
        // Plain hidden heading exercises the `try_start_tagged` path.
        format!(r#"<h1 style="visibility:hidden">{sentinel}</h1>"#),
        // Collapsed hidden heading is equivalent per CSS 2.1 §11.2 and
        // must be gated the same way.
        format!(r#"<h2 style="visibility:collapse">{sentinel}</h2>"#),
        // Hidden heading whose only child is a link forces per-run
        // tagging, exercising the `build_struct_tree` backfill sink.
        format!(
            r#"<h3 style="visibility:hidden"><a href="https://example.com">{sentinel}</a></h3>"#
        ),
        // Hidden image alt: `Tag::Figure` carries the alt text through
        // its `/Alt` attribute — the other free-text sink in
        // `pdf_tag_to_krilla_tag`.
        format!(r#"<img src="{png_data_url}" alt="{sentinel}" style="visibility:hidden">"#),
        format!(r#"<img src="{png_data_url}" alt="{sentinel}" style="visibility:collapse">"#),
    ];
    for body in cases {
        let pdf = Engine::builder()
            .tagged(true)
            .lang("en")
            .build()
            .render(&format!("<html><body>{body}</body></html>"))
            .unwrap_or_else(|e| panic!("tagged render must not fail for {body:?}: {e:?}"));
        let needle = sentinel.as_bytes();
        let leaks = pdf.windows(needle.len()).any(|w| w == needle);
        assert!(
            !leaks,
            "hidden heading text leaked into tagged PDF for {body:?}"
        );
    }
}

#[test]
fn test_render_html_huge_multicol_column_count_is_bounded() {
    // Security regression (unbounded `column-count` memory-exhaustion DoS):
    // a single multicol container with an enormous `column-count` must not
    // allocate `column-count`-sized layout metadata. Before the
    // MAX_COLUMN_COUNT clamp in `resolve_column_layout`, the multicol hook
    // ran `vec![0.0; n]` for `col_heights` (here n = 2_000_000_000 ≈ 8 GB)
    // unconditionally, and the inline-root split path amplified further to
    // O(column_count × paragraph_count) `column_slices`. With the clamp the
    // used count is bounded to a small constant, so this renders in bounded
    // memory/time. Paragraphs are included so the split path is exercised too.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="column-count:2000000000; column-gap:10px; width:300px">
            <p>Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do
               eiusmod tempor incididunt ut labore et dolore magna aliqua.</p>
            <p>Ut enim ad minim veniam, quis nostrud exercitation ullamco
               laboris nisi ut aliquip ex ea commodo consequat.</p>
            <p>Duis aute irure dolor in reprehenderit in voluptate velit esse
               cillum dolore eu fugiat nulla pariatur.</p>
        </div>
    </body></html>"#;
    let pdf = Engine::builder().build().render(html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_huge_gradient_stop_list_is_bounded() {
    // Security regression (unbounded gradient color stops): convert clones the
    // resolved stop vector into every element's BackgroundLayer, so a huge
    // stop list retains O(elements × stops); a repeating gradient additionally
    // period-expands `total_copies × stops.len()` at draw. The MAX_GRADIENT_STOPS
    // clamp bounds both. A repeating-linear-gradient with thousands of stops
    // must render in bounded memory/time.
    let mut stops = String::new();
    for i in 0..5000 {
        stops.push_str(if i % 2 == 0 { "red 0%," } else { "blue 1%," });
    }
    stops.push_str("red 2%");
    let html = format!(
        r#"<!DOCTYPE html><html><body>
        <div style="width:120px;height:80px;background:repeating-linear-gradient({stops})"></div>
    </body></html>"#
    );
    let pdf = Engine::builder().build().render(&html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_huge_page_name_is_bounded() {
    // Security regression (unbounded page-name cloning): a CSS named page is
    // a length-unbounded custom-ident retained in ColumnProps, cloned into the
    // column style table and once per node while resolving used page names
    // (O(nodes × name_len)). The MAX_PAGE_NAME_BYTES clamp at the parse site
    // bounds the retained name, so many nodes sharing a huge page name render
    // in bounded memory.
    let long_name = "a".repeat(20_000);
    let mut body = String::new();
    for _ in 0..500 {
        body.push_str("<div>x</div>");
    }
    let html = format!(
        r#"<!DOCTYPE html><html><head><style>
        div {{ page: {long_name}; }}
        </style></head><body>{body}</body></html>"#
    );
    let pdf = Engine::builder().build().render(&html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn tagged_li_with_inline_block_child_does_not_panic() {
    // Security regression (fulgur-z28x): a tagged inline-root list item that
    // contains an inline-block child used to hit Krilla's non-nestable
    // start_tagged assertion. `draw_list_item_with_block` opens an LBody
    // marked-content sequence around the paragraph draw; the paragraph then
    // dispatches the inline-block via `dispatch_inline_box_content`, which
    // reaches `dispatch_fragment` and calls `try_start_tagged` again while
    // the outer LBody MCS is still active — Krilla panics.
    let html = r#"<!DOCTYPE html><html><body>
        <ul>
          <li>outer <span style="display:inline-block">inline block</span> after</li>
        </ul>
    </body></html>"#;
    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(html)
        .expect("tagged li with inline-block must render");
    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/StructTreeRoot"));
}

#[test]
fn tagged_paragraph_with_inline_block_child_does_not_panic() {
    // Security regression (fulgur-z28x): the same nested-MCS assertion fires
    // for a tagged `<p>` (PdfTag::P) containing an inline-block child. Guards
    // must live at the choke point, not just for the `<li>` LBody branch.
    let html = r#"<!DOCTYPE html><html><body>
        <p>outer <span style="display:inline-block">inline block</span> after</p>
    </body></html>"#;
    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(html)
        .expect("tagged p with inline-block must render");
    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/StructTreeRoot"));
}

#[test]
fn tagged_li_with_linked_inline_block_child_does_not_panic() {
    // Security regression (fulgur-z28x): when the inline-block subtree
    // contains a link, `dispatch_fragment` sets `link_run_node_id` for the
    // nested paragraph and per-run tagging via `RunRegionTracker::transition_to`
    // opens a second MCS while the outer LBody sequence is still active. The
    // full PDF must round-trip: not just past `start_tagged`, but through
    // serialization — an unwired tagged annotation would panic later in
    // krilla's tag-tree consistency check.
    let html = r#"<!DOCTYPE html><html><body>
        <ul>
          <li>outer <span style="display:inline-block"><a href="https://example.test/">linked</a></span> after</li>
        </ul>
    </body></html>"#;
    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(html)
        .expect("tagged li with linked inline-block must render");
    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/StructTreeRoot"));
}

#[test]
fn tagged_p_with_nested_list_in_inline_block_does_not_panic() {
    // Security regression (fulgur-z28x): a tagged `<p>` opens a P MCS, then
    // dispatches an inline-block containing `<ul><li>`. Rendering the nested
    // `<li>` reaches `draw_list_item_marker_tagged` while the outer P MCS is
    // still active — Krilla's marker-tag `start_tagged` would panic on that
    // second open without the `in_marked_content` guard.
    let html = r#"<!DOCTYPE html><html><body>
        <p>outer <span style="display:inline-block"><ul><li>nested</li></ul></span> after</p>
    </body></html>"#;
    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render(html)
        .expect("tagged p with nested list in inline-block must render");
    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/StructTreeRoot"));
}

/// Regression for issue #639 (fulgur-rkfu): `font-size: 0` on an element with
/// text used to divide `parley` (or `skrifa`) glyph advances by `0.0`,
/// producing `NaN` glyph fields that propagated into the PDF `Tm`/`TJ`
/// operators. `pdftocairo` rejected the resulting stream with `Unknown
/// operator 'NaN'`.
///
/// Four convert-time sites normalize glyph advance/offsets by `font_size`
/// (see `ShapedGlyph::normalize_by_font_size` in `paragraph.rs`); the HTML
/// below is deliberately shaped to drive all four so a regression at any
/// single one fails this test:
///
/// - `<p>invisible <span>visible</span> …</p>` → `inline_root.rs` shaping
///   (the plain inline paragraph path).
/// - `<div class="mc">…</div>` with `columns: 2` → `convert/mod.rs`'s
///   `shape_paragraph_glyph_runs` (the multicol per-slice re-shape path).
/// - `<ul><li>outside</li></ul>` (default `list-style-position: outside`)
///   → `list_marker.rs::extract_marker_lines` (parley marker shaping).
/// - `<ul style="list-style-position:inside"><li>inside</li></ul>` →
///   `list_marker.rs::shape_marker_with_skrifa` (skrifa marker fallback).
///
/// Empirically verified by reverting each of the four guards one at a time
/// while running this test — each revert flips the test red.
#[test]
fn font_size_zero_does_not_produce_nan_glyphs() {
    let html = r#"<!DOCTYPE html><html><head><style>
        p, .mc, ul, ul li, ul li div { font-size: 0; }
        p > span { font-size: 9pt; }
        .mc { columns: 2; column-gap: 20pt; width: 400pt; }
        ul.inside { list-style-position: inside; }
    </style></head><body>
        <p>invisible <span>visible</span> also-invisible</p>
        <div class="mc">multicol invisible text spanning two columns of layout</div>
        <ul><li>outside marker item</li></ul>
        <ul class="inside"><li><div>inside marker with block child</div></li></ul>
    </body></html>"#;

    let out = Engine::builder()
        .build()
        .layout(html)
        .expect("font-size:0 layout must succeed");

    fn check_lines(lines: &[fulgur::paragraph::ShapedLine], source: &str, seen_zero: &mut u32) {
        for line in lines {
            for item in &line.items {
                let fulgur::paragraph::LineItem::Text(run) = item else {
                    continue;
                };
                let font_size_zero = run.font_size.to_f32() == 0.0;
                if font_size_zero {
                    *seen_zero += 1;
                }
                for g in &run.glyphs {
                    assert!(
                        g.x_advance.is_finite(),
                        "{source}: x_advance must be finite even at font-size:0 (got {})",
                        g.x_advance,
                    );
                    assert!(
                        g.x_offset.is_finite(),
                        "{source}: x_offset must be finite even at font-size:0 (got {})",
                        g.x_offset,
                    );
                    assert!(
                        g.y_offset.is_finite(),
                        "{source}: y_offset must be finite even at font-size:0 (got {})",
                        g.y_offset,
                    );
                    // Pin the *contract* of `normalize_by_font_size` at
                    // `font_size == 0.0`: not merely finite (which a
                    // regression to `raw / very_small_positive` could
                    // still satisfy at a garbage value) but exactly
                    // `0.0`. Downstream `x_advance * run.font_size` will
                    // multiply by zero anyway, so the only value that
                    // preserves the round-trip is `0.0` itself.
                    if font_size_zero {
                        assert_eq!(
                            g.x_advance, 0.0,
                            "{source}: x_advance at font-size:0 must be exactly 0.0"
                        );
                        assert_eq!(
                            g.x_offset, 0.0,
                            "{source}: x_offset at font-size:0 must be exactly 0.0"
                        );
                        assert_eq!(
                            g.y_offset, 0.0,
                            "{source}: y_offset at font-size:0 must be exactly 0.0"
                        );
                    }
                }
            }
        }
    }

    // Three separate counters so a coverage regression (e.g. multicol
    // slice path stops emitting) fails loudly instead of silently
    // shrinking the site coverage of this test.
    let mut para_runs = 0u32;
    let mut slice_runs = 0u32;
    let mut marker_runs = 0u32;

    for entry in out.drawables.paragraphs.values() {
        check_lines(&entry.lines, "paragraphs", &mut para_runs);
    }
    for entry in out.drawables.paragraph_slices.values() {
        for slice in &entry.slices {
            check_lines(&slice.lines, "paragraph_slices", &mut slice_runs);
        }
    }
    for entry in out.drawables.list_items.values() {
        if let fulgur::drawables::ListItemMarker::Text { lines, .. } = &entry.marker {
            check_lines(lines, "list_items.marker", &mut marker_runs);
        }
    }

    assert!(
        para_runs > 0,
        "expected inline_root (paragraphs) to emit at least one font-size:0 run",
    );
    assert!(
        slice_runs > 0,
        "expected multicol (paragraph_slices) to emit at least one font-size:0 run",
    );
    assert!(
        marker_runs > 0,
        "expected list-item markers (list_items.marker) to emit at least one \
         font-size:0 run",
    );

    // Full pipeline still produces a well-formed PDF (no NaN → no `Unknown
    // operator 'NaN'` from pdftocairo). We do not decompress and grep for
    // the substring "NaN" here because that would require a decompression
    // dev-dep; the layout assertion above already pins the source of the
    // NaN, and a `%PDF` prefix + non-empty body confirms rendering
    // completed without panic.
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("font-size:0 render must succeed");
    assert!(pdf.starts_with(b"%PDF"));
}

/// Attack fixture builder: `<N>` color-stops evenly distributed as a
/// red/blue alternation. Used by the gradient-fallback OOM tests below.
///
/// Guards `n <= 1` degenerate inputs so the helper can be reused
/// without hitting `100 / (n - 1) = ∞ → NaN` — invalid CSS that would
/// silently produce an unrenderable payload.
fn many_color_stops(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "red 0%".to_string(),
        _ => {
            let step = 100.0f32 / (n as f32 - 1.0);
            (0..n)
                .map(|i| {
                    let pct = (i as f32) * step;
                    let color = if i % 2 == 0 { "red" } else { "blue" };
                    format!("{color} {pct:.4}%")
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

/// Codex Security finding `01f477e4` — "Repeated tiny gradients can
/// exhaust render CPU". Before this fix, the attack HTML below (~300 B
/// serialized) produced a **~425 MB** PDF and **~975 MB** peak RSS in
/// the CLI: `background-size: 1.33px × 1.33px` with `background-repeat:
/// repeat` on an A4 clip generates ~500k logical tiles, which the
/// `compute_tile_positions_slow` `MAX_TILES = 10_000` cap truncates
/// mid-row. The truncated (non-rectangular) grid defeats
/// `try_uniform_grid`, so the per-tile fallback fires with up to 10_000
/// `draw_linear_gradient` calls, each rebuilding a krilla stop vector
/// bounded by `MAX_GRADIENT_STOPS = 256`. Fix: fold the leading
/// complete-row rectangle into a single Tiling Pattern (see
/// `split_truncated_grid` in `background.rs`); only the trailing
/// partial row falls through to per-tile draws.
///
/// The 5 MB budget below has comfortable headroom above the ~KB output
/// that a well-formed Pattern-emitted layer produces (`attack_uniform`
/// fixture is <50 KB with the same 300-stop payload) and stays orders
/// of magnitude below the pre-fix 425 MB regression window.
#[test]
fn linear_gradient_nonuniform_tile_repeat_is_bounded() {
    let stops = many_color_stops(300);
    let html = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{{margin:0;padding:0}}
body{{background-size:1.33px 1.33px;background-repeat:repeat;height:1130px;
     background-image:linear-gradient(45deg,{stops})}}
</style></head><body></body></html>"#
    );
    let pdf = Engine::builder()
        .build()
        .render(&html)
        .expect("linear-gradient exploit render must succeed");
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        pdf.len() < 5_000_000,
        "linear gradient with tiny repeat + many stops must be bounded, got {} bytes",
        pdf.len()
    );
}

/// Radial-gradient variant of the Codex `01f477e4` exploit. Same
/// truncated-grid fallback machinery, same Pattern fold applied.
#[test]
fn radial_gradient_nonuniform_tile_repeat_is_bounded() {
    let stops = many_color_stops(300);
    let html = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{{margin:0;padding:0}}
body{{background-size:1.33px 1.33px;background-repeat:repeat;height:1130px;
     background-image:radial-gradient(circle at center,{stops})}}
</style></head><body></body></html>"#
    );
    let pdf = Engine::builder()
        .build()
        .render(&html)
        .expect("radial-gradient exploit render must succeed");
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        pdf.len() < 5_000_000,
        "radial gradient with tiny repeat + many stops must be bounded, got {} bytes",
        pdf.len()
    );
}

/// Conic-gradient variant of the Codex `01f477e4` sibling issue
/// `6aa94631` — conic wedges (~360 per draw) multiplied across the
/// truncated-grid fallback produced ~46 MB PDF / ~833 MB RSS from
/// ~300 B HTML. Same Pattern fold applied.
#[test]
fn conic_gradient_nonuniform_tile_repeat_is_bounded() {
    let html = r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;padding:0}
body{background-size:1.33px 1.33px;background-repeat:repeat;height:1130px;
     background-image:conic-gradient(from 0deg,red,yellow,green,blue,red)}
</style></head><body></body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("conic-gradient exploit render must succeed");
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        pdf.len() < 5_000_000,
        "conic gradient with tiny repeat must be bounded, got {} bytes",
        pdf.len()
    );
}

/// Subpixel residual of Codex `01f477e4` — the Pattern fold defused
/// the primary attack (`background-size: 1.33px`), but tiles smaller
/// than the `try_uniform_grid` eps (`1e-3` pt) still collapsed in the
/// grid-shape dedup and re-entered per-tile emission. `background-size:
/// 0.001px 0.001px` = `0.00075` pt, below eps, previously produced
/// ~425 MB PDF / ~975 MB RSS from ~300 B HTML.
///
/// Fix: `MIN_GRADIENT_TILE_PT` (`lib.rs`) clamps gradient tile size to
/// `0.01 pt` = 10× the eps, well below any physical output-device dot
/// (1 dot @ 7200 dpi). Well-formed shape → Pattern emit.
#[test]
fn linear_gradient_subpixel_tile_is_bounded() {
    let stops = many_color_stops(300);
    let html = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{{margin:0;padding:0}}
body{{background-size:0.001px 0.001px;background-repeat:repeat;height:1130px;
     background-image:linear-gradient(45deg,{stops})}}
</style></head><body></body></html>"#
    );
    let pdf = Engine::builder()
        .build()
        .render(&html)
        .expect("subpixel-tile exploit render must succeed");
    assert!(pdf.starts_with(b"%PDF"));
    assert!(
        pdf.len() < 5_000_000,
        "subpixel-tile linear gradient must be bounded by MIN_GRADIENT_TILE_PT, got {} bytes",
        pdf.len()
    );
}

/// Body link annotations must never leak into the page-margin strips
/// where `@page @top-*`/`@bottom-*` margin boxes paint. `render_v2`
/// paints margin boxes AFTER body content so page headers/footers are
/// not hidden by page-filling body backgrounds; because PDF link
/// annotations are independent interactive objects, a body anchor
/// positioned into a margin strip would remain clickable under the
/// margin-box content that visually replaces it — click target and
/// visible text disagree. The fix splits `LinkCollector` into body/
/// margin collectors and clips body occurrences to the content area
/// rect.
///
/// This test pushes a body `<a>` far past the content-area bottom via
/// `margin-top` so the anchor's Y coordinate lands inside the bottom
/// margin strip. The rendered PDF must contain zero link annotations
/// whose rect intersects that strip.
#[test]
fn body_link_positioned_into_bottom_margin_strip_is_dropped() {
    // A4 with 60pt top/bottom margins. Content-area bottom (Y-up) is
    // 782 → 842 (60pt strip). The `.push` anchor gets margin-top: 750pt,
    // sending its Y-up rect below 782 (into the bottom-margin strip).
    let html = r#"<!doctype html><html><head><style>
@page {
  size: A4;
  margin: 60pt 40pt 60pt 40pt;
  @bottom-center { content: "Page footer"; }
}
body { margin: 0; padding: 0; }
a { display: block; margin-top: 750pt; font-size: 12pt; }
</style></head><body>
<a href="https://example.com/target">body link</a>
</body></html>"#;

    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("render must succeed");
    assert!(pdf.starts_with(b"%PDF"));

    let text = String::from_utf8_lossy(&pdf);
    let rects = regex_lite_link_rects(&text);
    // Every /Rect that survives must lie inside the content area
    // (Y-up ury >= 60pt).
    for cap in &rects {
        let (llx, lly, urx, ury) = *cap;
        assert!(
            ury >= 60.0,
            "body link annotation leaked into bottom margin strip: \
             /Rect [{llx} {lly} {urx} {ury}] — top edge (Y-up ury) \
             must be >= 60pt (content-area bottom in Y-up)"
        );
    }
    // Positive control for the scanner: without this, a hypothetical
    // break in `regex_lite_link_rects` (returning an empty vec) would
    // let the ury loop pass vacuously and hide a real regression.
    // A companion render with an anchor plainly inside the content
    // area must produce at least one /Rect the same scanner can find.
    let legit_html = r#"<!doctype html><html><head><style>
@page { size: A4; margin: 60pt 40pt 60pt 40pt; }
body { margin: 0; padding: 20pt; font-size: 12pt; }
</style></head><body>
<p>Visit <a href="https://example.com/in-content">our documentation</a>.</p>
</body></html>"#;
    let legit_pdf = Engine::builder()
        .build()
        .render(legit_html)
        .expect("positive-control render must succeed");
    let legit_text = String::from_utf8_lossy(&legit_pdf);
    let legit_rects = regex_lite_link_rects(&legit_text);
    assert!(
        !legit_rects.is_empty(),
        "scanner returned no /Rect entries for a plainly in-content link; \
         the primary loop above would pass vacuously if the scanner \
         itself broke, so guard against that here."
    );
    for cap in legit_rects {
        let (llx, lly, urx, ury) = cap;
        assert!(
            ury >= 60.0,
            "positive-control link ended up in a margin strip: \
             /Rect [{llx} {lly} {urx} {ury}]"
        );
    }
}

/// When a page has margins but no effective `@page` margin-box rule,
/// nothing paints on top of body content in the margin strips. Body
/// links that extend into those strips are visually present AND have
/// nothing later covering them, so their PDF annotation must remain
/// clickable — `clip_body_occurrences_to_content_area` must not be
/// applied on pages with no margin boxes. Otherwise a `position:
/// fixed; bottom: 0` page number rendered from body content ends up
/// visible but unclickable.
#[test]
fn body_link_in_margin_area_is_preserved_when_no_margin_box_paints() {
    // Same `margin-top: 750pt` shape as the security regression test,
    // but with no `@page` margin box: the anchor's rect lands in the
    // bottom-margin strip yet the annotation must survive because
    // nothing paints on top of it.
    let html = r#"<!doctype html><html><head><style>
@page { size: A4; margin: 60pt 40pt 60pt 40pt; }
body { margin: 0; padding: 0; }
a { display: block; margin-top: 750pt; font-size: 12pt; }
</style></head><body>
<a href="https://example.com/target">body-drawn footer link</a>
</body></html>"#;

    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("render must succeed");
    let text = String::from_utf8_lossy(&pdf);
    let rects = regex_lite_link_rects(&text);
    // The annotation MUST reach the PDF — a page with no margin box
    // never overpaints body content, so dropping the annotation would
    // leave the visible link unclickable.
    assert!(
        !rects.is_empty(),
        "body link in a margin strip on a page with no margin box was \
         dropped; nothing paints on top of it, so the annotation must \
         survive"
    );
    // Sanity: the annotation should be in the bottom margin strip
    // (Y-up ury < 60), matching the visible text position.
    let (_, _, _, ury) = rects[0];
    assert!(
        ury < 60.0,
        "positive-control render did not put the anchor in the bottom \
         margin strip as expected; rects = {rects:?}"
    );
}

/// Parse `/Subtype /Link ... /Rect [llx lly urx ury]` from a raw PDF
/// text stream. Returns `(llx, lly, urx, ury)` tuples in PDF-native
/// Y-up coordinates. Used by the security regression test above so
/// no new dependency (`regex`, `pdf-extract`) is pulled in just for
/// one assertion — the raw content stream shape is stable enough for
/// a bespoke scan.
fn regex_lite_link_rects(text: &str) -> Vec<(f32, f32, f32, f32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(link_off) = text[i..].find("/Subtype /Link") {
        let start = i + link_off;
        // Look ahead for /Rect [ ... ] within the next ~512 bytes.
        // `text` came from `String::from_utf8_lossy` over raw PDF
        // bytes, so it is valid UTF-8, but `start + 512` can still
        // land in the middle of a multi-byte code point (e.g.
        // metadata strings containing non-ASCII). Round down to the
        // nearest char boundary so `text[start..window_end]` cannot
        // panic on such inputs.
        let mut window_end = (start + 512).min(text.len());
        while window_end > start && !text.is_char_boundary(window_end) {
            window_end -= 1;
        }
        if let Some(rect_off) = text[start..window_end].find("/Rect [") {
            let after = start + rect_off + "/Rect [".len();
            if let Some(close) = text[after..window_end].find(']') {
                let body = &text[after..after + close];
                let nums: Vec<f32> = body
                    .split_whitespace()
                    .filter_map(|s| s.parse::<f32>().ok())
                    .collect();
                if nums.len() == 4 {
                    out.push((nums[0], nums[1], nums[2], nums[3]));
                }
            }
        }
        i = start + "/Subtype /Link".len();
    }
    out
}

/// `Engine::render` must reject non-finite / non-positive page size and
/// collapsing margins before they reach Blitz layout, instead of silently
/// producing a saturated viewport or a krilla PDF-generation error deep in
/// the pipeline. Exercised through the real `Engine::builder()` entry point
/// (not a direct `Config::validate` unit call) so the render path itself is
/// covered for codecov patch attribution — see CLAUDE.md "Coverage scope".
#[test]
fn render_rejects_non_finite_custom_page_size() {
    use fulgur::PageSize;

    let err = Engine::builder()
        .page_size(PageSize::custom(210.0, f32::INFINITY))
        .build()
        .render("<p>x</p>")
        .expect_err("infinite custom page height should be rejected");
    assert!(err.to_string().contains("invalid page size"));
}

#[test]
fn render_rejects_margin_collapsing_content_area() {
    use fulgur::{Margin, PageSize};

    let err = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(1000.0))
        .build()
        .render("<p>x</p>")
        .expect_err("margins exceeding the page size should be rejected");
    assert!(err.to_string().contains("invalid margin"));
}

/// `builder.margin(...)` sets `ConfigOverrides::margin`, which makes it
/// authoritative over `@page { margin: ... }` (see
/// `gcpm::page_settings::resolve_page_settings`) — so an invalid explicit
/// builder margin is never "rescued" by CSS, and `Config::validate` rejecting
/// it before GCPM resolution runs is correct, not a false positive.
#[test]
fn render_rejects_explicit_margin_override_even_with_at_page_css() {
    use fulgur::Margin;

    let html = r#"<style>@page { size: A4; margin: 20mm; }</style><p>x</p>"#;
    let err = Engine::builder()
        .margin(Margin::uniform(1000.0))
        .build()
        .render(html)
        .expect_err("explicit builder margin override must win over @page and be rejected");
    assert!(err.to_string().contains("invalid margin"));
}

/// The inverse of the test above: when the caller does *not* call
/// `builder.margin(...)`, `self.config` stays at its always-valid default,
/// so `Config::validate` is a no-op regardless of what `@page` declares.
/// A pathological `@page`-only margin is unaffected by this PR — it is
/// still handled by the pre-existing
/// `resolved_content_{width,height}_pt.max(0.0)` clamp in `Engine::render`.
#[test]
fn render_unaffected_by_at_page_only_margin_without_builder_override() {
    let html = r#"<style>@page { size: A4; margin: 1000mm; }</style><p>x</p>"#;
    let pdf = Engine::builder()
        .build()
        .render(html)
        .expect("no builder override means Config::validate must not reject @page-only CSS");
    assert!(!pdf.is_empty());
}
