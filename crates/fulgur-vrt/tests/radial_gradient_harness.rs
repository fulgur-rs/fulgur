//! Test↔ref pixel-diff harness for radial-gradient implementation.
//!
//! linear-gradient の strip 近似と同じ思想で、ref は同心リングの離散近似。
//! 各リングは外側に少しずつ大きくなる円 (`border-radius:50%`) で、
//! 中央の `(cx, cy)` から半径 r の位置の色 (red→blue を r/R で線形補間) を塗る。
//!
//! 注意: test fixture は `radial-gradient(circle closest-side, ...)` を使う。
//! CSS の default ending shape は `farthest-corner` で、正方形ボックスでは
//! 半径 = 96*sqrt(2) ≈ 135.76 px の非整数値になり、ring step も非整数化して
//! AA が広がるため tolerance が大幅に緩む。`closest-side` を明示することで
//! 半径 R = 96 CSS px (= 192/2) に固定でき、4 px ステップ × 24 ring の整数
//! raster アラインが成立する。半径 R を超える領域 (ボックス四隅) は最終色
//! (c1 = blue) で塗られるので、ref 側は `.box` に同色背景を敷いて一致させる。
//!
//! 採用しなかった案:
//! - SVG `<radialGradient>` ref → fulgur SVG 経路と HTML 経路の両方を verify
//!   したい主目的とずれる
//! - PNG raster ref → CI 再現性で扱いづらい
//! - `farthest-corner` (CSS default) のまま ref を組む案: ring step が
//!   非整数 (5.66 px) になり AA が広がって tolerance が破綻する

use fulgur_vrt::diff::{self};
use fulgur_vrt::manifest::Tolerance;
use fulgur_vrt::pdf_render::{RenderSpec, pdf_to_rgba, render_html_to_pdf};
use std::fs;
use std::path::PathBuf;

const GRADIENT_SIZE_PX: u32 = 192;
const GRADIENT_MARGIN_PX: u32 = 32;
const RING_COUNT: u32 = 24;
const RING_STEP_PX: u32 = GRADIENT_SIZE_PX / 2 / RING_COUNT; // 96 / 24 = 4

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let av = a as f32;
    let bv = b as f32;
    (av + (bv - av) * t).round().clamp(0.0, 255.0) as u8
}

/// 同心リング近似 ref。外側 (大半径) から内側に向けて塗り重ねる
/// (z-index で内側を上に — DOM order で後の要素が上に来る)。
/// CSS の `radial-gradient(circle, c0 0%, c1 100%)` は中心 (r=0) で c0、
/// 外周 (r=R) で c1 なので、半径 r のリング色は `lerp(c0, c1, r/R)`。
fn build_ring_ref_html(c0: (u8, u8, u8), c1: (u8, u8, u8)) -> String {
    let max_r = GRADIENT_SIZE_PX / 2;
    let mut rings = String::new();
    for k in (0..RING_COUNT).rev() {
        let outer_r_px = (k + 1) * RING_STEP_PX; // 4, 8, ..., 96
        let mid_r = outer_r_px as f32 - (RING_STEP_PX as f32) / 2.0;
        let t = mid_r / max_r as f32;
        let r = lerp_u8(c0.0, c1.0, t);
        let g = lerp_u8(c0.1, c1.1, t);
        let b = lerp_u8(c0.2, c1.2, t);
        let d = outer_r_px * 2;
        let off = (max_r - outer_r_px) as i32;
        rings.push_str(&format!(
            r#"<div style="position:absolute;left:{off}px;top:{off}px;width:{d}px;height:{d}px;border-radius:50%;background:rgb({r},{g},{b});"></div>"#
        ));
    }

    // ボックス背景は最終色 c1: closest-side gradient では半径 r > R
    // (ボックス四隅) は最終色で塗られるので、ref 側でも同じ色を敷く。
    let bg_r = c1.0;
    let bg_g = c1.1;
    let bg_b = c1.2;
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT ref: radial-gradient ring approximation</title>
<style>
  html, body {{ margin: 0; padding: 0; background: white; }}
  .box {{ position: relative; width: {w}px; height: {h}px; margin: {m}px; background: rgb({bg_r},{bg_g},{bg_b}); }}
</style>
</head>
<body>
  <div class="box">{rings}</div>
</body>
</html>"#,
        w = GRADIENT_SIZE_PX,
        h = GRADIENT_SIZE_PX,
        m = GRADIENT_MARGIN_PX,
        bg_r = bg_r,
        bg_g = bg_g,
        bg_b = bg_b,
        rings = rings,
    )
}

#[test]
fn radial_gradient_circular_matches_ring_reference() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_html_path = crate_root.join("fixtures/paint/radial-gradient-circular.html");
    let test_html = fs::read_to_string(&test_html_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", test_html_path.display()));

    let ref_html = build_ring_ref_html((0xe5, 0x39, 0x35), (0x1e, 0x88, 0xe5));

    let spec = RenderSpec {
        page_size: "A4",
        margin_pt: Some(0.0),
        dpi: 150,
    };

    let test_pdf = render_html_to_pdf(&test_html, spec).expect("render test pdf");
    let ref_pdf = render_html_to_pdf(&ref_html, spec).expect("render ref pdf");

    let work_dir = crate_root
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("target/vrt-radial-gradient-harness");
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

    // CodeRabbit #238: full A4 raster (~890k px @ 150dpi) を denominator にすると
    // gradient box (90k px) に対し effective tolerance が ~10x 緩くなる。
    // box 周辺だけを crop してから比較することで `max_diff_pixels_ratio` を box 基準にする。
    //
    // 150dpi で 1 CSS px = 25/16 raster px:
    //   margin 32 CSS px  → 50  raster px
    //   box    192 CSS px → 300 raster px
    //   total  256 CSS px → 400 raster px (margin + box + margin の上限)
    // box 周囲に 4 raster px の余白を付けて crop する (端の AA も含めて評価したいが、
    // ページ余白の白を denominator から除外するのが目的)。
    const CROP_MARGIN_RASTER: u32 = 4;
    let crop_x = (GRADIENT_MARGIN_PX as u64 * 25 / 16) as u32 - CROP_MARGIN_RASTER;
    let crop_y = crop_x;
    let crop_w = (GRADIENT_SIZE_PX as u64 * 25 / 16) as u32 + CROP_MARGIN_RASTER * 2;
    let crop_h = crop_w;
    let test_crop = image::imageops::crop_imm(&test_img, crop_x, crop_y, crop_w, crop_h).to_image();
    let ref_crop = image::imageops::crop_imm(&ref_img, crop_x, crop_y, crop_w, crop_h).to_image();

    // Tolerance (cropped 308x308 ≈ 95k px denominator):
    // - 12 channels: ring step (24 ring → 0.5*255/24 ≈ 5) + AA 余裕で 7 程度を見込み +5 の headroom
    // - 1.0%: 95k px × 1% = 950 pixel ≈ 24 ring の境界 1.5 px 帯と box 縁 AA で収まるはず
    //   (cropしたので box 基準で 1% になり、`cr` を倍にすると確実に超える)
    // 失敗したら数値を調整する前に crop 元の diff 画像を確認すること。
    let tol = Tolerance {
        max_channel_diff: 12,
        max_diff_pixels_ratio: 0.01,
    };

    let report = diff::compare(&ref_crop, &test_crop, tol);

    assert!(
        report.pass,
        "radial gradient test↔ref harness failed: {} of {} pixels differ ({:.3}%), max channel diff = {} (tolerance: max_diff={}, ratio<={:.3}%). \
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

/// `radial-gradient(circle closest-side, #e53935 -50%, #1e88e5 100%)` の renormalize 動作確認。
///
/// 範囲外 stop -50% は drop され、中心 (r=0, offset 0) の色は CSS Images 3 §3.5.1 に従い
/// `red + (1/3) * (blue - red)` で合成される。同心リング近似 ref の中心側をこの合成色、
/// 外周を blue にして比較する。
#[test]
fn radial_gradient_out_of_range_matches_ring_reference() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_html_path = crate_root.join("fixtures/paint/radial-gradient-out-of-range.html");
    let test_html = fs::read_to_string(&test_html_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", test_html_path.display()));

    // Synthesized color at offset 0 (alpha = 1/3 between red(-0.5) and blue(1.0)):
    //   r = 0xe5 + (1/3) * (0x1e - 0xe5) = 229 + (1/3) * (-199) ≈ 162.67 → 0xa3
    //   g = 0x39 + (1/3) * (0x88 - 0x39) =  57 + (1/3) *   79  ≈  83.33 → 0x53
    //   b = 0x35 + (1/3) * (0xe5 - 0x35) =  53 + (1/3) *  176  ≈ 111.67 → 0x70
    let ref_html = build_ring_ref_html((0xa3, 0x53, 0x70), (0x1e, 0x88, 0xe5));

    let spec = RenderSpec {
        page_size: "A4",
        margin_pt: Some(0.0),
        dpi: 150,
    };

    let test_pdf = render_html_to_pdf(&test_html, spec).expect("render test pdf");
    let ref_pdf = render_html_to_pdf(&ref_html, spec).expect("render ref pdf");

    let work_dir = crate_root
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("target/vrt-radial-gradient-oor-harness");
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

    // 既存 `radial_gradient_circular_matches_ring_reference` と同じ crop 設定を使う。
    const CROP_MARGIN_RASTER: u32 = 4;
    let crop_x = (GRADIENT_MARGIN_PX as u64 * 25 / 16) as u32 - CROP_MARGIN_RASTER;
    let crop_y = crop_x;
    let crop_w = (GRADIENT_SIZE_PX as u64 * 25 / 16) as u32 + CROP_MARGIN_RASTER * 2;
    let crop_h = crop_w;
    let test_crop = image::imageops::crop_imm(&test_img, crop_x, crop_y, crop_w, crop_h).to_image();
    let ref_crop = image::imageops::crop_imm(&ref_img, crop_x, crop_y, crop_w, crop_h).to_image();

    // Tolerance は `radial_gradient_circular_matches_ring_reference` と同値。
    let tol = Tolerance {
        max_channel_diff: 12,
        max_diff_pixels_ratio: 0.01,
    };

    let report = diff::compare(&ref_crop, &test_crop, tol);

    assert!(
        report.pass,
        "radial gradient out-of-range test↔ref harness failed: {} of {} pixels differ ({:.3}%), max channel diff = {} (tolerance: max_diff={}, ratio<={:.3}%). \
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
