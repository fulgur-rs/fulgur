# CSS `column-span: all` Implementation Plan (fulgur-0vd)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement CSS Multi-column Level 1 `column-span: all` in fulgur, including the "SpanAll subtree that itself crosses a page boundary" case.

**Architecture:** Extend `compute_multicol_layout` in `crates/fulgur/src/multicol_layout.rs` to partition the multicol container's **top-level** children into alternating `ColumnGroup` / `SpanAll` segments, lay out each segment (ColumnGroup children balanced at `col_w`, SpanAll child at full `container_w`), and stack segments vertically. Downstream (`convert.rs` → `paginate.rs`) sees a normal block tree with correct absolute positions, so page-spanning of a SpanAll subtree falls out of the existing `BlockPageable` split pipeline. Nested `column-span: all` inside a non-span child's subtree is ignored per CSS Multi-column L1.

**Tech Stack:** Rust, Taffy custom layout hook, blitz-dom, fulgur's existing `BlockPageable` pagination.

**Scope boundary:** Pagination of columns *themselves* across page boundaries (the `multicol-page-spanning` VRT fixture) is **A-6** (fulgur-e3z), not this task. This plan only needs to make a `column-span: all` child whose subtree is taller than one page render correctly when the multicol container is otherwise within a single page.

**Out of scope:**

- Changes to `convert.rs` / `pageable.rs` / `paginate.rs`. The Taffy hook's
  placement output is consumed by the existing block pipeline unchanged.
- `column-span: <integer>` (Multi-column Level 2).

---

## Task 1: Add `Segment` type + `partition_children_into_segments`

**Files:**

- Modify: `crates/fulgur/src/multicol_layout.rs`

**Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block at the bottom of
`crates/fulgur/src/multicol_layout.rs`:

```rust
// ── partition_children_into_segments ────────────────────────────

fn parse_multicol(html: &str) -> (blitz_dom::BaseDocument, usize) {
    let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);
    let mc_id = collect_multicol_node_ids(&doc)[0];
    (doc, mc_id)
}

#[test]
fn partition_all_children_are_columnar() {
    let html = r#"<!doctype html><html><body>
        <div style="column-count: 2;">
          <p>a</p><p>b</p><p>c</p>
        </div>
    </body></html>"#;
    let (doc, mc_id) = parse_multicol(html);
    let segments = partition_children_into_segments(&doc, mc_id);
    assert_eq!(segments.len(), 1);
    match &segments[0] {
        Segment::ColumnGroup(ids) => assert_eq!(ids.len(), 3),
        _ => panic!("expected ColumnGroup"),
    }
}

#[test]
fn partition_span_at_top() {
    let html = r#"<!doctype html><html><body>
        <div style="column-count: 2;">
          <h1 style="column-span: all;">title</h1>
          <p>a</p><p>b</p>
        </div>
    </body></html>"#;
    let (doc, mc_id) = parse_multicol(html);
    let segments = partition_children_into_segments(&doc, mc_id);
    assert_eq!(segments.len(), 2);
    assert!(matches!(&segments[0], Segment::SpanAll(_)));
    match &segments[1] {
        Segment::ColumnGroup(ids) => assert_eq!(ids.len(), 2),
        _ => panic!("expected ColumnGroup"),
    }
}

#[test]
fn partition_span_in_middle() {
    let html = r#"<!doctype html><html><body>
        <div style="column-count: 2;">
          <p>a</p>
          <h1 style="column-span: all;">title</h1>
          <p>b</p>
        </div>
    </body></html>"#;
    let (doc, mc_id) = parse_multicol(html);
    let segments = partition_children_into_segments(&doc, mc_id);
    assert_eq!(segments.len(), 3);
    assert!(matches!(&segments[0], Segment::ColumnGroup(_)));
    assert!(matches!(&segments[1], Segment::SpanAll(_)));
    assert!(matches!(&segments[2], Segment::ColumnGroup(_)));
}

#[test]
fn partition_span_at_end() {
    let html = r#"<!doctype html><html><body>
        <div style="column-count: 2;">
          <p>a</p><p>b</p>
          <h1 style="column-span: all;">title</h1>
        </div>
    </body></html>"#;
    let (doc, mc_id) = parse_multicol(html);
    let segments = partition_children_into_segments(&doc, mc_id);
    assert_eq!(segments.len(), 2);
    match &segments[0] {
        Segment::ColumnGroup(ids) => assert_eq!(ids.len(), 2),
        _ => panic!("expected ColumnGroup"),
    }
    assert!(matches!(&segments[1], Segment::SpanAll(_)));
}

#[test]
fn partition_two_consecutive_spans() {
    let html = r#"<!doctype html><html><body>
        <div style="column-count: 2;">
          <h1 style="column-span: all;">t1</h1>
          <h2 style="column-span: all;">t2</h2>
          <p>a</p>
        </div>
    </body></html>"#;
    let (doc, mc_id) = parse_multicol(html);
    let segments = partition_children_into_segments(&doc, mc_id);
    assert_eq!(segments.len(), 3);
    assert!(matches!(&segments[0], Segment::SpanAll(_)));
    assert!(matches!(&segments[1], Segment::SpanAll(_)));
    assert!(matches!(&segments[2], Segment::ColumnGroup(_)));
}

#[test]
fn partition_nested_span_is_ignored() {
    // column-span: all is evaluated only on direct children of the
    // multicol container. A <span style="column-span: all"> buried inside
    // a non-span <p> must NOT split the ColumnGroup.
    let html = r#"<!doctype html><html><body>
        <div style="column-count: 2;">
          <p>a <span style="column-span: all;">inline</span> tail</p>
          <p>b</p>
        </div>
    </body></html>"#;
    let (doc, mc_id) = parse_multicol(html);
    let segments = partition_children_into_segments(&doc, mc_id);
    assert_eq!(segments.len(), 1);
    match &segments[0] {
        Segment::ColumnGroup(ids) => assert_eq!(ids.len(), 2),
        _ => panic!("expected ColumnGroup"),
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p fulgur --lib multicol_layout::tests::partition -- --nocapture`
Expected: compile errors — `Segment` / `partition_children_into_segments` not found.

**Step 3: Add `Segment` + `partition_children_into_segments`**

Add near the top of `multicol_layout.rs` (after the `use` block, before
`FulgurLayoutTree`):

```rust
/// One top-level slice of a multicol container's flattened child list.
///
/// Children are partitioned by `column-span: all` **on direct children
/// only** — nested descendants with `column-span: all` deep inside a
/// non-span child are ignored (CSS Multi-column Level 1).
#[derive(Debug)]
pub(crate) enum Segment {
    /// Children that participate in the balanced column grid.
    ColumnGroup(Vec<NodeId>),
    /// A single child that spans the full container width.
    SpanAll(NodeId),
}

/// Walk the direct children of `container_id`, grouping runs of non-span
/// children into `Segment::ColumnGroup` and emitting `Segment::SpanAll`
/// for each direct child carrying `column-span: all`.
pub(crate) fn partition_children_into_segments(
    doc: &BaseDocument,
    container_id: usize,
) -> Vec<Segment> {
    let children: Vec<usize> = doc
        .get_node(container_id)
        .map(|n| n.children.clone())
        .unwrap_or_default();

    let mut out: Vec<Segment> = Vec::new();
    let mut group: Vec<NodeId> = Vec::new();
    for child_id in children {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };
        // Skip text nodes that are entirely whitespace — they're HTML
        // pretty-printing noise with no layout content. Non-whitespace
        // text nodes stay in the ColumnGroup so they are not silently
        // dropped.
        if let Some(text) = child_node.text_data()
            && text.content.chars().all(char::is_whitespace)
        {
            continue;
        }
        // `column-span: all` only applies to element children; non-elements
        // (text content, comments) flow through as ordinary members of the
        // current ColumnGroup so their layout is preserved.
        let is_span = child_node.element_data().is_some()
            && crate::blitz_adapter::has_column_span_all(child_node);
        if is_span {
            if !group.is_empty() {
                out.push(Segment::ColumnGroup(std::mem::take(&mut group)));
            }
            out.push(Segment::SpanAll(NodeId::from(child_id)));
        } else {
            group.push(NodeId::from(child_id));
        }
    }
    if !group.is_empty() {
        out.push(Segment::ColumnGroup(group));
    }
    out
}
```

**Step 4: Verify**

Run: `cargo test -p fulgur --lib multicol_layout::tests::partition -- --nocapture`
Expected: 6 tests pass.

Also run: `cargo test -p fulgur --lib`
Expected: no regressions (477 + 6 = 483 or more).

**Step 5: Commit**

```bash
git add crates/fulgur/src/multicol_layout.rs
git commit -m "feat(fulgur): add Segment type + partition for multicol (fulgur-0vd)"
```

---

## Task 2: Refactor `compute_multicol_layout` to use segments (behavior-preserving)

**Files:**

- Modify: `crates/fulgur/src/multicol_layout.rs`

**Step 1: Goal**

Before adding SpanAll handling, refactor the current flat distribution loop
into a helper `layout_column_group(tree, container_w, col_w, gap, n, avail_h, children, y_offset) -> (placements, segment_height)` that operates on a
single segment. Keep the existing single-ColumnGroup behavior identical so
all existing tests stay green.

**Step 2: Extract helper — write failing test first**

Append in `#[cfg(test)] mod tests`:

```rust
#[test]
fn layout_column_group_matches_legacy_flat_balance() {
    // Baseline: a container with no column-span: all children, laid out
    // through the new `layout_column_group` helper, must produce the
    // same placements as the current compute_multicol_layout.
    let html = r#"<!doctype html><html><body>
        <div style="column-count: 2; column-gap: 0;">
          <p>alpha alpha alpha alpha</p>
          <p>beta beta beta beta</p>
          <p>gamma gamma gamma gamma</p>
          <p>delta delta delta delta</p>
        </div>
    </body></html>"#;
    let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);

    let mut tree = FulgurLayoutTree::new(&mut doc);
    tree.layout_multicol_subtrees();

    // Sanity: two distinct x positions exist after the refactor.
    let mc_id = collect_multicol_node_ids(&doc)[0];
    let mc_node_id = NodeId::from(mc_id);
    let child_count = doc.child_count(mc_node_id);
    let mut xs: Vec<f32> = (0..child_count)
        .map(|i| doc.get_unrounded_layout(doc.get_child_id(mc_node_id, i)).location.x)
        .collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs.dedup_by(|a, b| (*a - *b).abs() < 0.1);
    assert!(xs.len() >= 2, "expected ≥2 column x positions");
}
```

This test is a regression assertion — it passes now, it must still pass
after refactor.

**Step 3: Extract `layout_column_group`**

Inside `multicol_layout.rs`, add below `compute_multicol_layout`:

```rust
/// Place `children` into `n` columns of `col_w`, stacking them vertically
/// starting at `y_offset`. `avail_h` is the per-column budget ceiling
/// (for balance / auto fallback); measurement happens inside via Taffy.
///
/// Returns `(placements, segment_height)` where `segment_height` is the
/// max column bottom relative to `y_offset` (i.e. the vertical extent
/// this segment contributes to the container).
fn layout_column_group(
    tree: &mut FulgurLayoutTree<'_>,
    col_w: f32,
    gap: f32,
    n: u32,
    avail_h: f32,
    children: &[NodeId],
    y_offset: f32,
    child_inputs: taffy::tree::LayoutInput,
) -> (Vec<(NodeId, Point<f32>, Size<f32>)>, f32) {
    // 1. Measure
    let mut measured: Vec<(NodeId, Size<f32>)> = Vec::with_capacity(children.len());
    for &child in children {
        let output = tree.compute_child_layout(child, child_inputs);
        measured.push((child, output.size));
    }

    // 2. Balance budget
    let total_h: f32 = measured.iter().map(|(_, s)| s.height).sum();
    let budget = if total_h <= 0.0 {
        0.0
    } else if total_h >= avail_h * n as f32 {
        avail_h
    } else {
        balance_budget(&measured, n, avail_h, total_h)
    };

    // 3. Distribute
    let mut placements: Vec<(NodeId, Point<f32>, Size<f32>)> =
        Vec::with_capacity(children.len());
    let mut col_idx: u32 = 0;
    let mut col_y: f32 = 0.0;
    for (child_id, size) in &measured {
        if col_y > 0.0 && col_y + size.height > budget && col_idx + 1 < n {
            col_idx += 1;
            col_y = 0.0;
        }
        let col_x = col_idx as f32 * (col_w + gap);
        placements.push((
            *child_id,
            Point {
                x: col_x,
                y: y_offset + col_y,
            },
            *size,
        ));
        col_y += size.height;
    }

    // 4. Segment height = tallest column bottom relative to y_offset
    let seg_h = placements
        .iter()
        .map(|(_, loc, sz)| (loc.y - y_offset) + sz.height)
        .fold(0.0f32, f32::max)
        .max(0.0);

    (placements, seg_h)
}
```

Now replace the body of `compute_multicol_layout` to call
`layout_column_group` for the single ColumnGroup built from *all* direct
children (SpanAll handling is added in Task 3):

```rust
pub fn compute_multicol_layout(
    tree: &mut FulgurLayoutTree<'_>,
    node_id: NodeId,
    inputs: taffy::tree::LayoutInput,
) -> taffy::LayoutOutput {
    let Some(props) = tree
        .doc
        .get_node(usize::from(node_id))
        .and_then(crate::blitz_adapter::extract_multicol_props)
    else {
        return tree.doc.compute_child_layout(node_id, inputs);
    };

    let container_w = inputs
        .known_dimensions
        .width
        .or(match inputs.available_space.width {
            AvailableSpace::Definite(w) => Some(w),
            _ => None,
        })
        .unwrap_or(0.0);

    let gap = props.column_gap.max(0.0);
    let (n, col_w) =
        resolve_column_layout(container_w, props.column_count, props.column_width, gap);

    let avail_h = match inputs.available_space.height {
        AvailableSpace::Definite(h) => h,
        _ => f32::INFINITY,
    };

    let children: Vec<NodeId> = (0..tree.child_count(node_id))
        .map(|i| tree.get_child_id(node_id, i))
        .collect();

    let child_inputs = taffy::tree::LayoutInput {
        run_mode: RunMode::PerformLayout,
        sizing_mode: SizingMode::InherentSize,
        axis: RequestedAxis::Both,
        known_dimensions: Size {
            width: Some(col_w),
            height: None,
        },
        parent_size: Size {
            width: Some(col_w),
            height: inputs.parent_size.height,
        },
        available_space: Size {
            width: AvailableSpace::Definite(col_w),
            height: AvailableSpace::MaxContent,
        },
        vertical_margins_are_collapsible: Line::FALSE,
    };

    let (placements, container_h) = layout_column_group(
        tree,
        col_w,
        gap,
        n,
        avail_h,
        &children,
        /* y_offset = */ 0.0,
        child_inputs,
    );

    for (child_id, location, size) in &placements {
        let layout = taffy::Layout {
            order: 0,
            location: *location,
            size: Size {
                width: col_w,
                height: size.height,
            },
            content_size: *size,
            scrollbar_size: Size::ZERO,
            border: taffy::Rect::zero(),
            padding: taffy::Rect::zero(),
            margin: taffy::Rect::zero(),
        };
        tree.set_unrounded_layout(*child_id, &layout);
    }

    taffy::LayoutOutput {
        size: Size {
            width: container_w,
            height: container_h,
        },
        content_size: Size {
            width: container_w,
            height: container_h,
        },
        first_baselines: Point::NONE,
        top_margin: CollapsibleMarginSet::ZERO,
        bottom_margin: CollapsibleMarginSet::ZERO,
        margins_can_collapse_through: false,
    }
}
```

**Step 4: Run all tests**

Run: `cargo test -p fulgur --lib`
Expected: all tests (baseline + partition tests from Task 1 + the new
regression assertion) pass.

**Step 5: Commit**

```bash
git add crates/fulgur/src/multicol_layout.rs
git commit -m "refactor(fulgur): extract layout_column_group helper (fulgur-0vd)"
```

---

## Task 3: Segment-aware dispatch inside `compute_multicol_layout`

**Files:**

- Modify: `crates/fulgur/src/multicol_layout.rs`

**Step 1: Write failing tests**

Append in `#[cfg(test)] mod tests`:

```rust
// ── segment dispatch inside compute_multicol_layout ─────────────

/// Find the layout of a child node of the multicol container by its DOM id.
fn layout_of_id(doc: &BaseDocument, html_id: &str) -> taffy::Layout {
    fn walk(doc: &BaseDocument, node_id: usize, target: &str) -> Option<usize> {
        let node = doc.get_node(node_id)?;
        if let Some(ed) = node.element_data()
            && ed.attrs().iter().any(|a| {
                a.name.local.as_ref() == "id" && a.value.as_str() == target
            })
        {
            return Some(node_id);
        }
        for &c in &node.children {
            if let Some(found) = walk(doc, c, target) {
                return Some(found);
            }
        }
        None
    }
    let id = walk(doc, doc.root_element().id, html_id).expect("id not found");
    doc.get_unrounded_layout(NodeId::from(id))
}

#[test]
fn span_all_occupies_full_container_width() {
    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0;">
          <p id="before">before</p>
          <h1 id="title" style="column-span: all;">title</h1>
          <p id="after">after</p>
        </div>
    </body></html>"#;
    let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);
    let mut tree = FulgurLayoutTree::new(&mut doc);
    tree.layout_multicol_subtrees();

    let mc_w = layout_of_id(&doc, "mc").size.width;
    let title = layout_of_id(&doc, "title");
    assert!(
        (title.size.width - mc_w).abs() < 0.5,
        "SpanAll width {} should match container width {}",
        title.size.width,
        mc_w
    );
    assert!(
        title.location.x.abs() < 0.5,
        "SpanAll should start at x=0, got {}",
        title.location.x
    );
}

#[test]
fn segments_stack_vertically() {
    // segment 0 (ColumnGroup with 'before')
    // segment 1 (SpanAll 'title')
    // segment 2 (ColumnGroup with 'after')
    // Each must start below the previous one.
    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0;">
          <p id="before">before</p>
          <h1 id="title" style="column-span: all;">title</h1>
          <p id="after">after</p>
        </div>
    </body></html>"#;
    let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);
    let mut tree = FulgurLayoutTree::new(&mut doc);
    tree.layout_multicol_subtrees();

    let before = layout_of_id(&doc, "before");
    let title = layout_of_id(&doc, "title");
    let after = layout_of_id(&doc, "after");

    // title.y >= before.y + before.h
    assert!(
        title.location.y + 0.5 >= before.location.y + before.size.height,
        "title ({}) must start at or below 'before' bottom ({})",
        title.location.y,
        before.location.y + before.size.height
    );
    // after.y >= title.y + title.h
    assert!(
        after.location.y + 0.5 >= title.location.y + title.size.height,
        "after ({}) must start at or below 'title' bottom ({})",
        after.location.y,
        title.location.y + title.size.height
    );
}

#[test]
fn span_at_top_produces_one_segment_below() {
    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0;">
          <h1 id="title" style="column-span: all;">title</h1>
          <p id="a">a</p>
          <p id="b">b</p>
        </div>
    </body></html>"#;
    let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);
    let mut tree = FulgurLayoutTree::new(&mut doc);
    tree.layout_multicol_subtrees();

    let title = layout_of_id(&doc, "title");
    let a = layout_of_id(&doc, "a");
    let b = layout_of_id(&doc, "b");

    // a and b are in columns below title. Both .y must be >= title bottom.
    let title_bottom = title.location.y + title.size.height;
    assert!(a.location.y + 0.5 >= title_bottom);
    assert!(b.location.y + 0.5 >= title_bottom);

    // a and b should appear in different columns (x positions).
    assert!(
        (a.location.x - b.location.x).abs() > 0.5,
        "a.x={} b.x={} should be in different columns",
        a.location.x,
        b.location.x
    );
}

#[test]
fn span_at_end_sits_below_columns() {
    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0;">
          <p id="a">a</p>
          <p id="b">b</p>
          <h1 id="title" style="column-span: all;">title</h1>
        </div>
    </body></html>"#;
    let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);
    let mut tree = FulgurLayoutTree::new(&mut doc);
    tree.layout_multicol_subtrees();

    let a = layout_of_id(&doc, "a");
    let b = layout_of_id(&doc, "b");
    let title = layout_of_id(&doc, "title");
    let col_bottom = (a.location.y + a.size.height).max(b.location.y + b.size.height);
    assert!(
        title.location.y + 0.5 >= col_bottom,
        "title ({}) must sit below column bottom ({})",
        title.location.y,
        col_bottom
    );
}

#[test]
fn nested_span_does_not_break_column_layout() {
    // A descendant span with column-span: all deep inside a non-span
    // direct child must NOT create a segment break. The outer <p> should
    // still participate in the column grid normally.
    let html = r#"<!doctype html><html><body>
        <div id="mc" style="column-count: 2; column-gap: 0;">
          <p id="a">a <span style="column-span: all;">inline</span> tail</p>
          <p id="b">b</p>
        </div>
    </body></html>"#;
    let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);
    let mut tree = FulgurLayoutTree::new(&mut doc);
    tree.layout_multicol_subtrees();

    let a = layout_of_id(&doc, "a");
    let b = layout_of_id(&doc, "b");
    // a and b either stack in one column or split into two columns; what
    // matters is that neither starts at a y forced by a phantom segment
    // break. Check they are in the column grid by width ≈ col_w, not
    // container_w.
    let mc_w = layout_of_id(&doc, "mc").size.width;
    assert!(
        a.size.width < mc_w * 0.9,
        "'a' width {} looks like full container width {} — nested span leaked out",
        a.size.width,
        mc_w
    );
    assert!(
        b.size.width < mc_w * 0.9,
        "'b' width {} looks like full container width {}",
        b.size.width,
        mc_w
    );
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p fulgur --lib multicol_layout -- --nocapture`
Expected: the 5 new segment-dispatch tests fail (`title` gets laid out
as a column child, not span-all).

**Step 3: Implement segment dispatch**

In `multicol_layout.rs`, rewrite `compute_multicol_layout` again — this
time driving the layout off the partitioned segments.

Replace the single `layout_column_group` call with:

```rust
    let segments = partition_children_into_segments(tree.doc, usize::from(node_id));

    // Pre-compute one SpanAll child_inputs variant keyed to container_w.
    let span_child_inputs = taffy::tree::LayoutInput {
        run_mode: RunMode::PerformLayout,
        sizing_mode: SizingMode::InherentSize,
        axis: RequestedAxis::Both,
        known_dimensions: Size {
            width: Some(container_w),
            height: None,
        },
        parent_size: Size {
            width: Some(container_w),
            height: inputs.parent_size.height,
        },
        available_space: Size {
            width: AvailableSpace::Definite(container_w),
            height: AvailableSpace::MaxContent,
        },
        vertical_margins_are_collapsible: Line::FALSE,
    };

    let mut all_placements: Vec<(NodeId, Point<f32>, Size<f32>, f32 /* width to assign */)> = Vec::new();
    let mut cursor_y: f32 = 0.0;
    for seg in &segments {
        match seg {
            Segment::ColumnGroup(children) => {
                let (placements, seg_h) = layout_column_group(
                    tree,
                    col_w,
                    gap,
                    n,
                    avail_h,
                    children,
                    cursor_y,
                    child_inputs,
                );
                for (id, loc, sz) in placements {
                    all_placements.push((id, loc, sz, col_w));
                }
                cursor_y += seg_h;
            }
            Segment::SpanAll(child_id) => {
                let output = tree.compute_child_layout(*child_id, span_child_inputs);
                all_placements.push((
                    *child_id,
                    Point { x: 0.0, y: cursor_y },
                    output.size,
                    container_w,
                ));
                cursor_y += output.size.height;
            }
        }
    }

    for (child_id, location, size, width) in &all_placements {
        let layout = taffy::Layout {
            order: 0,
            location: *location,
            size: Size {
                width: *width,
                height: size.height,
            },
            content_size: *size,
            scrollbar_size: Size::ZERO,
            border: taffy::Rect::zero(),
            padding: taffy::Rect::zero(),
            margin: taffy::Rect::zero(),
        };
        tree.set_unrounded_layout(*child_id, &layout);
    }

    let container_h = cursor_y.max(0.0);
```

Remove the prior single-call variant. Keep the `LayoutOutput` return as
before.

**Step 4: Run tests**

Run: `cargo test -p fulgur --lib`
Expected: all tests green — existing multicol tests still pass, new
segment tests now pass.

Also run: `cargo test -p fulgur`
Expected: integration tests untouched (no regression).

Also run: `cargo clippy -p fulgur --lib -- -D warnings`
Expected: clean.

**Step 5: Commit**

```bash
git add crates/fulgur/src/multicol_layout.rs
git commit -m "feat(fulgur): column-span: all segment layout in multicol hook (fulgur-0vd)"
```

---

## Task 4: Page-spanning SpanAll integration test

**Files:**

- Create: `crates/fulgur/tests/multicol_span_all.rs`

**Step 1: Write the test**

```rust
//! Integration test for CSS `column-span: all` (fulgur-0vd).
//!
//! Covers the "SpanAll child that itself page-breaks" acceptance case
//! from the issue. When a `column-span: all` subtree is larger than one
//! page, the existing BlockPageable pagination must split the full-width
//! block across pages cleanly — no column structure leaks into the spill.

use fulgur::{Engine, PageSize};

fn page_count(pdf_bytes: &[u8]) -> usize {
    // Lightweight: count PDF `/Type /Page` occurrences. Avoids pulling a
    // full parser. Matches the heuristic used by render.rs tests.
    let s = std::str::from_utf8(pdf_bytes).unwrap_or("");
    s.matches("/Type /Page\n").count() + s.matches("/Type /Page ").count()
}

#[test]
fn span_all_subtree_that_exceeds_one_page_splits_across_pages() {
    // Build a SpanAll block with enough text to spill beyond A6 (small
    // paper size → guaranteed multi-page even for modest content).
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

    let engine = Engine::builder()
        .page_size(PageSize::A6)
        .build()
        .expect("engine");
    let pdf = engine.render_html(&html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected ≥2 pages from oversized SpanAll, got {}",
        page_count(&pdf)
    );
}

#[test]
fn span_all_fits_single_page_for_short_content() {
    // Control: a short SpanAll block must not force extra pages.
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

    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .build()
        .expect("engine");
    let pdf = engine.render_html(html).expect("render");
    assert_eq!(
        page_count(&pdf),
        1,
        "short content should fit one A4 page"
    );
}
```

**Step 2: Run the integration tests**

Run: `cargo test -p fulgur --test multicol_span_all`
Expected: both tests pass. If they compile but fail, fix any discovered
issue in the Taffy hook.

**Step 3: Sanity — run the full suite**

Run: `cargo test -p fulgur`
Expected: everything green.

**Step 4: Commit**

```bash
git add crates/fulgur/tests/multicol_span_all.rs
git commit -m "test(fulgur): integration test for page-spanning SpanAll (fulgur-0vd)"
```

---

## Task 5: Doc comment + module comment pass

**Files:**

- Modify: `crates/fulgur/src/multicol_layout.rs`

**Step 1: Update the file-level doc**

The module doc says "routes them through `compute_multicol_layout`" —
add a sentence mentioning `column-span: all` segmentation:

```rust
//! Taffy custom layout hook for CSS Multi-column Layout.
//!
//! [`FulgurLayoutTree`] wraps a [`blitz_dom::BaseDocument`] as a Taffy
//! `LayoutPartialTree`, intercepts multicol containers, and routes them
//! through [`compute_multicol_layout`]. Direct children are partitioned
//! by `column-span: all` into alternating `ColumnGroup` / `SpanAll`
//! segments: columnar segments run through `layout_column_group`
//! (balance distribution at `col_w`), and SpanAll segments occupy the
//! full container width and stack vertically between column groups.
//!
//! Everything else delegates to `BaseDocument`'s built-in dispatch.
//! [...]
```

**Step 2: Update `compute_multicol_layout`'s doc block**

Replace the pipeline list to reflect segmentation.

**Step 3: Run formatting + lint**

Run: `cargo fmt --check`
Run: `cargo clippy -p fulgur --lib -- -D warnings`
Both expected clean.

**Step 4: Commit**

```bash
git add crates/fulgur/src/multicol_layout.rs
git commit -m "docs(fulgur): note column-span: all segmentation in multicol_layout (fulgur-0vd)"
```

---

## Task 6: Verify + push

**Step 1: Run every relevant test**

```bash
cargo test -p fulgur --lib
cargo test -p fulgur
cargo clippy -p fulgur --lib -- -D warnings
cargo fmt --check
```

All must pass clean.

**Step 2: Review the diff end-to-end**

```bash
git log --oneline epic/fulgur-qkg-css-columns..HEAD
git diff epic/fulgur-qkg-css-columns..HEAD -- crates/fulgur/src/multicol_layout.rs | head -200
```

Confirm there is no collateral change outside multicol_layout.rs + the
new integration test.

**Step 3: Push + PR (only after user confirms)**

Do not push automatically — wait for user to say so. Then:

```bash
git push -u origin feature/fulgur-0vd-multicol-span-all
```

Open PR against `epic/fulgur-qkg-css-columns`.

---

## Acceptance Checklist

- [ ] `partition_children_into_segments` unit tests pass (top / middle /
      end / two consecutive / all columnar / nested-ignored).
- [ ] SpanAll child's `final_layout` has `size.width == container_w`
      and `location.x == 0`.
- [ ] Segments stack vertically — `cursor_y` grows monotonically.
- [ ] Short-content control case still fits one page.
- [ ] Oversized SpanAll case renders ≥2 pages.
- [ ] All existing multicol tests green (no regression in
      balance / ancestor-propagation / resolve_column_layout).
- [ ] `cargo fmt --check` and `cargo clippy` clean.
