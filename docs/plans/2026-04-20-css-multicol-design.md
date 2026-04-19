# CSS Multi-column Layout Design

**Date**: 2026-04-20 (revised after Phase A-2 spike)
**Epic**: fulgur-qkg
**Status**: Design v2 — Taffy custom-layout-hook direction

## Goal

Support CSS Multi-column Layout in fulgur so that newspaper- and magazine-style
page layouts render correctly. Scope is delivered in phases to keep each change
reviewable and to derisk the pagination interaction.

## Phased scope

### Phase A (MVP)

- `column-count`, `column-width`, `column-gap`
- `column-fill: balance` (CSS default) — short content balanced across columns
- `column-fill: auto` — fills first column completely before advancing
- `column-span: all`
- Page-spanning: a multicol container larger than one page keeps flowing across
  subsequent pages
- `column-rule-*` (`style`, `color`, `width`, shorthand)
- `break-inside: avoid`

### Phase B

- `break-before`, `break-after` with `column | page | avoid`
- Inline images, borders, and padding inside the column flow

### Phase C (follow-up epics)

- Nested multicol
- `double` / `groove` / `ridge` / `inset` / `outset` column-rule styles
- `column-span: <integer>` (Multi-column Level 2)

## Why fulgur implements this itself

- `taffy 0.9.2` has no multicol display mode; its `column_count` APIs are for
  CSS Grid tracks, not multicol.
- `stylo 0.8.0` parses most `column-*` properties but `blitz-dom 0.2.4` does
  not lay them out — a multicol container is treated as a regular block.
- So fulgur must own the column layout between Blitz (which gives us the
  styles + Parley-shaped inline content) and Krilla (which gets the final
  positioned fragments).

### stylo engine gating (A-0 finding, 2026-04-20)

`stylo 0.8.0` uses engine-gated property definitions (see
`properties/longhands/column.mako.rs`). Blitz inherits stylo's default feature
`servo`, so these are available on `ComputedValues`:

- `column-width`, `column-count`, `column-gap`, `column-span`

But these are **gecko-only** and therefore inaccessible via blitz:

- `column-fill`
- `column-rule-width`, `column-rule-style`, `column-rule-color`

Impact on the plan:

- `column-fill`: Phase A defaults to balance (CSS default). A thin stylesheet
  parser lands alongside `column-rule-*` (A-4) and exposes an explicit
  `auto` opt-out at the same time.
- `column-rule-*`: A-4 adds a small custom CSS parser using `cssparser`
  (already a dep via `crates/fulgur/src/gcpm/parser.rs`) that sniffs these
  declarations from inline `style="…"` and top-level stylesheet rules with
  a minimal tag / class / id selector matcher. Full cascade re-implementation
  is out-of-scope.

## Architecture — Taffy custom layout hook

After the v1 spike (see *Spike retrospective* below) exposed structural issues
with the "reshape-after-Taffy" approach, the design pivots to integrating
multicol **into** the layout pass itself. The direction mirrors Blitz's own
solution for inline content, where Parley is wired into Taffy via a custom
per-node layout function: fulgur registers a layout hook for multicol
containers so Taffy sees the balanced column height as the container's
intrinsic size and positions surrounding siblings accordingly.

```text
HTML
  ↓ Blitz (parse + stylo styles + Parley shaping)
Style-resolved DOM
  ↓ fulgur layout pass (wraps blitz's BaseDocument as a Taffy tree with a
  │                     custom multicol compute)
Taffy layout output (includes correctly-sized multicol containers)
  ↓ convert.rs (reads Taffy positions, builds Pageable tree)
Pageable tree
  ↓ paginate.rs (unchanged)
  ↓ render.rs  (unchanged)
Krilla PDF
```

Key property: **no post-pass reshape or dynamic height adjustment**. Taffy
owns the single source of truth for every element's final layout, and
multicol is just one more display mode it knows how to compute.

## Integration constraints

- **No blitz fork.** fulgur ships to crates.io; forking blitz would block
  publishing (transitive fork of stylo/taffy). All integration happens at
  fulgur's boundary with blitz.
- **Re-use blitz's inline shaping.** Parley is expensive; we must not re-shape
  text that blitz already shaped. The custom multicol compute re-*breaks*
  lines at `col_w` on the existing parley `Layout` clone (the same technique
  the Phase A-2 spike validated).
- **Taffy 0.9.2 as-is.** No taffy fork. Use `LayoutPartialTree` /
  `compute_*_layout` public APIs only.
- **Deterministic output.** Layout must be idempotent; the multicol compute
  function must not rely on global state.

## Implementation plan

### Step 1 — Hook surface (spike)

Prove the Taffy hook pattern works end-to-end:

- Identify how Blitz dispatches per-node layout inside `BaseDocument::resolve`.
- Determine the cheapest way for fulgur to inject a custom compute for
  multicol containers without editing blitz:
  - Option a. Re-run Taffy on multicol subtrees using our own
    `LayoutPartialTree` wrapper, writing results back into blitz's tree.
  - Option b. Wrap `BaseDocument` entirely; fulgur drives Taffy, delegating
    non-multicol layout to blitz's built-in code paths.
- Build a minimum `compute_multicol_layout(tree, node_id, inputs)` that
  re-breaks inline content at `col_w` and returns a `LayoutOutput` with the
  correct size. Exercise it on a single fixture.

Deliverable: working 2-column block with text that doesn't overlap
siblings and respects the container's natural balanced height.

### Step 2 — Port A-1 / A-2 semantics

- Column-count / column-width / column-gap → the `resolve_column_layout`
  function from the v1 spike (already unit-tested) ports over unchanged.
- Reshape via `ParleyReshapeSource` — the mechanism works and can be lifted.
  Instead of caching on `ParagraphPageable`, the hook invokes it inline when
  Taffy asks for the multicol's size.
- `column-fill: balance` — run `distribute_with_fill` inside the hook,
  matching the v1 spike's budget search. Because Taffy sees the balanced
  height directly, the parent-sibling-overflow issue from v1 disappears.

### Step 3 — A-3: column-span: all

At layout time, a descendant with `column-span: all` truncates the current
column-group, lays itself out at the container's full content width, then
starts a new column-group below. The custom compute walks the flattened
children list and dispatches per-segment.

### Step 4 — A-4: column-rule + custom CSS parser

Parse `column-rule-*` and `column-fill` from the stylesheet into a side-table
keyed by DOM node id. The multicol compute consults the side-table at layout
time and emits rule geometry alongside the positioned columns, which the
render pass paints between adjacent non-empty columns.

### Step 5 — A-5: break-inside: avoid

Children flagged with `break-inside: avoid` are promoted to the next column
(or the next page at the last column) when they don't fit the current
column budget. Integrates cleanly with the hook's distribute step.

### Step 6 — A-6: fixtures + VRT + examples

Integration tests per `crates/fulgur/tests/multicol_integration.rs` plus VRT
goldens in `crates/fulgur-vrt`:

- `multicol-basic-2col`
- `multicol-rule-solid`
- `multicol-span-all`
- `multicol-page-spanning`
- `multicol-break-inside-avoid`
- `multicol-column-width-resolution`
- `multicol-balance`

Add a headline example under `examples/multicol/`.

## Spike retrospective — why v1 (reshape-after-Taffy) was shelved

The v1 design (see the commit history on `feature/fulgur-qkg-multicol-phase-a`
and PR #123) introduced a `MulticolPageable` that re-broke inline content at
`col_w` *after* Taffy laid out the tree, then distributed across columns at
the Pageable layer. The spike proved three things:

1. **Reshape works.** Parley's `Layout::break_all_lines(Some(width))` does
   produce correctly-widthed lines without re-shaping glyphs. A round-trip
   test confirmed ≥1.5× line count at column width vs container width.
2. **`column-fill: balance` is tractable.** A greedy budget-search
   distribution converges in ≤20 iterations and visually balances.
3. **Taffy-level layout is non-negotiable.** The blocker is structural:
   Taffy positions siblings after a multicol using the pre-reshape single-
   column height, and there is no safe way to re-flow them afterwards. A
   naive "recompute parent `pc.y` from cumulative heights" pass broke
   unrelated paginate tests that intentionally pack children at duplicate y
   positions. Correctly propagating a post-layout height change requires a
   second Taffy pass, which is effectively what this v2 design does up-front.

Artifacts to carry forward from the spike:

- `blitz_adapter::extract_multicol_props` — already on this branch (A-0).
- `blitz_adapter::has_column_span_all` — already on this branch.
- `extract_vertical_align` px → pt fix — unrelated drive-by from the spike,
  already on this branch.
- Design ideas about `ParleyReshapeSource`, `resolve_column_layout`,
  `distribute_with_fill`, and balance search — re-implemented inside the
  Taffy hook.

## Phase A work breakdown (v2)

| Seq | Title | Type |
|---|---|---|
| A-0 | multicol: verify stylo parsing + thread column-* through blitz_adapter | task (closed) |
| A-1b | multicol: Taffy-hook scaffold — integrate custom compute for multicol containers | feature |
| A-2b | multicol: column-fill: balance + auto inside the Taffy hook | feature |
| A-3 | multicol: `column-span: all` | feature |
| A-4 | multicol: `column-rule-*` + custom stylesheet parser | feature |
| A-5 | multicol: `break-inside: avoid` | feature |
| A-6 | multicol: VRT fixtures + integration tests + examples | task |

Dependency chain: `A-0 → A-1b → A-2b → {A-3, A-4, A-5}` (parallelisable) `→ A-6`.
