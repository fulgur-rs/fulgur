# fulgur pagination: single-pass fragmentation, stated as one design

**Goal:** State fulgur's pagination architecture as it would have been designed
on day one — one theorem licensing the architecture, one walker, one oracle —
rather than as the accreted history of the fulgur-pgbrk campaign. Everything
here is closed under the standing constraints: **single-pass layout,
byte-determinism, un-forked upstream dependencies** (decisions of record,
2026-08-18).

**Predecessors:**
[engineering review](./2026-08-18-fulgur-pgbrk-engineering-review.md)
(the diagnosis this design answers),
[page-fragmentation review](./2026-08-16-fulgur-pgbrk-page-fragmentation-review.md)
(the campaign's measured history).

**Spec baseline:** [CSS Fragmentation Module Level 3](https://www.w3.org/TR/css-break-3/).

**Tech stack:** Rust, `crates/fulgur/src/pagination_layout.rs` (walk),
`crates/fulgur/src/render.rs` (paint), `crates/fulgur-wpt` (oracle).

**Status: SHIPPED 2026-08-19.** Theorem, walk, and oracle all landed:
convergence phases 1–5 (`1dd9473e`, `a12f6a02`, `2cf9eb2d`, `f03fba9b`,
`a91855c5`, `3555418e`), R7 both clusters (`330274b9`, `fc2cd156`),
WPT `css-break` phase (`185ed3a4`). `pagination_layout.rs` `#[ignore]`
count: 0.

**How to read this document.** Sections 1–3 describe the *target* design as a
whole. Section 4 is the migration from the current tree; each phase is
independently committable and behavior-preserving. "Thought out, not accreted"
means: after the migration, a new contributor reads one theorem, one walker,
and one oracle — not four walkers and a campaign log.

---

## The licensing invariant: single-pass fragmentation theorem

### The theorem

**Single-pass fragmentation theorem.** Let `L` be the frozen Blitz/Taffy/Parley
layout of a document: every box has definite `x`, `y`, `width`, `height`, and
every inline root carries definite line-box boundaries computed by Parley. Let
`F` be the ordered sequence of page fragmentainers, all of identical inline
size (fulgur emits one paper size per document). fulgur's paginator is
single-pass *and* correct-by-construction if and only if, for every node and
every `Fragment` it emits, all three clauses hold:

1. **Vertical-only fragmentation.** `page_index` and `y` are chosen by
   comparing the node's frozen vertical extent against `F`'s vertical strips.
   The horizontal fields `x` and `width` are copied, unmodified, from `L`.
   Pagination never re-enters layout.
2. **Width invariance.** Because every fragmentainer in `F` shares the same
   inline size, every width-dependent decision — line wrapping, float
   placement, flex/grid track sizing — was made exactly once, in `L`, and is
   therefore page-invariant. A fragment's slice points need only vertical
   coordinates.
3. **Vertical composition, not re-layout.** Fragmenting a node across `k`
   pages is the composition of `k` clippings of its frozen vertical extent. No
   constraint of the form "this box's size depends on the fragmentainer's
   block size" is consulted, because none can be triggered: §5.1's
   per-fragmentainer re-layout ("recalculating sizes and positions using its
   own size") fires only for varying-size fragmentainers, and `F` is uniform
   by construction.

Each clause is falsifiable in one line: find a code path that re-invokes
Taffy/Parley with a fragmentainer-derived constraint (kills clauses 1 and 3),
or a fragment whose `width` differs from the frozen width (kills clause 2).
The byte-wise determinism goldens and the
`taffy_driven_dispatch_matches_direct_walk` parity test are the executable form
of that falsification. The theorem is what licenses the deliberate production
decision to post-walk `final_layout` instead of re-driving Taffy: re-layout is
not merely expensive, it is *unnecessary* under clause 3 and *harmful* under
the determinism invariant.

### Rule inventory

Reachability is decided per normative rule, once. **Reachable** = decidable
from vertical geometry alone. **Limitation** = unreachable in this
architecture, with the prescribed behavior stated. **Paint-only** = not a
geometry rule.

| § | Rule | Status | Basis / prescribed behavior |
| --- | --- | --- | --- |
| §3.1 | `break-before`/`break-after`: `auto`, `page`, `avoid`, column values | **reachable** | Forced/avoid are pure predicates over frozen sibling adjacency, consumed via the `ColumnStyleTable` side-table |
| §3.1 | `left`, `right`, `recto`, `verso` | **limitation** | Parity-based page targeting is unimplemented; normalized to `page` at style resolution. Reachable in principle (parity reduces to page-counter mod 2), so this is a feature gap, not an architecture ceiling |
| §3.1.1 | Child→parent break propagation | **reachable** | First/last in-flow child's value handed up via `SubtreeResult::RequestBreakBefore`; pinned by R5 |
| §3.2 | `break-inside`: `auto`/`avoid` | **reachable** | Style predicate consulted at class A/C points |
| §3.3 | `orphans`, `widows` | **reachable** | Line-box counts come from frozen Parley metrics; inheritance resolved by ancestor walk at the consumption point |
| §3.4 | `page-break-*` aliases | **reachable** | Parse-time normalization to `break-*` |
| §4.1 | Class A: between sibling boxes (in-flow blocks, float + adjacent in-flow/floated box, table rows) | **reachable** | Sibling vertical adjacency is enumerated entirely from frozen geometry |
| §4.1 | Class B: between line boxes | **reachable** | Split points are chosen among frozen line-box boundaries only |
| §4.1 | Class C: container content edge ↔ child outer edge, only with non-zero gap | **reachable** | Zero-gap predicate on frozen margin edges; pinned by `css_break3_no_class_c_point_without_gap_*` |
| §4.1 | Monolithic content (replaced, scroll containers, atomic inlines) | **reachable, with latitude** | §4.1 permits push-whole *or* graphical slicing; the R7 prescriptions below pick per-strip slicing uniformly |
| §4.2 | Break types: `page`, `spread`, `column` | **reachable** | Page (spread subsumed as page) and column (multicol module) are both modeled |
| §4.2 | `region` | **limitation** | No region chains; §2.2 nested fragmentation contexts beyond page×multicol are not modeled. Declined by scope, not by geometry |
| §4.3 | Forced breaks (override avoids; multi-value combination; named-page rule) | **reachable** | Predicates over the style side-table; named `page` values supported |
| §4.4 | Rules 1–4 (applicable avoids, ancestor `break-inside`, orphans/widows, ancestor `auto`) | **reachable** | All four are style predicates plus line-count constraints evaluated on frozen geometry |
| §4.4 | Relaxation ladder (drop rule 3, then rules 1/2/4, then "break anywhere" slicing; monolithic slicing option) | **reachable** | Deterministic ordered relaxation; never resolves by letting content escape the strip; pinned by R2/R4 tests |
| §4.5 | Optimizing unforced breaks (fewest breaks, equal fill, avoid replaced) | **reachable** | SHOULD-level guidance; fulgur's greedy earliest-fit realizes "fewest breaks" deterministically |
| §5.1 | Layout per varying-size fragmentainer | **limitation** | The one rule that mandates re-layout. Unreachable by theorem clause (3): `F` is uniform, so the trigger never fires. Prescribed behavior: sizes resolve once against the single fragmentainer size — §5.1's own intrinsic-size clause ("assume the size of the first fragmentainer") endorsed literally |
| §5.2 | Adjoining margins at breaks truncate to zero | **reachable** | Truncation is a vertical adjustment at the break point; pinned by `css_break3_s52_*` |
| §5.3 | Content box fills remaining fragmentainer extent before a break | **reachable** | Pure fragment-height bookkeeping over frozen extents |
| §5.4 | `box-decoration-break: slice`/`clone` | **paint-only** | Geometry carries `content_lead_in`/`lead_out` for slice semantics; `clone` needs a VRT fixture, not the geometry table |
| §5.4.1 | Joining boxes for `slice` | **paint-only** | Background compositing across reassembled fragments |
| §5.5 | Transforms/positioning interplay | **paint-only** | Per-fragment graphical effects; §5.5 explicitly permits UA latitude for absolutely-positioned boxes spanning breaks |

### Geometric treatment of the open R7 clusters

#### Flex/grid rows taller than the strip: per-cell internal fragmentation

§2.1 declares each cell of a grid row and each item of a flex row a *parallel
fragmentation flow*, and §4.1 explicitly allows layout models to add break
points beyond the base classes. Because width is page-invariant (clause 2),
slicing a cell is exact: for each page strip the row crosses, each cell emits
one fragment clipped to the strip at its computed content offsets. Where the
cell contains inline content, the slice point is chosen among its frozen Parley
line-box boundaries — a class B decision *inside* the cell, valid on every page
because the wrapping cannot change. Implementation shape: the existing
`RowState` co-split machinery extends past the strip-overflow point, so
per-cell fragment heights clip instead of overflowing; the `FragmentOverflow`
guard then has nothing left to catch. The seven `#[ignore]`d co-split tests
un-ignore once this lands.

#### Nested monolithic content: uniform per-strip slicing

The body-direct path already slices monolithic boxes per strip (pinned by
`css_break3_monolithic_body_direct_box_is_sliced_per_strip`); the nested path
emits once, oversized. §4.1's latitude makes both conformant, so the asymmetry
is a quality defect, not a conformance question. Prescribed: the nested walk
applies the identical rule — accumulate overshoot against the strip, emit
`(page_index, y, clipped_height)` fragments per strip crossed — making the
existing `monolithic_adjust` bookkeeping the only behavior rather than a
partial one. The four `#[ignore]`d nested tests un-ignore.

#### Floats at page breaks: push whole, slice when unbreakable

§2.1 makes float content a parallel flow; §3.1 says UAs *should* (not must)
apply break properties to floats; §4.1 leaves atomic/monolithic treatment open,
including the final-relaxation slicing option in §4.4. Prescribed choice,
uniform with the monolithic rule: a float that fits a fresh page is **pushed
whole**; a float taller than the fragmentainer is **sliced per strip** (the
body-direct path already does this, per
`body_direct_tall_float_sliced_across_pages_is_float_guard`). A float is never
re-flowed around at a break — width invariance means that when a slice is
taken, it is exact.

### Why this is not an approximation

The architecture partitions the spec into claims it decides by construction
and claims it declines by name. A **bug** is a reachable rule violated; a
**limitation** is a row above marked limitation or paint-only, each with its
prescribed fallback written down. The classification is decided once, here,
per rule — so conformance progress is a kill list, not a moving estimate. The
11 `#[ignore]`d R7 tests are pinned bugs-in-waiting, not refutations of the
theorem, and the `FragmentOverflow` guard draws the line at runtime: geometry
the walker cannot yet slice is caught and reported; geometry it sliced wrong
is a test failure. Everything in the "reachable" column is, by clauses (1)–(3),
decidable from the frozen tree — so the theorem holds, and the open work is
exactly the two prescriptions above.

---

## The walk: one `fragment_container`

Today there are two block-fragment walks: `fragment_pagination_root` (~L794,
method on `PaginationLayoutTree`) for body's direct children, and the
recursive `fragment_block_subtree` (~L2069) for everything below. They carry
four live cloned regions — the zero-height branch (body ~L972–1048 vs nested
~L2401–2461), inline-root handling (body ~L1110–1194 vs nested ~L2534–2701),
the recursion gate (body ~L1196 vs nested ~L2704–2726) — plus a body-only one:
oversized-per-strip slicing (~L1390–1514). The destination is one function:

```rust
fn fragment_container(
    cx: &FragmentationCtx<'_>,
    frame: &mut ContainerFrame,
    geometry: &mut PaginationGeometryTable,
) -> SubtreeResult
```

`fragment_pagination_root` becomes a thin entry that finishes Taffy layout,
builds body's depth-0 frame, and calls `fragment_container`. Body is the
`depth == 0` container, nothing more.

### Body's asymmetries become universal

- **Root entry.** Body's one real privilege is emitting its own whole-document
  fragment once on page 0 (~L826–836), deliberately excluded from overflow
  warnings by `find_overflowing_fragments`' `body_id` parameter. Encode this
  as one enum on the frame, checked only for that up-front emission:

  ```rust
  enum ContainerKind { RootBody, Nested }
  ```

  After the entry push, `RootBody` walks its children by the byte-identical
  rules as `Nested`.

- **Oversized slicing.** The body-only branch (~L1390–1514, including the
  childless-collapse guard and the `MAX_PAGES` truncation cap) moves into one
  helper, reachable from any depth once the gate says "no break points below":

  ```rust
  fn slice_oversized_leaf(
      cx: &FragmentationCtx<'_>,
      geometry: &mut PaginationGeometryTable,
      id: usize, x_in_body: f32, w: f32, h: f32,
      frame: &mut ContainerFrame,
  );
  ```

  Equivalence obligation for flipping nested whole-emit to this: the nested
  gate today routes every oversized child with grandchildren that split into
  recursion, so whole-emit is only reachable for leaf-like boxes; slicing a
  leaf-like box is what body already does. The guard set (transform check,
  `subtree_has_rendered_content`, `MAX_PAGES`) is identical at both call
  shapes, so the flip is a substitution, not a re-implementation.

- **Zero-height / whitespace.** The marker-survival branch (element enters
  with height 0; break-before/after still honoured) is merged, with
  `ParentSlice` threading done through the frame so body's "no parent to
  close" case is the `row_state: None` / `parent_slice: None` case, not a
  different algorithm.

- **`RowState`.** A resident frame field (`Option<RowState>`) at every depth.
  `ParentSlice::close_unforced` reads it through the frame — the flex/grid row
  co-split channel survives structurally.

- **`used_page_names`.** Stops being a parameter; becomes an input table on
  `FragmentationCtx`. The per-sibling `prev_used_page` comparison stays a
  frame local, float-skipped as today.

### Assimilating the gate: pure predicates over one enumeration

`would_split_block_subtree` (~L1765–1833) disappears as a separate simulator.
The gate check becomes one pure predicate composed from helpers also used by
the walk:

```rust
/// One child-enumeration policy for gate, walk, and future probes:
/// layout_children when Stylo synthesized anon wrappers for them,
/// else children (CSS 2.1 §9.2.1.1).
fn layout_children_of(doc: &BaseDocument, id: usize) -> Vec<usize>;

/// Shared filters: whitespace-only text, out-of-flow, non-element.
fn is_walkable_skip(doc: &BaseDocument, id: usize) -> bool;

fn subtree_requires_recursion(
    cx: &FragmentationCtx<'_>,
    node_id: usize,
    available_strip: f32,
    allow_leading_break: bool,
) -> bool
```

`subtree_requires_recursion` merges the three current probes:
`has_forced_break_below`, `has_page_name_change_below`, and a rewritten
`would_split` that walks `layout_children_of` with the same `(top, h)`
extraction *and calls `break_decision` with the parent's actual floor* (`0.0`
iff `allow_leading_break`, else `page_start_y`). The leading-child floor
disagreement — deferred during the Risk-1 extraction because simulator floor
and walk floor differ — is closed by construction: gate and walk now evaluate
the same `break_decision` on the same enumeration. That is also the only
honest way to claim "the simulator can never lie again".

### `FragmentationCtx` / `ContainerFrame`

```rust
/// Inputs fixed for the whole run. `doc` is immutable during the walk;
/// Taffy re-layout (`drive_taffy_root_layout`) stays a pre-pass on
/// `PaginationLayoutTree`, same as now.
struct FragmentationCtx<'a> {
    doc: &'a BaseDocument,
    styles: Option<&'a ColumnStyleTable>,
    used_page_names: Option<&'a UsedPageNameTable>,
    running: Option<&'a RunningElementStore>,
    page_h: f32,
}

struct ContainerFrame {
    id: usize, x_in_body: f32, width: f32,
    page: u32, cursor_y: f32, page_start_y: f32,
    page_taffy_origin: f32,
    origin_pending_target_y: Option<f32>,
    origin_pending_same_row: Option<(f32, f32, f32)>,
    prev_used_page: Option<Option<String>>,
    emitted_anything: bool,
    allow_leading_break: bool,
    depth: usize,
    row_state: Option<RowState>,
    parent_slice: Option<ParentSlice>,
    kind: ContainerKind,
}
```

`geometry` stays the single explicit `&mut` sink; `SubtreeResult`
(`Placed { page, cursor_y } | RequestBreakBefore`) stays the return; the
`RequestBreakBefore` proof obligation (`emitted_anything` untouched-table
invariant) moves onto the frame. The three `#[allow(clippy::too_many_arguments)]`
on the walk (on `fragment_block_subtree`, `fragment_inline_root`,
`scan_split_points`) are removed; `scan_split_points` receives one
`InlineSplitInput` struct (`line_metrics, lead_in/out, orphans/widows`).

### Phased migration

1. **Cx + Frame introduction.** Rethread `fragment_block_subtree`,
   `fragment_inline_root`, `scan_split_points` signatures; the body method
   reborrows `self` fields into the new structs. Purely mechanical, no
   ordering change.
2. **Unify child enumeration.** Extract `layout_children_of` +
   `is_walkable_skip`; wire the body walker, the nested walker, and the
   simulator to it. Fixes enumeration-drift risk before touching behavior.
3. **One child-visitor.** Move both copies into a single
   `visit_container_child(...)` on (cx, frame, geometry), keyed by
   `ContainerKind` so the body/nested outputs stay byte-identical per call
   shape.
4. **Oversized slicing universal.** With the gate now pure and shared,
   substitute `slice_oversized_leaf` for the nested whole-emit branch
   (equivalence obligation above). This is the P0/R7 monolithic cluster.
5. **Simulator assimilation.** Replace `would_split_block_subtree` with
   `subtree_requires_recursion` composed from the walk's own enumeration and
   floor; delete the deferred-divergence note from the break-decision design
   doc.

Each phase lands with the suite green and zero re-blessing; Phases 1–3 are
geometry-no-ops by construction.

### Impossible by construction afterwards

- **Body/nested rule fork** (e.g. nested inline-root splitting unreachable
  until fulgur-pgbrk added it) — one code path now; a rule change touches both
  depths.
- **Simulator/walk disagreement on leading-child floors** — gate derived from
  the same `break_decision` and floor.
- **Child-enumeration drift** (the anon-wrapper class) — one
  `layout_children_of`.
- **Parameter-order arity bugs** in 12-arg calls — Cx/Frame fields force named
  access.
- **Lint-tolerated sprawl** — the clippy `too_many_arguments` escape hatch is
  gone, so the next divergence is a compile error rather than a silenced lint.

---

## The oracle: WPT as the primary conformance mechanism

### Census of the css-break corpus

The snapshot under `target/wpt/css/css-break/` (fetched by
`scripts/wpt/fetch.sh`; `scripts/wpt/subset.txt` already lists the subtree,
plus the shared `css/reference` and `css/support` needed for cross-tree refs)
contains, for reference:

- 1,343 files total (recursive count); 638 top-level entries, plus subdirs
  `animation/`, `flexbox/`, `grid/`, `parsing/`, `reference/`, `table/`.
  Non-HTML files (`META.yml`, `WEB_FEATURES.yml`, `.tentative` variants) are
  ignored by the collector.
- 1,168 **candidate test files** — `.html` files whose name base is not
  `-ref`/`-notref`, excluding the `reference/`, `resources/`, `support/` dirs
  that `reftest::collect_reftest_files()` skips
  (`crates/fulgur-wpt/src/reftest.rs`).
- 152 `-ref.*html`/`-notref` files are excluded by that filter (13 more refs
  live under `reference/` and are likewise skipped as test candidates).

Running each candidate through a script equivalent to `classify()`'s decision
table over the snapshot yields:

| classification | count | note |
| --- | --- | --- |
| single `rel=match` (usable) | **1,005** | refs resolve inside the fetched subset |
| single `rel=mismatch` | **0** | the corpus's only 2 mismatch links use root-relative hrefs, which `classify()` drops to `NoMatch` |
| `MultipleMatches` / `MultipleMismatches` / `Mixed` | **0** | no file carries ≥2 usable match/mismatch tokens |
| `NoMatch` → SKIP | **163** | 115 `*-crash.html` files + 48 non-crash (`parsing/*`, `animation/*`, `hit-test-*`, legacy shorthands, inheritance, etc.) |

Two census caveats, both single-file anomalies: `ink-overflow-001-print.html`
points its `rel=match` at `about:blank` (unresolvable as a path — the seed
will land it as FAIL with a harness-error reason comment), and
mismatch-support is gated on removing the root-relative restriction in the
future. With that, the phase seeds as roughly `1,005 FAIL + 163 SKIP ≈ 1,168`
entries in `expectations/css-break.txt`.

### Phase plan

1. **Fetch (no subset edit needed).** `scripts/wpt/fetch.sh` writes
   non-comment lines of `subset.txt` into the sparse-checkout; `css/css-break`
   is already there. Idempotent re-run pins the same SHA via
   `scripts/wpt/pinned_sha.txt`.
2. **Seed FAIL baseline.**

   ```bash
   cargo run -p fulgur-wpt --example seed -- \
     --subdir css-break --wpt-root target/wpt \
     --out crates/fulgur-wpt/expectations/css-break.txt
   ```

   `seed` runs `harness::run_one` for every file at 96 DPI with the
   whole-snapshot fonts bundle, catches panics, and writes a header tally plus
   one `PASS|FAIL|SKIP path [ # reason ]` line per entry — the format
   `ExpectationFile::parse` accepts. Commit the output; it becomes the durable
   regression baseline.
3. **Wire the phase runner.** Add `crates/fulgur-wpt/tests/wpt_css_break.rs`
   mirroring `wpt_css_page.rs`: call
   `runner::run_phase(&workspace_root(), "css-break", 96)`; panic only when
   `FULGUR_WPT_REQUIRED=1` and prereqs are missing. `run_phase` emits
   `target/wpt-report/css-break/{report.json,regressions.json,summary.md}` per
   run, verdict-judged via `expectations::judge` (declared PASS × observed
   FAIL → `Regression`; declared FAIL × observed PASS → `Promotion`).
4. **CI.** Add one matrix entry to the `wpt` job in `.github/workflows/ci.yml`
   and the same entry to `wpt-nightly.yml`. The WPT job is
   `continue-on-error: true`, so a red css-break run never gates merge;
   nightly's aggregate step opens `wpt-nightly-regression` issues only on
   declared→observed regressions.
5. **Promotion workflow** (per `crates/fulgur-wpt/README.md`): verify locally
   with `cargo run -p fulgur-wpt --example run_one -- <test-path>`; in the
   same PR flip the line `FAIL → PASS`, dropping the trailing `# reason: …`
   comment; merge once the `wpt / css-break` job is green. SKIP rows require a
   beads-tracked reason; flip back once unblocked.

### Relationship to the hand-written `css_break3_*` block

The nine tests in `crates/fulgur/src/pagination_layout.rs` (plus the
`report_fragment_overflow` "overflow blanket" — an invariant that panics in
test builds and `log::warn!`s in prod) assert white-box geometry, not raster
output. Once `css-break` exists the raster oracle takes over spec-conformance
blame; the geometry table keeps its role as fulgur-internal invariants.
Per-test disposition against corpus names:

| hand test | css-break-3 § | WPT equivalent(s) from the corpus | decision |
| --- | --- | --- | --- |
| `class_a_unforced_break_between_siblings` | §4.1 class A | `break-at-end-container-edge-000..004.html`, `overflowed-block-with-room-after-*` | map to WPT; retire hand test on promote |
| `no_class_c_point_without_gap_breaks_before_container` | §4.1 class C | none apparent | stay invariant |
| `class_b_break_between_line_boxes` | §4.1 class B | `tall-line-in-short-fragmentainer-{000,001,002}.html` | keep invariant too (line geometry check) |
| `monolithic_body_direct_box_is_sliced_per_strip` | §4.1 monolithic | `monolithic-overflow-001..006.tentative.html`, `monolithic-with-overflow{,-lr,-rl}.html` | map to WPT |
| `s52_margin_adjoining_unforced_break_is_truncated` | §5.2 | `margin-at-break-001..005.html`, `truncated-margin-at-fragmentainer-end-{001,002}.html` | map to WPT |
| `s31_forced_break_on_first_child_propagates_to_container` | §3.1 | `forced-break-before-new-fc-001..003.html` | map partially; propagation invariant stays |
| `s44_rule2_ancestor_break_inside_avoid_forbids_class_a_break` | §4.4 rule 2 | `break-between-avoid-000..014.html`, `avoid-border-break.html` | map to WPT |
| `s44_widow_relaxation_prevents_lines_escaping_the_strip` | §4.4 relaxation | `widows-orphans-001..023.html`, `widows-001.html` | map to WPT; keep geometry invariant |
| `s44_rule3_author_widows_value_shifts_the_split` | §4.4 rule 3 | `widows-orphans-*` series (font-dependent) | keep invariant — hand test is font-independent via `<br>` splits |

The §5.4 paint-level note in the block header (previously routed to VRT) now
maps to `box-decoration-break-clone-001..036.html` in the corpus.
`report_fragment_overflow` itself is not a spec test; it stays as the
permanent invariant blanket and is not tagged for removal.

### Acceptance criteria

- `crates/fulgur-wpt/expectations/css-break.txt` exists, seeded by
  `example seed`, committed with its header tally.
- `crates/fulgur-wpt/tests/wpt_css_break.rs` registered and
  `cargo test -p fulgur-wpt --test wpt_css_break` succeeds under
  `scripts/wpt/fetch.sh` + poppler.
- `.github/workflows/ci.yml` `wpt` matrix and `wpt-nightly.yml` list
  `css-break`; CI green (continue-on-error still true; no merge gate added).
- Promotion workflow documented in this section and consistent with the
  README's promotion flow / `run_one` example.
- `pagination_layout.rs` `css_break3_*` header comment re-tagged as
  supplemental (promotion ledger lives in `expectations/css-break.txt`).

---

## Testing

Per CLAUDE.md's coverage rule, walker logic stays lib-level in
`#[cfg(test)] mod tests`; raster conformance moves to WPT; VRT is
byte-compared and never regenerated on macOS (29/64 differ there for
environment reasons, verified by the stash / re-run / diff protocol).

The migration's acceptance bar, per phase: **the full suite green with no test
edits**. Any test needing re-blessing means the phase was wrong — fix the
phase, not the test. The `--ignored` set (currently 11, all R7) shrinks only
when the two R7 prescriptions land, and each un-ignoring is its own commit.

New invariants this design makes structural, not conventional:

- The theorem's three clauses are falsifiable by the existing parity test and
  the determinism goldens; the design adds no new trusted base.
- The overflow blanket (`report_fragment_overflow`) is the runtime boundary:
  reachable-but-unimplemented geometry warns in production and panics in
  tests, so an unimplemented reachable rule can never again become silent
  content loss.
- `#[ignore]` remains the only gap statement; a beads issue mirrors it
  opportunistically but the test is the system of record.

## Out of scope

- **Any re-layout-driven architecture.** The theorem states why re-layout is
  unnecessary here; the determinism goldens state why it is also harmful.
  Production keeps skipping the Taffy re-drive; the wrapper traits stay
  test-only.
- **Forking or vendoring** Blitz/Taffy/Parley/Stylo.
- **Paint-level §5.4 `clone` conformance** beyond carrying
  `content_lead_in`/`lead_out` — that work is a VRT fixture, not the geometry
  table.
- **Parity page targeting** (`left`/`right`/`recto`/`verso`) — a documented
  feature gap, filed separately if product need arises.

## Verification commands

```bash
cargo test -p fulgur                          # 2525 passed / 2 ignored (non-lib)
cargo test -p fulgur --lib -- --ignored       # empty: all R7 gaps closed
cargo test -p fulgur-wpt                      # css-page + css-break phases
cargo clippy -p fulgur && cargo fmt --check
npx markdownlint-cli2 '**/*.md'
```
