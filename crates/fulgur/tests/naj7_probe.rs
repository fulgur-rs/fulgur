//! Reachability probes for the PR #710 review findings (epic `fulgur-naj7`).
//!
//! These are *verification* tests, not regression guards: each one asserts the
//! geometry that the review claimed, so running the file on unmodified `pr710`
//! tells us which findings are live bugs and which are defensive hardening.
//!
//! Run with:
//!   cargo test -p fulgur --test naj7_probe -- --nocapture

use fulgur::{Engine, Margin, PageSize};

/// 200x100 CSS px content box == 150x75 pt, matching the fixtures the review
/// measured against.
fn engine_200x100() -> Engine {
    Engine::builder()
        .page_size(PageSize {
            width: 150.0,
            height: 75.0,
        })
        .margin(Margin::uniform(0.0))
        .build()
}

/// `(page_index, y, height)` triples for the node whose `id` attribute matches,
/// looked up through the table drawables.
fn table_fragments(layout: &fulgur::LayoutOutput, dom_id: &str) -> Vec<(u32, f32, f32)> {
    let node_id = layout
        .drawables
        .tables
        .iter()
        .find(|(_, t)| t.id.as_deref().map(|s| s.as_str()) == Some(dom_id))
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("no table drawable with id={dom_id}"));
    fragment_triples(layout, node_id)
}

fn fragment_triples(layout: &fulgur::LayoutOutput, node_id: usize) -> Vec<(u32, f32, f32)> {
    layout
        .geometry
        .get(&node_id)
        .map(|g| {
            g.fragments
                .iter()
                .map(|f| (f.page_index, f.y.to_f32(), f.height.to_f32()))
                .collect()
        })
        .unwrap_or_default()
}

/// Blocks are looked up by `id` too — cells carry their content as blocks.
fn block_fragments(layout: &fulgur::LayoutOutput, dom_id: &str) -> Vec<(u32, f32, f32)> {
    let node_id = layout
        .drawables
        .block_styles
        .iter()
        .find(|(_, b)| b.id.as_deref().map(|s| s.as_str()) == Some(dom_id))
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("no block drawable with id={dom_id}"));
    fragment_triples(layout, node_id)
}

// ---------------------------------------------------------------------------
// Finding #3 / #5 / #6 — one instrument, three answers.
//
// Discriminating question: is there any (table, page) where the table's
// fragment height is 0 AND some id in `table.clip_descendants` has a fragment
// on that same page? If not, the `draw_under_clip_table` early return can never
// drop live cells, and finding #3 is hardening rather than a live bug.
// ---------------------------------------------------------------------------

/// The nested-oversized fixture from `table_header_test.rs`, with the inner
/// table given `overflow: hidden` so its cells route through
/// `draw_under_clip_table`.
const NESTED_OVERSIZED_CLIPPED: &str = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  #lead { height: 60px; }
  table { margin: 0; border-spacing: 0; width: 100px; }
  th, td { box-sizing: border-box; padding: 0; }
  #outer-header { height: 20px; }
  #inner { background: rgb(255, 0, 0); overflow: hidden; }
  #inner-header { height: 80px; }
  #inner-body { height: 30px; }
</style></head><body>
  <div id="lead"></div>
  <table><thead><tr><th id="outer-header">Outer</th></tr></thead><tbody><tr><td>
    <table id="inner"><thead><tr><th id="inner-header">Inner</th></tr></thead>
      <tbody><tr><td id="inner-body">Body</td></tr></tbody></table>
  </td></tr></tbody></table>
</body></html>"#;

/// A tall clipped table split across pages, no nesting — the ordinary
/// repeating-header shape with `overflow: hidden`.
const TALL_CLIPPED: &str = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  table { margin: 0; border-spacing: 0; width: 100px; overflow: hidden; }
  th, td { box-sizing: border-box; padding: 0; height: 20px; }
</style></head><body>
  <table id="t"><thead><tr><th>H</th></tr></thead><tbody>
    <tr><td>01</td></tr><tr><td>02</td></tr><tr><td>03</td></tr>
    <tr><td>04</td></tr><tr><td>05</td></tr><tr><td>06</td></tr>
    <tr><td>07</td></tr><tr><td>08</td></tr><tr><td>09</td></tr>
  </tbody></table>
</body></html>"#;

#[test]
fn probe_zero_height_table_fragment_never_coexists_with_live_cells() {
    let engine = engine_200x100();
    let mut coexisting: Vec<String> = Vec::new();

    for (name, html) in [
        ("nested-oversized-clipped", NESTED_OVERSIZED_CLIPPED),
        ("tall-clipped", TALL_CLIPPED),
    ] {
        let layout = engine.layout(html).expect("layout should succeed");

        for (&table_id, table) in layout.drawables.tables.iter() {
            if table.clip_descendants.is_empty() {
                continue;
            }
            let Some(geom) = layout.geometry.get(&table_id) else {
                continue;
            };
            for frag in &geom.fragments {
                if frag.height > fulgur::units::Px::ZERO {
                    continue;
                }
                // Zero-height slice on this page. Any live descendant here?
                let live: Vec<usize> = table
                    .clip_descendants
                    .iter()
                    .copied()
                    .filter(|desc| {
                        layout.geometry.get(desc).is_some_and(|dg| {
                            dg.fragments
                                .iter()
                                .any(|df| df.page_index == frag.page_index)
                        })
                    })
                    .collect();
                println!(
                    "[{name}] table={table_id} page={} h=0 live_clip_descendants={}",
                    frag.page_index,
                    live.len()
                );
                if !live.is_empty() {
                    coexisting.push(format!(
                        "{name}: table={table_id} page={} has {} live clipped descendants \
                         under a zero-height fragment",
                        frag.page_index,
                        live.len()
                    ));
                }
            }
        }
    }

    // This assertion encodes the VERDICT, and is expected to be revisited once
    // finding #5 (prev_height degeneracy) is fixed: if that fix lets a
    // zero-height parent fragment survive alongside live descendants, this
    // flips and finding #3 becomes a live bug.
    assert!(
        coexisting.is_empty(),
        "finding #3 is REACHABLE — zero-height table fragments coexist with live \
         clipped cells:\n{}",
        coexisting.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Finding #1 — a row's non-first cell loses `initial_page_occupied` and
// overflows the page bottom.
//
// Review measurement: right-tall = (page 0, y=80, h=30) -> bottom 110 on a
// 100px page. With the `thead { display: table-row-group }` opt-out it lands
// on (page 1, y=30).
// ---------------------------------------------------------------------------

const TWO_CELL_ROW: &str = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  #lead { height: 60px; }
  table { margin: 0; border-spacing: 0; width: 200px; }
  th, td { box-sizing: border-box; padding: 0; vertical-align: top; }
  #hdr { height: 20px; }
  #left-short { height: 5px; background: rgb(1,1,1); }
  #left-tall { height: 30px; background: rgb(2,2,2); }
  #right-tall { height: 30px; background: rgb(3,3,3); }
  THEAD_OVERRIDE
</style></head><body>
  <div id="lead"></div>
  <table>
    <thead><tr><th id="hdr" colspan="2">H</th></tr></thead>
    <tbody><tr>
      <td><div id="left-short"></div><div id="left-tall"></div></td>
      <td><div id="right-tall"></div></td>
    </tr></tbody>
  </table>
</body></html>"#;

#[test]
fn probe_finding1_non_first_cell_overflows_page_bottom() {
    let engine = engine_200x100();

    let repeating = TWO_CELL_ROW.replace("THEAD_OVERRIDE", "");
    let opted_out = TWO_CELL_ROW.replace("THEAD_OVERRIDE", "thead { display: table-row-group; }");

    let l_rep = engine.layout(&repeating).expect("layout (repeating)");
    let l_opt = engine.layout(&opted_out).expect("layout (opted out)");

    let rep_right = block_fragments(&l_rep, "right-tall");
    let opt_right = block_fragments(&l_opt, "right-tall");
    let rep_left = block_fragments(&l_rep, "left-tall");

    println!("[finding1] repeating right-tall = {rep_right:?}");
    println!("[finding1] repeating left-tall  = {rep_left:?}");
    println!("[finding1] opted-out right-tall = {opt_right:?}");

    // The page content box is 100 CSS px tall. Any fragment whose bottom
    // exceeds that is painted outside the page.
    let overflow: Vec<_> = rep_right
        .iter()
        .filter(|(_, y, h)| y + h > 100.0 + 0.01)
        .collect();
    assert!(
        overflow.is_empty(),
        "finding #1 REPRODUCED — right-tall overflows the page bottom: {overflow:?} \
         (opt-out places it at {opt_right:?})"
    );
}

// ---------------------------------------------------------------------------
// Finding #2 — `table-header-group` that is not the table's first child.
//
// css-tables-3: "If a table owns multiple display:table-header-group boxes,
// only the first is treated as a header; the others are treated as if they had
// display:table-row-group."
//
// Review measurement: table fragments = [(0, y=0, h=120), (1, h=100),
// (2, h=100)] — page 0 exceeds the 100px page box.
// ---------------------------------------------------------------------------

const HEADER_GROUP_NOT_FIRST: &str = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  table { margin: 0; border-spacing: 0; width: 100px; }
  td, th { box-sizing: border-box; padding: 0; height: 20px; }
  #mid { display: table-header-group; }
</style></head><body>
  <table id="t">
    <tbody><tr><td>a1</td></tr><tr><td>a2</td></tr></tbody>
    <tbody id="mid"><tr><td>H</td></tr></tbody>
    <tbody><tr><td>b1</td></tr><tr><td>b2</td></tr><tr><td>b3</td></tr><tr><td>b4</td></tr></tbody>
  </table>
</body></html>"#;

#[test]
fn probe_finding2_header_group_not_first_blows_up_page_height() {
    let engine = engine_200x100();
    let layout = engine.layout(HEADER_GROUP_NOT_FIRST).expect("layout");
    let frags = table_fragments(&layout, "t");
    println!("[finding2] table fragments = {frags:?}");

    let too_tall: Vec<_> = frags.iter().filter(|(_, _, h)| *h > 100.0 + 0.01).collect();
    assert!(
        too_tall.is_empty(),
        "finding #2 REPRODUCED — table fragment taller than the 100px page: {too_tall:?} \
         (all fragments: {frags:?})"
    );
}

// ---------------------------------------------------------------------------
// Finding #4 — a page that carries the repeated header but no body row.
//
// Review measurement: h = [(0, y=60, h=20), (1, y=0, h=20)] with both body
// rows on page 1 — page 0 shows an orphaned header.
// ---------------------------------------------------------------------------

const HEADER_ONLY_PAGE: &str = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  #lead { height: 60px; }
  table { margin: 0; border-spacing: 0; width: 100px; }
  th, td { box-sizing: border-box; padding: 0; }
  #hdr { height: 20px; }
  .row { height: 30px; background: rgb(4,4,4); }
</style></head><body>
  <div id="lead"></div>
  <table id="t">
    <thead><tr><th id="hdr">H</th></tr></thead>
    <tbody>
      <tr><td><div class="row" id="r1"></div></td></tr>
      <tr><td><div class="row" id="r2"></div></td></tr>
    </tbody>
  </table>
</body></html>"#;

#[test]
fn probe_finding4_no_header_only_page() {
    let engine = engine_200x100();
    let layout = engine.layout(HEADER_ONLY_PAGE).expect("layout");

    let table = table_fragments(&layout, "t");
    let r1 = block_fragments(&layout, "r1");
    let r2 = block_fragments(&layout, "r2");
    println!("[finding4] table = {table:?}");
    println!("[finding4] r1 = {r1:?}  r2 = {r2:?}");

    // A page carrying a table fragment but no body-row fragment is a
    // header-only page.
    let body_pages: std::collections::BTreeSet<u32> =
        r1.iter().chain(r2.iter()).map(|(p, _, _)| *p).collect();
    let header_only: Vec<u32> = table
        .iter()
        .map(|(p, _, _)| *p)
        .filter(|p| !body_pages.contains(p))
        .collect();

    assert!(
        header_only.is_empty(),
        "finding #4 REPRODUCED — pages {header_only:?} carry the table band but no body row \
         (table={table:?}, body pages={body_pages:?})"
    );
}

// ===========================================================================
// Round 2 probes — findings #5 #6 #7 #8 #9 #12.
// ===========================================================================

// ---------------------------------------------------------------------------
// Finding #8 (beads fulgur-naj7.10) — row co-split is only enabled for tables
// that have a repeating header, so a plain table's row cells stack vertically
// instead of staying side by side across a page break.
// ---------------------------------------------------------------------------

const TWO_CELL_ROW_NO_THEAD: &str = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  #lead { height: 60px; }
  table { margin: 0; border-spacing: 0; width: 200px; }
  td { box-sizing: border-box; padding: 0; vertical-align: top; }
  #left-tall { height: 60px; background: rgb(2,2,2); }
  #right-tall { height: 60px; background: rgb(3,3,3); }
</style></head><body>
  <div id="lead"></div>
  <table>
    <tbody><tr>
      <td><div id="left-tall"></div></td>
      <td><div id="right-tall"></div></td>
    </tr></tbody>
  </table>
</body></html>"#;

#[test]
fn probe_finding8_row_cosplit_without_thead() {
    let engine = engine_200x100();
    let layout = engine.layout(TWO_CELL_ROW_NO_THEAD).expect("layout");
    let left = block_fragments(&layout, "left-tall");
    let right = block_fragments(&layout, "right-tall");
    println!("[finding8] left  = {left:?}");
    println!("[finding8] right = {right:?}");

    // Two cells of the SAME row must occupy the same y band on each page they
    // share. If the row is not co-split they stack: right starts where left
    // ended instead of alongside it.
    for (lp, ly, _) in &left {
        for (rp, ry, _) in &right {
            if lp == rp {
                assert!(
                    (ly - ry).abs() < 0.01,
                    "finding #8 REPRODUCED — same-row cells not side by side on page {lp}: \
                     left y={ly}, right y={ry} (left={left:?}, right={right:?})"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Finding #5 (part of fulgur-naj7.3) — the parent fragment is dropped when
// `occupied_page_extent` is 0, so a table's background/border vanishes from a
// page where its cells still live.
// ---------------------------------------------------------------------------

#[test]
fn probe_finding5_parent_fragment_present_wherever_cells_are() {
    let engine = engine_200x100();
    let mut orphans: Vec<String> = Vec::new();

    for (name, html) in [
        ("nested-oversized-clipped", NESTED_OVERSIZED_CLIPPED),
        ("tall-clipped", TALL_CLIPPED),
        ("header-group-not-first", HEADER_GROUP_NOT_FIRST),
    ] {
        let layout = engine.layout(html).expect("layout");
        for (&table_id, table) in layout.drawables.tables.iter() {
            if table.clip_descendants.is_empty() {
                continue;
            }
            let table_pages: std::collections::BTreeSet<u32> = layout
                .geometry
                .get(&table_id)
                .map(|g| g.fragments.iter().map(|f| f.page_index).collect())
                .unwrap_or_default();
            let cell_pages: std::collections::BTreeSet<u32> = table
                .clip_descendants
                .iter()
                .filter_map(|d| layout.geometry.get(d))
                .flat_map(|g| g.fragments.iter().map(|f| f.page_index))
                .collect();
            let missing: Vec<u32> = cell_pages.difference(&table_pages).copied().collect();
            println!(
                "[finding5][{name}] table={table_id} table_pages={table_pages:?} \
                 cell_pages={cell_pages:?}"
            );
            if !missing.is_empty() {
                orphans.push(format!(
                    "{name}: table={table_id} has cells on pages {missing:?} \
                     but no fragment of its own there"
                ));
            }
        }
    }

    assert!(
        orphans.is_empty(),
        "finding #5 REPRODUCED — table box missing on pages where its cells live:\n{}",
        orphans.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Finding #6 (fulgur-naj7.4) — `fragmented_descendant_page_extent` ignores the
// node's own fragment, so a cell's padding-bottom falls out of the table's
// band height and the outer frame is cut short.
// ---------------------------------------------------------------------------

const CELL_PADDING_BOTTOM: &str = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  #lead { height: 60px; }
  table { margin: 0; border-spacing: 0; width: 100px; overflow: hidden;
          background: rgb(9,9,9); }
  th { box-sizing: border-box; padding: 0; height: 20px; }
  td { box-sizing: border-box; padding: 0 0 20px 0; }
  .inner { height: 30px; background: rgb(5,5,5); }
</style></head><body>
  <div id="lead"></div>
  <table id="t">
    <thead><tr><th>H</th></tr></thead>
    <tbody>
      <tr><td><div class="inner" id="c1"></div></td></tr>
      <tr><td><div class="inner" id="c2"></div></td></tr>
    </tbody>
  </table>
</body></html>"#;

#[test]
fn probe_finding6_table_box_covers_cell_padding() {
    let engine = engine_200x100();
    let layout = engine.layout(CELL_PADDING_BOTTOM).expect("layout");
    let table = table_fragments(&layout, "t");
    let c1 = block_fragments(&layout, "c1");
    let c2 = block_fragments(&layout, "c2");
    println!("[finding6] table = {table:?}");
    println!("[finding6] c1 = {c1:?}  c2 = {c2:?}");

    // The table's fragment on each page must reach at least as far down as its
    // deepest content on that page (padding-bottom means it should reach
    // FURTHER, but "not shorter" is the falsifiable part).
    for (tp, ty, th) in &table {
        let content_bottom = c1
            .iter()
            .chain(c2.iter())
            .filter(|(p, _, _)| p == tp)
            .map(|(_, y, h)| y + h)
            .fold(f32::NEG_INFINITY, f32::max);
        if content_bottom.is_finite() {
            assert!(
                ty + th >= content_bottom - 0.01,
                "finding #6 REPRODUCED — table box on page {tp} ends at {} but content \
                 reaches {content_bottom} (table={table:?})",
                ty + th
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Finding #7 (fulgur-naj7.7) — the header template bypasses the fragmenter, so
// a forced break inside a header cell is silently dropped.
// ---------------------------------------------------------------------------

const FORCED_BREAK_IN_HEADER: &str = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  table { margin: 0; border-spacing: 0; width: 100px; }
  th, td { box-sizing: border-box; padding: 0; }
  #brk { height: 10px; break-after: page; background: rgb(7,7,7); }
  td { height: 20px; }
</style></head><body>
  <table id="t">
    <thead><tr><th><div id="brk"></div></th></tr></thead>
    <tbody><tr><td>a</td></tr><tr><td>b</td></tr></tbody>
  </table>
</body></html>"#;

#[test]
fn probe_finding7_forced_break_inside_header_cell() {
    let engine = engine_200x100();
    let pdf = engine.render(FORCED_BREAK_IN_HEADER).expect("render");
    let doc = lopdf::Document::load_mem(&pdf).expect("parse");
    let pages = doc.get_pages().len();
    let layout = engine.layout(FORCED_BREAK_IN_HEADER).expect("layout");
    println!("[finding7] pages = {pages}");
    println!("[finding7] brk = {:?}", block_fragments(&layout, "brk"));

    // `break-after: page` inside the header cell must push the body to a new
    // page — i.e. more than one page for a table that otherwise fits on one.
    assert!(
        pages > 1,
        "finding #7 REPRODUCED — break-after:page inside a header cell was dropped \
         (rendered {pages} page(s))"
    );
}

// ---------------------------------------------------------------------------
// Finding #9 (fulgur-naj7.9) — `is_repeat` is no longer exclusive to
// `position: fixed`; a multicol container inside a header cell breaks the
// invariant that render.rs:1357 relies on.
// ---------------------------------------------------------------------------

const MULTICOL_IN_HEADER: &str = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  table { margin: 0; border-spacing: 0; width: 100px; }
  th, td { box-sizing: border-box; padding: 0; }
  #mc { columns: 2; column-gap: 0; height: 30px; background: rgb(8,8,8); }
  td { height: 20px; }
</style></head><body>
  <table id="t">
    <thead><tr><th><div id="mc"><p>x</p><p>y</p><p>z</p></div></th></tr></thead>
    <tbody>
      <tr><td>1</td></tr><tr><td>2</td></tr><tr><td>3</td></tr><tr><td>4</td></tr>
      <tr><td>5</td></tr><tr><td>6</td></tr><tr><td>7</td></tr><tr><td>8</td></tr>
    </tbody>
  </table>
</body></html>"#;

#[test]
fn probe_finding9_multicol_inside_repeated_header() {
    let engine = engine_200x100();
    let layout = engine.layout(MULTICOL_IN_HEADER).expect("layout");
    let mc = block_fragments(&layout, "mc");
    println!("[finding9] mc fragments = {mc:?}");

    // Report whether any geometry carries is_repeat=true for a multicol node —
    // that is the combination the render.rs comment says cannot happen.
    let mc_id = layout
        .drawables
        .block_styles
        .iter()
        .find(|(_, b)| b.id.as_deref().map(|s| s.as_str()) == Some("mc"))
        .map(|(id, _)| *id);
    if let Some(id) = mc_id {
        if let Some(g) = layout.geometry.get(&id) {
            let repeats = g.fragments.len();
            let is_repeat = g.is_repeat;
            println!("[finding9] mc geometry: fragments={repeats} is_repeat={is_repeat}");
            assert!(
                !is_repeat,
                "finding #9 REPRODUCED — a multicol container carries is_repeat=true, \
                 breaking the invariant render.rs:1357 relies on"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Finding #12 (fulgur-naj7.2) — the inner `fragment_height > 0` branch of
// `table_box_size` needs `layout_size == None` on an unsplit table. Probe
// whether `layout_size` is ever None in practice.
// ---------------------------------------------------------------------------

#[test]
fn probe_finding12_table_layout_size_always_present() {
    let engine = engine_200x100();
    let mut missing: Vec<String> = Vec::new();
    for (name, html) in [
        ("nested-oversized-clipped", NESTED_OVERSIZED_CLIPPED),
        ("tall-clipped", TALL_CLIPPED),
        ("header-group-not-first", HEADER_GROUP_NOT_FIRST),
        ("cell-padding-bottom", CELL_PADDING_BOTTOM),
    ] {
        let layout = engine.layout(html).expect("layout");
        for (&id, t) in layout.drawables.tables.iter() {
            if t.layout_size.is_none() {
                let split = layout.geometry.get(&id).is_some_and(|g| g.is_split());
                missing.push(format!("{name}: table={id} layout_size=None split={split}"));
            }
        }
    }
    println!(
        "[finding12] tables with layout_size=None: {}",
        missing.len()
    );
    for m in &missing {
        println!("[finding12]   {m}");
    }
    assert!(
        missing.is_empty(),
        "finding #12: layout_size=None occurs in practice, so the inner branch is \
         reachable:\n{}",
        missing.join("\n")
    );
}
