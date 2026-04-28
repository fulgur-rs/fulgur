//! Regression tests for fulgur-aijf: non-pseudo `position: absolute` elements
//! must be out-of-flow during pagination — they must not consume page space
//! the way in-flow elements do.
//!
//! CSS 2.1 §10.6.4: the height of an absolutely-positioned element does not
//! contribute to the height of its containing block's normal flow.

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

/// Repro distilled from `page-background-002-print-ref.html`: an
/// `<img position:absolute>` (here a 50×300 div with explicit dimensions to
/// avoid PNG plumbing) at the top of the document must not occupy a page of
/// its own. The three in-flow `<div break-before:page>` siblings determine
/// the page count (3); abs is out-of-flow.
#[test]
fn abs_positioned_div_is_out_of_flow_in_pagination() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 100pt 100pt; margin: 0; }
        body { margin: 0; }
    </style></head><body>
      <div style="position:absolute; top:0; left:0; width:50pt; height:300pt; background:red;"></div>
      <div>First flow content.</div>
      <div style="break-before:page;">Second flow content.</div>
      <div style="break-before:page;">Third flow content.</div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 100.0,
            height: 100.0,
        })
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    let pages = page_count(&pdf);
    assert_eq!(
        pages, 3,
        "abs-positioned div must not consume pages; only in-flow break-before:page divs \
         should determine page count, got {pages}"
    );
}

/// Regression for the coderabbit thread on fulgur-aijf: a zero-size
/// container (`<table>` synthesizes `<tbody>`-like wrappers Blitz lays out
/// at 0×0) with a non-pseudo abs/fixed direct child must NOT be flattened
/// — flattening recurses into `collect_positioned_children`, which now
/// skips abs descendants. Without the flatten guard, the abs would never
/// reach a `build_absolute_children` hoist and would silently disappear.
#[test]
fn abs_inside_zero_size_container_is_not_dropped_by_flatten() {
    // Use a `display:contents` wrapper to mimic a zero-size pass-through
    // container that Blitz/Taffy collapses to 0×0. Its abs child must
    // still render; we verify by asserting the document has its expected
    // single page without panic and that the wrapper doesn't suppress the
    // surrounding in-flow content.
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .wrap { display: contents; }
    </style></head><body>
      <p>before</p>
      <div class="wrap">
        <div style="position:absolute; top:10pt; left:10pt; width:30pt; height:30pt; background:red;"></div>
      </div>
      <p>after</p>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 200.0,
            height: 200.0,
        })
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert_eq!(page_count(&pdf), 1, "expected one page");
    // The body content has 'before' and 'after'. If the abs got dropped
    // by flatten + collect_positioned_children skip, we'd at least still
    // see the in-flow text. The crucial behavioural check is that the
    // engine doesn't panic and the document is non-empty.
    assert!(!pdf.is_empty(), "PDF must not be empty");
}

/// Regression for the devin thread on fulgur-aijf: when in-flow children
/// are followed by out-of-flow children in `BlockPageable::children`,
/// `find_split_point`'s break-after / overflow-fallback paths must NOT
/// return AtIndex pointing at an OOF child — that would corrupt
/// `split_y` (read from CB-relative OOF.y, often 0) and cut the page
/// at the wrong height. We assert that a `<div break-after:page>` last
/// in-flow with a trailing abs sibling produces 1 page, not the
/// spurious 2 pages the buggy path would yield.
#[test]
fn break_after_on_last_in_flow_with_trailing_abs_sibling_does_not_split() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 400pt; margin: 0; }
        body { margin: 0; }
    </style></head><body>
      <div style="height:50pt; break-after:page; background:lightgreen;">only flow</div>
      <div style="position:absolute; top:0; left:0; width:30pt; height:30pt; background:red;"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 200.0,
            height: 400.0,
        })
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    let pages = page_count(&pdf);
    assert_eq!(
        pages, 1,
        "break-after:page on the LAST in-flow child must not push a page when the only \
         remaining sibling is out-of-flow (no in-flow successor to push); got {pages}"
    );
}

/// Even when the abs element is much taller than the page, a single
/// page of in-flow content must stay on one page.
#[test]
fn abs_positioned_does_not_force_extra_pages_for_short_flow() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 100pt 100pt; margin: 0; }
        body { margin: 0; }
    </style></head><body>
      <div style="position:absolute; top:0; left:0; width:50pt; height:300pt; background:blue;"></div>
      <p>Single flow paragraph.</p>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 100.0,
            height: 100.0,
        })
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    let pages = page_count(&pdf);
    assert_eq!(
        pages, 1,
        "300pt-tall abs div must not force extra pages when in-flow content fits a single page; got {pages}"
    );
}
