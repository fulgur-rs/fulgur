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
                        layout
                            .geometry
                            .get(desc)
                            .is_some_and(|dg| {
                                dg.fragments.iter().any(|df| df.page_index == frag.page_index)
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
    let opted_out =
        TWO_CELL_ROW.replace("THEAD_OVERRIDE", "thead { display: table-row-group; }");

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
