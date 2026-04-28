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
| long paragraph wraps into multiple pages (after p55h) | 2 | **2** | ✓ |
| small lead block + oversized block | 2 | 2 | ✓ |

(Historical baseline, pre-p55h: the long-paragraph fixture used to
disagree as `Pageable=2 / spike=1` because the block-only spike treated
a tall paragraph as a single oversized fragment. Listed here for
posterity but not counted in the table above.)

10/10 agree after fulgur-p55h landed the Parley line-box probe in
`fragment_inline_root`. The original disagreement (block-only spike
treating a tall paragraph as a single oversized fragment) was
closed by reading `node.element_data().inline_layout_data` — a
`Box<TextLayout>` carrying a `parley::Layout` — and splitting at line
boundaries via `LineMetrics::min_coord` / `max_coord`. Pageable's
`ParagraphPageable::split` (paragraph.rs:945) and the spike's
`fragment_inline_root` now produce identical page counts for this
fixture.

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

   **Filed as fulgur-k0g0. Resolved (2026-04-28)**: reused the
   existing `column_css::ColumnStyleTable` (already harvested by
   `extract_column_style_table` and consumed by Pageable via
   `extract_pagination_from_column_css`) instead of building a
   separate `BreakStyleTable`. New `run_pass_with_break_styles`
   entry threads it into `PaginationLayoutTree` as an optional
   borrow. `fragment_pagination_root` checks
   `props.break_before == Some(Page)` before each child and
   advances the page when there is in-flow content above; checks
   `props.break_after == Some(Page)` after; checks `break_inside ==
   Some(Avoid)` to suppress inline line splitting and fall back to
   the block path. Two new harness fixtures (`break-before`,
   `break-after`) flipped to agreement; a third (`break-inside:
   avoid` on a tall paragraph) intentionally diverges — see below.

   **Side-finding**: the spike honours `break-inside: avoid` on
   inline roots (paragraphs); Pageable does not. `paragraph.rs:945`
   `ParagraphPageable::split` never checks `self.pagination.
   break_inside`, so a tall paragraph with `avoid` still splits at
   line boundaries on the Pageable side. The spike behaviour is
   correct per CSS Fragmentation §3.3. Recorded as `expected_
   agreement = false` for the relevant fixture; fixing Pageable is
   out of spike scope.

2. **per-strip `available_space` constraint experiment**
   — change `drive_taffy_root_layout` to call
   `compute_root_layout` with
   `available_space.height = page_height_px` and observe what
   Taffy/Blitz produce. May reveal that current `MaxContent` walk is
   doing more work than necessary, or it may surface child clipping
   that needs a workaround.

   **Result (fulgur-ik6o, executed)**: no observable difference. All
   10 comparison fixtures produced identical page counts under
   `StripMode::MaxContent` and `StripMode::Definite(page_height_px)`,
   including the long-paragraph case that disagrees with Pageable.
   Conclusion: Taffy's block layout does not use
   `available_space.height` to drive mid-element splitting (it is
   only consulted for shrink-to-fit / orthogonal-flow corner cases).
   This rules out the "free fragmentation by constraining
   available_space" hypothesis — true pagination requires the hook
   to iterate strip-by-strip and re-issue per-child layouts itself.
   The `StripMode` enum and `run_pass_constrained` entry are kept in
   the spike code as reproducible instrumentation, but they do not
   change behaviour.

3. **inline-aware fragmenter prototype** —
   the F2 work. Either parallel ParagraphPageable-like, or
   Parley line-box probe. Highest-value since it closes the only
   observed disagreement.

   **Filed as fulgur-p55h. Resolved (2026-04-28)**: chose option
   (a), the Parley line-box probe. Implementation reads
   `node.element_data().inline_layout_data` (a Box<TextLayout>
   wrapping `parley::Layout`), iterates `Layout::lines()`, and
   collects `LineMetrics::min_coord` / `max_coord` per line. The
   new `fragment_inline_root` helper splits at line boundaries
   when the cumulative paragraph-local height plus the current
   `cursor_y` would push the next line past the page strip. This
   is ~80 lines added to `pagination_layout.rs` versus the
   estimated several hundred lines that option (b) (parallel
   ParagraphPageable representation) would have required.

   Outcome: the `long paragraph wraps into multiple pages` fixture
   flipped from `expected_agreement = false` to `true` (both sides
   now report 2 pages). All 10 comparison fixtures agree. No
   widow / orphan support yet — that lands when a fixture demands
   it. The Parley line-box approach generalises trivially to other
   inline roots (any node with `inline_layout_data` populated by
   Blitz's first-pass `resolve()`).

4. **per-page `position: fixed` repetition** — already on
   the roadmap (`blitz_adapter::relayout_position_fixed` doc
   explicitly defers to "paginate-time concern owned by
   `crate::paginate`"). The spike's `PaginationGeometryTable` is the
   natural side-table for the per-page fragments.

   **Filed as fulgur-jkl5. Resolved (2026-04-28, in two stages)**:

   *Stage 1 — spike-side scaffolding*: added
   `pagination_layout::append_position_fixed_fragments(geometry,
   doc, total_pages)` that walks the DOM (including `::before` /
   `::after` pseudo slots) for `position: fixed` nodes and emits
   one Fragment per page into the existing geometry table. Helper
   `implied_page_count(geometry)` derives `max_page_index + 1`. Four
   unit tests cover 2-page repetition, 0-pages normalisation, and
   `implied_page_count` invariants.

   *Stage 2 — production wiring (after fulgur-tbxs merged into
   `main` as PR #262)*: investigation revealed the
   `pagination_layout` side-table was **not** required for the
   user-visible fix. The existing `out_of_flow: true` mechanism in
   `pageable.rs` already replicated fixed children to both halves of
   `BlockPageable::split`, but the second-half y-shift (correct for
   `position: absolute` anchored to the abs-CB which slides with
   body) pushed `position: fixed` to negative y on every page after
   the first (≈ -760pt for A4) so they were silently clipped
   off-screen. A pdftotext smoke confirmed page 1 had the fixed
   element, page 2+ did not.

   Surgical fix: added `PositionedChild::is_fixed: bool` flag and
   `PositionedChild::fixed(child, x, y)` constructor.
   `clone_pc_with_offset` skips the y-shift entirely when
   `is_fixed`, leaving the element at its viewport-relative
   coordinates on every page. `convert::positioned` (both pseudo
   and non-pseudo abs paths) sets
   `is_fixed: is_position_fixed(node)`. All other call sites
   default `is_fixed: false` so `position: absolute` keeps its
   existing y-shift — strict improvement, no regression. 877 lib
   tests pass; new
   `tests/render_smoke.rs::position_fixed_repeats_on_every_page`
   confirms end-to-end via pdftotext that "FXFXFX" appears on both
   pages of a 2-page document.

   The `pagination_layout::append_position_fixed_fragments`
   side-table remains in place as scaffolding — not the path the
   production fix took, but available for a future architecture
   where convert consumes geometry directly.

5. **fragmentation upstream proposal** — write an issue
   on DioxusLabs/blitz suggesting Taffy/Parley primitives the spike
   needed (continuation token in `LayoutOutput`, line-box probe in
   Parley). The spike's commit history is the concrete evidence that
   this design works in practice.

### Resolved follow-ups (post-merge of follow-up #1)

**Counter / string-set accumulation (fulgur-6tco)**: added
`pagination_layout::collect_string_set_states` that walks
`PaginationGeometryTable` page-by-page and threads per-name
`StringSetPageState` (`start` / `first` / `last`) across fragments,
mirroring `paginate::collect_string_set_states` — markers fire only
on a node's first appearance (subsequent split fragments do not
re-emit), and `last` carries forward as the next page's `start`.
`StringSetPageState` gained `PartialEq` / `Eq` derives so the
spike's tests can compare structs directly. Three unit tests cover
carry across pages with first/last divergence, split-paragraph
markers fire once not per-fragment, and empty geometry returns one
empty page (Pageable's "always at least one page" convention). Full
end-to-end parity vs `paginate::collect_string_set_states` is
implicit: both functions implement the same algorithm against
isomorphic input. A direct Pageable-vs-spike comparison test is
deferred until the spike has a path to consume the engine's real
GCPM `string_set_by_node` map (today's tests use synthetic input).

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
