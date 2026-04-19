mod support;
use support::content_stream::count_ops;

use fulgur::config::PageSize;
use fulgur::engine::Engine;
use std::path::PathBuf;

fn render_example(name: &str) -> Vec<u8> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(name);
    let html = std::fs::read_to_string(root.join("index.html")).unwrap();

    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .base_path(root)
        .build();

    engine
        .render_html(&html)
        .expect("render_html should succeed")
}

#[test]
fn table_header_uses_rect_for_uniform_borders() {
    let pdf = render_example("table-header");
    let Some(counts) = count_ops(&pdf) else {
        eprintln!("qpdf not installed — skipping");
        return;
    };

    // Task 3 collapses 4 abutting strokes per cell into one rect path.
    // krilla 0.7 does not emit the PDF `re` operator — `PathBuilder::push_rect`
    // decomposes to `m + 3l + h`. We measure the real win via combined
    // line-segment count rather than rect count.
    // Baseline (pre-Task-3): m=822, l=670 (total 1492).
    // Measured 2026-04-19: m=170, l=510, m+l=680, PDF=35,125 B.
    // Thresholds below are measured × ~1.2 safety margin, rounded.
    assert!(
        counts.m < 220,
        "expected m < 220 (measured 170; rect branch regressed?), got m={} l={}",
        counts.m,
        counts.l,
    );
    assert!(
        counts.m + counts.l < 800,
        "expected m+l < 800 (measured 680; rect consolidation regressed?), got m={} l={}",
        counts.m,
        counts.l,
    );
    // PDF-size bound catches regressions in content-stream verbosity that
    // wouldn't show up via operator counts alone (e.g. redundant gs state
    // or duplicated paths without new m/l ops).
    assert!(
        pdf.len() < 44_032,
        "expected PDF size < 44032 B (measured 35125 B × 1.25), got {} B",
        pdf.len(),
    );
}

#[test]
fn dashed_uniform_border_keeps_per_edge_phase() {
    // Dashed/dotted borders MUST stay on the 4-line fallback so each edge's
    // dash phase starts from the edge origin (per-edge symmetry, matching
    // browsers). Collapsing to a single closed rect path would let dash
    // phase run continuously around the perimeter, breaking corner
    // symmetry. See VRT basic/borders.html and plan Task 4 for the revert
    // rationale.
    let html = r#"
        <html><head><style>
            .b { width: 200px; height: 100px; border: 3px dashed #333; }
        </style></head><body><div class="b"></div></body></html>
    "#;

    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render_html(html).unwrap();

    let Some(counts) = count_ops(&pdf) else {
        eprintln!("qpdf not installed — skipping");
        return;
    };

    assert!(
        counts.m >= 4 && counts.l >= 4,
        "dashed borders must stay on 4-line path for CSS per-edge phase conformance, got m={} l={}",
        counts.m,
        counts.l,
    );
}

#[test]
fn double_uniform_border_uses_two_rects() {
    let html = r#"
        <html><head><style>
            .b { width: 200px; height: 100px; border: 9px double #444; }
        </style></head><body><div class="b"></div></body></html>
    "#;

    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render_html(html).unwrap();

    let Some(counts) = count_ops(&pdf) else {
        eprintln!("qpdf not installed — skipping");
        return;
    };

    // Double = 2 closed rect subpaths. In krilla 0.7 this decomposes to
    // 2 × (m + 3l + h), so m=2, l=6. A future krilla upgrade emitting
    // actual `re` operators would produce m=0, l=0, re=2 — accept both.
    // We pin "exactly 2 rect subpaths" via a combined count.
    assert!(
        counts.m <= 2 && counts.l <= 6 && counts.re <= 2,
        "expected at most 2 rect subpaths, got m={} l={} re={}",
        counts.m,
        counts.l,
        counts.re,
    );
    assert!(
        counts.m + counts.re >= 2,
        "expected 2 rect subpaths (m or re >= 2 total), got m={} re={}",
        counts.m,
        counts.re,
    );
}
