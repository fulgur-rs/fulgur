# Pagination Layout Spike (fulgur-4cbc)

Spike: feasibility evaluation for replacing fulgur's post-layout
`Pageable` pagination with a Taffy `LayoutPartialTree` wrapper that
intercepts the body and dispatches its own block-fragmentation logic —
the same idiom `multicol_layout.rs` already uses for column balancing
and `blitz_adapter::relayout_position_fixed` already uses for
`position: fixed` viewport-CB recovery.

Status: spike complete on branch `spike/fulgur-4cbc-pagination-layout`,
not merged. This document captures findings and recommends a follow-up
plan.

## What landed

`crates/fulgur/src/pagination_layout.rs` (~570 lines including tests)
adds a sibling of `multicol_layout.rs`:

- `PaginationLayoutTree<'a>` wraps `BaseDocument` as a Taffy tree and
  forwards every `LayoutPartialTree` / `RoundTree` / `CacheTree` /
  `TraversePartialTree` method to `BaseDocument`.
- `compute_child_layout` intercepts the body and routes it through
  `compute_pagination_layout`, which delegates the actual layout to
  `BaseDocument` and post-walks the children to populate a
  `PaginationGeometryTable: BTreeMap<usize, PaginationGeometry>`.
- `run_pass(doc, page_height_px)` drives the wrapper through
  `taffy::compute_root_layout(&mut tree, body_id, MaxContent height)`
  and saves/restores body's `location` so downstream readers see the
  same coordinates Blitz's first pass produced.
- 5 unit tests + 1 comparison harness (`mod compare_with_pageable`)
  with 10 fixtures comparing page count against
  `paginate::paginate(...)`.

The module is wired into `lib.rs` as `pub(crate)` and is **not** called
from `engine.rs`. `#![allow(dead_code)]` on the file documents the
spike status.

## Comparison harness results

10 fixtures driven through both `paginate::paginate` (Pageable side)
and `pagination_layout::run_pass` (spike side). Page counts:

| Fixture | Pageable | Spike | Agree |
|---|---|---|---|
| empty body | 1 | 1 | ✓ |
| three short blocks fit one page | 1 | 1 | ✓ |
| two blocks split across two pages | 2 | 2 | ✓ |
| five blocks span three pages | 3 | 3 | ✓ |
| single 100vh div (taller than content) | 1 | 1 | ✓ |
| two 50vh divs sum to 100vh | 2 | 2 | ✓ |
| one 1028px div (just over content) | 1 | 1 | ✓ |
| nested 100% height with no parent height | 1 | 1 | ✓ |
| long paragraph wraps into multiple pages | **2** | **1** | ✗ |
| small lead block + oversized block | 2 | 2 | ✓ |

9 of 10 agree. The single disagreement is the **inline text wrapping**
case: Pageable's `ParagraphPageable::split` (paragraph.rs:945) splits
at Parley line boundaries, so a tall paragraph spans multiple pages.
The block-only spike treats the paragraph as one block child and emits
it as a single oversized fragment.

## Findings

### F1. `vh` resolution is not the bottleneck the spike was trying to solve

Going in, the hypothesis was that `vh` semantics would diverge between
Pageable (post-layout, `wrap()` probes with `avail_height = 10000`) and
the spike (Taffy-time). In practice **both report identical page
counts** for `100vh`, two `50vh`, `1028px`, and nested `100%` fixtures.
Reasons:

- `engine.rs:88-89` already passes `pt_to_px(page_height) as u32` as
  the viewport height, so Stylo resolves `1vh = 1% of one A4 page`
  before either path runs. The 10000 ghost we feared lives only in
  `Pageable::wrap` measure probes (`pageable.rs:579-580`,
  `convert/block.rs:71`, etc.) — by then `vh` has already been folded
  into f32 px and the probe doesn't disturb the value.
- For oversized first-children (100vh on A4 content area is ~95 px
  taller than the strip), Pageable's `BlockPageable::split` returns
  `Err(unsplit)` rather than emit an empty page, and the spike's
  `cursor_y > 0` guard does the same on its side. **Convergent
  fallback, not shared correctness.**

This means: any future Taffy-time fragmentation can rely on `vh`
working without doing anything special at the dispatch layer. The work
to do is around break-point semantics, not unit resolution.

### F2. Inline text wrapping is the real divergence

The long-paragraph fixture (50px font, line-height 1.5, ~70 wrapped
lines spanning ~5000 css px) splits across 2 pages on the Pageable
side and produces a single oversized fragment on the spike side. This
is the boundary the block-only spike cannot cross without inline
awareness.

A real fragmenter has to either:

- Mirror Pageable's approach: keep a parallel `ParagraphPageable`-like
  representation that knows about Parley line boxes and can return
  per-line height for `find_split_point`. Effectively reproducing the
  Pageable line accumulation logic inside the Taffy hook.
- Push the work upstream: convince Parley to expose line boxes with
  Y offsets so a generic block fragmenter can split at line boundaries
  without owning a parallel data structure.

The second option aligns with nicoburns' stated goal of getting
fragmented layout into Taffy/Parley
([blitz#128 comment](https://github.com/DioxusLabs/blitz/issues/128#issuecomment-4246086994)),
but is upstream work that doesn't exist yet.

### F3. The `multicol_layout` pattern is the right structural fit

`PaginationLayoutTree` was written by deliberately mirroring
`FulgurLayoutTree` line-for-line. Three forwarding trait impls
(`TraversePartialTree`, `CacheTree`, `RoundTree`), the
`compute_child_layout` is_x dispatch, the
`save-prior-location → compute_root_layout → restore-location` idiom,
and the side-table pattern (`PaginationGeometryTable` mirroring
`MulticolGeometryTable`) all transferred cleanly. This is strong
evidence that:

- A unified Taffy-hook layer (multicol + pagination + position:fixed)
  would not require a third design — it's the same pattern three
  times.
- The next round of out-of-flow work (`@page` margin boxes, running
  elements, bookmark anchors) can plug into the same layer.

### F4. Pageable replacement is **not** a 1:1 swap

Even with all 10 fixtures matching, the spike does not replicate:

- `break-inside: avoid`, `break-before`, `break-after` (`paginate.rs`
  reads these from style and threads them through the Pageable tree;
  no equivalent in the spike).
- `widows`, `orphans` (`ParagraphPageable::split` enforces; no
  equivalent).
- `string-set` / counter accumulation across pages
  (`paginate.rs:60-101` walks pages collecting state; no equivalent).
- Running elements, page-margin boxes, GCPM constructs (Pageable's
  `RunningElementStore` flows through paginate; no equivalent).
- Bookmark / anchor mapping into PDF outline.

Replacing Pageable would mean reproducing all of the above **inside
the Taffy hook**, not bolting them onto the existing convert-side
plumbing. The estimated step count from "block-only fragmenter that
agrees with Pageable on 9/10 trivial fixtures" to "fragmenter that
passes the existing fulgur-cli `examples_determinism` byte tests" is
**6–10 issues**, each individually non-trivial.

## Recommendation

**Adoption verdict: not yet.** The spike validates the pattern but the
gap to feature parity with Pageable is large.

**Keep the spike as scaffolding**, do **not** delete the module. Future
out-of-flow work (next: per-page repetition of `position: fixed`, see
the page-repetition TODO in `blitz_adapter::relayout_position_fixed`'s
docstring) can plug a `Vec<Fragment>` per node into
`PaginationGeometryTable` without re-bootstrapping the wrapper.

**Recommended next issues** (file as follow-ups to fulgur-4cbc):

1. **`break-before` / `break-after` honoring in the spike**
   — read break-style side-table (analog of
   `column_styles`) and force a page break in
   `fragment_pagination_root` when a child has
   `break-before: page`. Cheap, expands the agreement to a few
   `break-*` fixtures.

2. **per-strip `available_space` constraint experiment**
   — change `drive_taffy_root_layout` to call
   `compute_root_layout` with
   `available_space.height = page_height_px` and observe what
   Taffy/Blitz produce. May reveal that current `MaxContent` walk is
   doing more work than necessary, or it may surface child clipping
   that needs a workaround.

3. **inline-aware fragmenter prototype** —
   the F2 work. Either parallel ParagraphPageable-like, or
   Parley line-box probe. Highest-value since it closes the only
   observed disagreement.

4. **per-page `position: fixed` repetition** — already on
   the roadmap (`blitz_adapter::relayout_position_fixed` doc
   explicitly defers to "paginate-time concern owned by
   `crate::paginate`"). The spike's `PaginationGeometryTable` is the
   natural side-table for the per-page fragments.

5. **fragmentation upstream proposal** — write an issue
   on DioxusLabs/blitz suggesting Taffy/Parley primitives the spike
   needed (continuation token in `LayoutOutput`, line-box probe in
   Parley). The spike's commit history is the concrete evidence that
   this design works in practice.

**Do not merge** the spike branch as-is. Either:

- Land it behind a feature flag for follow-up issues to extend, or
- Extract the `multicol_layout` ↔ `pagination_layout` shared
  forwarding boilerplate into a `taffy_tree_wrapper` helper and
  re-introduce both modules as thin specializations on top of it.

The latter is preferable if more layout hooks are coming (running
elements, page-margin boxes), since three modules with identical
boilerplate is the threshold where extraction pays off.

## References

- Issue: fulgur-4cbc
- Branch: `spike/fulgur-4cbc-pagination-layout`
- Sibling pattern: `crates/fulgur/src/multicol_layout.rs`
- Position-fixed precedent: `crates/fulgur/src/blitz_adapter.rs:364`
  (`relayout_position_fixed`)
- Long-term direction: nicoburns'
  [blitz#128 comment](https://github.com/DioxusLabs/blitz/issues/128#issuecomment-4246086994)
