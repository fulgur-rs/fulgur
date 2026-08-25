# fulgur-pgbrk R3: page-overflow detection

**Goal:** Detect every fragment placed past the bottom of its page's content
strip, warn on it in production, and make it a blanket test invariant — so that
silent content loss becomes impossible to ship unnoticed, and so the remaining
pagination bugs (R1, R2) become testable in-process.

**Parent review:** [2026-08-16-fulgur-pgbrk-page-fragmentation-review.md](./2026-08-16-fulgur-pgbrk-page-fragmentation-review.md)
(item R3).

**Spec baseline:** [CSS Fragmentation Module Level 3](https://www.w3.org/TR/css-break-3/).

**Tech stack:** Rust, `crates/fulgur/src/pagination_layout.rs`.

---

## Why this goes first

R1–R7 in the parent review are not seven independent changes. They cluster by
which function signature they touch:

- **R1 + R2 + R6** all land in `fragment_inline_root` and its two duplicated
  call sites (`:741`, `:2061`). Done separately, that signature is edited three
  times and the same tests re-blessed three times.
- **R4 + R5** both need one thing: `fragment_block_subtree` returning a
  `SubtreeResult::RequestBreakBefore` channel instead of `(u32, f32)`.
- **R3** is orthogonal to both, and is the only item that closes the *class* of
  defect rather than an instance.

R3 is also test infrastructure, not just a diagnostic. Today `render_smoke`'s
pagination test hard-requires `pdftotext` (poppler), because `fulgur::inspect`
reports byte-identical output for a broken and a correct render — the lost
glyphs are in the content stream, merely painted outside the page box. A
predicate over the geometry table sees that difference in-process, which turns
every R1/R2 test from "shell out to poppler" into a cheap assertion.

Order: **R3 → (R1 + R2 + R6) → (R4 + R5)**, with the parent review's Risk 1
`break_decision` extraction folded into whichever bundle first touches
`would_split_block_subtree`.

---

## The predicate

One free function, next to `run_pass_inner`:

```rust
/// One fragment that was placed past the bottom of its page's content strip.
pub(crate) struct FragmentOverflow {
    pub node_id: usize,
    pub page_index: u32,
    /// px by which the fragment's bottom exceeds the content strip.
    pub overshoot_px: f32,
    /// The node has a further fragment on a later page, so this one is
    /// a slice whose height should have been clipped to the strip.
    pub continues_on_later_page: bool,
}

/// Every fragment whose bottom falls below the content strip, in
/// deterministic (node, page) order.
pub(crate) fn find_overflowing_fragments(
    table: &PaginationGeometryTable,
    page_height_px: f32,
    body_id: Option<usize>,
) -> Vec<FragmentOverflow>
```

The test is `f.y + f.height > page_height_px + 0.5`, matching the epsilon
convention already used inline in this file.

### `body_id` and `continues_on_later_page`

Both were added during implementation, from measurement, and neither was in the
original sketch.

**`body_id`** excludes body — the one box the fragmenter never fragments. Its
entry records the whole document content height once, on page 0, as a
document-level total rather than a per-page placement. Excluding it took the
failing-test count from 60 to 23. Every *other* container is properly split (a
wrapper spanning two pages gets one correctly clipped fragment per page), so this
is a genuine one-off, not a carve-out for containers.

**`continues_on_later_page`** separates two defect classes the raw predicate
conflates. If an overflowing fragment is not the node's last, the node continues
on a later page, so that fragment is a *slice* whose height should have been
clipped — a bookkeeping bug, fixable immediately. If it is the last, the content
genuinely had nowhere to go, and fixing it needs new fragmentation machinery.
Both are defects; only the first was in scope here.

`PaginationGeometryTable` is a `BTreeMap`, so iteration order is deterministic
and the result needs no sort. That matters because the warn text reaches
user-visible logs, and determinism is a project invariant (CLAUDE.md).

### Insertion point

The tail of `run_pass_inner`. All three public entry points — `run_pass`,
`run_pass_with_break_styles`, `run_pass_with_break_and_running` — funnel through
it, so the check reaches all 77 existing call sites with no signature change and
no call-site edits.

### Relationship to the existing test helper

`assert_no_fragment_starts_below_page` (`pagination_layout.rs:7377`) is rewritten
as a thin wrapper over `find_overflowing_fragments`. This is **not** a pure
rename: the old helper tests `f.y > page_h` (the fragment *starts* below the
strip), the new one tests `f.y + f.height > page_h` (the fragment *ends* below
it). The new predicate is strictly stronger and is expected to be where most new
test failures come from.

---

## Production behaviour

At the end of `run_pass_inner`, one `log::warn!` per record. The crate uses the
`log` facade, not `tracing` (`asset.rs`, `blitz_adapter.rs`, `column_css.rs` all
use `log::warn!`; `tracing` is not a dependency):

```rust
for o in find_overflowing_fragments(&table, page_height_px, body_id) {
    log::warn!(
        "node {}: fragment on page {} extends {:.2}px past the {:.2}px page \
         content strip; content may be painted over the bottom margin or \
         clipped away entirely",
        o.node_id, o.page_index, o.overshoot_px, page_height_px,
    );
}
```

`log` routes to whatever logger the host installs and never touches fd 1
directly, satisfying CLAUDE.md's rule that `crates/fulgur` must not write to
stdout under any circumstance. A library consumer with no logger gets silence;
`fulgur-cli` already installs one.

No public API change, no `Engine` result field, no CLI flag. Those were
considered and deferred — see [Rejected alternatives](#rejected-alternatives).

---

## Test behaviour

The same loop, under `#[cfg(test)]`, panics instead of warning. This makes the
invariant blanket across every existing pagination test with zero per-test
edits.

The tradeoff, named explicitly: test and production builds diverge in behaviour
at this one point. Accepted, because the divergence is only "the same condition
is fatal instead of logged" — the ordinary shape of an invariant assertion.

### Known-failing fixtures

The check has true positives on day one. There is **no allowlist**. Failing
fixtures join the `#[ignore]` convention already established by the
`css_break3_*` block: an ignored test that fails under `--ignored` is a runnable
statement of an open gap. No new mechanism is introduced.

One member needed more than an `#[ignore]`:
`oversized_unbreakable_leading_leaf_at_page_top_emits_once` *asserted* that the
overflowing output was correct — a 900px probe on a 400px page emitting one
fragment at `y≈0` with `height=900`. Ignoring it as written would have pinned
the defect under a new name. It is renamed to
`..._is_sliced_per_strip` and now states the R7 target: three slices
(400 + 400 + 100) on consecutive pages, each starting at its page top, summing to
the box height, none extending past the strip. It keeps pinning the original
concern — that the leading-child floor never becomes an infinite page advance —
via the per-slice `y` assertions.

---

## Outcome: two defects the check found

The check was expected to red-flag known gaps. It also surfaced two live
rendering bugs that were in none of R1–R7. Both were fixed here.

### Defect A: the outgoing-page height counted the splitting child in full

`fragment_block_subtree`'s fulgur-oc51 block computed the parent's
outgoing-page fragment as:

```rust
let logical_height = (pre_recursion_cursor_y + child_h - page_start_y).max(0.0);
let prev_height = logical_height.max((page_height_px - page_start_y).max(0.0));
```

`logical_height` counts the *splitting* child at its full unfragmented height on
the page it is leaving, though only the first slice landed there. For a parent
starting mid-page (`page_start_y=200`, `child_h=400`, 400px page) it yields
`h=400` at `y=200` — a fragment bottom of 600.

That is not cosmetic. `render.rs:2793` feeds `frag.height` straight into
`draw_background` / `draw_box_shadows` / border painting whenever `is_split()`,
so the surplus painted the container's decorations through the bottom margin and
over any running footer — precisely the failure `render.rs`'s own comment says
the code exists to prevent.

The fix is `prev_height = (page_height_px - page_start_y).max(0.0)`. The
block's own comment already established the premise: the parent's content
reached the page bottom, or the recursion would not have advanced past that
page. Corroboration that this was a defect rather than a deliberate trade-off:
the inline-root twin of this bookkeeping (`:2251`) already computed it exactly
that way. The two sites simply disagreed.

The superseded comment cited fixtures `mo-006/008` as expecting "margin-area
paint". Those are not tests in this repo — they appear only in comments — so the
claim could not be exercised. VRT is byte-identical with and without the change
(see below), so nothing observable depended on it.

**Result: 23 failing tests → 12.**

### Defect B: parent slice heights carried a trailing margin past the page bottom

The parent's `cursor_y` legitimately sits past the page bottom: it carries the
trailing margin of the last child placed on the page. In the multicol fixture the
last paragraph occupies `362..394` on a 400px strip, but `cursor_y` reaches 408
— its ~13px bottom margin — and every parent push used
`height: cursor_y - page_start_y` verbatim.

css-break-3 §5.2 truncates margins adjoining an unforced break to zero, and a
container's fragment can never legitimately paint below the page bottom. Both
point to the same fix — a shared helper applied at all nine parent push sites:

```rust
fn parent_slice_height(cursor_y: f32, page_start_y: f32, page_height_px: f32) -> f32 {
    let strip = (page_height_px - page_start_y).max(0.0);
    (cursor_y - page_start_y).clamp(0.0, strip)
}
```

This clamps the *container* only. A child that genuinely does not fit keeps its
overflowing fragment, so unbreakable-content defects stay visible to
`find_overflowing_fragments` rather than being masked.

**Result: 12 failing tests → 11, and the CLIP-BUG class is fully eliminated.**

### What remains: the NO-ROOM class

All 11 remaining failures are content with nowhere to go, in two groups, now
`#[ignore]`d with the gap each is blocked on:

- **flex / grid (7 tests)** — flex and grid items are not class A break points
  (§4.1), so fulgur co-splits rows in place and a row taller than the strip
  overflows. Needs internal fragmentation of flex/grid rows.
- **monolithic (4 tests)** — content taller than the fragmentainer emitted whole,
  including `overflow: hidden` boxes. §4.1 permits overflow, but fulgur already
  slices in the body-direct path, so this is the R7 asymmetry.

### Final numbers

| Check | Baseline | After |
| --- | --- | --- |
| `cargo test -p fulgur --lib` | 2027 passed / 0 failed / 4 ignored | 2029 passed / 0 failed / 15 ignored |
| `cargo test -p fulgur-vrt` | 29 of 64 differ | 29 of 64 differ, byte-identical |

The 13 new tests offset the 11 moved to `#[ignore]`.

VRT was verified by the stash / re-run / diff protocol: the failing fixture list
matches exactly, including byte sizes. That also means **no VRT fixture exercises
a mid-page container split**, so neither fix is covered visually — the lib tests
are the only regression barrier. A VRT fixture for a wrapper with a visible
background splitting mid-page would be worth adding.

---

## Testing

Per CLAUDE.md's coverage rule, this is lib-level logic and belongs in
`#[cfg(test)] mod tests` in `pagination_layout.rs`. No VRT fixture is needed —
the predicate is invisible to rendering.

Unit tests for `find_overflowing_fragments`:

- Empty table yields no records.
- A fragment ending exactly at `page_height_px` yields no record (epsilon
  boundary).
- A fragment ending `0.4px` past yields no record; `0.6px` past yields one.
- A fragment that *starts* below the strip is caught (the old helper's case).
- Multiple overflows across nodes and pages come back in deterministic order.
- `overshoot_px` is the bottom minus the strip height, not the fragment height.

---

## Rejected alternatives

**Two-tier detection (content strip vs. paper edge).** Considered reporting
overflow past the page box separately from overflow past the content strip, on
the theory that the first is silent content loss and the second is merely
overlap with the footer. Rejected: paper-edge overflow is a strict subset of
content-strip overflow, so the second predicate adds zero detection coverage.
Any severity distinction is a message field computed from an already-known
number, not a separate pass.

**Suppressing "spec-legal" monolithic overflow.** §4.1 permits monolithic
content taller than the fragmentainer to overflow. Rejected as a suppression
rule: the spec's other permitted option is slicing, and fulgur already slices in
the body-direct path. The overflow in the nested path is therefore a fulgur
inconsistency (R7), so the predicate firing there is a true positive, not noise.

**Allowlist of known-failing fixtures.** Rejected as unnecessary machinery.
`#[ignore]` already carries that meaning in this file.

**Per-test opt-in assertion.** Rejected: leaves existing coverage unchecked and
requires deciding, per test, whether the invariant applies — when it always
applies.

**`Engine` result count plus `fulgur-cli --strict-pagination`.** Deferred. It is
a public API commitment, and it should not be made before R1 and R2 have reduced
the population of real overflows a strict mode would trip on.

---

## Verification commands

```bash
cargo test -p fulgur --lib
cargo test -p fulgur --lib find_overflowing_fragments
cargo test -p fulgur --lib -- --ignored     # open gaps: expected to FAIL
cargo clippy -p fulgur && cargo fmt --check
npx markdownlint-cli2 'docs/plans/2026-08-16-fulgur-pgbrk-r3-overflow-detection-design.md'
```

VRT is not expected to move. Per the parent review, `cargo test -p fulgur-vrt`
shows 29/64 differing on macOS independent of this work — an environment/goldens
mismatch. Verify any change the same way (stash, re-run, diff the failing list
including byte sizes) before concluding it moved VRT.
