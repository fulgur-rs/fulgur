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
    // After Task 3: m≈170, l≈510 (total ≈680).
    assert!(
        counts.m < 300,
        "expected m < 300 (moveto collapsed into single rect paths), got m={} l={}",
        counts.m,
        counts.l,
    );
    assert!(
        counts.m + counts.l < 900,
        "expected m+l < 900 (rect paths share a single subpath), got m={} l={}",
        counts.m,
        counts.l,
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
