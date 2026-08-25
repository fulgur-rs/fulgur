# Risk 1 extraction implementation plan

**Goal:** Replace the duplicated page-break logic in
`crates/fulgur/src/pagination_layout.rs` with one `break_decision` predicate
(3 call sites) and one `ParentSlice` emission helper (9 call sites), changing
no output.

**Architecture:** Two extractions, landed separately. `break_decision` is a
pure function returning a two-variant enum, consumed by the block
strip-overflow cut, the nested inline-root push-whole, and the body-direct
inline push-whole. `ParentSlice` is a borrow struct built once per
`fragment_block_subtree` call, exposing `close_forced` and `close_unforced` so
the `row_state.emitted_parent_pages` dedup policy is visible at each call site.
The five-variable page advance is deliberately left inline — it has three
incompatible shapes.

**Tech Stack:** Rust, Blitz/Taffy/Parley layout, krilla PDF output.

**Design:** [2026-08-17-fulgur-pgbrk-break-decision-extraction-design.md](./2026-08-17-fulgur-pgbrk-break-decision-extraction-design.md)

---

## Before you start

Read the design document. Then read these two functions end to end — the whole
plan lives inside them:

- `fragment_block_subtree` (`pagination_layout.rs:1813-2770`)
- `fragment_pagination_root`'s body-direct child loop, around `:860-970`

### The one rule that matters

**This refactor changes no output.** The acceptance bar for every task is the
full suite green *with no test edits*. If a test needs re-blessing, the
extraction is wrong — do not update the test, fix the extraction.

### Baseline, measured at commit `1d0ea8da`

```text
cargo test -p fulgur   ->  2481 passed, 0 failed, 18 ignored
cargo test -p fulgur --lib  ->  2034 passed, 0 failed, 16 ignored
```

The 18th ignored test is `forced_break_does_not_close_a_grid_parent_twice_on_one_page`
(R8), landed in `1d0ea8da`. It must still fail under `--ignored` when you are
done — this refactor does not fix it.

### Conventions you will otherwise get wrong

- Units: `f32` locals in this file are **CSS px**. `Fragment` fields are
  `units::Px`, so every value crossing into a `Fragment` gets `.as_px()`. Do
  not introduce a `pt` conversion anywhere in this work. See
  `.claude/rules/coordinate-system.md`.
- Determinism: iteration that affects PDF output uses `BTreeMap` / `BTreeSet`,
  never `HashMap` / `HashSet`.
- `cargo fmt --check` is CI-enforced. Run `cargo fmt -p fulgur` before every
  commit.
- Tests for pure helpers go in the existing `#[cfg(test)] mod tests` at the
  bottom of `pagination_layout.rs`, not in a new file.

---

## Task 1: `break_decision`, tests first

**Files:**

- Modify: `crates/fulgur/src/pagination_layout.rs` (add beside
  `parent_slice_height`, around `:199-222`)

### Step 1: Write the failing tests

Add to `mod tests`, next to the existing `parent_slice_height_*` tests
(around `:7886`):

```rust
#[test]
fn break_decision_pushes_a_child_that_overflows_below_the_floor() {
    // Child starts at 200 on a 400 strip and is 300 tall: bottom 500.
    assert_eq!(
        super::break_decision(200.0, 300.0, 0.0, 400.0),
        super::BreakDecision::PushToNextPage
    );
}

#[test]
fn break_decision_places_a_child_that_fits() {
    assert_eq!(
        super::break_decision(200.0, 100.0, 0.0, 400.0),
        super::BreakDecision::PlaceHere
    );
}

#[test]
fn break_decision_floor_decides_a_leading_child() {
    // A leading child sits exactly at its container's page start. With
    // leading-edge propagation permitted the floor is 0, so the break is
    // legal; with the container pinning its children the floor is
    // page_start_y and the child stays put and overflows (fulgur-pgbrk).
    assert_eq!(
        super::break_decision(200.0, 300.0, 0.0, 400.0),
        super::BreakDecision::PushToNextPage
    );
    assert_eq!(
        super::break_decision(200.0, 300.0, 200.0, 400.0),
        super::BreakDecision::PlaceHere
    );
}

#[test]
fn break_decision_is_strict_at_the_floor() {
    // `child_top > floor`, not `>=`.
    assert_eq!(
        super::break_decision(0.0, 500.0, 0.0, 400.0),
        super::BreakDecision::PlaceHere
    );
}

#[test]
fn break_decision_places_a_child_ending_exactly_on_the_page_bottom() {
    // `child_top + h > page_height_px`, not `>=` — a child whose bottom
    // lands exactly on the boundary fits.
    assert_eq!(
        super::break_decision(100.0, 300.0, 0.0, 400.0),
        super::BreakDecision::PlaceHere
    );
}

#[test]
fn break_decision_keeps_an_oversized_child_at_the_page_top() {
    // Nothing to push to. This is the gate that stops the leading-child
    // floor from becoming an infinite page advance.
    assert_eq!(
        super::break_decision(0.0, 900.0, 0.0, 400.0),
        super::BreakDecision::PlaceHere
    );
}
```

### Step 2: Run them and watch them fail

```bash
cargo test -p fulgur --lib break_decision
```

Expected: compile error, `cannot find function break_decision in module super`.

### Step 3: Implement

Add beside `parent_slice_height`:

```rust
/// Whether a child may break before itself on the current strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakDecision {
    /// Place the child where the cursor is.
    PlaceHere,
    /// Close the parent on this page and place the child on the next.
    PushToNextPage,
}

/// The single break decision, shared by the block strip-overflow cut, the
/// nested inline-root push-whole, and the body-direct inline push-whole.
///
/// `floor` is the y below which a break is legal on this strip:
///
/// - `0.0` when an overflowing LEADING child may propagate its break up to
///   the box's own leading edge (css-break-3 §3 — a break before a box's
///   first child is also a break before the box). This is fulgur-pgbrk's
///   fix; before it the gate was `page_start_y`, which is never exceeded by
///   a first in-flow child, so such a child could never break.
/// - `page_start_y` inside a container that does not paginate its children
///   independently — flex / grid (whose items are not class A break points,
///   §4.1), atomic inline containers, orthogonal flow. See
///   `suppress_page_check`.
///
/// `child_box_h` is the **border box** height (fulgur-pgbrk R1): the block
/// path passes Taffy's `child_h`; the inline-root paths pass
/// `lead_in + lines_h + lead_out`, because Parley's line metrics are
/// content-box relative and omit the box's own padding and border.
///
/// At `child_top_on_page == floor` the answer is always `PlaceHere`: we are
/// at the top of a fresh strip with nowhere to push to, and returning
/// `PushToNextPage` there would advance pages forever.
fn break_decision(
    child_top_on_page: f32,
    child_box_h: f32,
    floor: f32,
    page_height_px: f32,
) -> BreakDecision {
    if child_top_on_page > floor && child_top_on_page + child_box_h > page_height_px {
        BreakDecision::PushToNextPage
    } else {
        BreakDecision::PlaceHere
    }
}
```

### Step 4: Run the tests

```bash
cargo test -p fulgur --lib break_decision
```

Expected: `6 passed`. Clippy will warn that `BreakDecision` and
`break_decision` are never used outside tests — that is expected until Task 2
and is resolved there. Do not silence it with `#[allow(dead_code)]`; just
proceed.

### Step 5: Commit

```bash
cargo fmt -p fulgur
git add crates/fulgur/src/pagination_layout.rs
git commit -m "refactor(pagination): add break_decision, the single break predicate"
```

---

## Task 2: Route the three predicate sites through it

**Files:**

- Modify: `crates/fulgur/src/pagination_layout.rs:2623-2628` (block
  strip-overflow cut)
- Modify: `crates/fulgur/src/pagination_layout.rs:2254-2259` (nested
  inline-root push-whole)
- Modify: `crates/fulgur/src/pagination_layout.rs:935` (body-direct inline
  push-whole)

No new tests. The existing suite is the test: this is a pure substitution and
2481 tests already cover it.

### Step 1: Block strip-overflow cut

Replace:

```rust
let overflow_floor = if propagate_leading_break {
    0.0
} else {
    page_start_y
};
if child_page_y > overflow_floor && child_page_y + child_h > page_height_px {
```

with:

```rust
let overflow_floor = if propagate_leading_break {
    0.0
} else {
    page_start_y
};
if break_decision(child_page_y, child_h, overflow_floor, page_height_px)
    == BreakDecision::PushToNextPage
{
```

Keep the long fulgur-pgbrk comment above it — it explains *why* the floor is
what it is, which `break_decision`'s doc comment does not repeat in terms of
this call site.

### Step 2: Nested inline-root push-whole

Same substitution, with `box_total_h` in place of `child_h` and the local
named `inline_overflow_floor`.

### Step 3: Body-direct inline push-whole

Replace:

```rust
if cursor_y > 0.0 && cursor_y + box_total_h > self.page_height_px {
```

with:

```rust
// Body level: `page_start_y` is always 0 and leading-edge propagation is
// always permitted, so the floor is 0.
if break_decision(cursor_y, box_total_h, 0.0, self.page_height_px)
    == BreakDecision::PushToNextPage
{
```

### Step 4: Verify nothing moved

```bash
cargo test -p fulgur
```

Expected: `2481 passed, 0 failed, 18 ignored`. **Any other number means
stop** — you have changed behaviour. The most likely cause is passing
`cursor_y` where the original passed `child_page_y` (they differ for
flex / grid parallel siblings), or dropping the `self.` on
`self.page_height_px` at the body-direct site.

```bash
cargo test -p fulgur --lib -- --ignored 2>&1 | tail -3
```

Expected: `0 passed; 16 failed` — the same open gaps as before, including R8.

### Step 5: Commit

```bash
cargo fmt -p fulgur && cargo clippy -p fulgur --all-targets
git add crates/fulgur/src/pagination_layout.rs
git commit -m "refactor(pagination): route all three break sites through break_decision"
```

---

## Task 3: `ParentSlice`

**Files:**

- Modify: `crates/fulgur/src/pagination_layout.rs` (type beside
  `parent_slice_height`; construction in `fragment_block_subtree` after the
  locals at `:1863-1913`)

### Step 1: Add the type

```rust
/// The unchanging half of the "close the parent's fragment on the page it is
/// leaving" idiom, which appears at nine sites in `fragment_block_subtree`.
///
/// Only the page and the two y values vary across those sites; the parent's
/// identity, x and width do not, so they are captured once per call.
///
/// # Dedup policy
///
/// `RowState::emitted_parent_pages` exists so that N parallel flex / grid
/// cells, each independently deciding to close the parent on the current
/// page, emit only one parent fragment for it.
///
/// Today exactly two sites consult it — the unforced, overflow-driven ones —
/// and the seven forced `break-before` / `break-after: page` sites do not.
/// That asymmetry is a **defect**, pinned by the ignored test
/// `forced_break_does_not_close_a_grid_parent_twice_on_one_page` (fulgur-pgbrk
/// R8): a cell that crosses a page by recursion sets `crossed_by_recursion`,
/// which restores a same-row sibling to the row-start page, and that
/// sibling's forced break then closes the parent a second time on it.
///
/// The two methods below preserve today's behaviour exactly, so the defect is
/// reproducible rather than accidentally masked. Fixing it means switching the
/// forced sites to `close_unforced` and un-ignoring R8 — deliberately not done
/// here, because this refactor changes no output.
struct ParentSlice {
    id: usize,
    x_in_body: f32,
    width: f32,
    page_height_px: f32,
}

impl ParentSlice {
    /// Close the parent unconditionally. Used by the forced-break sites and
    /// by the function tail. See the dedup note on the type.
    fn close_forced(
        &self,
        geometry: &mut PaginationGeometryTable,
        page_index: u32,
        page_start_y: f32,
        cursor_y: f32,
    ) {
        geometry
            .entry(self.id)
            .or_default()
            .fragments
            .push(Fragment {
                page_index,
                x: self.x_in_body.as_px(),
                y: page_start_y.as_px(),
                width: self.width.as_px(),
                height: parent_slice_height(cursor_y, page_start_y, self.page_height_px).as_px(),
            });
    }

    /// Close the parent at most once per page across parallel flex / grid
    /// cells. Used by the two overflow-driven sites.
    fn close_unforced(
        &self,
        geometry: &mut PaginationGeometryTable,
        row_state: Option<&mut RowState>,
        page_index: u32,
        page_start_y: f32,
        cursor_y: f32,
    ) {
        let should_emit = row_state
            .map(|rs| rs.emitted_parent_pages.insert(page_index))
            .unwrap_or(true);
        if should_emit {
            self.close_forced(geometry, page_index, page_start_y, cursor_y);
        }
    }
}
```

### Step 2: Construct it

After `propagate_leading_break` (`:1913`):

```rust
let parent_slice = ParentSlice {
    id: parent_id,
    x_in_body: parent_x_in_body,
    width: parent_w,
    page_height_px,
};
```

### Step 3: Build

```bash
cargo build -p fulgur
```

Expected: compiles, with dead-code warnings for both methods. Commit nothing
yet.

---

## Task 4: Convert the nine sites, one at a time

Convert **one site per step**, running `cargo test -p fulgur --lib` after
each. Converting several at once makes a behaviour change impossible to
attribute.

Sites, in the order to do them (line numbers drift as you edit — re-grep with
`rg -n 'parent_slice_height\(cursor_y' crates/fulgur/src/pagination_layout.rs`
before each):

| # | line | trigger | method |
| --- | --- | --- | --- |
| 1 | `:2757` | function tail | `close_forced` |
| 2 | `:2710` | `break-after: page`, plain child | `close_forced` |
| 3 | `:2651` | block strip-overflow cut | `close_unforced` |
| 4 | `:2578` | `break-after: page`, after recursion | `close_forced` |
| 5 | `:2366` | `break-after: page`, after inline branch | `close_forced` |
| 6 | `:2275` | inline-root push-whole | `close_unforced` |
| 7 | `:2189` | `break-before: page` | `close_forced` |
| 8 | `:2144` | `break-after: page`, zero-height child | `close_forced` |
| 9 | `:2108` | `break-before: page`, zero-height child | `close_forced` |

The tail first because it is the simplest and outside the loop; the two
`close_unforced` sites in the middle so a mistake in the dedup wiring shows up
against a suite you have already re-run several times.

For a `close_forced` site, replace the whole `geometry.entry(parent_id)…push(…)`
expression with:

```rust
parent_slice.close_forced(geometry, page_index, page_start_y, cursor_y);
```

For a `close_unforced` site, replace the `let should_emit = …; if should_emit
{ … }` block — guard included — with:

```rust
parent_slice.close_unforced(
    geometry,
    row_state.as_mut(),
    page_index,
    page_start_y,
    cursor_y,
);
```

Leave the surrounding `if cursor_y > page_start_y {` guard and the page-advance
assignments exactly where they are.

**After each site:**

```bash
cargo test -p fulgur --lib
```

Expected: `2034 passed, 0 failed, 16 ignored`, every time.

**After all nine:**

```bash
cargo fmt -p fulgur && cargo clippy -p fulgur --all-targets
cargo test -p fulgur
rg -c 'geometry\s*$' crates/fulgur/src/pagination_layout.rs  # sanity: fewer builder chains
```

Expected: `2481 passed, 0 failed, 18 ignored`; clippy clean with no remaining
dead-code warning on either method.

### Commit

```bash
git add crates/fulgur/src/pagination_layout.rs
git commit -m "refactor(pagination): funnel nine parent-slice emissions through ParentSlice"
```

---

## Task 5: Confirm the two token-identical blocks collapsed

**Files:** none — this is a verification step.

The design predicted that sites 3 and 6 (`:2651`, `:2275`), previously 26
token-identical lines, reduce to five self-evident assignments each. Confirm:

```bash
rg -n -A 8 'parent_slice\.close_unforced' crates/fulgur/src/pagination_layout.rs
```

Both should now read as: the guard, one `close_unforced` call, then
`page_index += 1; cursor_y = 0.0; page_start_y = 0.0; page_taffy_origin =
this_top_in_parent; child_page_y = 0.0;`.

If either still carries a builder chain, a site was missed.

---

## Task 6: Final verification

```bash
cargo test -p fulgur
cargo test -p fulgur --lib -- --ignored 2>&1 | tail -3
cargo clippy -p fulgur --all-targets
cargo fmt --check
npx markdownlint-cli2 'docs/plans/2026-08-17-fulgur-pgbrk-break-decision-extraction-plan.md'
```

Expected:

- `2481 passed, 0 failed, 18 ignored` — identical to baseline.
- `--ignored`: `0 passed; 16 failed`, the same set as baseline, R8 among them.
- clippy and fmt clean.

### VRT

VRT shows 29 of 64 differing on macOS for environment reasons unrelated to this
work. Confirm this change did not move it, using the stash / re-run / diff
protocol:

```bash
cargo test -p fulgur-vrt 2>&1 | rg '^test .* FAILED' | sort > /tmp/vrt-after.txt
git stash
cargo test -p fulgur-vrt 2>&1 | rg '^test .* FAILED' | sort > /tmp/vrt-before.txt
git stash pop
diff /tmp/vrt-before.txt /tmp/vrt-after.txt && echo "VRT unmoved"
```

Expected: no diff. **Do not regenerate goldens on macOS.**

---

## Out of scope — do not do these here

- **Fixing R8.** Switching the forced sites to `close_unforced` changes output.
  It is a separate commit with its own reasoning, and it un-ignores a test.
- **`would_split_block_subtree` (`:1509`).** It is floor-blind and disagrees
  with the real walk for leading children. Folding it in changes which subtrees
  are recursed into. Moved to R4, which needs it anyway.
- **Extracting the page advance.** Three incompatible shapes; a mode enum would
  reintroduce the branching. See the design.
- **R2 / R6 / R4 / R5.** Unchanged in scope and order by this work.
