# convert/style/ Phase 2 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Issue:** fulgur-zcmt — Refactor convert.rs extract_block_style into per-property files (Phase 2)

**Goal:** Split `extract_block_style` (~245 lines, `crates/fulgur/src/convert/mod.rs:610`) into per-property `convert/style/<prop>.rs` modules so that adding a new CSS property is local to one small file instead of editing a single monolithic function.

**Architecture:** Keep `BlockStyle` (defined in `pageable.rs`) unchanged. Introduce `convert/style/` with a thin `mod.rs` that calls `apply_to(...)` on each per-property module in order. Use a small `StyleContext` bundle for the inner-styles modules (`background` / `border` / `shadow` / `overflow`) to avoid recomputing `current_color` and re-borrowing `final_layout` in each module. `box_metrics` takes only the layout (no styles needed), and `extract_opacity_visible` lives in `opacity.rs` but is still a free function called by callers outside `extract_block_style`.

**Tech Stack:** Rust 2024, Stylo via blitz-dom, Taffy layout, no external new deps.

**Output contract:** **VRT goldens MUST stay byte-identical** at every commit. This is *the* load-bearing constraint — the refactor moves code, it does not change behaviour.

---

## Deviations from the bd issue description

The issue (`bd show fulgur-zcmt`) was a sketch authored before auditing the current code. We deviate in three places. The plan is written to the *code as it stands*, not the issue text. We do not edit the bd issue description; deviations are explicit here.

| Issue text | Reality | Plan |
|---|---|---|
| Move all 11 bg helpers (incl. `absolute_to_rgba`) to `background.rs` | `absolute_to_rgba` is also used by `shadow` + `border` | Keep `absolute_to_rgba` in `style/mod.rs` as `pub(super) fn`, move only the 10 bg-specific helpers to `background.rs` (also `resolve_conic_gradient` which the issue text missed) |
| Add `transform.rs` | `BlockStyle` has no transform field. Transform is handled by `maybe_wrap_transform` (`convert/mod.rs:315`) which wraps a `Pageable`, not a `BlockStyle` field | **Drop `transform.rs` from this phase.** `maybe_wrap_transform` stays where it is |
| Modules: `box_metrics / border / background / shadow / transform / opacity` | `extract_block_style` also fills `overflow_x` / `overflow_y`, which is missing from the issue list | **Add `overflow.rs`.** Same shape as the others |
| Uniform 5-param `apply_to` signature on every module | `box_metrics` does not need `styles` / `current_color` / `assets`; `overflow` does not need `current_color` / `layout` / `assets` | **Use a small `StyleContext` bundle for the inner-styles modules; `box_metrics` keeps a thin `&Layout`-only signature.** Better than passing unused params |

---

## Final target shape

```text
crates/fulgur/src/convert/
  mod.rs          // unchanged dispatcher; just imports `extract_block_style` from style/
  style/
    mod.rs        // BlockStyle assembly + StyleContext + absolute_to_rgba (shared)
    box_metrics.rs   // border_widths + padding (layout only)
    border.rs        // border_color + border_radii + border_styles
    background.rs    // background_color + background_layers (+ all bg/gradient helpers)
    shadow.rs        // box_shadows
    overflow.rs      // overflow_x / overflow_y
    opacity.rs       // extract_opacity_visible (free fn — callers call directly)
```

`StyleContext` (in `style/mod.rs`):

```rust
pub(super) struct StyleContext<'a> {
    pub styles: &'a blitz_dom::node::Style,            // alias for Stylo ComputedStyles
    pub current_color: &'a style::color::AbsoluteColor,
    pub layout: &'a taffy::Layout,
    pub assets: Option<&'a crate::asset::AssetBundle>,
}
```

`mod.rs` assembles in order, **preserving exactly the field-write order from the original** so a side-by-side diff is trivial:

```rust
pub(super) fn extract_block_style(
    node: &blitz_dom::Node,
    assets: Option<&AssetBundle>,
) -> BlockStyle {
    let layout = node.final_layout;
    let mut style = BlockStyle::default();
    box_metrics::apply_to(&mut style, &layout);
    if let Some(styles) = node.primary_styles() {
        let current_color = styles.clone_color();
        let ctx = StyleContext { styles: &styles, current_color: &current_color, layout: &layout, assets };
        background_color::apply_to(&mut style, &ctx); // bg color
        border::apply_to(&mut style, &ctx);
        shadow::apply_to(&mut style, &ctx);
        // border_styles is folded into border::apply_to in the same step
        overflow::apply_to(&mut style, &ctx);
        background::apply_to(&mut style, &ctx);       // bg layers (image / gradient) — last, matches original
    }
    style
}
```

**Note:** the original code also performs the `border_widths`/`padding` initialisation in the struct literal *before* the `if let Some(styles)` block. We preserve the same temporal order: `box_metrics` runs first, then the styles-gated block. There is no behavioural difference because `BlockStyle::default()` is identical to the original initialiser, but keeping the move surgical aids review.

---

## Execution discipline

Each task follows the same six steps:

1. **Move:** create / edit the target files exactly as described.
2. **Build:** `cargo build -p fulgur` — must compile with no warnings introduced by this task.
3. **Lib tests:** `cargo test -p fulgur --lib` — must pass.
4. **Crate-local clippy:** `cargo clippy -p fulgur --lib --all-targets -- -D warnings` — must be clean. (Workspace-wide clippy stays in Task 8; the per-task crate-local check is fast — ~3s — and prevents lint debt accumulating across Tasks 4–6 from per-task fixes-on-top.)
5. **VRT byte-identical:** `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt` — must pass without `FULGUR_VRT_UPDATE=1`. *If this fails, stop and diagnose before moving on — do not continue stacking commits.*
6. **Commit:** one commit per task. Conventional Commits style, `refactor(convert):` scope.

`cargo clippy --workspace -- -D warnings` and `cargo fmt --check` run **once** at the end (Task 8) — not per task — to keep iteration tight.

We do **not** write new failing tests per task. The existing lib test suite + VRT goldens are the contract; this is a "move code, byte-identical out" refactor and the failing-test-first discipline does not buy anything here.

---

## Task 1: Skeleton — relocate `extract_block_style` into `convert/style/mod.rs` *with no extraction*

This is the insurance task. It moves the function — and its private helpers — verbatim into a new submodule, with **zero behavioural change**. If VRT regresses here, the *only* possible cause is import / visibility plumbing, not refactoring.

**Files:**

- Create: `crates/fulgur/src/convert/style/mod.rs`
- Modify: `crates/fulgur/src/convert/mod.rs`

**Step 1.1 — Create `convert/style/mod.rs` with the verbatim function bodies**

Move the following items from `convert/mod.rs` into `convert/style/mod.rs`, **byte-for-byte**:

- `fn extract_block_style` (lines 610–853 in mod.rs)
- `fn extract_opacity_visible` (lines 855–865)
- `fn absolute_to_rgba` (line 874)
- `fn resolve_linear_gradient` (line 908)
- `fn resolve_color_stops` (line 985)
- `fn resolve_radial_gradient` (line 1056)
- `fn resolve_conic_gradient` (line 1137)
- `fn map_extent` (line 1217)
- `fn convert_bg_size` (line 1231)
- `fn convert_lp_to_bg` (line 1262)
- `fn convert_bg_position` (line 1284)
- `fn convert_bg_repeat` (line 1294)
- `fn convert_bg_origin` (line 1309)
- `fn convert_bg_clip` (line 1321)
- `fn try_convert_lp_to_bg` (helper that lives between `convert_lp_to_bg` and `convert_bg_position`; sole callers are `resolve_radial_gradient` / `resolve_conic_gradient`, both moved — leaving it behind would orphan it as `dead_code`)

In `style/mod.rs`, make every moved function `pub(super)` so siblings inside `convert/` (block.rs / list_item.rs / inline_root.rs / pseudo.rs / positioned.rs) can keep calling them.

Keep `extract_asset_name` in `convert/mod.rs` — it has unrelated callers; do not move it.

**Step 1.2 — Wire `style` module from `convert/mod.rs`**

In `convert/mod.rs`:

- Add `mod style;` next to the other `mod` lines (around line 30).
- Replace each of the 14 moved function definitions with re-imports at the top of `mod.rs`:

```rust
use style::{extract_block_style, extract_opacity_visible};
```

(The other 12 helpers are pure submodule internals; they should not leak back to `mod.rs`.)

Also re-import any types only the moved functions used (`BgImageContent`, `BgSize`, etc.) inside `style/mod.rs` — do **not** double-import them in `convert/mod.rs` if no remaining user there needs them. Run `cargo build -p fulgur` and let unused-import warnings drive cleanup.

**Step 1.3 — Build / test / VRT / commit**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
```

Expected: all pass, VRT byte-identical (no diff PNGs written).

```bash
git add crates/fulgur/src/convert/mod.rs crates/fulgur/src/convert/style/mod.rs
git commit -m "refactor(convert): move extract_block_style to convert/style/ submodule"
```

---

## Task 2: Extract `box_metrics.rs`

The simplest extraction — uses `layout` only.

**Files:**

- Create: `crates/fulgur/src/convert/style/box_metrics.rs`
- Modify: `crates/fulgur/src/convert/style/mod.rs`

**Step 2.1 — Create `box_metrics.rs`**

```rust
//! border-width and padding extraction (layout-only).

use crate::convert::px_to_pt;
use crate::pageable::BlockStyle;

pub(super) fn apply_to(style: &mut BlockStyle, layout: &taffy::Layout) {
    style.border_widths = [
        px_to_pt(layout.border.top),
        px_to_pt(layout.border.right),
        px_to_pt(layout.border.bottom),
        px_to_pt(layout.border.left),
    ];
    style.padding = [
        px_to_pt(layout.padding.top),
        px_to_pt(layout.padding.right),
        px_to_pt(layout.padding.bottom),
        px_to_pt(layout.padding.left),
    ];
}
```

**Step 2.2 — Rewire `extract_block_style` in `style/mod.rs`**

Remove the `border_widths` / `padding` fields from the `BlockStyle { ... }` struct literal — they now default to `[0.0; 4]`. Insert the apply_to call right after `BlockStyle::default()`:

```rust
let layout = node.final_layout;
let mut style = BlockStyle::default();
box_metrics::apply_to(&mut style, &layout);
// ... remaining code unchanged
```

Add `mod box_metrics;` at the top of `style/mod.rs`.

**Step 2.3 — Build / test / VRT / commit**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt

git add crates/fulgur/src/convert/style/
git commit -m "refactor(convert): extract box_metrics from extract_block_style"
```

---

## Task 3: Extract `overflow.rs`

Pure styles-driven, no `current_color` / `layout` / `assets` needed. We still pass `&StyleContext` for signature uniformity across the inner-styles modules — `current_color` / `layout` / `assets` are unused but the consistency is worth more than the avoided wart.

**Files:**

- Create: `crates/fulgur/src/convert/style/overflow.rs`
- Modify: `crates/fulgur/src/convert/style/mod.rs`

**Step 3.1 — First introduce `StyleContext` in `style/mod.rs`**

Add the struct definition near the top of `style/mod.rs`:

```rust
pub(super) struct StyleContext<'a> {
    pub styles: &'a blitz_dom::node::Style,
    pub current_color: &'a style::color::AbsoluteColor,
    pub layout: &'a taffy::Layout,
    pub assets: Option<&'a crate::asset::AssetBundle>,
}
```

(Verify the actual primary-styles type name — `blitz_dom::node::Style` is the ergonomic name; if Stylo's `Arc<ComputedValues>` surfaces directly, use that. `cargo build` will tell you.)

**Step 3.2 — Create `overflow.rs`**

```rust
//! overflow-x / overflow-y extraction (styles → axis-independent Clip / Visible).

use crate::pageable::{BlockStyle, Overflow};
use super::StyleContext;

pub(super) fn apply_to(style: &mut BlockStyle, ctx: &StyleContext<'_>) {
    let map = |o: ::style::values::computed::Overflow| -> Overflow {
        use ::style::values::computed::Overflow as S;
        match o {
            S::Visible => Overflow::Visible,
            S::Hidden | S::Clip | S::Scroll | S::Auto => Overflow::Clip,
        }
    };
    style.overflow_x = map(ctx.styles.clone_overflow_x());
    style.overflow_y = map(ctx.styles.clone_overflow_y());
}
```

(Note the `::style::...` prefix — Stylo's crate is `style`, which collides with our submodule name. Use the leading `::` to disambiguate.)

**Step 3.3 — Rewire**

Inside `extract_block_style`'s `if let Some(styles)` block, after computing `current_color` and building `ctx`, replace the existing overflow block with `overflow::apply_to(&mut style, &ctx);`. Add `mod overflow;` at the top.

**Step 3.4 — Build / test / VRT / commit**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt

git add crates/fulgur/src/convert/style/
git commit -m "refactor(convert): extract overflow from extract_block_style"
```

---

## Task 4: Extract `shadow.rs`

Uses `current_color`. Calls `absolute_to_rgba` (which lives in `style/mod.rs` — accessed as `super::absolute_to_rgba`).

**Files:**

- Create: `crates/fulgur/src/convert/style/shadow.rs`
- Modify: `crates/fulgur/src/convert/style/mod.rs`

**Step 4.1 — Create `shadow.rs`**

Move the **box-shadow loop** from `extract_block_style` byte-for-byte into `shadow::apply_to`. The loop iterates `styles.clone_box_shadow().0` and pushes to `style.box_shadows`. **Do not touch the loop body's structure** — preserving order and skip-conditions is what keeps VRT byte-identical.

```rust
//! box-shadow extraction.

use crate::convert::px_to_pt;
use crate::pageable::{BlockStyle, BoxShadow};
use super::{StyleContext, absolute_to_rgba};

pub(super) fn apply_to(style: &mut BlockStyle, ctx: &StyleContext<'_>) {
    let shadow_list = ctx.styles.clone_box_shadow();
    for shadow in shadow_list.0.iter() {
        if shadow.inset {
            log::warn!("box-shadow: inset is not yet supported; skipping");
            continue;
        }
        let blur_px = shadow.base.blur.px();
        if blur_px > 0.0 {
            log::warn!(
                "box-shadow: blur-radius > 0 is not yet supported; \
                 drawing as blur=0 (blur={}px)",
                blur_px
            );
        }
        let rgba = absolute_to_rgba(shadow.base.color.resolve_to_absolute(ctx.current_color));
        if rgba[3] == 0 {
            continue;
        }
        style.box_shadows.push(BoxShadow {
            offset_x: px_to_pt(shadow.base.horizontal.px()),
            offset_y: px_to_pt(shadow.base.vertical.px()),
            blur: px_to_pt(blur_px),
            spread: px_to_pt(shadow.spread.px()),
            color: rgba,
            inset: false,
        });
    }
}
```

**Step 4.2 — Rewire**

In `style/mod.rs`, replace the inline box-shadow block in `extract_block_style` with `shadow::apply_to(&mut style, &ctx);`. Add `mod shadow;`.

**Step 4.3 — Build / test / VRT / commit**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt

git add crates/fulgur/src/convert/style/
git commit -m "refactor(convert): extract shadow from extract_block_style"
```

---

## Task 5: Extract `border.rs`

Combines `border_color` + `border_radii` + `border_styles`. `border_radii` resolution uses CSS-px basis (`layout.size.width/height`) — this is the load-bearing px/pt boundary. **Do not refactor the closure** — move `resolve_radius` and `convert_border_style` verbatim.

**Files:**

- Create: `crates/fulgur/src/convert/style/border.rs`
- Modify: `crates/fulgur/src/convert/style/mod.rs`

**Step 5.1 — Create `border.rs`**

```rust
//! border-color, border-radius, border-style extraction.

use crate::convert::px_to_pt;
use crate::pageable::{BlockStyle, BorderStyleValue};
use super::{StyleContext, absolute_to_rgba};

pub(super) fn apply_to(style: &mut BlockStyle, ctx: &StyleContext<'_>) {
    let bc = ctx.styles.clone_border_top_color();
    style.border_color = absolute_to_rgba(bc.resolve_to_absolute(ctx.current_color));

    // Stylo evaluates length-percentage values in CSS px; basis must be CSS px,
    // result converted to pt via px_to_pt. See .claude/rules/coordinate-system.md.
    let width = ctx.layout.size.width;
    let height = ctx.layout.size.height;
    let resolve_radius =
        |r: &::style::values::computed::length_percentage::NonNegativeLengthPercentage,
         basis: f32|
         -> f32 {
            px_to_pt(r.0.resolve(::style::values::computed::Length::new(basis)).px())
        };

    let tl = ctx.styles.clone_border_top_left_radius();
    let tr = ctx.styles.clone_border_top_right_radius();
    let br = ctx.styles.clone_border_bottom_right_radius();
    let bl = ctx.styles.clone_border_bottom_left_radius();
    style.border_radii = [
        [resolve_radius(&tl.0.width, width), resolve_radius(&tl.0.height, height)],
        [resolve_radius(&tr.0.width, width), resolve_radius(&tr.0.height, height)],
        [resolve_radius(&br.0.width, width), resolve_radius(&br.0.height, height)],
        [resolve_radius(&bl.0.width, width), resolve_radius(&bl.0.height, height)],
    ];

    let convert_border_style = |bs: ::style::values::specified::BorderStyle| -> BorderStyleValue {
        use ::style::values::specified::BorderStyle as BS;
        match bs {
            BS::None | BS::Hidden => BorderStyleValue::None,
            BS::Dashed => BorderStyleValue::Dashed,
            BS::Dotted => BorderStyleValue::Dotted,
            BS::Double => BorderStyleValue::Double,
            BS::Groove => BorderStyleValue::Groove,
            BS::Ridge => BorderStyleValue::Ridge,
            BS::Inset => BorderStyleValue::Inset,
            BS::Outset => BorderStyleValue::Outset,
            BS::Solid => BorderStyleValue::Solid,
        }
    };
    style.border_styles = [
        convert_border_style(ctx.styles.clone_border_top_style()),
        convert_border_style(ctx.styles.clone_border_right_style()),
        convert_border_style(ctx.styles.clone_border_bottom_style()),
        convert_border_style(ctx.styles.clone_border_left_style()),
    ];
}
```

**Step 5.2 — Rewire**

Replace the three blocks (`border_color`, `border_radii`, `border_styles`) in `extract_block_style` with `border::apply_to(&mut style, &ctx);`. Add `mod border;`.

**Step 5.3 — Build / test / VRT / commit**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt

git add crates/fulgur/src/convert/style/
git commit -m "refactor(convert): extract border from extract_block_style"
```

---

## Task 6: Extract `background.rs` — the largest move

Carries all 11 helpers (10 bg-specific + `resolve_conic_gradient`). `absolute_to_rgba` stays in `style/mod.rs` (called via `super::absolute_to_rgba`).

**Note:** `absolute_to_rgba` is currently re-exported back to `convert/mod.rs` because `get_text_color` and `get_text_decoration` (which live in `convert/mod.rs`, outside this refactor's scope) call it. After this task lands, the re-export remains; a follow-up may relocate `get_text_color` / `get_text_decoration` so the re-export goes away, but that is out of scope for this PR.

**Files:**

- Create: `crates/fulgur/src/convert/style/background.rs`
- Modify: `crates/fulgur/src/convert/style/mod.rs`

**Step 6.1 — Create `background.rs`**

Move the following from `style/mod.rs` into `background.rs`:

- the **bg-color** block (~6 lines: `let bg = clone_background_color(); let bg_rgba = ...`)
- the **bg-image layers** loop (the entire `let bg_images = ...` block, including the `if has_real_bg_image` guard)
- the 11 helpers: `resolve_linear_gradient`, `resolve_color_stops`, `resolve_radial_gradient`, `resolve_conic_gradient`, `map_extent`, `convert_bg_size`, `convert_lp_to_bg`, `convert_bg_position`, `convert_bg_repeat`, `convert_bg_origin`, `convert_bg_clip`
- `extract_asset_name` is **not** moved — it stays in `convert/mod.rs` (other callers exist; verify with `grep -rn "extract_asset_name" crates/fulgur/src/`).

Public surface — only `pub(super) fn apply_to` is exposed; helpers are private to `background.rs`.

```rust
//! background-color and background-image-layers extraction.

use std::sync::Arc;
use crate::asset::AssetBundle;
use crate::convert::extract_asset_name; // borrowed from parent
use crate::image::{AssetKind, ImagePageable};
use crate::pageable::{
    BackgroundLayer, BgBox, BgClip, BgImageContent, BgLengthPercentage, BgRepeat, BgSize, BlockStyle,
};
use super::{StyleContext, absolute_to_rgba};

pub(super) fn apply_to(style: &mut BlockStyle, ctx: &StyleContext<'_>) {
    // bg-color
    let bg = ctx.styles.clone_background_color();
    let bg_rgba = absolute_to_rgba(bg.resolve_to_absolute(ctx.current_color));
    if bg_rgba[3] > 0 {
        style.background_color = Some(bg_rgba);
    }

    // bg-image layers — preserve loop body byte-for-byte
    let bg_images = ctx.styles.clone_background_image();
    let has_real_bg_image = bg_images
        .0
        .iter()
        .any(|i| !matches!(i, ::style::values::computed::image::Image::None));
    if has_real_bg_image {
        let bg_sizes = ctx.styles.clone_background_size();
        let bg_pos_x = ctx.styles.clone_background_position_x();
        let bg_pos_y = ctx.styles.clone_background_position_y();
        let bg_repeats = ctx.styles.clone_background_repeat();
        let bg_origins = ctx.styles.clone_background_origin();
        let bg_clips = ctx.styles.clone_background_clip();

        for (i, image) in bg_images.0.iter().enumerate() {
            // ... move resolved-tuple match block verbatim; ctx.assets, ctx.current_color
            //     replace `assets` and `current_color` references
        }
    }
}

// 11 private helpers below — moved verbatim from style/mod.rs
fn absolute_to_rgba_unused() {} // (placeholder so editor finds; remove)
// resolve_linear_gradient, resolve_color_stops, resolve_radial_gradient,
// resolve_conic_gradient, map_extent, convert_bg_size, convert_lp_to_bg,
// convert_bg_position, convert_bg_repeat, convert_bg_origin, convert_bg_clip
```

**Critical:** when moving the bg-image loop body, replace `assets` with `ctx.assets` and `current_color` with `ctx.current_color`. **Do not change the structure**; do not collapse `match` arms; do not reorder pushes. The order of `style.background_layers.push(...)` must match the original.

**Step 6.2 — Rewire**

In `style/mod.rs`:

- Remove the bg-color block and the entire bg-image-layers block from `extract_block_style`.
- Insert `background::apply_to(&mut style, &ctx);` at the appropriate position. *Place it last in the inner-styles assembly* — the original code runs the bg-image loop after all other style extractions, so we keep the same temporal order.
- Remove the 11 helpers (now in `background.rs`).
- Add `mod background;`.

**Step 6.3 — Build / test / VRT / commit**

This is the move most likely to break VRT. Run the full sequence:

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
```

If VRT regresses: do **not** continue. Diff `git show HEAD` against the working tree, find the byte-shift cause (likely a reordered `push`, a missing `f32` cast, or a `current_color` substitution typo). Fix in place. Do not commit until VRT is byte-identical.

```bash
git add crates/fulgur/src/convert/style/
git commit -m "refactor(convert): extract background from extract_block_style"
```

---

## Task 7: Extract `opacity.rs`

`extract_opacity_visible` is a free function called directly by `block.rs`, `list_item.rs`, `inline_root.rs`, `pseudo.rs`. It is **not** called from `extract_block_style`. The move is a relocation + import-site update.

**Files:**

- Create: `crates/fulgur/src/convert/style/opacity.rs`
- Modify: `crates/fulgur/src/convert/style/mod.rs`
- Modify: every caller's import (block / list_item / inline_root / pseudo, plus any other hits from `grep`)

**Step 7.1 — Create `opacity.rs`**

```rust
//! opacity + visibility extraction.
//!
//! Free function — called directly by block / list_item / inline_root / pseudo,
//! not from extract_block_style.

use blitz_dom::Node;

pub(crate) fn extract_opacity_visible(node: &Node) -> (f32, bool) {
    use ::style::properties::longhands::visibility::computed_value::T as Visibility;
    node.primary_styles()
        .map(|s| {
            let opacity = s.clone_opacity();
            let v = s.clone_visibility();
            let visible = v != Visibility::Hidden && v != Visibility::Collapse;
            (opacity, visible)
        })
        .unwrap_or((1.0, true))
}
```

Visibility is `pub(crate)` (not `pub(super)`) because it crosses module boundaries — `convert/block.rs` is a sibling of `convert/style/`, not a child.

**Step 7.2 — Re-export from `convert/mod.rs`**

In `convert/mod.rs`, replace the existing `use style::extract_opacity_visible;` (added in Task 1) with `pub(super) use style::opacity::extract_opacity_visible;` so siblings see the same name they always did.

Actually — since callers already use `extract_opacity_visible` unqualified, and they live in sibling modules of `style`, the cleanest route is a re-export at `convert/mod.rs`:

```rust
pub(super) use self::style::opacity::extract_opacity_visible;
```

That way no caller changes its import.

Verify no caller said `crate::convert::extract_opacity_visible` (which would still resolve via the re-export). `grep -rn "extract_opacity_visible" crates/fulgur/src/`.

**Step 7.3 — Remove from `style/mod.rs`**

Delete the in-line definition in `style/mod.rs`. Add `pub(super) mod opacity;` at the top.

**Step 7.4 — Build / test / VRT / commit**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt

git add crates/fulgur/src/convert/
git commit -m "refactor(convert): extract opacity into convert/style/opacity.rs"
```

---

## Task 8: Final lint + workspace verification

**Files:** none (read-only verification)

**Step 8.1 — Workspace-wide checks**

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo test -p fulgur
cargo test -p fulgur --test gcpm_integration
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
```

**Step 8.2 — Per-task line-count sanity check**

Verify the split shape:

```bash
wc -l crates/fulgur/src/convert/style/*.rs
wc -l crates/fulgur/src/convert/mod.rs
```

Expected rough shape:

- `style/mod.rs` ≈ 50–100 lines (extract_block_style scaffold + StyleContext + absolute_to_rgba + module declarations)
- `style/box_metrics.rs` ≈ 20 lines
- `style/border.rs` ≈ 70 lines
- `style/background.rs` ≈ 600 lines (the bulk of the original — gradient helpers dominate)
- `style/shadow.rs` ≈ 40 lines
- `style/overflow.rs` ≈ 20 lines
- `style/opacity.rs` ≈ 15 lines
- `convert/mod.rs` should drop by ~700 lines (was 1806; expect ~1100)

If `convert/mod.rs` did not shrink by roughly that much, helpers were left behind — go back and audit.

**Step 8.3 — Acceptance criteria recheck (matches bd issue)**

- [ ] `cargo test -p fulgur` all pass
- [ ] VRT goldens byte-identical (no `goldens/` modifications staged)
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo fmt --check` clean
- [ ] Adding a new CSS property is local to one file under `convert/style/`

**Step 8.4 — No commit** — Task 8 is verification-only. If anything fails, fix in a follow-up commit on the same task scope.

---

## Out of scope (Phase 3 candidates, do **not** start in this PR)

- `list_marker` / pseudo / abs-position helper module splits beyond what Phase 1 already did (issue text already lists this as out of scope)
- Adding a transform field to `BlockStyle` to fold `maybe_wrap_transform` into the style pipeline (would need a separate design discussion)
- `convert.rs` ↔ `blitz_adapter.rs` boundary tightening (tracked separately as fulgur-x92a)

---

## Rollback strategy

One commit per task. If a task fails verification:

1. **Within the task:** fix in place; do not commit until green.
2. **After commit, regression discovered downstream:** `git revert <task-N-commit>` (the per-task granularity is the whole point).
