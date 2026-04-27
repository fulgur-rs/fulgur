# Relocate convert/ tests to per-category submodules — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or
> superpowers:subagent-driven-development) to implement this plan task-by-task.
> Beads issue: **fulgur-2dhp** (`bd show fulgur-2dhp`).

**Goal:** Phase 1 (fulgur-odgo) split `convert.rs` into per-category submodules
but kept all tests in `mod.rs` with `super::pseudo::*` / `super::replaced::*`
prefixes. Move each test home, then add new unit-level tests for three pure
helpers in `inline_root.rs` raised by coderabbit on PR #250.

**Architecture:** Two-phase commit-set.

- **Phase A — Relocation only.** Cut-and-paste tests from
  `convert/mod.rs::{tests,inline_box_extraction_tests}` into per-category
  `#[cfg(test)] mod tests` blocks. Strip `super::<module>::` prefixes that
  become redundant after the move; promote them to `super::super::<module>::`
  for cross-module references where needed. Keep shared helpers
  (`sample_png_arc`, `find_h1`, `find_node_by_tag`, `find_tag`,
  `find_marker_text_in_tree`, `make_ctx!` macro) in `mod.rs::tests`. Sibling
  submodule tests reference them as `super::super::tests::<helper>`. Test count
  and names remain byte-identical.

- **Phase B — Add new helper unit tests.** Three pure helpers in
  `inline_root.rs` (`metrics_from_line`, `recalculate_paragraph_line_boxes`,
  `resolve_enclosing_anchor`) get dedicated `#[cfg(test)]` coverage in their
  home module per CLAUDE.md rule "純関数 → 当該モジュールの `#[cfg(test)] mod
  tests`". Audit of other helpers (`size_raster_marker`, `resolve_inset_px`,
  `resolve_pseudo_size`) is **out of scope** for this issue — file as separate
  follow-ups if warranted.

**Tech Stack:** Rust 1.x, cargo, `crates/fulgur` workspace member.

**Invariants per phase:**

| Invariant | Phase A | Phase B |
|-----------|---------|---------|
| `cargo test -p fulgur --lib` passes | yes | yes |
| Test count unchanged | yes (43 → 43 in `convert::*`) | no (43 → 52+) |
| Test names unchanged | yes | adds new `*` names; existing untouched |
| VRT byte-identical | yes (test relocation cannot affect codegen) | yes |
| `cargo clippy --workspace -- -D warnings` clean | yes | yes |
| `cargo fmt --check` clean | yes | yes |

---

## Test inventory and target mapping

Total `convert::*` tests: **43** (verified by
`cargo test -p fulgur --lib 2>&1 | grep -E "^test convert::"`).

### From `convert::tests` (34 tests)

| Test | Target file |
|---|---|
| `test_make_image_pageable_both_dimensions` | `convert/replaced.rs::tests` |
| `test_make_image_pageable_width_only_uses_intrinsic_aspect` | `convert/replaced.rs::tests` |
| `test_make_image_pageable_height_only_uses_intrinsic_aspect` | `convert/replaced.rs::tests` |
| `test_make_image_pageable_intrinsic_fallback` | `convert/replaced.rs::tests` |
| `test_convert_content_url_normal_element` | `convert/replaced.rs::tests` |
| `test_convert_content_url_no_content_falls_through` | `convert/replaced.rs::tests` |
| `test_convert_content_url_missing_asset_falls_through` | `convert/replaced.rs::tests` |
| `test_build_pseudo_image_reads_content_url` | `convert/pseudo.rs::tests` |
| `test_build_pseudo_image_width_only_uses_intrinsic_aspect` | `convert/pseudo.rs::tests` |
| `test_build_pseudo_image_missing_asset_returns_none` | `convert/pseudo.rs::tests` |
| `test_build_pseudo_image_no_assets_returns_none` | `convert/pseudo.rs::tests` |
| `test_build_pseudo_image_height_percent_resolves_against_parent_height` | `convert/pseudo.rs::tests` |
| `test_build_inline_pseudo_image_returns_some_for_inline_pseudo` | `convert/pseudo.rs::tests` |
| `test_build_inline_pseudo_image_does_not_filter_display` | `convert/pseudo.rs::tests` |
| `test_dom_to_pageable_emits_block_pseudo_image` | `convert/pseudo.rs::tests` |
| `test_dom_to_pageable_inline_pseudo_ignored_phase1` | `convert/pseudo.rs::tests` |
| `test_dom_to_pageable_emits_pseudo_on_childless_element` | `convert/pseudo.rs::tests` |
| `test_dom_to_pageable_emits_pseudo_on_zero_size_block_leaf` | `convert/pseudo.rs::tests` |
| `test_dom_to_pageable_emits_pseudo_on_list_item_with_text` | `convert/pseudo.rs::tests` |
| `test_inject_before_shifts_existing_items` | `convert/list_marker.rs::tests` |
| `test_inject_after_appends_at_end` | `convert/list_marker.rs::tests` |
| `test_inject_both_before_and_after` | `convert/list_marker.rs::tests` |
| `paragraph_attaches_external_link_to_glyph_run_inside_anchor` | `convert/inline_root.rs::tests` |
| `paragraph_attaches_internal_link_for_fragment_href` | `convert/inline_root.rs::tests` |
| `paragraph_shares_arc_linkspan_across_glyph_runs_under_same_anchor` | `convert/inline_root.rs::tests` |
| `paragraph_leaves_link_none_for_anchor_without_href` | `convert/inline_root.rs::tests` |
| `paragraph_leaves_link_none_for_anchor_with_empty_href` | `convert/inline_root.rs::tests` |
| `paragraph_linkspan_alt_text_uses_anchor_text_content` | `convert/inline_root.rs::tests` |
| `inside_marker_on_block_child_li` | `convert/list_item.rs::tests` |
| `inside_marker_on_block_child_ol` | `convert/list_item.rs::tests` |
| `inside_marker_on_empty_li` | `convert/list_item.rs::tests` |
| `h1_wraps_block_with_bookmark_marker` | **stays in `mod.rs::tests`** (dispatcher-level: `BookmarkMarkerWrapperPageable` from `mod.rs::convert_node`) |
| `h3_produces_level_3_marker` | stays in `mod.rs::tests` |
| `orphan_bookmark_marker_survives_empty_element` | stays in `mod.rs::tests` (uses `emit_orphan_bookmark_marker` in `mod.rs`) |

### From `convert::unit_oracle_tests` (4 tests)

| Test | Target file |
|---|---|
| `width_100_percent_equals_content_width` | **stays in `mod.rs::unit_oracle_tests`** (dispatcher-level Taffy oracle via `dom_to_pageable`) |
| `width_10cm_is_283_46_pt` | stays in `mod.rs::unit_oracle_tests` |
| `width_1in_is_72_pt` | stays in `mod.rs::unit_oracle_tests` |
| `width_360px_is_270_pt` | stays in `mod.rs::unit_oracle_tests` |

### From `convert::inline_box_extraction_tests` (5 tests)

| Test | Target file |
|---|---|
| `inline_block_baseline_aligns_with_surrounding_text` | `convert/inline_root.rs::tests` (rename module to `inline_box_extraction_tests` inside, OR fold into single `mod tests`) |
| `inline_block_becomes_line_item_inline_box` | `convert/inline_root.rs::tests` |
| `inline_block_inner_id_is_registered_with_destination_registry` | `convert/inline_root.rs::tests` |
| `inline_block_with_block_child_has_block_content` | `convert/inline_root.rs::tests` |
| `inline_block_with_transform_preserves_wrapper` | `convert/inline_root.rs::tests` |

**Decision:** fold into a single `#[cfg(test)] mod tests` per file (no nested
sub-module). Cleaner to maintain.

### Final test count per file (Phase A)

| File | tests in `convert::*` | added by Phase A |
|---|---|---|
| `mod.rs` `mod tests` | 3 (h1/h3/orphan bookmark) | — |
| `mod.rs` `mod unit_oracle_tests` | 4 (unchanged) | — |
| `replaced.rs` | 7 | +7 |
| `pseudo.rs` | 12 | +12 |
| `list_marker.rs` | 3 | +3 |
| `inline_root.rs` | 11 (6 paragraph + 5 inline_box) | +11 |
| `list_item.rs` | 3 | +3 |
| **Total** | **43** | **+36 (== removed from mod.rs::tests)** |

### Shared helpers and macros

These stay in `mod.rs::tests` (top of the module):

- `sample_png_arc()` — produces a 1x1 PNG `Arc<DynamicImage>` for asset bundles
- `find_h1(...)` — DFS for first `<h1>` node id
- `find_node_by_tag(...)` — DFS for first node by tag name
- `find_tag(...)` — alternative DFS used by paragraph tests
- `find_marker_text_in_tree(...)` — recursive marker-text search
- `make_ctx!` macro — boilerplate `ConvertContext` builder

Sibling submodule tests reference them as
`super::super::tests::<helper>` (one level up to `convert/`, then into
`tests`). Macros are imported via `use super::super::tests::make_ctx;` (or the
macro is re-exported via `#[macro_use]` where needed).

Some of these helpers may end up unused inside `mod.rs::tests` after Phase A
(e.g. if all callers move out). In that case mark with `#[allow(dead_code)]`
or move to a `pub(super)` position; **do not** delete — they may be needed by
the bookmark tests still living in `mod.rs::tests`.

---

## Phase A — Relocation

### Task A0: Pre-flight verification

**Files:** none (read-only).

**Step 1: Confirm baseline test count**

Run: `cargo test -p fulgur --lib 2>&1 | grep -c "^test convert::"`
Expected: `43`

Run: `cargo test -p fulgur --lib 2>&1 | tail -5`
Expected: `test result: ok. 794 passed; 0 failed; ...`

**Step 2: Confirm working directory**

Run: `git -C /home/ubuntu/fulgur/.worktrees/relocate-convert-tests rev-parse --abbrev-ref HEAD`
Expected: `refactor/relocate-convert-tests`

Run: `pwd`
Expected: `/home/ubuntu/fulgur/.worktrees/relocate-convert-tests` (cd if not).

---

### Task A1: Move 7 tests to `convert/replaced.rs`

**Files:**

- Modify: `crates/fulgur/src/convert/replaced.rs` (append `#[cfg(test)] mod tests`)
- Modify: `crates/fulgur/src/convert/mod.rs` (remove the 7 tests from `mod tests`)

**Step 1: Read source bodies**

Run: `grep -n "fn test_make_image_pageable\|fn test_convert_content_url" crates/fulgur/src/convert/mod.rs`

Use `Read` to load each test body. Note any cross-module references
(e.g. `super::replaced::make_image_pageable` becomes `super::make_image_pageable`
because we're now inside `replaced.rs`).

**Step 2: Append `#[cfg(test)] mod tests` block to `convert/replaced.rs`**

Append at end of file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::sample_png_arc;
    use crate::asset::AssetBundle;
    // ... other test imports as needed (HashMap, etc.)

    #[test]
    fn test_make_image_pageable_both_dimensions() {
        // body — strip `super::replaced::` prefixes (now redundant)
    }
    // ... 6 more tests
}
```

**Step 3: Remove the 7 tests from `convert/mod.rs::tests`**

Delete the corresponding `fn test_*` blocks. Helper fns
(`sample_png_arc` etc.) stay.

**Step 4: Run lib tests**

Run: `cargo test -p fulgur --lib 2>&1 | tail -5`
Expected: `794 passed; 0 failed`

Run: `cargo test -p fulgur --lib 2>&1 | grep -c "^test convert::"`
Expected: `43`

**Step 5: Run clippy + fmt**

Run: `cargo clippy -p fulgur --all-targets -- -D warnings`
Expected: clean

Run: `cargo fmt --check -p fulgur`
Expected: clean

**Step 6: Commit**

```bash
git add crates/fulgur/src/convert/replaced.rs crates/fulgur/src/convert/mod.rs
git commit -m "refactor(convert): move replaced.rs tests into home module"
```

---

### Task A2: Move 12 tests to `convert/pseudo.rs`

Same shape as A1. Tests:

- `test_build_pseudo_image_reads_content_url`
- `test_build_pseudo_image_width_only_uses_intrinsic_aspect`
- `test_build_pseudo_image_missing_asset_returns_none`
- `test_build_pseudo_image_no_assets_returns_none`
- `test_build_pseudo_image_height_percent_resolves_against_parent_height`
- `test_build_inline_pseudo_image_returns_some_for_inline_pseudo`
- `test_build_inline_pseudo_image_does_not_filter_display`
- `test_dom_to_pageable_emits_block_pseudo_image`
- `test_dom_to_pageable_inline_pseudo_ignored_phase1`
- `test_dom_to_pageable_emits_pseudo_on_childless_element`
- `test_dom_to_pageable_emits_pseudo_on_zero_size_block_leaf`
- `test_dom_to_pageable_emits_pseudo_on_list_item_with_text`

**Cross-module ref note:** `dom_to_pageable` lives in `mod.rs`. From
`pseudo.rs::tests`, call as `super::super::dom_to_pageable(...)` or import
with `use super::super::dom_to_pageable;`. Likewise `ConvertContext` is in
`mod.rs` (struct definition) — `use super::super::ConvertContext;`.

Verify, clippy, fmt, commit:

```bash
git commit -m "refactor(convert): move pseudo.rs tests into home module"
```

---

### Task A3: Move 3 tests to `convert/list_marker.rs`

Tests:

- `test_inject_before_shifts_existing_items`
- `test_inject_after_appends_at_end`
- `test_inject_both_before_and_after`

These exercise `inject_inside_marker_item_into_children`. After move,
prefix becomes bare name.

Verify, clippy, fmt, commit:

```bash
git commit -m "refactor(convert): move list_marker.rs tests into home module"
```

---

### Task A4: Move 11 tests to `convert/inline_root.rs`

Two source groups merge into a single `mod tests`:

From `convert::tests` (6 tests):

- `paragraph_attaches_external_link_to_glyph_run_inside_anchor`
- `paragraph_attaches_internal_link_for_fragment_href`
- `paragraph_shares_arc_linkspan_across_glyph_runs_under_same_anchor`
- `paragraph_leaves_link_none_for_anchor_without_href`
- `paragraph_leaves_link_none_for_anchor_with_empty_href`
- `paragraph_linkspan_alt_text_uses_anchor_text_content`

From `convert::inline_box_extraction_tests` (5 tests):

- `inline_block_baseline_aligns_with_surrounding_text`
- `inline_block_becomes_line_item_inline_box`
- `inline_block_inner_id_is_registered_with_destination_registry`
- `inline_block_with_block_child_has_block_content`
- `inline_block_with_transform_preserves_wrapper`

**Cross-module ref notes:**

- `find_tag(...)` helper is local to `mod.rs::tests` — use
  `super::super::tests::find_tag` OR copy the small helper into
  `inline_root.rs::tests` (decide based on uses elsewhere; if only
  paragraph tests use it, copy to keep coupling local).
- `make_ctx!` macro — re-export with `#[macro_use]` from the parent or
  inline its expansion in tests that need it.
- `dom_to_pageable` lives in `mod.rs` — `super::super::dom_to_pageable`.

**Step 1: Append combined `#[cfg(test)] mod tests` to `inline_root.rs`**

**Step 2: Remove the 6 paragraph_ tests from `mod.rs::tests`**

**Step 3: Remove the entire `mod inline_box_extraction_tests` from `mod.rs`**

**Step 4-7: verify, clippy, fmt, commit**

```bash
git commit -m "refactor(convert): move inline_root.rs tests into home module"
```

---

### Task A5: Move 3 tests to `convert/list_item.rs`

Tests:

- `inside_marker_on_block_child_li`
- `inside_marker_on_block_child_ol`
- `inside_marker_on_empty_li`

These exercise `dom_to_pageable` end-to-end with `list-style-position:inside`,
verifying the list-item dispatcher path that uses
`inject_inside_marker_item_into_children` (`list_marker.rs`) under the hood.

**Cross-module refs:**

- `dom_to_pageable`, `ConvertContext` — `super::super::*`
- `find_marker_text_in_tree` helper — `super::super::tests::find_marker_text_in_tree`

Verify, clippy, fmt, commit:

```bash
git commit -m "refactor(convert): move list_item.rs tests into home module"
```

---

### Task A6: Verify final state of `mod.rs::tests`

**Files:**

- Modify: `crates/fulgur/src/convert/mod.rs` (cleanup only)

**Step 1: Confirm what remains in `mod tests`**

Run: `grep -E "^    fn (h1_|h3_|orphan|paragraph_|inside_|test_|inline_block)" crates/fulgur/src/convert/mod.rs`

Expected to match only: `h1_wraps_block_with_bookmark_marker`,
`h3_produces_level_3_marker`, `orphan_bookmark_marker_survives_empty_element`
(plus shared helpers, which are not `#[test]`).

**Step 2: Remove the now-removed `mod inline_box_extraction_tests`**

If still present, delete entirely.

**Step 3: Re-pub helpers as `pub(super)` so siblings can reach them**

Make these accessible from sibling submodule tests:

```rust
#[cfg(test)]
mod tests {
    // ... existing imports ...

    pub(super) fn sample_png_arc() -> ... { ... }
    pub(super) fn find_h1(...) -> ... { ... }
    pub(super) fn find_node_by_tag(...) -> ... { ... }
    pub(super) fn find_tag(...) -> ... { ... }
    pub(super) fn find_marker_text_in_tree(...) -> ... { ... }
    // make_ctx! — keep as macro, accessible via #[macro_use] or scoped use

    // 3 bookmark tests follow
}
```

**Step 4: Confirm test count and naming unchanged**

Run: `cargo test -p fulgur --lib 2>&1 | grep -c "^test convert::"`
Expected: `43`

Run: `cargo test -p fulgur --lib 2>&1 | tail -5`
Expected: `794 passed`

**Step 5: Run VRT (byte-identical guard)**

Run: `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -10`
Expected: all VRT tests pass with byte-identical goldens.

**Step 6: Run clippy on workspace**

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean

**Step 7: Final commit if anything cleaned up**

```bash
git commit -m "refactor(convert): finalize mod.rs::tests as dispatcher-only"
```

---

## Phase B — New helper unit tests

### Task B1: Add tests for `metrics_from_line` (3 cases)

**Files:**

- Modify: `crates/fulgur/src/convert/inline_root.rs::tests`

Coverage from coderabbit PR #250 review:

1. `metrics_fallback_when_line_has_no_text_items` — pass a `ShapedLine` with
   only `Image`/`InlineBox` items (no `Text`). Expect fallback metrics
   (default ascent/descent values produced by the helper's no-text branch).
2. `metrics_picks_text_metrics_in_mixed_line` — `ShapedLine` with
   `Text` + `Image` + `InlineBox`. Expect ascent/descent driven by the
   text item's font (skrifa-derived).
3. `metrics_skrifa_ascent_matches_font` — single `Text` item with a known
   font; assert returned `LineFontMetrics.ascent`/`descent` match the
   skrifa lookup for that font (use a bundled `AssetBundle` font).

**Implementation steps per test:**

1. Construct `ShapedLine` directly (the test doesn't go through
   layout — these are pure-fn tests). May require a small fixture builder.
2. Call `metrics_from_line(&line)`.
3. Assert the returned `LineFontMetrics` fields.

**Verify + commit:**

```bash
cargo test -p fulgur --lib 2>&1 | grep -E "metrics_from_line|metrics_fallback|metrics_picks|metrics_skrifa"
git commit -m "test(convert): add unit tests for inline_root::metrics_from_line"
```

---

### Task B2: Add tests for `recalculate_paragraph_line_boxes` (3 cases)

**Files:**

- Modify: `crates/fulgur/src/convert/inline_root.rs::tests`

Cases:

1. `recalculate_rebases_multiple_lines` — three lines with known heights;
   assert each line's `y` after recalculation equals the cumulative offset.
2. `recalculate_handles_mixed_font_sizes` — lines with different font sizes;
   assert baselines respect each line's metrics rather than a global one.
3. `recalculate_empty_paragraph_is_noop` — pass `&mut []`; assert no panic
   and no side-effect.

**Implementation:** build `Vec<ShapedLine>` fixtures, call
`recalculate_paragraph_line_boxes(&mut v)`, inspect `line.line_box.y`/height.

**Verify + commit:**

```bash
cargo test -p fulgur --lib 2>&1 | grep "recalculate_"
git commit -m "test(convert): add unit tests for inline_root::recalculate_paragraph_line_boxes"
```

---

### Task B3: Add tests for `resolve_enclosing_anchor` (3 cases)

**Files:**

- Modify: `crates/fulgur/src/convert/inline_root.rs::tests`

Cases:

1. `resolve_anchor_returns_none_when_no_ancestor_anchor` — `<p>text</p>`
   with no `<a>` ancestor. Expect `None`.
2. `resolve_anchor_walks_past_intermediate_ancestors` — `<a><span><b>text</b></span></a>`,
   query `<b>`. Expect `Some(...)` resolving to the outer `<a href>`.
3. `resolve_anchor_distinguishes_external_and_fragment` — two parallel
   subtrees: one with `href="https://example.com"`, one with `href="#sec"`.
   Assert the resolved `LinkSpan`'s link kind matches the href form
   (external vs internal-fragment).

**Implementation:** parse small HTML via `blitz_adapter::parse_and_layout`,
locate the inline-leaf node id, call `resolve_enclosing_anchor(doc, node_id, ...)`.

**Verify + commit:**

```bash
cargo test -p fulgur --lib 2>&1 | grep "resolve_anchor_"
git commit -m "test(convert): add unit tests for inline_root::resolve_enclosing_anchor"
```

---

## Final verification

After all tasks:

**Step 1: Full lib test suite**

Run: `cargo test -p fulgur --lib 2>&1 | tail -5`
Expected: `(794 + 9) = 803 passed` (or whatever the Phase B addition totals).

**Step 2: VRT**

Run: `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt`
Expected: all goldens byte-identical.

**Step 3: Workspace clippy + fmt**

Run: `cargo clippy --workspace -- -D warnings`
Run: `cargo fmt --check`
Both expected: clean.

**Step 4: Markdownlint on the plan doc**

Run: `npx markdownlint-cli2 docs/plans/2026-04-27-relocate-convert-tests.md`
Expected: clean.

**Step 5: Push branch + open PR**

Coordinate with user — auto mode does not push or open PRs without
confirmation.

---

## Out of scope

- Audit of additional pure helpers (`size_raster_marker`,
  `resolve_inset_px`, `resolve_pseudo_size`) — file separate beads issues
  if warranted.
- Phase 2 (`extract_block_style` decomposition — fulgur-zcmt).
- Renaming or rewriting test bodies during relocation (cut-and-paste only
  in Phase A).
