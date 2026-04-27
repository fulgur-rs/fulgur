# convert.rs Module Split (Phase 1) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Split `crates/fulgur/src/convert.rs` (5705 lines, 86 fns) into a `convert/` directory by node category so parallel WPT branches stop conflicting on a single file. Phase 1 covers the dispatcher (`convert_node_inner`) only — `extract_block_style` is Phase 2 (fulgur-zcmt).

**Architecture:** Pure refactor with **zero behavioral change** target. `convert.rs` becomes `convert/mod.rs` keeping the public boundary (`px_to_pt`, `pt_to_px`, `dom_to_pageable`, `ConvertContext`) unchanged. Per-category submodules (`list_marker`, `pseudo`, `positioned`, `replaced`, `table`, `list_item`, `inline_root`, `block`) host their respective fns. Dispatcher rewrites to `try_convert` chain so adding a new category is +1 file +1 line in mod.rs.

**Tech Stack:** Rust 2024 edition, `cargo` workspaces, VRT byte-comparison via `crates/fulgur-vrt`.

**Beads:** fulgur-odgo (Phase 1 — this plan). Blocks fulgur-zcmt (Phase 2).

**Branch:** `refactor/convert-module-split` (worktree at `.worktrees/convert-module-split`).

---

## Pre-Implementation Baseline (already done)

- Worktree built clean (`cargo check -p fulgur` OK on `9b982ea`).
- convert.rs survey complete: 86 fns, public boundary = `pub(crate) fn px_to_pt/pt_to_px`, `pub fn dom_to_pageable`, `pub struct ConvertContext`.
- External callers (`engine.rs`, `render.rs`, `paragraph.rs`, `blitz_adapter.rs`, `background.rs`, `pageable.rs`) reach convert via these 4 symbols only — re-export from `mod.rs` keeps them untouched.

---

## Verification Commands (run after each task)

```bash
# from worktree root
cargo build -p fulgur                                      # must compile
cargo test -p fulgur                                       # unit + integration (gcpm_integration etc.)
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt                               # MUST be byte-identical
cargo clippy -p fulgur -- -D warnings                      # clean
```

`cargo test -p fulgur` (without `--lib`) is required so that `tests/gcpm_integration` and any other integration tests that exercise the convert path through gcpm are run on every move — not just at Task 12.

The VRT job is the regression backstop — if any helper visibility / call ordering subtly drifted, golden bytes diverge.

> **Line numbers drift.** Source line citations below (e.g. `~3581`) are correct against the baseline `9b982ea`. After Task 3 they will shift. From Task 4 onward, **do not trust the cited line numbers** — re-find each fn at task start with e.g. `rg -n '^fn resolve_inside_image_marker' crates/fulgur/src/convert/mod.rs`.

**Commit message convention** (matches repo style):

```text
refactor(convert): <description>
```

Frequent commits — one per task — let us bisect any VRT drift to a single move.

---

## Task 1: Convert convert.rs to convert/mod.rs (no-op move)

**Files:**

- Move: `crates/fulgur/src/convert.rs` → `crates/fulgur/src/convert/mod.rs`

**Step 1: Move file via git**

```bash
mkdir crates/fulgur/src/convert
git mv crates/fulgur/src/convert.rs crates/fulgur/src/convert/mod.rs
```

**Step 2: Verify build still works (no source change yet)**

```bash
cargo build -p fulgur
```

Expected: success. Rust auto-resolves `mod convert;` in `lib.rs` to either `convert.rs` OR `convert/mod.rs`.

**Step 3: Run full check suite**

```bash
cargo test -p fulgur --lib
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
cargo clippy -p fulgur -- -D warnings
```

Expected: all pass, VRT byte-identical (we changed zero source bytes).

**Step 4: Commit**

```bash
git add -A
git commit -m "refactor(convert): rename convert.rs to convert/mod.rs"
```

---

## Task 2: Declare empty submodules

**Files:**

- Modify: `crates/fulgur/src/convert/mod.rs` (top of file, add module declarations)
- Create: `crates/fulgur/src/convert/list_marker.rs` (empty)
- Create: `crates/fulgur/src/convert/pseudo.rs` (empty)
- Create: `crates/fulgur/src/convert/positioned.rs` (empty)
- Create: `crates/fulgur/src/convert/replaced.rs` (empty)
- Create: `crates/fulgur/src/convert/table.rs` (empty)
- Create: `crates/fulgur/src/convert/list_item.rs` (empty)
- Create: `crates/fulgur/src/convert/inline_root.rs` (empty)
- Create: `crates/fulgur/src/convert/block.rs` (empty)

**Step 1: Create empty submodule files**

```bash
touch crates/fulgur/src/convert/{list_marker,pseudo,positioned,replaced,table,list_item,inline_root,block}.rs
```

**Step 2: Add `mod` declarations after the existing `use` block in `convert/mod.rs`**

Insert immediately after the last `use` (around line 27 — `use crate::MAX_DOM_DEPTH;`):

```rust
mod block;
mod inline_root;
mod list_item;
mod list_marker;
mod positioned;
mod pseudo;
mod replaced;
mod table;
```

**Step 3: Verify**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
cargo clippy -p fulgur -- -D warnings
```

Empty modules are harmless. Expect everything green, VRT byte-identical.

**Step 4: Commit**

```bash
git add crates/fulgur/src/convert/
git commit -m "refactor(convert): declare empty per-category submodules"
```

---

## Per-Module Extraction Pattern (Tasks 3–10)

Every extraction task follows the same 6-step recipe. Read this once; the per-task entries below give only the source line ranges and any task-specific notes.

**Recipe per extraction:**

1. **Identify** the line range of the fns to move from `convert/mod.rs`.
2. **Cut** those fns into `convert/<target>.rs`. At the top of `<target>.rs`, add `use super::*;` (this gives the moved fns access to all imports + sibling helpers via `super::`). If specific external imports are needed beyond what `super::*` reaches, copy those `use` lines too.
3. **Bump visibility** in the target file: any fn that mod.rs (or another sibling) still calls becomes `pub(super) fn`. Items used only inside the same target file stay `fn`. (Audit after the cut by running `cargo build` — every "unresolved name" pointing into the moved set tells you which to mark `pub(super)`.)
4. **Update call sites in mod.rs** to qualify with the module: `<target>::fn_name(…)`. Same for any sibling target file that calls the moved fn (rare in early extractions; common as we go).
5. **Run the verification suite.** Build, lib tests, VRT byte-identical, clippy clean. **Any VRT golden mismatch = stop and bisect.** A subtle visibility change can re-monomorphize a generic and reorder a side effect; the byte test catches this.
6. **Commit** with `refactor(convert): extract <module> module`.

**Key invariants:**

- **No body edits during a move.** Cut → paste → adjust visibility/imports only. Resist any "while I'm here" cleanup.
- `pub(super)` is the right visibility for "called by mod.rs or sibling submodule" — never bump to `pub(crate)` unless the symbol was already `pub(crate)` before the move.
- If a moved fn uses a private helper that stays in mod.rs, mark that helper `pub(super) fn` in mod.rs.

---

## Task 3: Extract list_marker module

**Source lines (`convert/mod.rs`):**

- `resolve_list_style_image_asset` ~3581
- `size_raster_marker` ~3605
- `resolve_list_marker` ~3628
- `resolve_inside_image_marker` ~3676
- `extract_marker_lines` ~3718
- `find_marker_font` ~3821
- `shape_marker_with_skrifa` ~3902
- `inject_inside_marker_item_into_children` ~3954

**Target:** `crates/fulgur/src/convert/list_marker.rs`

**Notes:** This is a leaf — only mod.rs (and later list_item.rs) calls these. Apply the recipe.

Commit: `refactor(convert): extract list_marker module`.

---

## Task 4: Extract pseudo module

**Source lines (`convert/mod.rs`):**

- `is_block_pseudo` ~1477
- `is_pseudo_node` ~1523
- `wrap_with_pseudo_content` ~1908
- `node_has_block_pseudo_image` ~1935
- `node_has_inline_pseudo_image` ~1954
- `node_has_absolute_pseudo` ~1987
- `build_block_pseudo_images` ~2052
- `wrap_with_block_pseudo_images` ~2086
- `build_inline_pseudo_image` ~2117
- `attach_link_to_inline_image` ~2161
- `inject_inline_pseudo_images` ~2175
- `build_pseudo_image` ~1446
- `effective_pseudo_size_px` ~1882
- `resolve_pseudo_size` ~2305

**Target:** `crates/fulgur/src/convert/pseudo.rs`

**Notes:** Many siblings will need this; `pub(super)` everything that mod.rs / abs / inline_root / block calls. `build_pseudo_image` is also referenced by absolute-pseudo helpers — those move in Task 5, so for now from positioned helpers it stays `super::pseudo::build_pseudo_image`.

Commit: `refactor(convert): extract pseudo module`.

---

## Task 5: Extract positioned module

**Source lines (`convert/mod.rs`):**

- `is_absolutely_positioned` ~1489
- `is_position_fixed` ~1498
- `is_position_static` ~1506
- `cb_padding_box` ~1556
- `resolve_cb_for_absolute` ~1589
- `resolve_inset_px` ~1637
- `build_absolute_pseudo_children` ~1670
- `build_absolute_pseudo_child` ~1811
- `try_build_absolute_pseudo_image` ~1834
- `collect_positioned_children` ~1137

**Target:** `crates/fulgur/src/convert/positioned.rs`

**Notes:** `collect_positioned_children` is the largest item (~140 lines) and the public-ish API used by container / block paths. Mark it `pub(super)`. The `build_absolute_pseudo_*` helpers reach into `pseudo::build_pseudo_image` — already extracted in Task 4, so cross-module call.

Commit: `refactor(convert): extract positioned module`.

---

## Task 6: Extract replaced module

**Source lines (`convert/mod.rs`):**

- `resolve_image_dimensions` ~1396
- `make_image_pageable` ~1414
- `wrap_replaced_in_block_style` ~1317
- `convert_content_url` ~2335
- `convert_image` ~2358
- `convert_svg` ~2390

**Target:** `crates/fulgur/src/convert/replaced.rs`

**Notes:** These are called from `convert_node_inner` — keep them `pub(super)` so the dispatcher in mod.rs can reach them. `wrap_replaced_in_block_style` is generic over `F`; the move preserves the bound.

**Add `try_convert` entry point in this task** (don't defer to Task 11). Wrap the dispatcher's image / SVG / `content: url` branches into:

```rust
pub(super) fn try_convert(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut super::ConvertContext<'_>,
) -> Option<Box<dyn crate::pageable::Pageable>> {
    // image / svg / content:url match — preserve exact precedence from convert_node_inner
    …
}
```

mod.rs's `convert_node_inner` after this task calls `if let Some(p) = replaced::try_convert(doc, node_id, ctx) { return p; }` for those branches.

Commit: `refactor(convert): extract replaced module with try_convert entry`.

---

## Task 7: Extract table module

**Source lines (`convert/mod.rs`):**

- `convert_table` ~2412
- `is_table_section` ~2464
- `collect_table_cells` ~2473

**Target:** `crates/fulgur/src/convert/table.rs`

**Notes:** Self-contained.

**Add `try_convert` entry point in this task** (don't defer to Task 11). Wrap the dispatcher's table branch into:

```rust
pub(super) fn try_convert(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut super::ConvertContext<'_>,
) -> Option<Box<dyn crate::pageable::Pageable>> {
    // match `display: table*` / table-row / table-cell / etc. — preserve exact precedence
    …
}
```

mod.rs's `convert_node_inner` after this task calls `if let Some(p) = table::try_convert(doc, node_id, ctx) { return p; }`.

Commit: `refactor(convert): extract table module with try_convert entry`.

---

## Task 8: Extract list_item module

**Source lines (`convert/mod.rs`):**

- `build_list_item_body` ~444
- The 3 list-item branches inside `convert_node_inner` (~610–~736 area, the first three early-return blocks). Move them into a `list_item::try_convert` (this is the one piece of structure work in this task — see "Notes" below).

**Target:** `crates/fulgur/src/convert/list_item.rs`

**Notes:** This is the first task that does more than pure-cut. The three list-item branches in `convert_node_inner` need to become a single `pub(super) fn try_convert(doc, node_id, ctx, depth) -> Option<Box<dyn Pageable>>` that returns `Some(...)` for any of the three matched cases and `None` for fallthrough. mod.rs's dispatcher calls `if let Some(p) = list_item::try_convert(…) { return p; }` for all three at once.

Be careful: the three branches have **distinct guards** that combine into one matcher — preserve their order exactly, since list-item logic depends on the precedence.

Run VRT after this task with extra scrutiny — this is the highest-risk move in the plan.

Commit: `refactor(convert): extract list_item module with try_convert entry`.

---

## Task 9: Extract inline_root module

**Source lines (`convert/mod.rs`):**

- The inline-root branch inside `convert_node_inner` (~lines around 800–1000)
- `convert_inline_box_node` ~2655
- `extract_paragraph` ~2685
- `metrics_from_line` ~2221
- `recalculate_paragraph_line_boxes` ~2270
- `resolve_enclosing_anchor` ~2565

**Target:** `crates/fulgur/src/convert/inline_root.rs`

**Notes:** Same pattern as Task 8 — extract the inline-root branch into `pub(super) fn try_convert`. Helpers `extract_paragraph`, `convert_inline_box_node`, etc. become `pub(super)` so other targets (notably tests in pageable.rs / paragraph.rs reach via `crate::convert::ConvertContext` — they don't touch these directly).

Commit: `refactor(convert): extract inline_root module with try_convert entry`.

---

## Task 10: Extract block module

**Source lines (`convert/mod.rs`):**

- The remaining branches of `convert_node_inner` (childless + container fallback)
- `compute_content_box` ~2020 (used by block path)
- `is_non_visual_element` ~3565

**Target:** `crates/fulgur/src/convert/block.rs`

**Notes:** Final categorical extraction. Convert remainder of `convert_node_inner` into `pub(super) fn convert(doc, node_id, ctx, depth) -> Box<dyn Pageable>` (always returns — no `Option`).

Commit: `refactor(convert): extract block module with convert entry`.

---

## Task 11: Replace `convert_node_inner` with try_convert dispatch chain

**Files:**

- Modify: `crates/fulgur/src/convert/mod.rs:610-…` (`convert_node_inner` body)

After Tasks 6/7/8/9/10, the body of `convert_node_inner` should already be just early-return calls into each module's `try_convert` plus a final `block::convert`. Now collapse to the canonical dispatcher shape from the design:

**Step 1: Rewrite the dispatcher**

```rust
fn convert_node_inner(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    if let Some(p) = list_item::try_convert(doc, node_id, ctx, depth) {
        return p;
    }
    if let Some(p) = table::try_convert(doc, node_id, ctx) {
        return p;
    }
    if let Some(p) = replaced::try_convert(doc, node_id, ctx) {
        return p;
    }
    if let Some(p) = inline_root::try_convert(doc, node_id, ctx, depth) {
        return p;
    }
    block::convert(doc, node_id, ctx, depth)
}
```

All five modules (`list_item`, `table`, `replaced`, `inline_root`, `block`) should already expose their `try_convert` / `convert` entry points from Tasks 6–10. **Preserve the current branch precedence** (list_item → table → replaced → inline_root → block).

**Step 2: Verify**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
cargo clippy -p fulgur -- -D warnings
```

Highest-risk verification of the plan. Any VRT byte mismatch here means the dispatch order or guard semantics shifted — bisect against Task 10.

**Step 3: Commit**

```bash
git add crates/fulgur/src/convert/
git commit -m "refactor(convert): collapse dispatcher to try_convert chain"
```

---

## Task 12: Final pass — fmt, clippy, line-count sanity

**Step 1: Format**

```bash
cargo fmt
```

**Step 2: Run the full project check (CI-equivalent)**

```bash
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test -p fulgur
cargo test -p fulgur-cli
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
```

**Step 3: Sanity check the split**

```bash
wc -l crates/fulgur/src/convert/*.rs
```

Expected: `mod.rs` should drop from 5705 lines to roughly 1500–2500. The 8 categorical files together hold the rest. No single file should exceed `mod.rs`.

**Residue checklist** — `convert/mod.rs` after Phase 1 should still contain (deliberately, not bugs):

- **Entry points:** `convert_node`, `convert_node_inner` (now the try_convert chain), `dom_to_pageable`
- **Public API (re-exported by `mod convert`):** `pub struct ConvertContext`, `pub(crate) fn px_to_pt`, `pub(crate) fn pt_to_px`
- **Shared helpers used by multiple submodules:** `layout_in_pt`, `size_in_pt`, `is_pseudo_node`, `compute_content_box`
- **Glue:** `maybe_prepend_string_set`, `maybe_prepend_counter_ops`, `maybe_wrap_multicol_rule`, `maybe_wrap_transform`, `emit_orphan_string_set_markers`, `emit_counter_op_markers`, `emit_orphan_bookmark_marker`, `take_running_marker`, `debug_print_tree`
- **Phase 2 territory (intentionally untouched in this PR — they live in mod.rs until fulgur-zcmt lands):** `extract_block_style`, `extract_opacity_visible`, `absolute_to_rgba`, `resolve_linear_gradient`, `resolve_radial_gradient`, `resolve_conic_gradient`, `resolve_color_stops`, `map_extent`, `convert_bg_size` / `convert_bg_position` / `convert_bg_repeat` / `convert_bg_origin` / `convert_bg_clip` / `convert_lp_to_bg` / `try_convert_lp_to_bg`, `extract_block_id`, `extract_pagination_from_column_css`, `get_text_color`, `get_text_decoration`, `extract_asset_name`, `wrap_replaced_in_block_style` (if not pulled into `replaced.rs`)

If something on the **shared / glue / Phase 2** lists is _missing_, that's a refactor bug — restore it. If something _not_ on these lists remains and isn't an obvious omission, audit before committing.

**Step 4: Commit any final fmt fixups**

```bash
git add -A
git diff --cached --stat
git commit -m "refactor(convert): rustfmt fixups after module split" || echo "nothing to commit"
```

---

## Out of Scope (Phase 2 — fulgur-zcmt)

`extract_block_style` decomposition into `convert/style/<prop>.rs`. Tracked separately. Phase 1 keeps `extract_block_style` as a single fn in `convert/mod.rs`.

## Risks Recap

- **VRT golden drift.** Mitigation: byte-identical VRT after every task, commit per task for bisect.
- **WPT branch rebases.** Mitigation: this work fronts during a low-WIP window. The branch rename `convert.rs` → `convert/mod.rs` is the load-bearing rebase pain point — tell anyone with an in-flight WPT branch to rebase past Task 1 before touching their per-property code.
- **Visibility creep.** Mitigation: only `pub(super)`, never `pub(crate)`. Any newly-`pub(crate)` symbol after the refactor is a regression of encapsulation.
