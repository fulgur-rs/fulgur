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
