# Multicol — split single inline-root paragraph across columns

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to
> implement this plan task-by-task.

**Goal:** Make `<div column-count=2>long-text-one-paragraph</div>` render
with the first half of the lines in column 0 and the remainder in column 1.

**Architecture:** The Taffy multicol hook (`compute_multicol_layout` /
`layout_column_group` in `crates/fulgur/src/multicol_layout.rs`) decides
per-line splits during layout, records them in
`MulticolGeometry.paragraph_splits`. Convert reads the geometry and
populates a new `Drawables.paragraph_slices` side-table holding per-column
`ShapedLine` slices. Render's paragraph dispatcher consults the side-table
and emits one slice per non-empty column at the correct origin instead of
the default single-rectangle path.

**Tech Stack:** Rust, Taffy 0.9.2 (custom layout hook),
`parley::Layout::lines()` for line iteration, `blitz_dom`
(`inline_layout_data`, `flags.is_inline_root()`),
`crate::paragraph::ShapedLine` (Drawables payload type).

---

## Pre-flight verification

Before writing any code, confirm one assumption that affects whether
Case B can reuse the post-measurement parley::Layout or must clone +
re-break manually like Case A.

**Files (read-only):** `crates/fulgur/src/multicol_layout.rs:430-510`,
`blitz_dom-0.2.4/src/layout/inline.rs` (whichever module owns
`compute_inline_layout`).

**Question:** When `compute_child_layout(child_id, known_dimensions =
col_w)` runs on an inline-root child whose `inline_layout_data` was
originally shaped at the container's content width, does Blitz re-break
the parley layout at `col_w` so that subsequent reads of
`elem_data.inline_layout_data.layout.lines()` yield lines wrapped to
`col_w`?

**Verification method:** add a temporary `#[ignore]`'d test under
`multicol_layout::tests` that:

1. Parses `<div style="column-count:2"><p>long text x 30 alphas</p></div>`.
2. Calls `crate::blitz_adapter::resolve` (full layout pass).
3. Reads `<p>`'s parley layout — counts lines and records max line width.
4. Drives the Taffy multicol hook on the div (the hook will call
   `compute_child_layout(p, col_w)` internally as part of measurement).
5. Reads `<p>`'s parley layout *again* — counts lines and max line width.

If line count grew and max width ≤ `col_w`, Case B can read the
post-measure layout in place. If line count is unchanged, the hook must
clone + `break_all_lines(Some(col_w))` for Case B as well as Case A.

**Outcome captured in plan:** record the answer at the top of Task 3 (the
detection step) and adjust the slice helper signature accordingly.

The probe stays under `#[ignore]` for the duration of the implementation
and gets removed in Task 9 cleanup.

---

## Task 1: Geometry — add `paragraph_splits` to `ColumnGroupGeometry`

**Files:**

- Modify: `crates/fulgur/src/multicol_layout.rs:25-83` (struct
  definitions), `crates/fulgur/src/multicol_layout.rs` (impls and
  consumers as the compiler points out)

**Step 1: Write the failing test**

Add inside `multicol_layout::tests` (sibling to the existing
`MulticolGeometry` smoke tests):

```rust
#[test]
fn column_group_geometry_default_paragraph_splits_is_empty() {
    let g = ColumnGroupGeometry::default();
    assert!(g.paragraph_splits.is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p fulgur --lib column_group_geometry_default_paragraph_splits_is_empty`

Expected: compilation FAIL — `paragraph_splits` field does not exist.

**Step 3: Add the field + supporting types**

Add to `crates/fulgur/src/multicol_layout.rs` near the existing
`ColumnGroupGeometry` definition:

```rust
/// Per-column slice of an inline-root paragraph distributed across
/// columns by `layout_column_group`. Empty `line_range` means the column
/// receives no content from this paragraph; this happens when the
/// paragraph fits entirely in fewer columns than `n`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColumnLineSlice {
    /// Half-open parley line index range. Indices reference the parley
    /// layout reachable through `source_node_id`'s `inline_layout_data`.
    pub line_range: std::ops::Range<usize>,
    /// Slice top-left in the multicol container's content-box frame
    /// (CSS pixels). `compute_multicol_layout` shifts this into the
    /// border-box frame after the segment loop, mirroring the existing
    /// `(x_offset, y_offset)` shift on `ColumnGroupGeometry` itself.
    pub origin: taffy::Point<f32>,
    /// Slice size — `col_w × Σ line_height(line_range)`. CSS pixels.
    pub size: taffy::Size<f32>,
}

/// Plan for one paragraph distributed across `ColumnGroupGeometry`'s
/// columns. `column_slices.len() == ColumnGroupGeometry.n` always; entries
/// for unused columns have empty `line_range`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParagraphSplitEntry {
    /// DOM `usize` NodeId whose `inline_layout_data` was sliced. In Case A
    /// this equals the multicol container's own NodeId; in Case B it's
    /// the inline-root child's NodeId.
    pub source_node_id: usize,
    pub column_slices: Vec<ColumnLineSlice>,
}
```

Then extend `ColumnGroupGeometry`:

```rust
pub struct ColumnGroupGeometry {
    // ... existing fields ...
    /// Inline-root paragraphs distributed across the columns of this
    /// group. Empty when no child paragraph needed splitting.
    pub paragraph_splits: Vec<ParagraphSplitEntry>,
}
```

Update the `Default` derive — already present — so existing call sites
that build `ColumnGroupGeometry` literally need a `paragraph_splits:
Vec::new()` field. Find every literal construction
(`ColumnGroupGeometry { ... }`) and add the field. The compiler will
list them all.

**Step 4: Run test to verify it passes**

Run: `cargo test -p fulgur --lib column_group_geometry_default_paragraph_splits_is_empty`

Expected: PASS. Run `cargo test -p fulgur --lib` and confirm the existing
multicol tests still pass (no behaviour change yet, just data widening).

**Step 5: Commit**

```bash
git add crates/fulgur/src/multicol_layout.rs
git commit -m "feat(multicol): add paragraph_splits field to ColumnGroupGeometry (fulgur-6q5)"
```

---

## Task 2: Slicer — `slice_lines_by_budget` pure helper

**Files:**

- Modify: `crates/fulgur/src/multicol_layout.rs` (new private fn near the
  existing `balance_budget` / `fits_in_n_columns` helpers)

**Step 1: Write the failing tests**

Add to `multicol_layout::tests`:

```rust
#[test]
fn slice_lines_by_budget_short_paragraph_one_slice() {
    // Heights [10, 10, 10] — total 30, budget 100, n=2.
    // All three lines fit in column 0; column 1 stays empty.
    let heights = [10.0_f32, 10.0, 10.0];
    let slices = slice_lines_by_budget(&heights, /*budget*/ 100.0, /*n*/ 2);
    assert_eq!(slices.len(), 2);
    assert_eq!(slices[0], (0..3, 30.0));
    assert_eq!(slices[1], (3..3, 0.0));
}

#[test]
fn slice_lines_by_budget_splits_at_budget_boundary() {
    // Heights [10; 10] = total 100, budget 50, n=2.
    // 5 lines per column.
    let heights = [10.0_f32; 10];
    let slices = slice_lines_by_budget(&heights, 50.0, 2);
    assert_eq!(slices.len(), 2);
    assert_eq!(slices[0], (0..5, 50.0));
    assert_eq!(slices[1], (5..10, 50.0));
}

#[test]
fn slice_lines_by_budget_admits_overflow_when_one_line_exceeds_budget() {
    // The first line exceeds the budget — we must not loop forever.
    // Spec: monolithic line, allowed to overflow.
    let heights = [80.0_f32, 20.0, 20.0];
    let slices = slice_lines_by_budget(&heights, 50.0, 2);
    assert_eq!(slices[0], (0..1, 80.0));
    assert_eq!(slices[1], (1..3, 40.0));
}

#[test]
fn slice_lines_by_budget_overflow_into_unavailable_columns_truncates() {
    // 5 lines × 20pt = 100pt total, budget 30pt, n=2 → only 60pt of
    // capacity. Last two lines have nowhere to go; the helper assigns
    // them to the final column (overflow allowed) so we never silently
    // drop content.
    let heights = [20.0_f32; 5];
    let slices = slice_lines_by_budget(&heights, 30.0, 2);
    assert_eq!(slices[0], (0..1, 20.0));        // 1 line fits in 30pt
    assert_eq!(slices[1].0.start, 1);           // last column absorbs the rest
    assert_eq!(slices[1].0.end, 5);
}

#[test]
fn slice_lines_by_budget_empty_input_returns_n_empty_slices() {
    let slices = slice_lines_by_budget(&[], 100.0, 3);
    assert_eq!(slices.len(), 3);
    for (range, h) in &slices {
        assert!(range.is_empty());
        assert_eq!(*h, 0.0);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p fulgur --lib slice_lines_by_budget`

Expected: compilation FAIL — function does not exist.

**Step 3: Implement `slice_lines_by_budget`**

Add near `balance_budget` (line ~810 in `multicol_layout.rs`):

```rust
/// Distribute `line_heights` (per-line total height in CSS px) across
/// `n` columns of `budget` height each.
///
/// Returns one `(line_range, slice_height)` pair per column; ranges are
/// half-open against the input slice index space.
///
/// Greedy fill semantics:
/// - Each column receives lines starting at the previous column's end.
/// - When the next line would push the column over `budget`, advance to
///   the next column.
/// - Exception: a column always receives at least one line even when
///   that line alone exceeds `budget` (CSS monolithic-line rule —
///   otherwise we loop forever and never make progress).
/// - When the column count is exhausted before all lines are placed,
///   the final column absorbs every remaining line. This violates the
///   per-column budget but never silently drops content; the caller
///   ensures the budget is `≥ avail_h / n` so this branch fires only on
///   pathological inputs where the multicol container itself overflows
///   the page (handled elsewhere).
fn slice_lines_by_budget(
    line_heights: &[f32],
    budget: f32,
    n: u32,
) -> Vec<(std::ops::Range<usize>, f32)> {
    let n = n.max(1) as usize;
    let mut out = Vec::with_capacity(n);
    let mut start = 0_usize;
    for col in 0..n {
        let mut consumed = 0.0_f32;
        let mut end = start;
        let last_col = col == n - 1;
        while end < line_heights.len() {
            let h = line_heights[end];
            let line_count = end - start;
            // First line in this column always fits (monolithic line rule).
            // Otherwise, stop when adding this line would overflow,
            // unless we are the final column (absorb remainder).
            if line_count > 0 && consumed + h > budget && !last_col {
                break;
            }
            consumed += h;
            end += 1;
        }
        out.push((start..end, consumed));
        start = end;
    }
    out
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p fulgur --lib slice_lines_by_budget`

Expected: 5 PASS.

Run: `cargo test -p fulgur --lib` to confirm no regressions.

**Step 5: Commit**

```bash
git add crates/fulgur/src/multicol_layout.rs
git commit -m "feat(multicol): line-budget slicer for paragraph splits (fulgur-6q5)"
```

---

## Task 3: parley line-height extractor

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs` (new public helper near the
  existing `list_position_outside_layout` accessor — line ~70, same
  module that already imports `parley`).

**Step 1: Write the failing test**

Add a test that does not depend on knowing parley's internal field names:

```rust
#[test]
fn parley_layout_line_heights_matches_lines_count_and_sum() {
    // Render a fixed paragraph at a fixed width through Engine, then
    // recover the parley layout via the inline-root NodeId and inspect
    // the helper's output against parley::Line::metrics directly.
    let html = r#"<!doctype html><html><body><p style="width: 80px; font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p></body></html>"#;
    let mut doc = parse(html, 200.0, &[]);
    resolve(&mut doc);
    let p_id = (0..doc.nodes.len())
        .find(|&i| {
            doc.get_node(i)
                .and_then(|n| n.element_data())
                .map(|e| e.name.local.as_ref() == "p")
                .unwrap_or(false)
        })
        .expect("<p> must exist");
    let parley_layout = &doc
        .get_node(p_id)
        .unwrap()
        .element_data()
        .unwrap()
        .inline_layout_data
        .as_ref()
        .unwrap()
        .layout;
    let heights = parley_line_heights(parley_layout);
    assert_eq!(heights.len(), parley_layout.lines().count());
    let sum: f32 = heights.iter().sum();
    let total_height = parley_layout
        .lines()
        .last()
        .map(|l| {
            let m = l.metrics();
            m.baseline + m.descent + m.leading
        })
        .unwrap_or(0.0);
    assert!(
        (sum - total_height).abs() < 0.5,
        "sum of per-line heights {sum} should approximate total layout height {total_height}"
    );
    assert!(heights.iter().all(|h| *h > 0.0));
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p fulgur --lib parley_layout_line_heights`

Expected: compilation FAIL — `parley_line_heights` does not exist.

**Step 3: Implement the helper**

The exact metric API depends on parley's version pinned by Blitz. Open
`/home/ubuntu/.cargo/registry/src/index.crates.io-*/parley-*/src/layout/line.rs`
and confirm the field. The current expectation is that
`Line::metrics().line_height` returns the line's pen-advance in CSS px
matching what fulgur reads in `convert/inline_root.rs:458` (which already
calls `line.metrics()`). If that field is named differently, derive the
height from `metrics().baseline + metrics().descent + metrics().leading`
(or `ascent + descent + leading`, depending on the API).

Add to `crates/fulgur/src/blitz_adapter.rs`:

```rust
/// Extract per-line height (CSS px) from a `parley::Layout`.
///
/// Returns one `f32` per line, matching `layout.lines().count()`. The
/// line-height definition follows what `convert::inline_root` uses when
/// it emits `ShapedLine.height`: `metrics().line_height` is preferred;
/// `metrics().ascent + descent + leading` is the documented fallback for
/// parley versions that do not surface `line_height` directly.
pub fn parley_line_heights(
    layout: &parley::Layout<blitz_dom::node::TextBrush>,
) -> Vec<f32> {
    layout
        .lines()
        .map(|line| {
            let m = line.metrics();
            // Prefer the dedicated field when present.
            // (As of parley 0.x, `line_height` is the per-line advance.)
            m.line_height
        })
        .collect()
}
```

If `m.line_height` is not the field name in the version the workspace
locks to, replace the body with the fallback computation. Update the
doc comment accordingly.

**Step 4: Run to verify it passes**

Run: `cargo test -p fulgur --lib parley_layout_line_heights`

Expected: PASS. Sanity-check by also running
`cargo test -p fulgur --lib` (no regressions).

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(blitz_adapter): parley_line_heights helper (fulgur-6q5)"
```

---

## Task 4: Multicol hook — split inline-root child (Case B)

**Files:**

- Modify: `crates/fulgur/src/multicol_layout.rs` (`layout_column_group`
  body around line 730 — the distribute step).

**Step 1: Write the failing end-to-end test**

Add to `multicol_layout::tests`, alongside
`column_fill_auto_leaves_later_columns_empty_when_content_fits` (line
2111):

```rust
#[test]
fn multicol_splits_single_inline_root_p_child_across_two_columns() {
    // Single <p> with enough text to force lines into both columns.
    // 200pt container width, column-count: 2, column-gap: 0 → col_w 100pt.
    // 30 alphas at 16px font is far more than 100pt × 1 column can hold.
    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0;">
          <p style="font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p>
        </div>
    </body></html>"#;
    let mut doc = crate::blitz_adapter::parse(html, 200.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);

    let mc_id = collect_multicol_node_ids(&doc)[0];
    let column_styles = crate::column_css::ColumnStyleTable::new();
    let geometry_table = run_pass(&mut doc, &column_styles);
    let mc_geom = geometry_table
        .get(&mc_id)
        .expect("multicol container should have a geometry entry");
    assert_eq!(mc_geom.groups.len(), 1);
    let group = &mc_geom.groups[0];
    assert_eq!(group.n, 2);

    // Both columns should have content.
    assert!(
        group.col_heights[0] > 0.0,
        "col 0 must be filled, got {:?}",
        group.col_heights
    );
    assert!(
        group.col_heights[1] > 0.0,
        "col 1 must be filled (the bug we're fixing), got {:?}",
        group.col_heights
    );

    // The split metadata must be populated.
    assert_eq!(
        group.paragraph_splits.len(),
        1,
        "exactly one inline-root child should have been split"
    );
    let split = &group.paragraph_splits[0];
    assert_eq!(split.column_slices.len(), 2);
    assert!(!split.column_slices[0].line_range.is_empty());
    assert!(!split.column_slices[1].line_range.is_empty());
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p fulgur --lib multicol_splits_single_inline_root_p_child_across_two_columns`

Expected: FAIL — `col_heights[1] > 0.0` assertion. The single paragraph
is currently placed wholly in column 0.

**Step 3: Wire the split into `layout_column_group`**

Inside `layout_column_group` (line 692), after step 2 (budget selection)
and before step 3 (distribute), insert the split-detection step. This
expands the iterator the distribute loop walks — instead of iterating
the raw `measured` list, build a flattened list of "items" where each
item is either an unsplittable measured child or one slice of a split
inline-root child.

Pseudocode (refine into Rust at implementation time):

```rust
enum DistItem<'a> {
    Atomic { id: NodeId, size: Size<f32> },
    InlineRootSlice {
        source_id: NodeId,           // for paragraph_splits.source_node_id
        atomic_id: NodeId,           // for placements (the same source_id)
        line_range: Range<usize>,
        size: Size<f32>,             // (col_w, slice_height)
    },
}
```

Build the list:

```rust
let mut items: Vec<DistItem> = Vec::new();
for (id, size) in &measured {
    let id_u: usize = (*id).into();
    let inline_root_layout = tree
        .doc
        .get_node(id_u)
        .filter(|n| n.flags.is_inline_root())
        .and_then(|n| n.element_data())
        .and_then(|e| e.inline_layout_data.as_ref())
        .map(|tl| &tl.layout);

    match inline_root_layout {
        Some(layout) if size.height > budget => {
            let line_heights = crate::blitz_adapter::parley_line_heights(layout);
            let slices = slice_lines_by_budget(&line_heights, budget, n);
            for (line_range, slice_h) in slices {
                if !line_range.is_empty() {
                    items.push(DistItem::InlineRootSlice {
                        source_id: *id,
                        atomic_id: *id,
                        line_range,
                        size: Size { width: col_w, height: slice_h },
                    });
                }
            }
        }
        _ => items.push(DistItem::Atomic { id: *id, size: *size }),
    }
}
```

Then change the distribute loop to walk `items` instead of `measured`.
For atomic items, behaviour is unchanged. For slices, each slice is
sized to fit ≤ budget, so the existing column-advance logic packs
column 0 to budget then advances. Critically: emit the placement only
once per source paragraph (the first slice "owns" the
`set_unrounded_layout` write); keep the rest as geometry-only entries.

The simplest implementation:

- The distribute loop only writes Taffy `set_unrounded_layout` for the
  *first* slice of each source paragraph (covers column 0). Subsequent
  slices contribute to `paragraph_splits` but not to `placements` —
  Taffy never sees them.
- After the loop, build `paragraph_splits` from the per-source slice
  origins and ranges.

**Step 4: Run to verify it passes**

Run: `cargo test -p fulgur --lib multicol_splits_single_inline_root_p_child_across_two_columns`

Expected: PASS.

Run: `cargo test -p fulgur --lib` to verify no regression in other
multicol tests.

**Step 5: Commit**

```bash
git add crates/fulgur/src/multicol_layout.rs
git commit -m "feat(multicol): split inline-root child paragraph across columns (fulgur-6q5)"
```

---

## Task 5: Multicol hook — split self-inline-root container (Case A)

**Files:**

- Modify: `crates/fulgur/src/multicol_layout.rs::compute_multicol_layout`
  (around line 430).

**Step 1: Write the failing test**

```rust
#[test]
fn multicol_splits_self_inline_root_container_with_bare_text() {
    // Bare text directly inside the multicol container — the div itself
    // is inline-root, no <p> wrapper.
    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0; font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</div>
    </body></html>"#;
    let mut doc = crate::blitz_adapter::parse(html, 200.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);

    let mc_id = collect_multicol_node_ids(&doc)[0];
    let column_styles = crate::column_css::ColumnStyleTable::new();
    let geometry_table = run_pass(&mut doc, &column_styles);
    let mc_geom = geometry_table
        .get(&mc_id)
        .expect("multicol container should have a geometry entry");
    let group = &mc_geom.groups[0];
    assert!(group.col_heights[0] > 0.0);
    assert!(
        group.col_heights[1] > 0.0,
        "self-inline-root container must spread lines across both columns, got {:?}",
        group.col_heights
    );
    assert_eq!(group.paragraph_splits.len(), 1);
    assert_eq!(group.paragraph_splits[0].source_node_id, mc_id);
}
```

**Step 2: Run to verify it fails**

Expected: FAIL — `col_heights[1] > 0.0`. Container's own parley layout
is laid out at full container width and not redistributed.

**Step 3: Implement Case A in `compute_multicol_layout`**

At the top of `compute_multicol_layout`, after extracting `props` /
`container_w` / `(n, col_w)` and computing `avail_h` (around line 545),
detect "self-inline-root" containers:

```rust
let is_self_inline_root = tree
    .doc
    .get_node(usize::from(node_id))
    .map(|n| {
        n.flags.is_inline_root()
            && n.element_data()
                .and_then(|e| e.inline_layout_data.as_ref())
                .is_some()
    })
    .unwrap_or(false);
```

When true, take a different path before the `partition_children_into_segments`
call:

1. Clone the parley layout from `inline_layout_data`.
2. Re-break at `col_w` via parley's `Layout::break_all_lines(Some(col_w))`
   followed by alignment (mirroring the v1 spike — see commit `e8a4be1`
   `feature/fulgur-qkg-multicol-phase-a`). The clone is local; we never
   write back to Blitz.
3. Read `parley_line_heights` on the re-broken clone.
4. Call `slice_lines_by_budget` against the budget (avail_h for
   `column-fill: auto`, balance result for default).
5. Build a single `ColumnGroupGeometry` with one
   `ParagraphSplitEntry { source_node_id: mc_id, ... }` and
   `col_heights` derived from the slice heights.
6. Skip the per-segment loop entirely; container size is
   `(container_w, max(col_heights) + insets)`.
7. Stash geometry, return `LayoutOutput`.

The Case A path duplicates a small amount of bookkeeping from the
generic path; encapsulate it in a helper
`fn layout_self_inline_root_container(...)` to keep
`compute_multicol_layout` readable.

**Step 4: Run to verify it passes**

Run: `cargo test -p fulgur --lib multicol_splits_self_inline_root_container_with_bare_text`

Expected: PASS. Re-run `cargo test -p fulgur --lib` for regressions.

**Step 5: Commit**

```bash
git add crates/fulgur/src/multicol_layout.rs
git commit -m "feat(multicol): split self-inline-root container across columns (fulgur-6q5)"
```

---

## Task 6: Drawables side-table for paragraph slices

**Files:**

- Modify: `crates/fulgur/src/drawables.rs` (~line 256 add field, ~line 322
  add `is_empty` clause).

**Step 1: Write the failing test**

```rust
#[test]
fn drawables_default_paragraph_slices_is_empty() {
    let d = Drawables::new();
    assert!(d.paragraph_slices.is_empty());
    assert!(d.is_empty());  // adding the new field must not change is_empty for default
}
```

**Step 2: Run to verify it fails**

Expected: FAIL — `paragraph_slices` does not exist.

**Step 3: Add the field**

```rust
/// Per-source-paragraph multicol slicing emitted by
/// `convert::convert_multicol_paragraph_slices` from
/// `MulticolGeometry.paragraph_splits`. When non-empty for a NodeId,
/// `render_v2`'s paragraph dispatcher renders one entry per non-empty
/// slice at slice origin instead of the default single-rectangle path
/// that uses `paragraphs[node_id]`.
pub paragraph_slices: BTreeMap<NodeId, ParagraphSlicesEntry>,
```

```rust
#[derive(Clone, Default)]
pub struct ParagraphSlicesEntry {
    /// Multicol container's NodeId — render needs its body-relative
    /// position to anchor each slice.
    pub container_node_id: NodeId,
    /// One slice per non-empty column. Length matches the number of
    /// non-empty `ColumnLineSlice` entries (empty columns skipped).
    pub slices: Vec<ParagraphSlice>,
}

#[derive(Clone, Debug)]
pub struct ParagraphSlice {
    /// Slice top-left in PDF pt, relative to the multicol container's
    /// border-box top-left. Render adds the container's body-relative
    /// position to obtain the final page coordinates.
    pub origin_pt: (f32, f32),
    /// Slice size in PDF pt — `col_w × slice_height`. Width is needed
    /// only for clipping or downstream debugging; height matters because
    /// it tells the renderer how to vertically allocate the lines.
    pub size_pt: (f32, f32),
    /// Lines included in this slice with baselines rebased so y=0 is
    /// the slice's top edge — same convention as
    /// `paragraph::ParagraphPageable::split` second-fragment rebase
    /// (commit 9c0e092 in the repository).
    pub lines: Vec<crate::paragraph::ShapedLine>,
}
```

Update `Drawables::is_empty` to include `&& self.paragraph_slices.is_empty()`.

**Step 4: Run to verify it passes**

Run: `cargo test -p fulgur --lib drawables_default_paragraph_slices_is_empty`

Expected: PASS. Run the full suite — no behaviour change yet.

**Step 5: Commit**

```bash
git add crates/fulgur/src/drawables.rs
git commit -m "feat(drawables): paragraph_slices side-table for multicol splits (fulgur-6q5)"
```

---

## Task 7: Convert — populate `paragraph_slices` from geometry

**Files:**

- Modify: `crates/fulgur/src/convert/mod.rs` (new fn similar to
  `convert_multicol_rule` at line ~482).
- Modify: `crates/fulgur/src/convert/inline_root.rs` if needed for
  helpers.

**Step 1: Write the failing test**

Smoke test exercising the full convert pipeline. Place under
`crates/fulgur/tests/render_smoke.rs` (per CLAUDE.md, lib-side smoke
tests cover render-path code that VRT alone misses):

```rust
#[test]
fn multicol_inline_root_split_emits_paragraph_slices() {
    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0; font-size: 16px;">
          <p>alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p>
        </div>
    </body></html>"#;
    let engine = fulgur::Engine::builder()
        .page_size(fulgur::config::PageSize { width: 400.0, height: 600.0 })
        .build();
    let drawables = engine.build_drawables_for_testing_no_gcpm(html);
    assert!(
        !drawables.paragraph_slices.is_empty(),
        "split paragraph must register paragraph_slices entry"
    );
    let entry = drawables.paragraph_slices.values().next().unwrap();
    assert_eq!(entry.slices.len(), 2);
    for slice in &entry.slices {
        assert!(!slice.lines.is_empty());
        assert!(slice.size_pt.1 > 0.0);
    }
}
```

**Step 2: Run to verify it fails**

Expected: FAIL — `paragraph_slices` is empty (convert hasn't been wired
to populate it).

**Step 3: Implement the convert step**

After `convert_multicol_rule` (which already iterates multicol
containers and emits `MulticolRuleEntry`), add a parallel pass:

```rust
fn convert_multicol_paragraph_slices(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    out: &mut crate::drawables::Drawables,
) {
    let Some(geometry) = ctx.multicol_geometry.get(&node_id) else { return };
    for group in &geometry.groups {
        for split in &group.paragraph_splits {
            let source_id = split.source_node_id;
            let Some(parley_layout) = doc
                .get_node(source_id)
                .and_then(|n| n.element_data())
                .and_then(|e| e.inline_layout_data.as_ref())
                .map(|tl| &tl.layout)
            else {
                continue;
            };

            // Reuse the convert/inline_root pipeline to produce one
            // ShapedLine per parley line, then partition by line range
            // for each non-empty column slice.
            //
            // Implementation detail: convert/inline_root::extract_paragraph
            // already iterates parley lines and produces ShapedLines —
            // we factor that loop into a reusable helper that returns
            // a Vec<ShapedLine> indexed identically to parley_layout.lines().
            let all_lines = inline_root::shape_paragraph_lines(doc, source_id, ctx);

            let mut slices = Vec::new();
            for (col_idx, col_slice) in split.column_slices.iter().enumerate() {
                if col_slice.line_range.is_empty() { continue; }
                let mut lines: Vec<crate::paragraph::ShapedLine> = all_lines
                    [col_slice.line_range.clone()]
                    .to_vec();
                inline_root::recalculate_paragraph_line_boxes(&mut lines);
                slices.push(crate::drawables::ParagraphSlice {
                    origin_pt: (
                        px_to_pt(col_slice.origin.x),
                        px_to_pt(col_slice.origin.y),
                    ),
                    size_pt: (
                        px_to_pt(col_slice.size.width),
                        px_to_pt(col_slice.size.height),
                    ),
                    lines,
                });
            }
            if !slices.is_empty() {
                out.paragraph_slices.insert(
                    source_id,
                    crate::drawables::ParagraphSlicesEntry {
                        container_node_id: node_id,
                        slices,
                    },
                );
            }
        }
    }
}
```

Wire the new function into the multicol container handling — same place
as `convert_multicol_rule` runs. Factor `shape_paragraph_lines` out of
`extract_paragraph` if needed (it currently does the same work inline).

**Step 4: Run to verify it passes**

Run: `cargo test -p fulgur --test render_smoke multicol_inline_root_split_emits_paragraph_slices`

Expected: PASS. Re-run `cargo test -p fulgur` (full crate).

**Step 5: Commit**

```bash
git add crates/fulgur/src/convert/mod.rs crates/fulgur/src/convert/inline_root.rs crates/fulgur/tests/render_smoke.rs
git commit -m "feat(convert): emit paragraph_slices from multicol geometry (fulgur-6q5)"
```

---

## Task 8: Render — paint paragraph slices

**Files:**

- Modify: `crates/fulgur/src/render.rs` (paragraph dispatcher around
  line 819, and the paragraph-painting helpers around lines 2436–2670).

**Step 1: Write the failing smoke test**

```rust
#[test]
fn multicol_inline_root_split_renders_to_pdf() {
    let html = r#"<!doctype html><html><body>
        <div style="column-count: 2; column-gap: 0; font-size: 16px;">
          <p>alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p>
        </div>
    </body></html>"#;
    let engine = fulgur::Engine::builder()
        .page_size(fulgur::config::PageSize { width: 400.0, height: 600.0 })
        .build();
    let pdf = engine.render_html(html).expect("render must succeed");
    assert!(!pdf.is_empty());
    // We can't byte-assert without a golden, but PDFs containing two text
    // streams in the column layout are larger than the single-column
    // baseline — sanity bound.
    assert!(pdf.len() > 1000);
}
```

**Step 2: Run to verify it fails**

Expected: PASS at the byte-length assertion (render still runs), but the
*visual* output silently puts the paragraph in column 0 only because
render hasn't been wired. We need a tighter signal — extend the test to
inspect the PDF text-stream offsets:

```rust
// Assert the rendered PDF contains text drawn at two distinct x
// positions matching column 0 and column 1 origins. Use
// `lopdf` (already a workspace dep) to extract Td operands.
//
// (Defer full pdf-parsing if it adds complexity — alternatively keep
//  the byte-length check and rely on the VRT golden in Task 10.)
```

If the lopdf inspection is heavy, **drop this Step 2** and rely on the
geometry-level test (Task 4) plus the VRT golden (Task 10) for visual
regression. The render Step 3 is mechanical wiring.

**Step 3: Implement render dispatch**

In `render::render_v2` paragraph dispatch path (around line 819), before
falling through to the standard `draw_shaped_lines` for `paragraphs[id]`,
check `drawables.paragraph_slices.get(&node_id)`. If present:

```rust
if let Some(slices_entry) = drawables.paragraph_slices.get(&node_id) {
    // Compute the multicol container's body-relative origin from
    // geometry/block_styles, then for each slice emit
    // draw_shaped_lines(slices_entry.slices[i].lines, container_origin
    // + slices_entry.slices[i].origin_pt). Skip the standard
    // paragraphs[node_id] entry below.
    paint_multicol_paragraph_slices(...);
    continue;  // or return depending on the dispatch pattern
}
```

Implementation details:

- The container's body-relative position is already in
  `block_styles[container_node_id]` (the multicol container's own
  Block paint info).
- Use the existing `draw_shaped_lines` helper (the one
  `paragraph.rs:691` documents) — just call it once per slice with the
  pre-rebased lines and the slice origin.
- Honour `paragraphs[node_id].opacity` / `visible` / `id` —
  `paragraph_slices` is an extension, not a replacement.

**Step 4: Run to verify it passes**

Run: `cargo test -p fulgur` (full suite). All existing render tests
still pass. The new smoke from Step 1 passes.

**Step 5: Commit**

```bash
git add crates/fulgur/src/render.rs crates/fulgur/tests/render_smoke.rs
git commit -m "feat(render): paint multicol paragraph slices (fulgur-6q5)"
```

---

## Task 9: Cleanup — remove the verification probe

**Files:**

- Modify: `crates/fulgur/src/multicol_layout.rs` or
  `crates/fulgur/src/blitz_adapter.rs` (wherever the pre-flight probe
  was placed).

**Step 1: Delete the `#[ignore]`'d probe test added during pre-flight.**

**Step 2: Run `cargo test -p fulgur --lib` to confirm clean.**

**Step 3: Commit**

```bash
git commit -am "chore(multicol): remove pre-flight verification probe (fulgur-6q5)"
```

---

## Task 10: VRT goldens

**Files:**

- Create: `crates/fulgur-vrt/fixtures/multicol-inline-root-split-case-a.html`
- Create: `crates/fulgur-vrt/fixtures/multicol-inline-root-split-case-b.html`
- Create: `crates/fulgur-vrt/goldens/fulgur/multicol-inline-root-split-case-a.pdf`
- Create: `crates/fulgur-vrt/goldens/fulgur/multicol-inline-root-split-case-b.pdf`
- Modify: `crates/fulgur-vrt/tests/...rs` (whatever lists the fixture
  set — read the file before editing to find the exact registration
  point).

**Step 1: Write the fixture HTML files**

Case A:

```html
<!doctype html>
<html>
<head>
<style>
  body { margin: 16px; font-family: 'Noto Sans'; font-size: 14px; }
  .mc { column-count: 2; column-gap: 16px; }
</style>
</head>
<body>
  <div class="mc">
    Lorem ipsum dolor sit amet ... [enough text to span many lines] ...
  </div>
</body>
</html>
```

Case B uses the same body but wraps the text in `<p>...</p>`.

**Step 2: Register fixtures in the VRT runner**

Find the existing fixture registration (see `crates/fulgur-vrt/src/...`)
and add the two new entries. Run:

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  FULGUR_VRT_UPDATE=1 \
  cargo test -p fulgur-vrt
```

This generates the golden PDFs.

**Step 3: Verify reproducibility**

Run without `FULGUR_VRT_UPDATE` — both fixtures must pass byte-equal:

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  cargo test -p fulgur-vrt
```

**Step 4: Inspect the goldens**

Convert to PNG with `pdftocairo` for a manual eyeball:

```bash
pdftocairo -png -r 100 \
  crates/fulgur-vrt/goldens/fulgur/multicol-inline-root-split-case-b.pdf \
  /tmp/case-b
```

Open `/tmp/case-b-1.png`. Confirm: lines fill column 0 then continue in
column 1, no overlap with surrounding content.

**Step 5: Commit**

```bash
git add crates/fulgur-vrt/fixtures crates/fulgur-vrt/goldens crates/fulgur-vrt/...
git commit -m "test(multicol): VRT goldens for inline-root paragraph split (fulgur-6q5)"
```

---

## Task 11: Final validation

**Files:** none.

**Step 1: Full test suite**

```bash
cargo test -p fulgur
cargo test -p fulgur-vrt
cargo clippy --all-targets -- -D warnings
cargo fmt --check
npx markdownlint-cli2 'docs/plans/2026-05-03-multicol-inline-root-split.md'
```

All must be clean.

**Step 2: Manual acceptance**

```bash
cat > /tmp/acceptance.html <<'EOF'
<!doctype html>
<html><body>
<div style="column-count: 2; column-gap: 16px; padding: 8px;">
  Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do
  eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim
  ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut
  aliquip ex ea commodo consequat.
</div>
</body></html>
EOF

cargo run --bin fulgur -- render /tmp/acceptance.html -o /tmp/acceptance.pdf
pdftocairo -png -r 100 /tmp/acceptance.pdf /tmp/acceptance
```

Eyeball `/tmp/acceptance-1.png`. Lines must fill column 0, remainder in
column 1.

**Step 3: No commit needed.**

---

## Risks & rollback

- **R2 Mitigation outcome**: If the pre-flight probe shows that
  `compute_child_layout(child, col_w)` does not re-break the parley
  layout, Case B (Task 4) needs to clone + `break_all_lines(Some(col_w))`
  before reading `parley_line_heights`. The plan covers this via the
  Task 5 path (Case A already does the clone). Hoist that helper into
  Task 3 (`parley_line_heights` → `parley_line_heights_at_width`) if
  R2 fires.
- **R3 Mitigation outcome**: Drawables shape change is one new
  `BTreeMap` field. If review pushes back on the new struct, fall back
  to repurposing `paragraphs[node_id].lines` to hold the first-column
  slice and adding a sibling `paragraph_slices_extra: BTreeMap<NodeId,
  Vec<ParagraphSlice>>` for columns 1+. Less symmetric but smaller diff.
- **Rollback**: each task is a single commit. Reverting the chain
  removes the feature without touching unrelated code. The geometry
  field added in Task 1 is the only persistent shape change visible
  from outside the multicol module; everything downstream gates on
  `paragraph_slices.is_empty()`.

## Out of scope (separate beads issues)

- 3+ column splits — the algorithm generalises but acceptance & VRT
  cover only n=2.
- Mixing split paragraphs with other children in the same group — the
  distribute loop handles it because slices are pre-sized; visual
  fixtures live in fulgur-e3z (multicol A-6).
- Per-column page-spanning when a single column overflows page height
  (orthogonal to per-column slicing; existing paginate handles it for
  the container as a whole).
- Nested multicol inside an inline-root subtree (fulgur-wfd).
