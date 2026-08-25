# fulgur-pgbrk: page-fragmentation review and remaining work

**Goal:** Record the review outcome for the fulgur-pgbrk page-wrap fix
(leading-edge break propagation + nested inline-root splitting), assess the
pagination architecture against CSS Fragmentation Module Level 3, and specify
the remaining engineered work with reproducible failure cases so the next
contributor can start immediately.

**Spec baseline:** [CSS Fragmentation Module Level 3](https://www.w3.org/TR/css-break-3/)
(css-break-3). All section references below are to that document.

**Origin:** `FULGUR_PAGINATION_BUG.md` (downstream investigation against
`@fulgur-rs/cli` 0.40.0) — content laid out past the page bottom, through the
margin strip, off the paper, and silently discarded with exit code 0.

**Beads:** `fulgur-pgbrk` (the shipped fix). Follow-up issues in
[Remaining work](#remaining-work) are not filed yet — file them before starting.

**Tech stack:** Rust, Blitz/Taffy/Parley layout, krilla PDF output,
`crates/fulgur/src/pagination_layout.rs`.

---

## Status: what has shipped

Two independent defects in `fragment_block_subtree` were fixed. Both ended the
same way — content placed past the page bottom and discarded.

### Defect 1: no break before a box's leading child

The strip-overflow cut was gated on `child_page_y > page_start_y`. That is never
true for a parent's **first** in-flow child (its rebased `child_page_y` *is*
`page_start_y`), so a box that began mid-page could never break before its own
leading child. The gate is now `child_page_y > overflow_floor`, where
`overflow_floor` is `0.0` when leading-edge propagation is permitted
(`pagination_layout.rs:2437`, mirrored for inline roots at `:2079`).

This is the spec-correct behaviour. §4.1 defines a class C break point between a
container's content edge and its first child **only when there is a non-zero
gap**. With no gap there is no break point there, so the nearest legal break is
the class A point *before the container*, recursively up the chain — exactly
what propagation to the leading edge means.

Permission is threaded through the recursion (`:1739`, `:2260`) and cleared for
the whole subtree under any container whose children are not class A break
points (flex/grid per §4.1, atomic inline containers, orthogonal flow). Reusing
the existing `suppress_page_check` as the gate is the right call — it already
enumerates exactly that set.

### Defect 2: nested inline roots never split at line boundaries

Line-level (class B) splitting existed only in `fragment_pagination_root`'s
body-direct branch. A multi-line `<p>` nested inside a recursed subtree fell
through to the block path and emitted as one oversized fragment. The new branch
at `:2038-2208` mirrors the body-direct one, including fulgur-oc51 parent
bookkeeping so wrapper backgrounds/borders survive on crossed pages.

`break-inside: avoid` now suppresses the split **unless honouring it is
impossible** (`avoid_is_fulfillable`, `:741` and `:2061`), per §4.4's relaxation
clause: restrictions are dropped rather than losing content off the edge.

### Verification performed

| Check | Result |
| --- | --- |
| `cargo test -p fulgur --lib` | 2027 passed, 0 failed, 4 ignored |
| `cargo test -p fulgur --test render_smoke leading_child_that_must_break_does_not_lose_content` | passed (real `pdftotext`) |
| `cargo clippy -p fulgur` | clean |
| `cargo fmt --check` | clean |
| `cargo test -p fulgur-vrt` | 29/64 differ — **identical with and without the fix** |

The VRT result was confirmed by stashing the change, re-running, and diffing the
failing fixture lists including byte sizes: they match exactly. The 29 failures
are a pre-existing local/goldens environment mismatch (goldens are generated on
CI Linux), **not** caused by this work. Do not regenerate goldens on macOS on
account of this change.

### Test coverage added by the fix

All ten gaps from the first-iteration review are covered, except one partial:

- Gap 6 (`suppress_page_check` has three sources) covers grid, flex, and
  inline-block. **The orthogonal-writing-mode source is still unguarded.**

---

## Architecture assessment

The single-pass design — Taffy lays out once, geometry is recorded into
`PaginationGeometryTable`, pagination walks that table with no re-layout — is
sound for fulgur's determinism goal and should be kept. Two structural risks are
worth naming.

### Risk 1: break logic is duplicated across three sites that must agree

`would_split_block_subtree` (`:1335`) simulates the walk to decide whether to
recurse; `fragment_block_subtree` (`:1639`) performs it; `fragment_inline_root`
(`:2641`) handles line-level splitting. The simulator was **not** updated for
leading-edge propagation. This is safe today only because the no-recursion path
also pushes whole, so both paths reach the same page — but it is a standing
drift hazard, and the next change to either will not obviously break a test.

**Recommendation:** extract the "may this child break before itself here?"
decision into one pure function consumed by both the simulator and the real
walk. Signature sketch:

```rust
/// Where a child may break relative to the current strip.
/// Single source of truth for the simulator and the real walk.
fn break_decision(
    child_top_on_page: f32,
    child_height: f32,
    page_start_y: f32,
    page_height_px: f32,
    allow_leading_break: bool,
) -> BreakDecision {
    let floor = if allow_leading_break { 0.0 } else { page_start_y };
    if child_top_on_page > floor && child_top_on_page + child_height > page_height_px {
        BreakDecision::PushToNextPage
    } else {
        BreakDecision::PlaceHere
    }
}
```

### Risk 2: no re-layout means break-edge geometry is approximated

Because splitting never re-runs layout, everything at a break edge is
reconstructed by cursor rebasing (`page_taffy_origin`). That is an acceptable
trade-off, but it is the direct cause of the padding defect below: fragment
heights for inline roots are derived from Parley line metrics, which do not
include the box's own padding or border. Any future work touching fragment
heights should treat "line metrics ≠ border box" as a first-class invariant, not
an edge case.

---

## Remaining work

> **Progress as of 2026-08-17.** R1–R6 have all shipped, along with the Risk 1
> extraction below and one defect it exposed (R8, new — see
> [Shipped since this review](#shipped-since-this-review)). **Only R7 remains.**
> The `--ignored` count in `pagination_layout.rs` is down from 16 to 11, now
> entirely R7: the flex/grid co-split cluster (7) and the monolithic-overflow
> cluster (4). Every css-break-3 rule in the conformance map below now passes.
> Sections whose work has landed are kept for the failure analysis, with a
> status line at the top of each.
>
> **Campaign closed 2026-08-19.** R7 shipped (`330274b9` monolithic cluster,
> `fc2cd156` flex/grid cluster): all 11 remaining `#[ignore]`s un-ignore and
> the `--ignored` count in `pagination_layout.rs` is **0**. The walker was
> converged per
> [2026-08-18-fulgur-single-pass-fragmentation-design.md](./2026-08-18-fulgur-single-pass-fragmentation-design.md)
> (`1dd9473e`…`3555418e`), and the WPT `css-break` phase is seeded as the
> conformance ledger (`185ed3a4`: 36 PASS / 969 FAIL / 163 SKIP).

**Historical snapshot** (see the campaign-closed note above for current
status): at the time this review was written, two of these still destroyed
content, each with a reproducible end-to-end case measured against the
tree as it stood then. Priority order is as listed.

### R1: paragraph padding and border are excluded from break measurement (P1, loses content)

**Status: SHIPPED** (`a2e103ad`). See
[the R1 design](./2026-08-16-fulgur-pgbrk-r1-border-box-design.md).

`para_total_h` and `avoid_is_fulfillable` are computed from Parley line metrics
(`last.1 - first.0`), which cover the line boxes only. A paragraph's own
`padding` / `border` is excluded, so both the push-whole decision and the
"can this box ever fit a page?" test under-measure the box by that amount, and
the tail runs off the paper.

**Measured repro (current tree):**

```html
<!DOCTYPE html>
<style>@page { size: 600px 500px; margin: 50px; }
body { margin:0; font-size:14px; }</style>
<body>
<div style="height:100px"></div>
<div><div><p style="margin:0;padding:150px 0;line-height:20px">W0000 … W0119</p></div></div>
</body>
```

Result: **10 of 120 probe words absent from the PDF**, exit code 0. Geometry
shows the box is 460px tall but the recorded fragment is `y=100, height=160` —
the 300px of padding is missing from the fragment entirely, so the overflow
check never fires.

**Engineered fix:** measure the border box, not the line box. Read the child's
used padding/border from Taffy (`child.final_layout` exposes `padding` and
`border` rects) and fold them in at both decision sites:

```rust
// pagination_layout.rs ~:2060 (and the body-direct twin at ~:740)
let lead_in  = layout.padding.top + layout.border.top;
let lead_out = layout.padding.bottom + layout.border.bottom;
let lines_h  = match (all_line_metrics.first(), all_line_metrics.last()) {
    (Some(f), Some(l)) => l.1 - f.0,
    _ => 0.0,
};
let box_total_h = lead_in + lines_h + lead_out;
let avoid_is_fulfillable = box_total_h <= page_height_px;
```

Then pass `lead_in` into `fragment_inline_root` so the first fragment's height
and the projected-bottom test both account for it, and add `lead_out` to the
final fragment. Prefer `child_h` (the Taffy border-box height) as the
authoritative total where it is finite and non-zero — it already includes
padding and border — and use line metrics only to choose *where* to cut.

**Tests:** add a lib unit test asserting the padded paragraph's fragments stay
within the strip, plus a `render_smoke.rs` case extending the existing
`leading_child_that_must_break_does_not_lose_content` sweep with a padded
variant.

### R2: widows/orphans restrictions are never relaxed (P1, loses content)

**Status: SHIPPED** (`8c88e74d`). The scan is factored into
`scan_split_points`, which returns a discardable plan; when the constrained
plan escapes the fragmentainer it is re-run with the minimums dropped to
1/1. Verified end to end: the repro below loses `LINETHREE` before the fix
and renders all three lines across two pages after it.

§4.4 rule 3 constrains where a line-level break may fall; the closing relaxation
clause requires dropping the restrictions rather than losing content: *"the UA
may break anywhere in order to avoid losing content off the edge."*

`fragment_inline_root`'s widow/orphan check only ever moves the split **later**
(`continue` accumulates lines forward, `:2674-2684`). It never backtracks and
never relaxes, so a paragraph whose only natural split violates widows emits one
oversized fragment whose tail lines land past the page bottom.

**Measured repro (current tree):**

```html
<!DOCTYPE html>
<style>@page { size: 600px 300px; margin: 20px; }
body { margin:0; font-size:14px; }</style>
<body><p style="margin:0;line-height:120px">LINEONE<br>LINETWO<br>LINETHREE</p></body>
```

Three 120px lines on a 260px strip. The split after line 2 leaves a 1-line tail
(widows = 2 violated), so no split is taken and the paragraph emits whole at
360px. Result: **`LINETHREE` is absent from the PDF**, 1 page, exit code 0.

**Engineered fix:** make relaxation a second pass rather than an inline
condition. Keep the current constrained scan; if it produces a final fragment
that exceeds the fragmentainer, re-run the scan with the constraints dropped.

```rust
// Pass 1: honour orphans/widows (current behaviour).
let frags = scan_split_points(line_metrics, /*respect_widows=*/ true, ...);
// §4.4 relaxation: never lose content off the edge.
let overflows = frags.iter().any(|f| f.y + f.height > page_height_px + EPS);
let frags = if overflows {
    scan_split_points(line_metrics, /*respect_widows=*/ false, ...)
} else {
    frags
};
```

This requires factoring the existing loop body into `scan_split_points` that
returns candidate fragments instead of pushing into `geometry` directly — a
mechanical refactor that also makes the function unit-testable without a
`geometry` fixture.

**Test to un-ignore:** `css_break3_s44_widow_relaxation_prevents_lines_escaping_the_strip`
(`pagination_layout.rs:8174`) already encodes the target. Note that
`widow_minimum_blocks_single_line_tail_fragment` (`:6362`) currently **pins the
defective behaviour** and must be updated in the same change.

### R3: no diagnostic when content escapes the page box (P2)

**Status: SHIPPED** (`28926fe4`, plus `a6d89aa5` for the CLI logger without
which the warning was inert). See
[the R3 design](./2026-08-16-fulgur-pgbrk-r3-overflow-detection-design.md).

Priority 1 of the original bug report. Even after R1 and R2, §4.1 permits
monolithic content to overflow, so silent loss remains possible for shapes not
yet enumerated. A cheap, total guard closes the whole class:

After `run_pass` completes, scan the geometry table for any fragment with
`y + height > page_height_px + EPS` and emit one `tracing::warn!` per offending
node with its node id, page index, and overshoot. Never write to fd 1 from
`crates/fulgur` (CLAUDE.md). Optionally surface a count on the `Engine` result so
`fulgur-cli` can exit non-zero under a `--strict-pagination` flag.

The helper already exists in test code —
`assert_no_fragment_starts_below_page` (`pagination_layout.rs:7377`) — and can
be promoted to a non-test function.

### R4: `break-inside: avoid` on block containers is ignored (P2, correctness)

**Status: SHIPPED** (`5784b4cb`). The `SubtreeResult` sketch below was
adopted as written. The one addition: the request fires only when the box
would fit a *fresh* page. A box that fits no page at all cannot be helped by
moving it — `avoid` is unfulfillable, §4.4's relaxation clause applies, and
pushing it would waste a page before splitting anyway.

§4.4 rule 2: breaking at a class A point is forbidden when a common ancestor of
the adjoining siblings has `break-inside: avoid`. fulgur reads `break_inside`
only on inline roots (`:720`, `:2058`); a block wrapper's `avoid` is never
consulted, so the wrapper splits between its children.

**Engineered fix:** in `fragment_block_subtree`, before the strip-overflow cut,
check whether `parent` carries `break-inside: avoid` and the whole subtree fits a
fresh page (`would_split_block_subtree(doc, parent_id, page_height_px, …)` is
false at full-page width). If so, propagate a "do not break inside me" signal to
the caller so the parent moves whole. The cleanest shape is for
`fragment_block_subtree` to return an enum rather than `(u32, f32)`:

```rust
enum SubtreeResult {
    Placed { page: u32, cursor_y: f32 },
    /// Nothing was emitted; the caller must break before this box and retry.
    RequestBreakBefore,
}
```

Callers already handle a page advance, so the retry loop is small. Guard against
re-entry (retry at most once per child) so an unfulfillable `avoid` at
`child_page_y == 0.0` cannot loop.

**Test to un-ignore:** `css_break3_s44_rule2_ancestor_break_inside_avoid_forbids_class_a_break`
(`:8128`).

**Downstream note:** the shipped fix already makes `break-inside: avoid`
*partially* effective, where 0.40 ignored it entirely. Documents that use it will
repaginate on upgrade. R4 will move them again. Coordinate both with downstream
snapshot refreshes rather than shipping them separately.

### R5: forced breaks do not propagate from a first child to its container (P3)

**Status: SHIPPED** (`cd24af40`). Landed first and introduced
`SubtreeResult`, so R4 added a second producer with no signature churn.
Termination comes from the spec rather than a retry counter: a producer
requires `cursor_in > 0.0`, so after the caller advances, the subtree starts
at a page top where a break before it is a no-op (§3.1.1 collapses it).

§3.1.1: *"A break-before value on a first in-flow child box is propagated to its
container."* fulgur gates a nested `break-before: page` on
`cursor_y > page_start_y`, so on a leading child it is dropped rather than
handed up. Reuse the `RequestBreakBefore` channel from R4.

**Test to un-ignore:** `css_break3_s31_forced_break_on_first_child_propagates_to_container`
(`:8096`).

### R6: CSS `orphans` / `widows` property values are not parsed (P3)

**Status: SHIPPED** (`d440a708`). Two things the sketch below did not
anticipate. Both properties are *inherited* while `ColumnStyleTable` has no
inheritance, so the value is resolved by an ancestor walk at the point of
use (`resolved_line_constraints`) rather than by densifying the table. And
honouring a large `widows` needs `scan_split_points` to back a split **up**:
the scan only ever moved splits later, so R2's relaxation would otherwise
fire and mask the author's value with the default answer.

`ORPHANS_MIN` / `WIDOWS_MIN` (`:2652-2654`) hardcode the initial value 2. Authors
cannot change them. Parse both into the existing break-style table
(`extract_column_style_table`) and thread them into `fragment_inline_root`
alongside the R2 refactor — doing both at once avoids touching the same
signature twice.

**Test to un-ignore:** `css_break3_s44_rule3_author_widows_value_shifts_the_split`
(`:8202`).

### R7: smaller items

- **Orthogonal-flow leading-break guard is untested.** Extend
  `leading_break_is_not_propagated_out_of_flex_or_inline_block` (`:7519`) with a
  `writing-mode: vertical-rl` container case.
- **Nested vs body-direct monolithic asymmetry.** A body-direct oversized
  childless box is *sliced* per strip (fulgur-sbw2); the nested leading-child
  equivalent emits once, oversized (pinned by
  `oversized_unbreakable_leading_leaf_at_page_top_emits_once`, `:7852`). §4.1
  permits either, so this is a limitation, not a violation — but slicing in both
  places would be more consistent and would subsume part of R3.
- **Still unaddressed from the bug report:** `display: contents` changing
  fragmentation and deleting sibling content (§5 of the report), zero-height
  boxes skipped as break candidates, and list markers repeating on
  page-break continuation.

---

## Spec conformance map

Tests live in `crates/fulgur/src/pagination_layout.rs`, `css_break3_*` block
(`:7920` onward). Ignored tests **fail** when run with `--ignored` — that is
deliberate; each is a runnable statement of an open gap. Un-ignore as each rule
lands.

| css-break-3 rule | Test | Status |
| --- | --- | --- |
| §4.1 class A: break between sibling blocks | `css_break3_class_a_unforced_break_between_siblings` | passes |
| §4.1 class C needs a gap → break before container | `css_break3_no_class_c_point_without_gap_breaks_before_container` | passes |
| §4.1 class B: break between line boxes | `css_break3_class_b_break_between_line_boxes` | passes |
| §4.1 monolithic content sliced per strip (body-direct) | `css_break3_monolithic_body_direct_box_is_sliced_per_strip` | passes |
| §5.2 margins adjoining an unforced break truncate | `css_break3_s52_margin_adjoining_unforced_break_is_truncated` | passes |
| §3.1.1 forced break on first child propagates to container | `css_break3_s31_forced_break_on_first_child_propagates_to_container` | **passes** (R5) |
| §4.4 rule 2: ancestor `break-inside: avoid` forbids class A break | `css_break3_s44_rule2_ancestor_break_inside_avoid_forbids_class_a_break` | **passes** (R4) |
| §4.4 relaxation: never lose lines off the edge | `css_break3_s44_widow_relaxation_prevents_lines_escaping_the_strip` | **passes** (R2) |
| §4.4 rule 3: author `orphans` / `widows` values | `css_break3_s44_rule3_author_widows_value_shifts_the_split` | **passes** (R6) |

Covered elsewhere, deliberately not duplicated:

- §4.4 relaxation of `break-inside: avoid` on an unfulfillable box —
  `nested_avoid_inside_is_relaxed_when_paragraph_exceeds_a_whole_page` (`:7707`).
- §4.1 flex/grid items are not class A break points —
  `leading_break_is_not_propagated_out_of_a_grid_row` (`:7880`).
- §5.4 `box-decoration-break` slice/clone — a paint-level rule invisible to the
  geometry table. Needs a VRT fixture, not a unit test.

---

---

## Shipped since this review

Landed on `fix/fulgur-pgbrk-page-fragmentation` after the review was written.

| Item | Commit | Note |
| --- | --- | --- |
| R3 page-overflow detection | `28926fe4` | plus `a6d89aa5`, the CLI stderr logger without which the warning was inert |
| R1 border-box inline-root fragments | `a2e103ad` | |
| Risk 1 extraction | `139666b7`, `0eef1585`, `034bed0a` | [design](./2026-08-17-fulgur-pgbrk-break-decision-extraction-design.md) / [plan](./2026-08-17-fulgur-pgbrk-break-decision-extraction-plan.md) |
| R8 forced-break parent dedup | `1d0ea8da`, `d782ed1a` | found by the Risk 1 extraction; not in the original R1–R7 list |
| R2 widows/orphans relaxation | `8c88e74d` | |
| R6 author `orphans` / `widows` | `d440a708` | |
| R5 forced-break propagation | `cd24af40` | introduced `SubtreeResult` |
| R4 `break-inside: avoid` on blocks | `5784b4cb` | second producer on R5's channel |

### R8: forced breaks closed a flex/grid parent twice on one page

Not in R1–R7 — surfaced by laying the nine parent-slice emission sites side by
side during the Risk 1 extraction. Only the two unforced (overflow-driven) sites
consulted `row_state.emitted_parent_pages`; the six forced `break-before` /
`break-after: page` sites did not.

Reaching it needs recursion, not merely two forced breaks: `page_index` is
restored to the row start only when `crossed_by_recursion` is set, and a forced
break does not set it. So it takes a first cell that crosses a page by recursion
— closing the parent on page 0 through the deduped path — followed by a same-row
cell whose forced break closes it again. The two fragments carried **different**
heights (400 and 400-strip vs 60), so `render.rs` painted the container's
background, border and shadow twice at two sizes on one page, and every
fragment-counting walk saw a phantom third fragment.

The dedup keys on "the parent is leaving this page", which a forced break
satisfies exactly as an unforced one does, so all six now dedup. The function
tail is deliberately excluded — it closes the parent's final fragment on a page
it never leaves.

**A note on how it was found.** The first probe written for this *passed*, and
passed vacuously: a forced break advances the page before the next cell is
processed, so two cells that merely both carry `break-after: page` never contend
for one. Only after checking that the fixture actually reached the code under
test did the real shape appear. This is the third time in this workstream a
pagination test has passed for the wrong reason (see the R1 design's note on
`padded_leading_child_...`, and gap 1 in the first-iteration review). **Confirm
that a new pagination test can fail before trusting it.**

## Behaviour worth flagging to downstream

fulgur **never** takes a mid-strip line split for a paragraph that starts
mid-page: it pushes the whole paragraph to a fresh page and only line-splits
paragraphs taller than a full page. This is spec-legal (breaking before is a
legal class A choice) but under-fills pages relative to paged.js and browsers, so
snapshot diffs on upgrade are expected and are not necessarily regressions.

---

## Verification commands

```bash
cargo test -p fulgur --lib                       # 2027 passed / 4 ignored
cargo test -p fulgur --lib css_break3            # spec conformance block
cargo test -p fulgur --lib css_break3 -- --ignored  # open gaps: expected to FAIL
cargo test -p fulgur --test render_smoke leading_child_that_must_break_does_not_lose_content
cargo clippy -p fulgur && cargo fmt --check
```

`render_smoke`'s pagination test hard-requires `pdftotext` (poppler-utils) and
panics rather than skipping when it is missing: the lost glyphs are still
present in the content stream, just painted outside the page box, so
`fulgur::inspect` reports byte-identical output for a broken and a correct
render. Only a MediaBox-respecting extractor sees the difference. CI preinstalls
poppler-utils; locally use `brew install poppler`.

VRT (`cargo test -p fulgur-vrt`) shows 29/64 differing on macOS both with and
without this work — an environment/goldens mismatch, not a regression. Verify
any future pagination change the same way (stash, re-run, diff the failing list)
before concluding it moved VRT.
