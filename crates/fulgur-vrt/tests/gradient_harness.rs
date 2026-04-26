//! Test↔ref pixel-diff harness for gradient implementation.
//!
//! WPT's `linear-gradient-{1,non-square}` reftests reference HTMLs also use
//! `linear-gradient(...)`, so on an engine that ignores gradients both
//! test/ref render blank and pixel-match — a silent false-positive PASS.
//! This harness defines a custom test↔ref pair where the ref does NOT use
//! `linear-gradient`, so an unimplemented engine will visibly FAIL.
//!
//! Approach: ref is a stack of N narrow vertical strips of solid colors
//! sampled along the gradient line at each strip's midpoint. Strip count
//! and box dimensions are chosen so all strip boundaries land on integer
//! raster pixels at 150dpi (see `STRIP_COUNT` doc), avoiding the cairo
//! adjacent-fill seam (fulgur-wtai) that would otherwise dominate the
//! diff. The current implementation should match within ~5 channels per
//! pixel of step-vs-smooth quantization.

use fulgur_vrt::diff::{self};
use fulgur_vrt::manifest::Tolerance;
use fulgur_vrt::pdf_render::{RenderSpec, pdf_to_rgba, render_html_to_pdf};
use std::fs;
use std::path::PathBuf;

// Strip count and box dimensions are chosen to put strip boundaries on
// integer raster pixel positions at 150dpi (1 CSS px = 25/16 raster px).
//
//   margin 32 CSS px → 50.00 raster px (integer)
//   width  400 CSS px → 625.00 raster px (integer)
//   height 192 CSS px → 300.00 raster px (integer)
//   strip   16 CSS px →  25.00 raster px (integer) → 25 strips
//
// This avoids the cairo seam artifact (white bleed-through at fractional-
// pixel rect boundaries) we'd otherwise get from adjacent fills, which would
// inflate the test↔ref diff by ~50 channels along every strip seam column.
//
// Color quantization: with 25 strips the per-pixel smooth-vs-step divergence
// is bounded by half a strip step, i.e. ≈ 0.5 * 255 / 25 ≈ 5 channels.
const STRIP_COUNT: u32 = 25;
const GRADIENT_WIDTH_PX: u32 = 400;
const GRADIENT_HEIGHT_PX: u32 = 192;
const GRADIENT_MARGIN_PX: u32 = 32;

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let av = a as f32;
    let bv = b as f32;
    (av + (bv - av) * t).round().clamp(0.0, 255.0) as u8
}

/// Build a strip-approximation reference HTML. `c0` is the color at the
/// left edge, `c1` at the right edge. The CSS layout (margin / width / height)
/// must mirror `linear-gradient-horizontal.html` so the ref's box geometry
/// overlays the test gradient exactly.
fn build_strip_ref_html(c0: (u8, u8, u8), c1: (u8, u8, u8)) -> String {
    assert_eq!(
        GRADIENT_WIDTH_PX % STRIP_COUNT,
        0,
        "width must divide evenly into strips"
    );
    let strip_w = GRADIENT_WIDTH_PX / STRIP_COUNT;

    let mut strips = String::new();
    for i in 0..STRIP_COUNT {
        // sample at strip midpoint so adjacent smooth gradient pixels deviate
        // symmetrically (max half a strip-step) instead of always biased.
        let t = (i as f32 + 0.5) / STRIP_COUNT as f32;
        let r = lerp_u8(c0.0, c1.0, t);
        let g = lerp_u8(c0.1, c1.1, t);
        let b = lerp_u8(c0.2, c1.2, t);
        let left = i * strip_w;
        strips.push_str(&format!(
            r#"<div style="position:absolute;top:0;bottom:0;left:{left}px;width:{strip_w}px;background:rgb({r},{g},{b});"></div>"#
        ));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT ref: linear-gradient strip approximation</title>
<style>
  html, body {{ margin: 0; padding: 0; background: white; }}
  .box {{ position: relative; width: {w}px; height: {h}px; margin: {m}px; }}
</style>
</head>
<body>
  <div class="box">{strips}</div>
</body>
</html>"#,
        w = GRADIENT_WIDTH_PX,
        h = GRADIENT_HEIGHT_PX,
        m = GRADIENT_MARGIN_PX,
        strips = strips,
    )
}

#[test]
fn linear_gradient_horizontal_matches_strip_reference() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_html_path = crate_root.join("fixtures/paint/linear-gradient-horizontal.html");
    let test_html = fs::read_to_string(&test_html_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", test_html_path.display()));

    let ref_html = build_strip_ref_html((0xe5, 0x39, 0x35), (0x1e, 0x88, 0xe5));

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
        .join("target/vrt-gradient-harness");
    fs::create_dir_all(&work_dir).expect("create work dir");

    // Rasterize test and ref into separate sub-dirs so pdftocairo's
    // hard-coded `page-1.png` output does not race / overwrite. Wipe both
    // sub-dirs first so a stale `page-1.png` from a prior run can't be read
    // back as the current output if rasterization unexpectedly fails.
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

    // Tolerance: 25 strips of 16 CSS px each gives a max smooth-vs-step
    // divergence of ≈ 0.5 * 255 / 25 ≈ 5 channels per pixel, plus 1-2 channels
    // for sRGB rounding. We allow 10 to give comfortable headroom while still
    // catching real implementation regressions.
    //
    // Diff-pixels budget: with strip boundaries pixel-aligned the seam columns
    // disappear; only the raster top/bottom edge AA may exceed the channel
    // tolerance, which is a thin band ≪ 1% of the box area. 0.5% is generous
    // and a true pass should be well under it.
    let tol = Tolerance {
        max_channel_diff: 10,
        max_diff_pixels_ratio: 0.005,
    };

    let report = diff::compare(&ref_img, &test_img, tol);

    assert!(
        report.pass,
        "gradient test↔ref harness failed: {} of {} pixels differ ({:.3}%), max channel diff = {} (tolerance: max_diff={}, ratio<={:.3}%). \
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
