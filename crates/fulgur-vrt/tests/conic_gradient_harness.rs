//! Test↔ref pixel-diff harness for conic-gradient implementation.
//!
//! 4 個の 90° セクターを持つ pie chart を、CSS `clip-path: polygon(...)` で扇形を
//! 作った 4 個の絶対配置 div の ref と pixel-wise 比較する。conic-gradient の
//! 角度規約 (CSS: 0deg=top, 増加方向=clockwise) を empirical に検証する。
//!
//! red sector   = 0..90deg = top → right (右上 1/4)
//! yellow sector = 90..180deg = right → bottom (右下 1/4)
//! green sector = 180..270deg = bottom → left (左下 1/4)
//! blue sector = 270..360deg = left → top (左上 1/4)
//!
//! 採用しなかった案:
//! - SVG `<linearGradient>` 集合 ref → 自前 SVG パイプラインに依存
//! - PNG raster ref → CI 再現性で扱いづらい
//! - サイズが異なる box → ref polygon 頂点の整数化が崩れて tolerance 緩和必要

use fulgur_vrt::diff;
use fulgur_vrt::manifest::Tolerance;
use fulgur_vrt::pdf_render::{RenderSpec, pdf_to_rgba, render_html_to_pdf};
use std::fs;
use std::path::PathBuf;

const BOX_PX: u32 = 192;
const MARGIN_PX: u32 = 32;

/// 4 個の 90° セクターを絶対配置の矩形で構築する ref HTML。
///
/// 4-sector axis-aligned conic では各セクターが box の 1/4 矩形と一致する
/// (中心と box 辺の中点が一致するため)。`clip-path: polygon` は Blitz が未対応
/// なので採用せず、`position: absolute` + `width:50%; height:50%` の単色矩形を
/// 4 個並べる。
///
/// 配置:
/// - red    = top-right (CSS conic 0..90deg = top → right)
/// - yellow = bottom-right (90..180deg)
/// - green  = bottom-left (180..270deg)
/// - blue   = top-left (270..360deg)
fn build_quadrant_ref_html() -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT ref: conic-gradient 4 quadrant pie chart</title>
<style>
  html, body {{ margin: 0; padding: 0; background: white; }}
  .box {{ position: relative; width: {w}px; height: {h}px; margin: {m}px; }}
  .quad {{ position: absolute; width: 50%; height: 50%; }}
  .red    {{ top: 0;   right: 0; background: red; }}
  .yellow {{ bottom: 0; right: 0; background: yellow; }}
  .green  {{ bottom: 0; left: 0;  background: green; }}
  .blue   {{ top: 0;   left: 0;  background: blue; }}
</style>
</head>
<body>
  <div class="box">
    <div class="quad red"></div>
    <div class="quad yellow"></div>
    <div class="quad green"></div>
    <div class="quad blue"></div>
  </div>
</body>
</html>"#,
        w = BOX_PX,
        h = BOX_PX,
        m = MARGIN_PX,
    )
}

#[test]
fn conic_gradient_quadrants_match_rect_reference() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_html_path = crate_root.join("fixtures/paint/conic-gradient-quadrant.html");
    let test_html = fs::read_to_string(&test_html_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", test_html_path.display()));

    let ref_html = build_quadrant_ref_html();

    let spec = RenderSpec {
        page_size: "A4",
        margin_pt: Some(0.0),
        dpi: 150,
        bookmarks: false,
    };

    let test_pdf = render_html_to_pdf(&test_html, spec).expect("render test pdf");
    let ref_pdf = render_html_to_pdf(&ref_html, spec).expect("render ref pdf");

    let work_dir = crate_root
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("target/vrt-conic-gradient-harness");
    fs::create_dir_all(&work_dir).expect("create work dir");

    let test_dir = work_dir.join("test");
    let ref_dir = work_dir.join("ref");
    let _ = fs::remove_dir_all(&test_dir);
    let _ = fs::remove_dir_all(&ref_dir);
    fs::create_dir_all(&test_dir).expect("create test dir");
    fs::create_dir_all(&ref_dir).expect("create ref dir");

    let test_pdf_path = test_dir.join("test.pdf");
    let ref_pdf_path = ref_dir.join("ref.pdf");
    fs::write(&test_pdf_path, &test_pdf).expect("write test pdf");
    fs::write(&ref_pdf_path, &ref_pdf).expect("write ref pdf");

    let test_img = pdf_to_rgba(&test_pdf_path, spec.dpi, &test_dir).expect("rasterize test");
    let ref_img = pdf_to_rgba(&ref_pdf_path, spec.dpi, &ref_dir).expect("rasterize ref");

    // box 周辺だけを crop して評価 (radial harness と同じ思想):
    // 150dpi で 1 CSS px = 25/16 raster px、
    //   margin 32 CSS px  → 50 raster px
    //   box   192 CSS px  → 300 raster px
    const CROP_MARGIN_RASTER: u32 = 4;
    let crop_x = (MARGIN_PX as u64 * 25 / 16) as u32 - CROP_MARGIN_RASTER;
    let crop_y = crop_x;
    let crop_w = (BOX_PX as u64 * 25 / 16) as u32 + CROP_MARGIN_RASTER * 2;
    let crop_h = crop_w;
    let test_crop = image::imageops::crop_imm(&test_img, crop_x, crop_y, crop_w, crop_h).to_image();
    let ref_crop = image::imageops::crop_imm(&ref_img, crop_x, crop_y, crop_w, crop_h).to_image();

    // Tolerance:
    // - 12 channels: sector 境界 (4 本の十字線) AA で局所的に色が滲む
    // - 2.0%: cropped 308x308 ≈ 95k px に対し 4 本の境界線 (合計 ~600 px) +
    //   各 wedge edge AA 余裕で 1.5% 程度を見込み、+0.5% headroom
    let tol = Tolerance {
        max_channel_diff: 12,
        max_diff_pixels_ratio: 0.02,
    };

    let report = diff::compare(&ref_crop, &test_crop, tol);

    assert!(
        report.pass,
        "conic gradient quadrant test↔ref harness failed: {} of {} pixels differ ({:.3}%), max channel diff = {} (tolerance: max_diff={}, ratio<={:.3}%). \
         test PDF: {}\n  ref PDF: {}\n  test img: {}\n  ref img:  {}",
        report.diff_pixels,
        report.total_pixels,
        report.ratio() * 100.0,
        report.max_channel_diff,
        tol.max_channel_diff,
        tol.max_diff_pixels_ratio * 100.0,
        test_pdf_path.display(),
        ref_pdf_path.display(),
        test_dir.join("page-1.png").display(),
        ref_dir.join("page-1.png").display(),
    );
}
