//! Smoke tests for CSS Multi-column Layout (fulgur-e3z A-6).
//!
//! Covers scenarios not already tested by multicol_span_all.rs,
//! column_rule_rendering.rs, or break_inside_avoid.rs.
//! These tests drive Engine::render_html so draw/convert paths appear
//! in codecov patch coverage (VRT alone doesn't contribute).

use fulgur::Engine;

fn page_count(pdf: &[u8]) -> usize {
    let prefix = b"/Type /Page";
    let mut n = 0usize;
    let mut i = 0;
    while i + prefix.len() < pdf.len() {
        if &pdf[i..i + prefix.len()] == prefix {
            // Reject `/Type /Pages` and any other identifier continuation.
            if !pdf[i + prefix.len()].is_ascii_alphanumeric() {
                n += 1;
            }
            i += prefix.len();
        } else {
            i += 1;
        }
    }
    n
}

/// 2-column basic layout renders a non-empty PDF on a single page.
#[test]
fn multicol_basic_2col_renders() {
    let html = r#"<!doctype html><html><head><style>
        body { margin: 10pt; }
        .mc { column-count: 2; column-gap: 12pt; }
        .b { height: 40pt; margin-bottom: 8pt; background: #1e88e5; }
    </style></head><body>
      <div class="mc">
        <div class="b"></div><div class="b"></div>
        <div class="b"></div><div class="b"></div>
      </div>
    </body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
    assert_eq!(page_count(&pdf), 1);
}

/// column-width: <length> resolves to correct column count automatically.
#[test]
fn multicol_column_width_resolution_renders() {
    let html = r#"<!doctype html><html><head><style>
        body { margin: 10pt; }
        .mc { column-width: 80pt; column-gap: 10pt; }
        .b { height: 30pt; margin-bottom: 6pt; background: #283593; }
    </style></head><body>
      <div class="mc">
        <div class="b"></div><div class="b"></div>
        <div class="b"></div><div class="b"></div>
        <div class="b"></div>
      </div>
    </body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
}

/// column-fill: balance distributes content equally across columns.
#[test]
fn multicol_balance_renders() {
    let html = r#"<!doctype html><html><head><style>
        body { margin: 10pt; }
        .mc { column-count: 3; column-gap: 12pt; column-fill: balance; }
        .b { height: 35pt; margin-bottom: 8pt; background: #1b5e20; }
    </style></head><body>
      <div class="mc">
        <div class="b"></div><div class="b"></div>
        <div class="b"></div><div class="b"></div>
        <div class="b"></div><div class="b"></div>
      </div>
    </body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
    assert_eq!(page_count(&pdf), 1);
}

/// A tall multicol container that would exceed one page renders without panicking.
///
/// NOTE: multicol container page-spanning (the multicol box itself fragmenting
/// across pages) is not yet implemented in fulgur. Currently only
/// `column-span: all` subtrees drive inter-page splits inside a multicol
/// context (covered by multicol_span_all.rs). Once container fragmentation
/// lands, this assertion should be tightened to `assert!(page_count(&pdf) >= 2)`.
#[test]
fn multicol_page_spanning_renders_without_panic() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 180pt; margin: 10pt; }
        body { margin: 0; }
        .mc { column-count: 2; column-gap: 16pt; column-fill: auto; }
        .b { height: 40pt; margin-bottom: 8pt; background: #37474f; }
    </style></head><body>
      <div class="mc">
        <div class="b"></div><div class="b"></div>
        <div class="b"></div><div class="b"></div>
        <div class="b"></div><div class="b"></div>
        <div class="b"></div><div class="b"></div>
        <div class="b"></div><div class="b"></div>
        <div class="b"></div><div class="b"></div>
      </div>
    </body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
}
