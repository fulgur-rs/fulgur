# Phase 4 Design: Pageable Type Deletion

Date: 2026-04-30
Issue: fulgur-9t3z (Phase 4 epic)
Depends on: PR #297 (VRT GCPM fixtures), PR #298 (Pageable inventory)
Brainstormed: 2026-04-30 (`superpowers:brainstorming` session)

## TL;DR

Delete the `Pageable` trait and all 17 impls; rebuild the render path as a
geometry-driven walker over node-keyed side-channel maps (no central
`DrawOp` enum). Roll out via a parallel `render_v2` path with shadow
byte-equality testing across the full 56-fixture suite (VRT 39 +
examples_determinism 11 + GCPM 6). Migrate types in 8 PRs, hardest-first
on Paragraph in PR 3 to detect IR design failure early.

## Goals

- Delete `crates/fulgur/src/pageable.rs` and the `Pageable` trait.
- Keep `Engine::render_html` API source-compatible.
- Maintain byte-identical output across all 56 existing fixtures.
- Maintain or improve performance against the Phase 3 baseline.

## Non-goals

- New rendering features (deferred to Phase 5+).
- Refactoring outside the `Pageable` deletion path (e.g. `convert::*`
  module reshape, `pagination_layout` cleanup beyond what Phase 4 forces).
- Public API redesign beyond removing now-orphaned `Pageable`-typed
  parameters.

## Constraints

- **No parity gate from Phase 3**: Phase 3.4 deleted
  `assert_*_states_parity` helpers. The only correctness check available
  is end-to-end PDF byte equality.
- **Phase 3 lessons**: Phase 3.4 nearly shipped a silent regression
  (running elements disappearing from margin boxes) that all existing
  tests missed. PR #297 added GCPM fixtures to plug that gap; Phase 4
  must rely on those new fixtures plus shadow byte-eq.
- **`engine.rs` boundary**: `Engine::render_html` is the only public
  entry point that produces PDFs. Changes inside its body are free; the
  signature is fixed.

## Approach (decided in brainstorm)

### Rollout: parallel render path with shadow byte-equality

Build `render_v2` next to the existing `render_to_pdf`/
`render_to_pdf_with_gcpm`. CI runs every fixture through both paths and
asserts PDF byte equality. An `allowlist` (`crates/fulgur/tests/
render_path_parity.toml`) gates which fixtures must pass — PRs grow the
allowlist as types migrate. When the allowlist covers all 56 fixtures,
default flips to v2 in one PR; v1 deletion is the next PR.

Why not big-bang: Phase 3.4's silent regression precedent says we cannot
trust a single large drop without external byte-eq.

Why not type-by-type behind the live trait: keeping `Pageable` alive
during migration means every transition type would carry both old and
new wiring, which adds friction without reducing risk.

### IR shape: walker over node-keyed side-channel maps (no DrawOp enum)

`convert::dom_to_pageable` becomes `convert::dom_to_drawables`, returning
a `Drawables` struct holding `BTreeMap<NodeId, T>` for each draw concern:

```rust
pub struct Drawables {
    pub block_styles:    BTreeMap<NodeId, BlockStyle>,
    pub paragraphs:      BTreeMap<NodeId, ShapedPara>,    // Vec<ShapedLine>
    pub images:          BTreeMap<NodeId, ImageData>,
    pub svgs:            BTreeMap<NodeId, SvgData>,
    pub tables:          BTreeMap<NodeId, TableData>,
    pub list_items:      BTreeMap<NodeId, ListItemData>,
    pub multicol_rules:  BTreeMap<NodeId, MulticolRuleData>,
    pub transforms:      BTreeMap<NodeId, TransformOp>,
    pub bookmark_anchors: BTreeMap<NodeId, BookmarkInfo>,
    pub link_spans:      Vec<(NodeId, LinkSpan)>,
}
```

`render_v2` walks `PaginationGeometryTable` per page; for each
(node_id, fragment) it dispatches to a per-type pure draw function:

```rust
for page_idx in 0..page_count {
    for (node_id, frag) in geometry.fragments_on_page(page_idx) {
        draw_node(canvas, node_id, frag, drawables, transform_stack);
    }
}
```

Why not a `DrawOp` enum: a 17-variant enum mirroring the Pageable types
adds an abstraction layer that the inventory shows is not needed. The
concrete data types (`BlockStyle`, `ShapedLine`, etc.) already exist and
are owned per node — keying them by `NodeId` directly removes 17 enum
variants and their match arms.

Why not re-shape paragraphs at render time: parley shaping is expensive
and `ShapedLine` is already produced once at convert time. `Drawables`
keeps that single computation; render only walks it.

## Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│  Engine::render_html(html)                                  │
│  ┌────────────────┐    ┌──────────────────────────────┐     │
│  │ Blitz parse +  │    │ pagination_layout fragmenter │     │
│  │ resolve + DOM  │───▶│  → PaginationGeometryTable   │     │
│  │ passes         │    │    (existing, unchanged)     │     │
│  └────────────────┘    └──────────────────────────────┘     │
│           │                      │                          │
│           ▼                      ▼                          │
│  ┌──────────────────────────────────────────────────┐       │
│  │  convert::dom_to_drawables(doc, ctx) → Drawables │       │
│  │    (replaces dom_to_pageable, no Box<dyn ...>)   │       │
│  └──────────────────────────────────────────────────┘       │
│                          │                                  │
│                          ▼                                  │
│  ┌──────────────────────────────────────────────────┐       │
│  │  render::render_v2(geometry, drawables, gcpm)    │       │
│  │  for page in pages:                              │       │
│  │    for (node_id, fragment) in                    │       │
│  │        geometry.fragments_on_page(page):         │       │
│  │      draw_node(canvas, node_id, fragment,        │       │
│  │                drawables, transform_stack)       │       │
│  └──────────────────────────────────────────────────┘       │
└─────────────────────────────────────────────────────────────┘
```

`draw_node` is a thin dispatcher: lookup `node_id` in each map, call the
per-type pure draw function, recurse for containers.

## Hard-case handling

### Transform (link-rect transform stack)

Inventory's only non-trivial wrapper. `Drawables.transforms[node_id]`
stores the `TransformOp { matrix, origin }`. `draw_node` carries a
`transform_stack: &mut Vec<Affine2D>` and pushes/pops while recursing
into children. Link rects emitted inside the transform get the stack
applied at emit time, mirroring the existing `LinkCollector` behavior.

### Paragraph (line-internal state)

`Drawables.paragraphs[node_id]: ShapedPara = Vec<ShapedLine>`. The
existing `paragraph::draw_shaped_lines(canvas, &lines, x, y)` is reused
directly — Phase 4 only changes the call site, not the per-line
glyph/decoration/link logic.

Inline-box recursion (`LineItem::InlineBox`) requires the inline-box
content to be reachable from `Drawables`. The inline-box's content is
already represented as a Pageable subtree today; in Phase 4 it becomes
a node entry in the appropriate `Drawables` map (`block_styles`,
`paragraphs`, etc.) and the recursion edge calls `draw_node` again.

### Table (per-page header repetition)

`Drawables.tables[node_id]: TableData {
    header_cell_ids: Vec<NodeId>,
    body_cell_offsets: Vec<(NodeId, Pt)>,  // y in table-local pt
    header_height: Pt,
}`. `draw_table_node` per page:

1. Paint table border-box at fragment position.
2. Draw all `header_cell_ids` at y in [0, header_height) — repeated on
   every page that the table spans.
3. Draw the subset of `body_cell_offsets` whose y falls within the
   page's table fragment, rebased so first kept body sits at
   `header_height`.

The "subset on this page" filter mirrors current `slice_for_page`, but
operates on `&[NodeId]` instead of cloning Pageable subtrees.

### Markers and marker wrappers (4 types)

`StringSetPageable` / `RunningElementMarker` / `CounterOpMarker` already
have no draw side effect — Phase 3.4 routed their state collection
through fragmenter geometry. Their wrappers (`*Wrapper`) only delegate
draw. **All four types are deleted from the draw path entirely** in
Phase 4. Bookmark anchors stay (they need a per-page (page_idx, y)
record), but move from a Canvas-side collector to
`Drawables.bookmark_anchors` consulted at render time.

## Rollout sequence

| PR | Scope | Outcome |
|----|-------|---------|
| 1 | `Drawables` struct + `render_v2` skeleton (no-op for every node) + shadow harness wire-up + empty allowlist | CI green, configuration plumbed |
| 2 | Trivial leaves: Spacer / Image / SVG + 5 markers (metadata-only) | walker pattern + harness validated; allowlist gains ~5 fixtures |
| 3 | Paragraph (hardest leaf — line-internal state, link spans, decorations, inline-box recursion) | Hard-first design validation; allowlist gains text-heavy fixtures |
| 4 | BlockPageable (background / border / shadow / overflow clip) | Largest type by code mass; allowlist gains majority of fixtures |
| 5 | Table + ListItem (per-page header repeat / list marker) | Container types |
| 6 | TransformWrapper + MulticolRule + 4 marker wrappers | Composition concerns; allowlist should reach 56/56 |
| 7 | Default flip — `Engine::render_html` calls `render_v2` | Single-PR cutover, easy revert |
| 8 | v1 deletion — `pageable.rs`, `Pageable` trait, `convert::dom_to_pageable`, all 17 impls | Bulk deletion, ~5K LOC |

PRs 2-6 each grow the allowlist; PR 7 requires 56/56 coverage by CI
gate. PRs 7 and 8 can be the same PR if review tolerates the diff size,
but separating them reduces revert blast radius.

## Risks

| ID | Risk | Mitigation |
|----|------|------------|
| R1 | Paragraph IR design fails in PR 3 | Hard-first order detects this within 1 sprint of starting; rework PR 3 + adjust later PRs |
| R2 | Shadow harness long-tail diff (1-2 fixtures fail with no obvious cause) | Allowlist grows per-type, scope of regression is bounded to last added type |
| R3 | Inline-box recursion edge inflates v2 design | Inline-box uses existing `LineItem::InlineBox` shape; if blocked, embed v1 result via fallback (1 PR slip) |
| R4 | Table per-page slicing diverges from Phase 3 `slice_for_page` | Read current `slice_for_page` carefully; ship Block + List before Table so reference behavior is locked |
| R5 | PR 7 flip uncovers fixture not in allowlist | CI gate prevents flip until allowlist == 56 fixtures |
| R6 | Two-system cognitive load during PRs 1-7 | Code comments mark v1/v2; PR 8 deletion follows immediately |

## Open questions (resolve at PR 1)

1. `Drawables` lifetime — extend `ConvertContext` or new struct returned
   by `dom_to_drawables`?
2. GCPM context (`running_store` / `page_settings`) — fold into
   `Drawables` or pass alongside as today?
3. Shadow harness location — `crates/fulgur/tests/render_path_parity.rs`
   vs separate binary. Default: in-tree test, gated by `nextest` filter
   if runtime exceeds 5 seconds.
4. PR 6 ordering — Transform first or Multicol first? Transform affects
   link rect path; Multicol depends on Block being live in v2. Default:
   Transform → Multicol → 4 marker wrappers.

## Acceptance criteria (Phase 4 epic)

- `crates/fulgur/src/pageable.rs` deleted.
- `Pageable` trait, all 17 impls, `convert::dom_to_pageable` deleted.
- `Engine::render_html` API source-compatible (no caller changes).
- All 56 fixtures byte-identical: VRT 39 + examples_determinism 11 +
  GCPM 6.
- `cargo doc --no-deps` warning-free.
- `cargo test -p fulgur --lib` all green (existing 840 + Phase 4
  additions).
- Perf: long-document fixture (30+ pages) v2 at parity or faster
  vs current.

## Relationship to future phases

After Phase 4, the only remaining "Pageable-shaped" code is `Drawables`
itself — a flat per-NodeId map structure with no trait dispatch. Future
work (Phase 5 if it exists) could collapse `Drawables` into
`PaginationGeometryTable` directly, making each fragmenter entry carry
its draw payload inline. That is out of scope here.
