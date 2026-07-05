//! Regression tests for fulgur-aijf: non-pseudo `position: absolute` elements
//! must be out-of-flow during pagination — they must not consume page space
//! the way in-flow elements do.
//!
//! CSS 2.1 §10.6.4: the height of an absolutely-positioned element does not
//! contribute to the height of its containing block's normal flow.

mod support;
use support::content_stream::text_matrix_ys;

use fulgur::{Engine, Margin, PageSize};

fn page_count(pdf: &[u8]) -> usize {
    lopdf::Document::load_mem(pdf)
        .expect("load PDF for page count")
        .get_pages()
        .len()
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
    let pdf = engine.render(html).expect("render");
    let pages = page_count(&pdf);
    assert_eq!(
        pages, 3,
        "abs-positioned div must not consume pages; only in-flow break-before:page divs \
         should determine page count, got {pages}"
    );
}

/// Count `BlockEntry` instances in `Drawables.block_styles` whose
/// `layout_size` approximately matches `(target_w, target_h)` (in pt).
/// Used by the flatten-zero-size-container regression test below to
/// verify the abs child reached the Drawables map (rather than being
/// silently dropped by the convert path's flatten branch).
///
/// PR 8i: the v2 Drawables surface is flat — there is no `out_of_flow`
/// flag to discriminate against. Presence in `block_styles` with the
/// expected `layout_size` is the structural invariant we pin here.
fn count_blocks_with_size(
    drawables: &fulgur::drawables::Drawables,
    target_w: f32,
    target_h: f32,
) -> usize {
    drawables
        .block_styles
        .values()
        .filter(|entry| {
            entry.layout_size.is_some_and(|s| {
                (s.width.to_f32() - target_w).abs() < 0.5
                    && (s.height.to_f32() - target_h).abs() < 0.5
            })
        })
        .count()
}

/// Regression for the coderabbit thread on fulgur-aijf: a zero-size
/// container with a non-pseudo abs/fixed direct child must NOT be
/// flattened — flattening recurses into `collect_positioned_children`,
/// which now skips abs descendants. Without the flatten guard, the abs
/// would never reach a `build_absolute_children` hoist and would
/// silently disappear from the Pageable tree.
///
/// `assert!(!pdf.is_empty())` and `page_count == 1` are *not* sufficient
/// oracles here — both stay true even when the abs child is dropped,
/// because krilla always serialises a complete PDF and the surrounding
/// in-flow text alone fills one page. We instead inspect the Pageable
/// tree directly and assert a 30×30 pt out-of-flow `BlockPageable`
/// (the abs `<div>`) is present (PR #260, CodeRabbit).
#[test]
fn abs_inside_zero_size_container_is_not_dropped_by_flatten() {
    // A `<div>` with explicit `height:0; width:0;` and `overflow:visible`
    // is a real zero-size container that Blitz lays out at 0×0 and that
    // `collect_positioned_children`'s flatten branch would otherwise
    // collapse — recursing into its children with no parent to pick the
    // abs back up. Without the flatten guard, the abs `<div>` is silently
    // dropped from the Pageable tree.
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .zero { width: 0; height: 0; overflow: visible; }
    </style></head><body>
      <p>before</p>
      <div class="zero">
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
    let drawables = engine.build_drawables_for_testing_no_gcpm(html);
    let abs_blocks = count_blocks_with_size(&drawables, 30.0, 30.0);
    assert!(
        abs_blocks >= 1,
        "expected at least one 30x30 pt BlockEntry in Drawables for the abs <div>; \
         the convert flatten guard must not drop it. Found {abs_blocks}."
    );
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
    let pdf = engine.render(html).expect("render");
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
    let pdf = engine.render(html).expect("render");
    let pages = page_count(&pdf);
    assert_eq!(
        pages, 1,
        "300pt-tall abs div must not force extra pages when in-flow content fits a single page; got {pages}"
    );
}

/// fulgur-puml 原因①: nested abs (abs 内 abs) の高さがページ数を駆動する。
#[test]
fn nested_abs_height_drives_page_count() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 100pt 100pt; margin: 0; }
        body { margin: 0; }
    </style></head><body>
      <div style="position:absolute;">
        outer
        <div style="position:absolute; top:0; height:300vh; width:50px;"></div>
      </div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 100.0,
            height: 100.0,
        })
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render(html).expect("render");
    assert_eq!(
        page_count(&pdf),
        3,
        "nested abs height:300vh must drive 3 pages"
    );
}

/// fulgur-xa9q: in-flow があっても abs はページ拡張できるべき (Chrome 準拠)。
/// fulgur-puml では未対応 — naive な may_extend 緩和は fixedpos-008 /
/// page-background-003 を regress させると bisect で判明したため別 issue に分離。
#[test]
fn abs_extends_pages_despite_in_flow_content() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 100pt 100pt; margin: 0; }
        body { margin: 0; }
    </style></head><body>
      in-flow text here
      <div style="position:absolute; bottom:-200vh;">x</div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 100.0,
            height: 100.0,
        })
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render(html).expect("render");
    assert_eq!(
        page_count(&pdf),
        3,
        "abs bottom:-200vh must extend to 3 pages even with in-flow content"
    );
}

/// fulgur-puml trap A: nested abs の offset は CB 基準で解決される。
#[test]
fn nested_abs_offset_resolves_against_cb_not_flow() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 100pt 100pt; margin: 0; }
        body { margin: 0; }
    </style></head><body>
      <div style="position:absolute; top:100vh;">
        outer on page 2
        <div style="position:absolute; top:300vh;">inner on page 5</div>
      </div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 100.0,
            height: 100.0,
        })
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render(html).expect("render");
    assert_eq!(
        page_count(&pdf),
        5,
        "inner abs top:300vh under outer top:100vh must land on page 5 (400vh)"
    );
}

/// fulgur-puml (CB = nearest positioned ancestor, addressing coderabbit/gemini
/// review): when a nested abs's immediate parent is `position:static`, its
/// containing block is the nearest POSITIONED ancestor, not the static parent
/// (CSS 2.1 §10.1.4). Here B's `top:0` is relative to A (the abs), so B starts
/// on page 1 and its `height:300vh` drives exactly 3 pages — even though a
/// 200pt spacer pushes B's static wrapper down. The buggy immediate-parent
/// anchoring would start B at 200pt and inflate the page count to 5.
#[test]
fn nested_abs_cb_is_nearest_positioned_ancestor_not_static_parent() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 100pt 100pt; margin: 0; }
        body { margin: 0; }
    </style></head><body>
      <div style="position:absolute; top:0;">
        <div style="height:200pt;"></div>
        <div>
          <div style="position:absolute; top:0; height:300vh; width:10px;"></div>
        </div>
      </div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 100.0,
            height: 100.0,
        })
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render(html).expect("render");
    assert_eq!(
        page_count(&pdf),
        3,
        "nested abs under a static parent must anchor to the positioned ancestor (A, top:0), \
         not the static parent's 200pt flow offset"
    );
}

/// fulgur-puml (CB = positioned ancestor's PADDING box, addressing coderabbit
/// re-review): a nested abs `top:0` sits at the ancestor's padding edge, so a
/// top border on the ancestor pushes it down by the border width. Compare
/// with vs without the border — the nested text baseline must shift by ~the
/// border (a border-box CB would leave it unmoved). 40px border = 30pt.
#[test]
fn nested_abs_cb_uses_positioned_ancestor_padding_box() {
    let render = |border: &str| -> Vec<u8> {
        let html = format!(
            r#"<!doctype html><html><head><style>
            @page {{ size: 200pt 200pt; margin: 0; }}
            body {{ margin: 0; }}
        </style></head><body>
          <div style="position:absolute; top:0; {border} width:100px; height:100px;">
            <div style="position:absolute; top:0;">X</div>
          </div>
        </body></html>"#
        );
        let engine = Engine::builder()
            .page_size(PageSize {
                width: 200.0,
                height: 200.0,
            })
            .margin(Margin::uniform(0.0))
            .build();
        engine.render(&html).expect("render")
    };
    let pdf_plain = render("");
    let pdf_border = render("border-top: 40px solid black;");
    let (Some(ys_plain), Some(ys_border)) =
        (text_matrix_ys(&pdf_plain), text_matrix_ys(&pdf_border))
    else {
        eprintln!("qpdf not installed — skipping");
        return;
    };
    assert_eq!(ys_plain.len(), 1, "one text run expected, got {ys_plain:?}");
    assert_eq!(
        ys_border.len(),
        1,
        "one text run expected, got {ys_border:?}"
    );
    let delta = ys_border[0] - ys_plain[0];
    // 40px top border = 30pt; the padding-box CB pushes the nested abs down by
    // it. A border-box CB would give delta ~= 0.
    assert!(
        (28.0..=32.0).contains(&delta),
        "padding-box CB must shift the nested abs down by ~30pt (40px border); \
         got delta {delta} (plain={}, border={})",
        ys_plain[0],
        ys_border[0]
    );
}

/// fulgur-puml (end-side margin fix C): an end-anchored absolute element
/// positions its *margin box* against the CB edge, so `bottom:0;
/// margin-bottom:N` must sit N above a plain `bottom:0` sibling. Before the
/// fix `resolve_viewport_cb_location` dropped the end-side margin and both
/// collapsed onto the same baseline (the fixedpos-004/005/006 ref-side
/// overlap that blocked promotion). This is a distinct bug from nested-abs
/// pagination — assert the vertical offset directly via the text matrices.
#[test]
fn abs_bottom_margin_offsets_above_sibling() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 100pt 100pt; margin: 0; }
        body { margin: 0; }
    </style></head><body>
      <div style="position:absolute; bottom:0; margin-bottom:30pt;">A</div>
      <div style="position:absolute; bottom:0;">B</div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize {
            width: 100.0,
            height: 100.0,
        })
        .margin(Margin::uniform(0.0))
        .build();
    let pdf = engine.render(html).expect("render");

    let Some(ys) = text_matrix_ys(&pdf) else {
        eprintln!("qpdf not installed — skipping");
        return;
    };
    // Exactly the two single-glyph runs "A" and "B".
    assert_eq!(ys.len(), 2, "expected two text runs, got {ys:?}");
    let gap = (ys[0] - ys[1]).abs();
    // margin-bottom:30pt must separate the two baselines by ~30pt. The bug
    // (margin dropped) collapses them to gap ~= 0.
    assert!(
        (29.0..=31.0).contains(&gap),
        "bottom:0 + margin-bottom:30pt must sit ~30pt above a bottom:0 sibling; got baselines {ys:?} (gap {gap})"
    );
}

/// fulgur-xa9q: a nested `position: absolute` whose start lands beyond the
/// in-flow page budget must (1) EXTEND the page count and (2) actually RENDER
/// its text on the extended page — cleanly anchored at the page top, not split
/// across a page boundary by the accumulated `Nvh` sub-px residual.
///
/// This guards the blind spot that hid the bug through fulgur-puml: the prior
/// `nested_abs_height_drives_page_count` asserted ONLY the page count, so the
/// nested text could (and did) silently vanish while the count looked right.
/// Uses the DEFAULT page (no `@page`) so the Stylo `Nvh` vs `page_h_px`
/// residual is present — the exact condition that previously mis-paged the
/// line onto a boundary and clipped it away.
#[test]
fn nested_abs_text_renders_cleanly_on_extended_page() {
    use support::content_stream::{count_ops, text_matrix_ys};
    let html = r#"<!doctype html><html><head><style>body{margin:0}</style></head><body>
      <div style="height:200vh;">flow</div>
      <div style="position:absolute; top:100vh;">
        <div style="position:absolute; top:200vh;">nestedtext</div>
      </div>
    </body></html>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render(html).expect("render");

    // in-flow 200vh = pages 1-2; nested abs at 100vh+200vh = 300vh extends to
    // the 4th page (its start is the top of page 4).
    assert_eq!(
        page_count(&pdf),
        4,
        "nested abs at effective 300vh must extend the page count to 4"
    );

    // Exactly two text runs render: the in-flow "flow" and the nested
    // "nestedtext". A dropped nested line would give 1; a boundary-split line
    // would give 3. Skips gracefully when qpdf is unavailable.
    if let Some(ops) = count_ops(&pdf) {
        assert_eq!(
            ops.bt, 2,
            "expected exactly 2 text runs (flow + nested); nested text was dropped or split"
        );
    }

    // Both runs sit at the top of their page: the nested text's baseline must
    // equal the in-flow text's page-top baseline (the layer-2b snap), proving
    // it is not offset onto a page boundary.
    if let Some(ys) = text_matrix_ys(&pdf) {
        assert_eq!(ys.len(), 2, "expected two text-run baselines");
        assert!(
            (ys[0] - ys[1]).abs() < 0.5,
            "nested abs text must share the in-flow text's page-top baseline; got {ys:?}"
        );
    }
}
