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

    // Thresholds are measured baseline × ~1.2 safety margin. A regression
    // that disables the rect branch pushes m+l back to ~1500.
    assert!(counts.m < 220, "got m={} l={}", counts.m, counts.l);
    assert!(
        counts.m + counts.l < 800,
        "got m={} l={}",
        counts.m,
        counts.l,
    );
    // Size bound catches content-stream verbosity regressions (redundant
    // gs state, duplicated paths) that operator counts alone miss.
    assert!(pdf.len() < 44_032, "PDF size {} B", pdf.len());
}

#[test]
fn dashed_uniform_border_keeps_per_edge_phase() {
    // Per-edge stroking keeps each edge's dash phase starting at the edge
    // origin, matching how browsers draw CSS dashed borders. Collapsing to
    // a single closed rect path would run the dash phase around the full
    // perimeter and break corner symmetry.
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

    // Double = 2 concentric rect subpaths. Bounds accept both krilla's
    // current m+3l+h decomposition and a future `re`-operator emission.
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

#[test]
fn double_uniform_border_below_3px_falls_back_to_solid() {
    // CSS Backgrounds L3: computed border-width < 3 の double は solid で描く。
    // rect fast path (draw_block_border の Double 分岐) が < 3px のとき
    // 2 本の hairline を emit せず、solid と同じ 1 本の rect 経路になるかを確認する。
    let html = r#"
        <html><head><style>
            .b { width: 200px; height: 100px; border: 2px double #444; }
        </style></head><body><div class="b"></div></body></html>
    "#;

    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render_html(html).unwrap();

    let Some(counts) = count_ops(&pdf) else {
        eprintln!("qpdf not installed — skipping");
        return;
    };

    // Solid と同じく 1 本の rect subpath のみ許容。2 本なら未修正。
    assert!(
        counts.m + counts.re <= 1,
        "double < 3px should collapse to single solid rect, got m={} re={}",
        counts.m,
        counts.re,
    );
}

#[test]
fn double_per_edge_below_3px_falls_back_to_solid() {
    // non-uniform 境界（幅不一致）で rect fast path を避け、
    // draw_border_line の Double arm を通す経路。
    // < 3px のとき hairline 2 本ではなく単一 solid stroke になるか検証する。
    let html = r#"
        <html><head><style>
            .b {
                width: 200px;
                height: 100px;
                border-style: double;
                border-top-width: 2px;
                border-right-width: 4px;
                border-bottom-width: 2px;
                border-left-width: 4px;
                border-color: #444;
            }
        </style></head><body><div class="b"></div></body></html>
    "#;

    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render_html(html).unwrap();

    let Some(counts) = count_ops(&pdf) else {
        eprintln!("qpdf not installed — skipping");
        return;
    };

    // 修正前: 4 辺とも double で 2 本ずつ = 8 本の線 (m=8)。
    // 修正後: top/bottom (2px) は solid fallback で 1 本ずつ = 2 本、
    //         left/right (4px) は double のまま 2 本ずつ = 4 本、合計 6 本。
    // m <= 6 で未修正状態と区別できる。
    eprintln!("per-edge double: m={} l={}", counts.m, counts.l);
    assert!(
        counts.m <= 6,
        "double < 3px per-edge should reduce stroke count, got m={} l={}",
        counts.m,
        counts.l,
    );
}
