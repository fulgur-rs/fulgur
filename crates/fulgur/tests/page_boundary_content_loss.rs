use fulgur::engine::Engine;
use std::io::Write;

/// Total glyphs painted across every page of `html`.
///
/// [`fulgur::inspect`] hands back raw glyph ids from the content stream,
/// not decoded text (the fonts are subset and carry no reverse mapping),
/// so the words cannot be read back by name. The count is enough: these
/// tests compare two renders of identical content at identical width, so
/// line breaking is fixed and only pagination differs.
fn glyph_count(html: &str) -> usize {
    let pdf = Engine::builder().build().render(html).expect("render");
    assert!(!pdf.is_empty(), "render produced no bytes");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("out.pdf");
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(&pdf).expect("write");
    drop(f);
    let result = fulgur::inspect::inspect(&path).expect("inspect");
    // Glyph ids are 2 bytes each in these subset fonts.
    result
        .text_items
        .iter()
        .map(|t| t.text.chars().count())
        .sum()
}

fn probe_words(n: usize) -> String {
    (0..n).map(|i| format!("W{i:04} ")).collect()
}

/// Assert that paginating `body` loses nothing.
///
/// Renders it twice at the **same page width** — once on a page tall
/// enough to hold everything (so nothing splits) and once at `page_h` (so
/// it does) — and requires the same number of glyphs. Holding the width
/// fixed keeps line breaking identical between the two, so any difference
/// is pagination dropping content.
fn assert_pagination_loses_nothing(style: &str, body: &str, page_h: u32, ctx: &str) {
    assert_pagination_loses_nothing_at(
        "size: 400px 20000px; margin: 20px;",
        &format!("size: 400px {page_h}px; margin: 20px;"),
        style,
        body,
        ctx,
    );
}

/// As [`assert_pagination_loses_nothing`], but with both `@page` rules
/// given explicitly, for shapes whose margins matter. The two must differ
/// only in page *height*.
fn assert_pagination_loses_nothing_at(
    tall_page: &str,
    short_page: &str,
    style: &str,
    body: &str,
    ctx: &str,
) {
    let tall =
        format!("<!DOCTYPE html><style>@page {{ {tall_page} }} {style}</style><body>{body}</body>");
    let short = format!(
        "<!DOCTYPE html><style>@page {{ {short_page} }} {style}</style><body>{body}</body>"
    );
    let expected = glyph_count(&tall);
    let actual = glyph_count(&short);
    assert!(expected > 0, "{ctx}: reference render painted no glyphs");
    assert_eq!(
        actual,
        expected,
        "{ctx}: paginating dropped {} of {expected} glyphs — content is being \
         lost at a page boundary, silently and with exit code 0",
        expected.saturating_sub(actual)
    );
}

/// A paragraph whose first line cannot fit the remaining strip must move to
/// the next page rather than emit a fragment into a gap too small for it.
///
/// `fragment_inline_root`'s only split rule is "this line overflows", and
/// its orphan guard refuses to close a first fragment shorter than
/// `ORPHANS_MIN` — so with a 10px gap and 28px lines it accumulated and
/// emitted a 56px two-line fragment into that 10px gap.
///
/// This one asserts on **geometry**, not glyph counts, because here the
/// lost lines *are* emitted into the content stream — just painted below
/// the page box, where only a MediaBox-respecting extractor could see
/// them. `inspect` counts them either way, so the glyph invariant used by
/// the other tests in this file is blind to it. The fragment overflowing
/// its own fragmentainer is the defect, and that is directly checkable.
///
/// The sweep matters: only offsets where the box actually straddles the
/// boundary reproduce it, so a single filler value can pass by luck.
#[test]
fn paragraph_that_cannot_fit_its_first_line_moves_to_the_next_page() {
    const PAGE_H: f32 = 1690.0;
    const MARGIN_TOP: f32 = 190.0;
    const MARGIN_BOTTOM: f32 = 80.0;
    // The content strip every fragment must stay inside.
    const STRIP: f32 = PAGE_H - MARGIN_TOP - MARGIN_BOTTOM;
    // Taffy rounds line metrics to whole pixels; tolerate that much.
    const TOLERANCE: f32 = 1.0;

    let words = probe_words(300);
    for filler in [0u32, 1200, 1380, 1400, 1410, 1415, 1420, 1425, 1430] {
        let html = format!(
            r#"<!DOCTYPE html>
<style>
  @page {{ size: 1190px {PAGE_H}px; margin: {MARGIN_TOP}px 40px {MARGIN_BOTTOM}px 40px; }}
  body {{ margin: 0; font-size: 20px; line-height: 1.4; }}
</style>
<body>
  <div style="height:{filler}px"></div>
  <div>HEAD<div><p>{words}</p><p>TAIL</p></div></div>
</body>"#
        );
        let out = Engine::builder().build().layout(&html).expect("layout");
        for (node_id, geom) in out.geometry.iter() {
            if geom.is_repeat {
                // Repeated content (e.g. `position: fixed`) is a full
                // redraw per page, not a slice, so its extent is not a
                // statement about any one strip.
                continue;
            }
            // Only inline roots. A block *container* whose fragment runs
            // past the strip is a separate, pre-existing behaviour (its
            // box is sized from content and clipped at paint), and
            // asserting on it here would fail for reasons this fix has
            // nothing to do with. The defect under test is an inline
            // root's own fragment being placed where its lines cannot
            // fit.
            // Only *multi-line* inline roots: those are what
            // `fragment_inline_root` places, and its placement is the
            // subject here. A single-line paragraph is placed by the
            // block path, where an overflowing box is a separate,
            // pre-existing behaviour this fix does not address.
            let Some(para) = out.drawables.paragraphs.get(node_id) else {
                continue;
            };
            if para.lines.len() < 2 {
                continue;
            }
            for frag in &geom.fragments {
                let bottom = frag.y.to_f32() + frag.height.to_f32();
                assert!(
                    bottom <= STRIP + TOLERANCE,
                    "filler={filler}px: node {node_id} has a fragment running to \
                     {bottom}px on a {STRIP}px content strip — it was emitted into \
                     a gap too small for it, and the lines past the strip are \
                     painted outside the page box and lost"
                );
            }
        }
    }
}

/// A paragraph split at line boundaries must render every line it was
/// split into.
///
/// The fragmenter chose the partition from Parley's line metrics, which
/// are rounded to whole pixels, while `render::paragraph_lines_for_page`
/// *re-derived* that same partition by accumulating `ShapedLine::height`,
/// which keeps the fractional height. Where the two disagreed a line
/// belonged to no fragment at all and was dropped — 220 probe words across
/// the shapes below — until `PaginationGeometry::line_boundaries` published
/// the partition the fragmenter actually chose.
///
/// The line height has to be fractional in points for the two accountings
/// to drift, which is why these combinations are so specific.
#[test]
fn a_paragraph_split_across_pages_keeps_every_line() {
    for (font_px, line_height, n, page_h) in [
        (11.0, 1.5, 400usize, 300u32),
        (13.0, 1.3, 500, 300),
        (9.0, 1.7, 600, 250),
        (15.0, 1.15, 350, 400),
        (11.0, 1.5, 900, 200),
        (12.0, 1.45, 700, 320),
        (10.0, 1.33, 800, 260),
        (14.0, 1.25, 450, 280),
    ] {
        let words = probe_words(n);
        let style =
            format!("body {{ margin: 0; font-size: {font_px}px; line-height: {line_height}; }}");
        let body = format!("<div>{words}</div>");
        assert_pagination_loses_nothing(
            &style,
            &body,
            page_h,
            &format!("font={font_px}px line-height={line_height} page-height={page_h}px"),
        );
    }
}

/// The same divergence reached through the nested walker rather than the
/// body-direct one, with the paragraph starting partway down the page.
#[test]
fn a_nested_paragraph_split_across_pages_keeps_every_line() {
    for (font_px, line_height, n, page_h) in [
        (13.0, 1.3, 500usize, 300u32),
        (9.0, 1.7, 600, 250),
        (12.0, 1.45, 700, 320),
        (10.0, 1.33, 800, 260),
    ] {
        let words = probe_words(n);
        let half = page_h / 2;
        let style =
            format!("body {{ margin: 0; font-size: {font_px}px; line-height: {line_height}; }}");
        let body = format!(
            r#"<div style="height:{half}px"></div>
  <div>H<div><p>{words}</p></div></div>"#
        );
        assert_pagination_loses_nothing(
            &style,
            &body,
            page_h,
            &format!("nested font={font_px}px lh={line_height} page-height={page_h}px"),
        );
    }
}
