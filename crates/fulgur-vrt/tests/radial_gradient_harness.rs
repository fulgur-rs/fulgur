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

    // Tolerance: linear gradient harness は 10/0.5% で運用中。radial は
    // (a) 円弧境界の AA が strip 境界より広く出る、(b) 中心1点の収束特性、
    // の2点で linear より許容を緩める必要がある。だが緩すぎると
    // `cr` を倍にしても通ってしまうので、12 channels / 1.0% に絞る。
    // - 12 channels: ring step (24 ring → 0.5*255/24 ≈ 5) + AA 余裕で 7 程度を見込み +5 の headroom
    // - 1.0%: 192² ≈ 110k pixel × 1% = 1100 pixel ≈ 24 ring の境界 1.5 px 帯と box 縁 AA で収まるはず
    // 失敗したら数値を調整する前に diff 画像を確認すること (生 diff 画像は work_dir に出る)。
    let tol = Tolerance {
        max_channel_diff: 12,
        max_diff_pixels_ratio: 0.01,
    };

    let report = diff::compare(&ref_img, &test_img, tol);

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
