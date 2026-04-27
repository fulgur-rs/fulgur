//! Integration tests for CSS page-break-after / page-break-before wiring (fulgur-lje5).
//!
//! The split algorithm (`BlockPageable::find_split_point`) now detects forced
//! breaks at arbitrary nesting depth by recursively calling `split()` on each
//! child even when the child fits within the available page height. This means
//! both the 4-div overflow scenario and the simpler 2-div non-overflow scenario
//! correctly honour `page-break-after: always` / `break-after: page`.

use fulgur::{Engine, Margin, PageSize};

fn page_count(pdf: &[u8]) -> usize {
    let prefix = b"/Type /Page";
    let mut count = 0usize;
    let mut i = 0;
    while i + prefix.len() < pdf.len() {
        if &pdf[i..i + prefix.len()] == prefix {
            let next = pdf[i + prefix.len()];
            if !next.is_ascii_alphanumeric() {
                count += 1;
            }
            i += prefix.len();
        } else {
            i += 1;
        }
    }
    count
}

/// `page-break-after: always` on the first div forces an extra page split.
///
/// Without the break property, 4 × 80pt = 320pt across a 200pt page gives
/// 2 pages ([d1,d2][d3,d4]).  With break-after on d1 the split at index 1
/// fires first → [d1][d2,d3,d4] → wrap triggers another split → 3 pages.
#[test]
fn page_break_after_always_splits_pages() {
    let html = r#"<!doctype html><html><head><style>
        body { margin: 0; }
        .d1 { height: 80pt; page-break-after: always; }
        .d2 { height: 80pt; }
        .d3 { height: 80pt; }
        .d4 { height: 80pt; }
    </style></head><body>
      <div class="d1"></div>
      <div class="d2"></div>
      <div class="d3"></div>
      <div class="d4"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 3,
        "expected page-break-after: always to force >= 3 pages, got {}",
        page_count(&pdf)
    );
}

/// `break-after: page` (CSS Fragmentation Level 3) also forces the extra split.
#[test]
fn break_after_page_splits_pages() {
    let html = r#"<!doctype html><html><head><style>
        body { margin: 0; }
        .d1 { height: 80pt; break-after: page; }
        .d2 { height: 80pt; }
        .d3 { height: 80pt; }
        .d4 { height: 80pt; }
    </style></head><body>
      <div class="d1"></div>
      <div class="d2"></div>
      <div class="d3"></div>
      <div class="d4"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 3,
        "expected break-after: page to force >= 3 pages, got {}",
        page_count(&pdf)
    );
}

/// `page-break-before: always` on the second div forces an extra split.
///
/// `break_before` requires `i > 0 && pc.y > 0`, so d2 (index 1, y = 80pt)
/// is the first position where the condition can fire.  Result:
/// [d1][d2,d3,d4] → overflow → [d1][d2,d3][d4] = 3 pages.
#[test]
fn page_break_before_always_splits_pages() {
    let html = r#"<!doctype html><html><head><style>
        body { margin: 0; }
        .d1 { height: 80pt; }
        .d2 { height: 80pt; page-break-before: always; }
        .d3 { height: 80pt; }
        .d4 { height: 80pt; }
    </style></head><body>
      <div class="d1"></div>
      <div class="d2"></div>
      <div class="d3"></div>
      <div class="d4"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 3,
        "expected page-break-before: always to force >= 3 pages, got {}",
        page_count(&pdf)
    );
}

/// `break-before: page` (CSS Fragmentation Level 3) also forces the extra split.
#[test]
fn break_before_page_splits_pages() {
    let html = r#"<!doctype html><html><head><style>
        body { margin: 0; }
        .d1 { height: 80pt; }
        .d2 { height: 80pt; break-before: page; }
        .d3 { height: 80pt; }
        .d4 { height: 80pt; }
    </style></head><body>
      <div class="d1"></div>
      <div class="d2"></div>
      <div class="d3"></div>
      <div class="d4"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 3,
        "expected break-before: page to force >= 3 pages, got {}",
        page_count(&pdf)
    );
}

/// Without any break property, 2 × 80pt = 160pt < 200pt fits on exactly 1 page.
/// This control case confirms the break tests above detect forced splits, not overflow.
#[test]
fn no_break_property_stays_on_one_page() {
    let html = r#"<!doctype html><html><head><style>
        body { margin: 0; }
        .first { height: 80pt; }
        .second { height: 80pt; }
    </style></head><body>
      <div class="first"></div>
      <div class="second"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert_eq!(
        page_count(&pdf),
        1,
        "without break properties, both divs should fit on 1 page, got {}",
        page_count(&pdf)
    );
}

/// 2つの 80pt ブロックが 200pt ページに収まるが `page-break-after: always` で強制分割される。
///
/// overflow がない場合の強制改ページの基本ケース: `find_split_point` が直接の子要素の
/// `pagination()` だけでなく子孫も再帰的に確認することで正しく動作する。
#[test]
fn page_break_after_forces_split_when_both_fit_on_one_page() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .first { height: 80pt; page-break-after: always; }
        .second { height: 80pt; }
    </style></head><body>
      <div class="first"></div>
      <div class="second"></div>
    </body></html>"#;
    // PageSize::custom takes mm; 70.5556 mm × (72 / 25.4) ≈ 200 pt — matches @page rule.
    // Both 80pt divs (160pt total) fit comfortably on this 200pt page, so the split
    // is driven purely by the forced break, not by overflow.
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "page-break-after: always should force a split even when both divs fit on one page, got {} pages",
        page_count(&pdf)
    );
}
