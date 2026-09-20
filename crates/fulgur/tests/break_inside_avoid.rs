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

/// Build a document where `.keep` straddles the page boundary and *has
/// splittable children*, so `break-inside: avoid` changes the output.
///
/// The children matter: with a single childless box, fulgur already moves
/// the whole box to the next page when it does not fit, so `avoid` and no
/// declaration produce identical bytes and the comparison proves nothing.
/// Three 30pt rows after a 140pt spacer split 2/1 across the boundary
/// unless `avoid` holds them together.
fn render_keep_block(declaration: &str) -> Vec<u8> {
    let html = format!(
        r#"<!doctype html><html><head><style>
        @page {{ size: 200pt 200pt; margin: 0; }}
        body {{ margin: 0; }}
        .spacer {{ height: 140pt; background: #eee; }}
        .keep {{ {declaration} }}
        .row {{ height: 30pt; background: #036; }}
    </style></head><body>
      <div class="spacer"></div>
      <div class="keep">
        <div class="row"></div>
        <div class="row"></div>
        <div class="row"></div>
      </div>
    </body></html>"#
    );
    Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build()
        .render(&html)
        .expect("render")
}

/// fulgur-bodp: `page-break-inside: avoid` is the CSS 2.1 spelling and is
/// what wkhtmltopdf-era documents actually contain. It used to do nothing —
/// the sniffer aliased `page-break-before` and `page-break-after` but not
/// `page-break-inside` — so a ported document silently lost every
/// "keep this together" hint.
#[test]
fn legacy_page_break_inside_behaves_like_break_inside() {
    let modern = render_keep_block("break-inside: avoid;");
    let legacy = render_keep_block("page-break-inside: avoid;");
    let neither = render_keep_block("");

    // Guard the fixture itself: if `avoid` stopped mattering here, the
    // equality assertion below would pass for the wrong reason.
    assert_ne!(
        modern, neither,
        "fixture no longer discriminates — `break-inside: avoid` must change \
         this document's pagination for the alias comparison to mean anything"
    );
    assert_eq!(
        legacy, modern,
        "`page-break-inside: avoid` must render identically to `break-inside: avoid`"
    );
}

/// `page-break-inside: auto` is the legacy way to spell the initial value
/// and must not accidentally behave like `avoid`.
#[test]
fn legacy_page_break_inside_auto_matches_modern_auto() {
    assert_eq!(
        render_keep_block("page-break-inside: auto;"),
        render_keep_block("break-inside: auto;"),
    );
    assert_eq!(
        render_keep_block("page-break-inside: auto;"),
        render_keep_block(""),
        "`auto` is the initial value, so it must match no declaration at all"
    );
}
