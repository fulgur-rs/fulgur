# fulgur-pgbrk Risk 1: one break decision, one parent-slice emission

**Goal:** Collapse the duplicated page-break logic in `pagination_layout.rs`
into two named units — a pure `break_decision` predicate and a `ParentSlice`
emission helper — so that R2, R4 and R5 edit one site each instead of three,
and so the dedup policy governing parent fragments becomes a stateable rule
rather than an accident of nine hand-copied blocks.

**Parent review:** [2026-08-16-fulgur-pgbrk-page-fragmentation-review.md](./2026-08-16-fulgur-pgbrk-page-fragmentation-review.md)
(Risk 1, "break logic is duplicated across three sites that must agree").

**Predecessors:** [R3 overflow detection](./2026-08-16-fulgur-pgbrk-r3-overflow-detection-design.md),
[R1 border-box inline-root fragments](./2026-08-16-fulgur-pgbrk-r1-border-box-design.md).

**Spec baseline:** [CSS Fragmentation Module Level 3](https://www.w3.org/TR/css-break-3/).

**Tech stack:** Rust, `crates/fulgur/src/pagination_layout.rs`.

This change is **behaviour-preserving by construction**. It ships no fix. Its
value is that the two bundles queued behind it (R2 + R6, then R4 + R5) stop
being three-site edits.

---

## Measured state at commit `a6d89aa5`

The review named `break_decision` as the extraction target. Measuring the tree
first moved the target: the predicate is the smaller and less dangerous half of
the duplication.

### The predicate — three sites

| site | code |
| --- | --- |
| `:2623` block strip-overflow cut | `floor = if propagate_leading_break { 0.0 } else { page_start_y }`, then `child_page_y > floor && child_page_y + child_h > page_height_px` |
| `:2254` nested inline-root push-whole | the same, with `box_total_h` in place of `child_h` |
| `:935` body-direct inline push-whole | `cursor_y > 0.0 && cursor_y + box_total_h > page_height_px` — the same predicate with the floor specialized to `0.0` |

The first two are the same expression written twice; they agree only by
inspection. The third is the same rule with a constant folded in, which is
correct at body level (`page_start_y` is always `0` there, and leading-edge
propagation is always permitted).

### The consequence — nine sites

Sites `:2254` and `:2623` are followed by **26 token-identical lines**
(verified by normalizing whitespace and comments): the `cursor_y >
page_start_y` guard, the `row_state.emitted_parent_pages` dedup, the parent
fragment push, then a five-variable page advance.

Underneath that, the *emission* alone —

```rust
geometry.entry(parent_id).or_default().fragments.push(Fragment {
    page_index,
    x: parent_x_in_body.as_px(),
    y: page_start_y.as_px(),
    width: parent_w.as_px(),
    height: parent_slice_height(cursor_y, page_start_y, page_height_px).as_px(),
});
```

— appears at **nine** sites: `:2108`, `:2144`, `:2189`, `:2275`, `:2366`,
`:2578`, `:2651`, `:2710`, `:2757`. This is the half where a divergent edit
corrupts PDF output: a wrong `parent_slice_height` argument paints container
decoration through the bottom margin (exactly R3's Defect B), and a missed
`page_taffy_origin` rebase shifts every subsequent child.

### The page advance — three incompatible shapes

The five-variable advance that follows the emission is **not** a single idiom.
Across five sites it takes three forms:

- **Eager rebase** (`page_taffy_origin = this_top_in_parent`) — `:2115`,
  `:2199`, `:2283`, `:2662`.
- **Deferred rebase** (`origin_pending_target_y = Some(page_start_y)`, resolved
  at the next child) — `:2151`, `:2371`, `:2583`, `:2715`.
- **None** — `:2757`, the function tail, which closes the parent's final
  fragment and returns.

The `break-before: page` site (`:2179`) differs again: its `cursor_y >
page_start_y` guard sits *outside* the whole block, so a leading forced break
collapses to a no-op instead of advancing. That is correct per §3 and is
genuinely different control flow, not drift.

---

## Changes

### Extraction A: `break_decision`

A pure free function beside `parent_slice_height`, consumed at all three
predicate sites:

```rust
/// Whether a child may break before itself on the current strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakDecision {
    PlaceHere,
    PushToNextPage,
}

/// `floor` is the y below which a break is legal on this strip: `0.0` when a
/// leading child may propagate its break to the box's own leading edge
/// (fulgur-pgbrk), `page_start_y` when the container pins its children
/// (flex / grid, atomic inline, orthogonal flow — see `suppress_page_check`).
///
/// `child_box_h` is the **border box** height (fulgur-pgbrk R1): the block
/// path passes Taffy's `child_h`, the inline-root paths pass
/// `lead_in + lines_h + lead_out`.
fn break_decision(
    child_top_on_page: f32,
    child_box_h: f32,
    floor: f32,
    page_height_px: f32,
) -> BreakDecision;
```

An enum rather than a `bool` because R4 and R5 both need a third
`RequestBreakBefore` variant. Starting as an enum means they add a variant
instead of changing a signature and re-blessing the same tests a second time.

### Extraction B: `ParentSlice`

The nine-site emission, as a borrow struct built once per
`fragment_block_subtree` call:

```rust
/// The half of the parent-slice idiom that never changes across the walk.
struct ParentSlice {
    id: usize,
    x_in_body: f32,
    width: f32,
    page_height_px: f32,
}
```

with **two named methods** rather than one method taking a `dedup: bool`:

- `close_forced(..)` — the seven sites driven by `break-before` /
  `break-after: page`, plus the function tail.
- `close_unforced(.., row: Option<&mut RowState>)` — the two overflow-driven
  sites, which consult `row_state.emitted_parent_pages`.

Naming the policy puts it in the diff. A reviewer sees *which* rule a call site
takes without reading a twenty-line builder chain, and the seven/two split
becomes something that can be questioned.

### Not extracted: the page advance

Forcing three incompatible shapes into one helper needs a mode enum that
reintroduces the branching it was meant to remove. Once B lands, the two
token-identical sites reduce to five self-evident assignments — a residue a
reviewer can verify at a glance, unlike the emission logic.

---

## The rule the extraction exposes

Laid out together, the nine sites split on a rule that is nowhere written down:

| site | trigger | dedup |
| --- | --- | --- |
| `:2275` | inline-root push-whole | **yes** |
| `:2651` | block strip-overflow cut | **yes** |
| `:2108` | `break-before: page`, zero-height child | no |
| `:2144` | `break-after: page`, zero-height child | no |
| `:2189` | `break-before: page` | no |
| `:2366` | `break-after: page`, after the inline branch | no |
| `:2578` | `break-after: page`, after recursion | no |
| `:2710` | `break-after: page`, after a plain child | no |
| `:2757` | function tail, parent's final fragment | n/a |

**Unforced breaks dedup; forced breaks do not.** That is coherent:
`emitted_parent_pages` exists so that N parallel flex / grid cells, each
independently discovering "I overflow this strip", do not each close the parent
on the same page.

It also looks under-guarded. `row_state` is `Some` for any flex / grid parent,
and two same-row cells both carrying `break-after: page` would each close the
parent at the same `page_index`, `y` and height — two identical fragments.
Visually that repaints one background over itself, which is harmless, but it
flips `is_split()` and double-counts in the fragment walks performed by
`paragraph_lines_for_page` and `find_overflowing_fragments`.

**This refactor does not change it.** Encoding the rule in a method name is a
documentation act; changing it is a behavioural claim that needs its own test
and its own commit.

---

## Testing

### The dedup question is answered, not deferred

Whether parallel forced breaks actually double-close the parent is decidable in
one test run, so it is decided here rather than filed away. Add a probe — a
grid container whose two same-row cells both carry `break-after: page`,
asserting the parent has exactly one fragment per crossed page — and run it
against the pre-refactor tree.

- **Passes** → the seven forced sites are correct as they stand. It lands as a
  live regression test, and `ParentSlice`'s doc comment states a confirmed rule
  rather than a guess.
- **Fails** → it lands `#[ignore]`d with the gap named, joins the spec
  conformance map in the parent review, and gets a Remaining Work entry (R8).

`bd` is not installed on the author's machine and `.beads/issues.jsonl` syncs
from a separate remote, so the `#[ignore]` convention is the primary record —
which is the stronger form anyway. Per the R3 design: *an ignored test that
fails under `--ignored` is a runnable statement of an open gap.* A tracker entry
can be closed by someone who never ran it; a test cannot. Mirror it into beads
opportunistically, not as the system of record.

### Unit tests for `break_decision`

- Floor at `0.0` versus at `page_start_y`, same inputs, opposite verdicts.
- A child whose bottom lands exactly on `page_height_px` stays put.
- A leading child at `child_top == floor` stays put (the strict `>`).
- A child taller than the whole page at `child_top == 0.0` stays put — there is
  nowhere to push to, and this is the gate that stops an infinite page advance.

### Behaviour preservation is the bar

The extraction claims zero output change, so:

- `cargo test -p fulgur` fully green **with no test edits**. Any test that needs
  re-blessing means the refactor is wrong, not that the test was stale.
- `cargo test -p fulgur --lib -- --ignored` fails on exactly the same set as
  before (11 today, plus R8 if the probe fails).
- VRT shows the same 29 of 64 differing with identical byte sizes, verified by
  the stash / re-run / diff protocol from the R3 design. Do not regenerate
  goldens on macOS.

---

## Out of scope

**`would_split_block_subtree` (`:1509`).** The review framed Risk 1 as
simulator drift, and the drift is real: the simulator runs in available-strip
space (`cursor` from `0` against `available_h`) with **no floor concept at
all**, so for a leading child it always reports "would split" where the real
walk with `propagate_leading_break == false` places the child overflowing
instead. Today that costs a wasted recursion, not wrong output.

Folding it into `break_decision` means converting it to absolute page-local
space, and it is the recursion *gate* — the single most behaviour-sensitive
function in the file. Changing which subtrees are entered is not
behaviour-preserving and does not belong in the same commit as an extraction
that claims to be. R4 needs a trustworthy simulator for its "does this subtree
fit a fresh page?" test, so this is best done as R4's first step, where its
output changes can be attributed.

> **Resolved 2026-08-19 (`3555418e`).** Phase 5 of the walker convergence
> replaced the simulator with `subtree_requires_recursion`, which evaluates
> the walk's own `break_decision` with the parent's actual floor over the
> walk's own enumeration. The floor disagreement is closed by construction;
> output was proven unmoved by the full suite, the `--ignored` set, and the
> VRT stash/diff protocol.

**R2 / R6, R4 / R5.** Unchanged in scope and order by this work; that is the
point of it.

---

## Verification commands

```bash
cargo test -p fulgur
cargo test -p fulgur --lib break_decision
cargo test -p fulgur --lib -- --ignored        # open gaps: expected to FAIL
cargo clippy -p fulgur && cargo fmt --check
npx markdownlint-cli2 'docs/plans/2026-08-17-fulgur-pgbrk-break-decision-extraction-design.md'
```
