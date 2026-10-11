//! Repetition contract for `PaginationGeometry::is_repeat`.
//!
//! `is_split()` is `!is_repeat && fragments.len() > 1`, and `render.rs` gates
//! multicol partitioning on it. Two producers set `is_repeat`:
//! `append_position_fixed_fragments` and `append_repeated_header_fragments` —
//! so a multicol container inside a repeating table header carries
//! `is_repeat = true` with one fragment per page.
//!
//! The contract those fragments must satisfy: they are **copies of the whole
//! container, not slices of it**. Every page draws the full column content, and
//! `is_split()` stays false so the renderer never partitions a copy as if it
//! were a slice. `render.rs` once justified its gate by claiming `position:
//! fixed` was the only producer of `is_repeat`; repeating headers falsified
//! that premise while leaving the behaviour correct, and this test is what
//! keeps the behaviour pinned now that the old rationale is gone.
//!
//! Investigated under `fulgur-naj7.12`.

use fulgur::{Engine, Margin, PageSize};

/// Header carries a 2-column multicol with three distinguishable paragraphs;
/// the body is long enough to span four pages.
const MULTICOL_IN_HEADER: &str = r#"<!doctype html>
<html><head><style>
  html, body { margin: 0; padding: 0; }
  table { margin: 0; border-spacing: 0; width: 100px; }
  th, td { box-sizing: border-box; padding: 0; }
  #mc { columns: 2; column-gap: 0; height: 30px; }
  #mc p { margin: 0; font-size: 8px; }
  td { height: 20px; font-size: 8px; }
</style></head><body>
  <table id="t">
    <thead><tr><th><div id="mc"><p>AAA</p><p>BBB</p><p>CCC</p></div></th></tr></thead>
    <tbody>
      <tr><td>r1</td></tr><tr><td>r2</td></tr><tr><td>r3</td></tr><tr><td>r4</td></tr>
      <tr><td>r5</td></tr><tr><td>r6</td></tr><tr><td>r7</td></tr><tr><td>r8</td></tr>
    </tbody>
  </table>
</body></html>"#;

fn engine_200x100() -> Engine {
    Engine::builder()
        .page_size(PageSize {
            width: 150.0,
            height: 75.0,
        })
        .margin(Margin::uniform(0.0))
        .build()
}

#[test]
fn repeated_header_multicol_is_copied_not_split() {
    let engine = engine_200x100();

    // Geometry side: what the pagination pass believes.
    let layout = engine.layout(MULTICOL_IN_HEADER).expect("layout");
    let mc_id = layout
        .drawables
        .block_styles
        .iter()
        .find(|(_, b)| b.id.as_deref().map(|s| s.as_str()) == Some("mc"))
        .map(|(id, _)| *id)
        .expect("multicol drawable");
    let mc_geom = layout.geometry.get(&mc_id).expect("multicol geometry");
    println!(
        "[spike] mc: is_repeat={} is_split={} fragments={:?}",
        mc_geom.is_repeat,
        mc_geom.is_split(),
        mc_geom
            .fragments
            .iter()
            .map(|f| (f.page_index, f.y.to_f32(), f.height.to_f32()))
            .collect::<Vec<_>>()
    );

    // Render side: what actually lands on each page.
    let pdf = engine.render(MULTICOL_IN_HEADER).expect("render");
    let dir = std::env::temp_dir().join("naj7-spike");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("multicol-header.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");

    let inspected = fulgur::inspect::inspect(&path).expect("inspect");
    println!("[spike] pages = {}", inspected.pages);

    let mut per_page: std::collections::BTreeMap<u32, Vec<String>> =
        std::collections::BTreeMap::new();
    for item in &inspected.text_items {
        per_page
            .entry(item.page)
            .or_default()
            .push(item.text.trim().to_string());
    }
    for (page, texts) in &per_page {
        println!("[spike] page {page}: {texts:?}");
    }

    // `inspect` yields raw glyph ids here rather than decoded text (the subset
    // font carries no usable ToUnicode mapping for this fixture), so compare
    // glyph sequences instead of looking for "AAA". That is the stronger claim
    // anyway: the header's items must be *identical* on every page, which is
    // exactly what "repetition, not split" means.
    let pages: Vec<u32> = per_page.keys().copied().collect();
    assert!(
        pages.len() >= 3,
        "table must span at least 3 pages: {pages:?}"
    );

    // Derive the repeated set instead of assuming it is the first three items:
    // ordering and item count depend on font shaping and subsetting, and page 0
    // also carries body text. What the contract actually claims is that some
    // non-empty set of items appears identically on every page — that set is the
    // header, and computing it as the intersection makes no positional
    // assumption.
    //
    // Note this compares glyph sequences, not decoded text: the subset font
    // carries no usable ToUnicode mapping here, which is also why the fixture
    // depends on the bundled font configuration rather than system fonts.
    let mut repeated: std::collections::BTreeSet<String> =
        per_page[&pages[0]].iter().cloned().collect();
    for page in &pages[1..] {
        let on_page: std::collections::BTreeSet<String> = per_page[page].iter().cloned().collect();
        repeated = repeated.intersection(&on_page).cloned().collect();
    }
    assert!(
        !repeated.is_empty(),
        "no text item survives on every page, so nothing is being repeated: \
         {per_page:?}"
    );

    // The body rows differ per page, so the repeated set must be strictly
    // smaller than any single page's contents — otherwise we are comparing
    // pages that happen to be identical rather than isolating the header.
    assert!(
        repeated.len() < per_page[&pages[0]].len(),
        "every item repeats, so the fixture is not exercising a split body: \
         {per_page:?}"
    );

    // And the geometry must stay a repetition, never a split: partitioning a
    // repeated container per page is what `render.rs:1362` must NOT do.
    assert!(mc_geom.is_repeat, "multicol in a repeated header repeats");
    assert!(
        !mc_geom.is_split(),
        "a repeated container must not be treated as split"
    );
}
