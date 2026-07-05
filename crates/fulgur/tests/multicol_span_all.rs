//! Integration test for CSS `column-span: all` (fulgur-0vd).
//!
//! Covers the "SpanAll child that itself page-breaks" acceptance case
//! from the issue. When a `column-span: all` subtree is larger than one
//! page, the existing BlockPageable pagination must split the full-width
//! block across pages cleanly — no column structure leaks into the spill.

use fulgur::{Engine, PageSize};

/// Lightweight PDF page counter. Parses the PDF with lopdf, which
/// transparently decompresses krilla 0.8's object streams / compressed xref
/// (raw-byte `/Type /Page` scanning no longer works once the page dicts live
/// inside object streams). `get_pages()` returns exactly the leaf page
/// objects, excluding the `/Type /Pages` tree root.
fn page_count(pdf_bytes: &[u8]) -> usize {
    lopdf::Document::load_mem(pdf_bytes)
        .expect("load PDF for page count")
        .get_pages()
        .len()
}

#[test]
fn span_all_subtree_that_exceeds_one_page_splits_across_pages() {
    let mut long = String::new();
    for _ in 0..40 {
        long.push_str(
            "<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
             Sed do eiusmod tempor incididunt ut labore et dolore magna \
             aliqua. Ut enim ad minim veniam, quis nostrud exercitation.</p>",
        );
    }
    let html = format!(
        r#"<!doctype html><html><head><style>
            body {{ margin: 10pt; font-size: 10pt; }}
            .mc {{ column-count: 2; column-gap: 10pt; }}
            .span {{ column-span: all; }}
        </style></head><body>
          <div class="mc">
            <p>before column content.</p>
            <section class="span">{long}</section>
            <p>after column content.</p>
          </div>
        </body></html>"#,
        long = long
    );

    // A6 (105 x 148 mm) — small enough that 40 paragraphs of Lorem ipsum
    // inside a `column-span: all` subtree cannot fit on a single page.
    let engine = Engine::builder()
        .page_size(PageSize::custom(105.0, 148.0))
        .build();
    let pdf = engine.render(&html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected >=2 pages from oversized SpanAll, got {}",
        page_count(&pdf)
    );
}

#[test]
fn span_all_fits_single_page_for_short_content() {
    let html = r#"<!doctype html><html><head><style>
        body { margin: 10pt; font-size: 10pt; }
        .mc { column-count: 2; column-gap: 10pt; }
    </style></head><body>
      <div class="mc">
        <p>a</p><p>b</p>
        <h1 style="column-span: all;">title</h1>
        <p>c</p><p>d</p>
      </div>
    </body></html>"#;

    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render(html).expect("render");
    assert_eq!(page_count(&pdf), 1, "short content should fit one A4 page");
}
