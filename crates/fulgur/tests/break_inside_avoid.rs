//! Integration tests for CSS `break-inside: avoid` (fulgur-ftp).

use fulgur::{Engine, PageSize};

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

/// avoid block がページ境界にまたがる → 次ページへ promote。
#[test]
fn avoid_block_straddling_boundary_promotes_to_next_page() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .spacer { height: 160pt; background: #eee; }
        .keep { height: 60pt; background: #c00; break-inside: avoid; }
    </style></head><body>
      <div class="spacer"></div>
      <div class="keep"></div>
    </body></html>"#;
    let engine = Engine::builder()
        // 200pt × 200pt expressed in mm (PageSize::custom_pt not yet available)
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build();
    let pdf = engine.render(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected avoid block to promote to page 2, got {} pages",
        page_count(&pdf)
    );
}

/// 1ページより大きい avoid block は無限ループせず通常 split へ fallback。
///
/// Note: `.huge` has splittable children (rows). An empty `<div style="height:
/// 500pt">` would not exercise the fallback because block splitting cannot
/// synthesise children out of pure CSS-sized boxes; that is a separate
/// concern beyond Task 5's scope.
#[test]
fn avoid_block_taller_than_page_falls_back_to_split() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .huge { break-inside: avoid; }
        .row { height: 80pt; background: #036; }
    </style></head><body>
      <div class="huge">
        <div class="row"></div>
        <div class="row"></div>
        <div class="row"></div>
        <div class="row"></div>
        <div class="row"></div>
        <div class="row"></div>
        <div class="row"></div>
      </div>
    </body></html>"#;
    let engine = Engine::builder()
        // 200pt × 200pt expressed in mm (PageSize::custom_pt not yet available)
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build();
    let pdf = engine.render(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected oversized avoid block to still paginate, got {} pages",
        page_count(&pdf)
    );
}

/// ColumnGroup 内の avoid-child は `distribute` の whole placement で
/// 自動保護される。この挙動を regression-proof する。
#[test]
fn avoid_child_inside_multicol_fits_whole_column() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 300pt 400pt; margin: 10pt; }
        .mc { column-count: 2; column-gap: 10pt; }
        .block { height: 120pt; margin-bottom: 10pt; background: #ddd; }
        .keep { break-inside: avoid; }
    </style></head><body>
      <div class="mc">
        <div class="block"></div>
        <div class="block keep"></div>
        <div class="block"></div>
        <div class="block keep"></div>
      </div>
    </body></html>"#;
    let engine = Engine::builder()
        // 300pt × 400pt expressed in mm (PageSize::custom_pt not yet available)
        .page_size(PageSize::custom(105.8333, 141.1111))
        .build();
    let pdf = engine.render(html).expect("render");
    assert!(page_count(&pdf) >= 1);
    assert!(page_count(&pdf) <= 2);
    assert!(pdf.len() > 500, "PDF looks truncated");
}

/// Number of rendered line boxes in a PDF.
///
/// `fulgur::inspect` reports one text item per drawn line, so this counts
/// lines without needing to decode glyph ids back to Unicode. For the
/// fragmentation tests below that is exactly the quantity at stake: a
/// dropped line is a missing item, and the control document says how many
/// there should be.
fn line_count(pdf: &[u8]) -> usize {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("out.pdf");
    std::fs::write(&path, pdf).expect("write pdf");
    fulgur::inspect::inspect(&path)
        .expect("inspect")
        .text_items
        .len()
}

/// The set of pages that carry at least one line box, in ascending order.
fn pages_with_text(pdf: &[u8]) -> Vec<u32> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("out.pdf");
    std::fs::write(&path, pdf).expect("write pdf");
    let mut pages: Vec<u32> = fulgur::inspect::inspect(&path)
        .expect("inspect")
        .text_items
        .into_iter()
        .map(|item| item.page)
        .collect();
    pages.sort_unstable();
    pages.dedup();
    pages
}

/// Words `w001 … wNNN`, one space apart, as a single long paragraph.
fn numbered_words(count: usize) -> String {
    (1..=count)
        .map(|i| format!("w{i:03}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One long paragraph on a deliberately small page.
///
/// The page is 200pt square so a few hundred words overflow several
/// fragmentainers under *any* fallback font. fulgur does not bundle a font
/// here, so the host decides glyph advances and therefore how many words fit
/// on a line — a fixture tuned to fit A4 on one host overflows nothing on
/// another (this test previously passed on Linux and failed on macOS with
/// 44 lines, i.e. less than a single A4 page).
fn oversized_paragraph(declaration: &str, words: usize) -> Vec<u8> {
    let html = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
            body {{ margin: 0; font-size: 12px; }}
            p {{ margin: 0; {declaration} }}
        </style></head><body><p>{}</p></body></html>"#,
        numbered_words(words)
    );
    Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build()
        .render(&html)
        .expect("render")
}

/// fulgur-a3ek: `break-inside: avoid` on a paragraph taller than one page
/// used to corrupt the paragraph's line boxes.
///
/// `avoid` suppresses the line-split path so the paragraph can be emitted
/// whole. For an oversized paragraph there is no whole to emit: the block
/// path it fell through to cannot split a box whose only children are text
/// nodes. The observed damage depends on the page geometry — lines past the
/// first page bottom silently discarded on a large page, lines duplicated
/// across slice boundaries on a small one — so this asserts the property
/// that covers both: an `avoid` that cannot be honoured must leave the
/// output identical to omitting it. CSS Fragmentation 3 §4.4 is explicit
/// that `avoid` is dropped when honouring it would overflow the
/// fragmentainer.
#[test]
fn oversized_paragraph_with_avoid_keeps_every_line() {
    const WORDS: usize = 900;
    let control_pdf = oversized_paragraph("", WORDS);
    let avoided_pdf = oversized_paragraph("break-inside: avoid;", WORDS);

    // The comparison below is only meaningful while the control paragraph
    // really does overflow. Guard on *pages*, not on a line count: how many
    // words fit on a line depends on the host's fallback font, but "900 words
    // on a 200pt square page spans several pages" holds either way.
    let control_pages = pages_with_text(&control_pdf);
    assert!(
        control_pages.len() >= 3,
        "fixture no longer overflows several pages, got {control_pages:?}"
    );

    let control = line_count(&control_pdf);
    let avoided = line_count(&avoided_pdf);
    assert_eq!(
        avoided, control,
        "an unsatisfiable `break-inside: avoid` changed the line boxes: \
         {avoided} with `avoid` vs {control} without"
    );
}

/// Dropping `avoid` must be limited to the paragraph that cannot be
/// honoured. One that fits inside a page still has to come out on a single
/// page rather than being split across the boundary.
#[test]
fn paragraph_that_fits_a_page_is_still_kept_whole() {
    let words = numbered_words(40);
    let html = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
            @page {{ size: 200pt 200pt; margin: 0; }}
            body {{ margin: 0; font-size: 12px; }}
            .spacer {{ height: 150pt; background: #eee; }}
            p {{ width: 120pt; break-inside: avoid; }}
        </style></head><body><div class="spacer"></div><p>{words}</p></body></html>"#
    );
    let pdf = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build()
        .render(&html)
        .expect("render");

    let pages = pages_with_text(&pdf);
    assert_eq!(
        pages.len(),
        1,
        "a paragraph that fits a page must stay on one page under `avoid`, \
         but its lines landed on pages {pages:?}"
    );
}
