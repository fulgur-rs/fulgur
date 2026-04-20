//! End-to-end pixel-sampling tests for fulgur-v7a.
//!
//! These integration tests render a small multicol HTML through fulgur,
//! rasterise the PDF via `pdftocairo` (poppler-utils), decode the PNG and
//! then sample pixels to verify:
//!
//! 1. `column-rule: 4pt solid red` paints a red vertical stripe in the
//!    column-gap region.
//! 2. `column-fill: auto` leaves the second column empty when the content
//!    fits entirely in column 1.
//!
//! Byte-scanning the PDF is not sufficient — the rules are drawn paths
//! inside a content stream, so we need an actual raster to assert their
//! presence. When `pdftocairo` is not installed (common on local dev
//! boxes), the tests print a skip message to stderr and return cleanly.

use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;
use image::RgbaImage;
use std::path::Path;
use std::process::Command;

// ─── Test-infrastructure helpers ─────────────────────────────────────────

/// Is `pdftocairo` on PATH and executable? We probe with `-v` because it
/// returns non-zero on some poppler builds for `--help` but always prints
/// a version banner to stderr for `-v` and exits 0.
fn pdftocairo_available() -> bool {
    Command::new("pdftocairo")
        .arg("-v")
        .output()
        .map(|o| o.status.success() || !o.stderr.is_empty())
        .unwrap_or(false)
}

/// Rasterise page 1 of `pdf_bytes` at `dpi` and return the resulting RGBA
/// image. `work_dir` is used for intermediate `fixture.pdf` / `page-1.png`
/// files; the caller owns its lifecycle (usually a `tempfile::TempDir`).
fn pdf_to_rgba(pdf_bytes: &[u8], dpi: u32, work_dir: &Path) -> RgbaImage {
    let pdf_path = work_dir.join("fixture.pdf");
    std::fs::write(&pdf_path, pdf_bytes).expect("write pdf");

    let prefix = work_dir.join("page");
    let status = Command::new("pdftocairo")
        .args(["-png", "-r", &dpi.to_string(), "-f", "1", "-l", "1"])
        .arg(&pdf_path)
        .arg(&prefix)
        .status()
        .expect("spawn pdftocairo");
    assert!(status.success(), "pdftocairo exited with {status}");

    let png_path = work_dir.join("page-1.png");
    image::open(&png_path)
        .expect("decode rasterised png")
        .to_rgba8()
    // NOTE: the tempdir is cleaned up by the caller dropping the
    // TempDir; if a test fails mid-flight the files are preserved
    // inside the OS tempdir for post-mortem inspection until the TempDir
    // guard drops at test exit.
}

/// Matches a "loosely red" pixel — tolerant of anti-aliasing at the rule
/// edges where the rasteriser blends red with the white page background.
fn is_red_ish(p: &image::Rgba<u8>) -> bool {
    p[0] > 200 && p[1] < 80 && p[2] < 80
}

/// True iff any pixel in the vertical strip centred at `x_center ± x_tol`
/// and bounded by `[y_lo, y_hi)` is red-ish.
fn has_red_pixel_in_strip(
    rgba: &RgbaImage,
    x_center: u32,
    x_tol: u32,
    y_lo: u32,
    y_hi: u32,
) -> bool {
    let x_start = x_center.saturating_sub(x_tol);
    let x_end = (x_center + x_tol).min(rgba.width().saturating_sub(1));
    let y_start = y_lo.min(rgba.height());
    let y_end = y_hi.min(rgba.height());
    for y in y_start..y_end {
        for x in x_start..=x_end {
            if is_red_ish(rgba.get_pixel(x, y)) {
                return true;
            }
        }
    }
    false
}

// ─── Test 1: column-rule paints a red stripe in the column gap ──────────

#[test]
fn column_rule_solid_red_is_visible_between_columns() {
    if !pdftocairo_available() {
        eprintln!(
            "skipping column_rule_solid_red_is_visible_between_columns: pdftocairo not installed"
        );
        return;
    }

    // Layout math (page coords, in pt):
    //   @page 200×200, margin 10 → body content-box is 180×180.
    //   column-count 2, column-gap 20 → each column = 80pt wide.
    //   Column 1 spans x ∈ [10, 90], gap spans [90, 110], column 2 [110, 190].
    //   Rule centre x = 100pt.
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 10pt; }
        body { margin: 0; color: black; font-family: sans-serif; }
        .mc {
            column-count: 2;
            column-gap: 20pt;
            column-rule: 4pt solid red;
        }
        .mc p { margin: 0 0 8pt 0; }
    </style></head><body>
      <div class="mc">
        <p>Aaa aaa aaa aaa aaa aaa aaa.</p>
        <p>Bbb bbb bbb bbb bbb bbb bbb.</p>
        <p>Ccc ccc ccc ccc ccc ccc ccc.</p>
      </div>
    </body></html>"#;

    let engine = Engine::builder()
        .page_size(PageSize {
            width: 200.0,
            height: 200.0,
        })
        .margin(Margin::uniform(10.0))
        .build();
    let pdf = engine.render_html(html).expect("render");

    let tmp = tempfile::tempdir().expect("tempdir");
    let rgba = pdf_to_rgba(&pdf, 200, tmp.path());

    // Derive pixel coords from the actual raster dims (rasterisers may
    // round up or add a fringe pixel; always trust the image over the
    // arithmetic).
    let w = rgba.width() as f32;
    let h = rgba.height() as f32;
    let page_w_pt: f32 = 200.0;
    let page_h_pt: f32 = 200.0;

    // Rule x-centre: body_left (10) + col1_w (80) + gap/2 (10) = 100 pt.
    let gap_center_pt: f32 = 100.0;
    let x_center = ((gap_center_pt / page_w_pt) * w) as u32;

    // Y range covering the multicol's vertical extent. The rule is drawn
    // between adjacent non-empty columns only, so keep the range well
    // inside the content flow (skip top 15pt of margin + breathing room).
    let y_lo = ((30.0_f32 / page_h_pt) * h) as u32;
    let y_hi = ((170.0_f32 / page_h_pt) * h) as u32;

    // ±3 px tolerance on x to absorb rasteriser AA on the 4pt-wide stroke.
    assert!(
        has_red_pixel_in_strip(&rgba, x_center, 3, y_lo, y_hi),
        "expected a red column-rule pixel around x={x_center}±3, y={y_lo}..{y_hi} \
         (image {}×{}); dump left at {:?}",
        rgba.width(),
        rgba.height(),
        tmp.path(),
    );
}

// ─── Test 2: column-fill: auto leaves column 2 empty for short content ──

#[test]
fn column_fill_auto_leaves_second_column_empty_for_short_content() {
    if !pdftocairo_available() {
        eprintln!(
            "skipping column_fill_auto_leaves_second_column_empty_for_short_content: pdftocairo not installed"
        );
        return;
    }

    // Layout math (pt):
    //   @page 300×300, margin 20 → body content-box 260×260.
    //   column-count 2, column-gap 20 → each column = 120pt wide.
    //   Column 1 spans x ∈ [20, 140], gap [140, 160], column 2 [160, 280].
    //   Column 2 mid x = 220pt. Single 40pt-tall black block goes in col 1
    //   only under `column-fill: auto`.
    let html = r#"<!doctype html><html><head><style>
        @page { size: 300pt 300pt; margin: 20pt; }
        body { margin: 0; }
        .mc {
            column-count: 2;
            column-gap: 20pt;
            column-fill: auto;
        }
        .mc p { height: 40pt; background: black; margin: 0 0 8pt 0; color: white; }
    </style></head><body>
      <div class="mc">
        <p>tiny</p>
      </div>
    </body></html>"#;

    let engine = Engine::builder()
        .page_size(PageSize {
            width: 300.0,
            height: 300.0,
        })
        .margin(Margin::uniform(20.0))
        .build();
    let pdf = engine.render_html(html).expect("render");

    let tmp = tempfile::tempdir().expect("tempdir");
    let rgba = pdf_to_rgba(&pdf, 200, tmp.path());

    let w = rgba.width() as f32;
    let h = rgba.height() as f32;
    let page_w_pt: f32 = 300.0;
    let page_h_pt: f32 = 300.0;

    // Column-2 centre x in pt = body_left (20) + col1_w (120) + gap (20) + col2_w/2 (60) = 220.
    let x_center_pt: f32 = 220.0;
    let x_center = ((x_center_pt / page_w_pt) * w) as u32;

    // Sample near the top of column 2, well inside the column's vertical
    // extent (30pt down from the page top — ~10pt into the content box).
    let y_pt: f32 = 30.0;
    let y = ((y_pt / page_h_pt) * h) as u32;

    let p = rgba.get_pixel(x_center, y);
    assert!(
        p[0] > 240 && p[1] > 240 && p[2] > 240,
        "expected white pixel at col-2 top (pt {x_center_pt}×{y_pt} → px {x_center}×{y}), \
         got rgba={:?}; dump left at {:?}",
        p,
        tmp.path(),
    );
}
