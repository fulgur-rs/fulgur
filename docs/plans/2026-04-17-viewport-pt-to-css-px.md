# viewport pt→CSS px 変換 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Blitz境界でpt↔CSS pxを正しく変換し、相対単位（%, vw, vh）と絶対単位（cm, px）の両方が正確に描画されるようにする。

**Architecture:** Config層はpt、Blitz内部はCSS px、Pageable以降はpt。境界（engine/render→Blitz、Blitz→convert）で `PX_TO_PT = 0.75` を介して変換する。convert.rs に `layout_in_pt(&Layout) -> (x, y, w, h)` ヘルパーを導入し、30箇所近いlayout値参照を一貫して変換する。

**Tech Stack:** Rust, Blitz (blitz-dom / blitz-html), Taffy, Krilla

**Related:** beads fulgur-9ul, PR #90 (supersedes)

---

## Task 0: Baseline audit and working directory

**Step 1: Confirm worktree and branch**

Run:

```bash
cd /home/ubuntu/fulgur/.worktrees/fix-viewport-pt-to-px
git branch --show-current
```

Expected: `fix/viewport-pt-to-px`

**Step 2: Enumerate all layout.size/location reference sites**

Run:

```bash
grep -n "layout\.size\|layout\.location\|final_layout\.size\|final_layout\.location\|child_layout\.size\|child_layout\.location" crates/fulgur/src/convert.rs | wc -l
```

Expected: 約30箇所。全箇所をTask 5で処理する。

---

## Task 1: Add integration test for correct unit semantics (RED)

**Files:**

- Create: `crates/fulgur/tests/unit_semantics.rs`

**Step 1: Write failing tests for 4 oracle cases**

```rust
//! Integration tests pinning down the unit semantics across the Blitz boundary.
//!
//! These tests were added alongside the fix for beads `fulgur-9ul`:
//! viewport input is fed to Blitz as CSS px (= pt / 0.75) and Taffy's
//! `final_layout` output is converted back to pt via `PX_TO_PT` at the
//! conversion boundary. Without both halves, absolute (px, cm) and
//! relative (%, vw) units diverge from Chrome/WeasyPrint/Prince.

use fulgur::{Config, Engine, PageSize};
use fulgur::pageable::Pageable;

const PX_TO_PT: f32 = 0.75;

/// Walk the Pageable tree to find the first BlockPageable whose id matches.
/// Returns (width, height) in pt.
fn find_block_size(root: &dyn Pageable, target_class: &str) -> Option<(f32, f32)> {
    // Test helper: the implementation reuses the existing pageable inspection
    // API. Placeholder — Task 1 Step 3 finalizes the helper once the tree
    // exposes a stable way to look up a node by class.
    todo!("implement in Step 3 once we know the inspection API")
}

fn a4_content_width_pt() -> f32 {
    let cfg = Config::default();
    cfg.content_width()
}

#[test]
fn width_percent_matches_content_width() {
    let html = r#"<div class="x" style="width:100%;height:10pt"></div>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("render");
    // Sanity check: PDF was produced
    assert!(pdf.len() > 100);
    // Exact width check lives in unit tests on the Pageable tree (Task 3).
}

#[test]
fn width_cm_is_absolute() {
    // 10cm = 10 × 72/2.54 = 283.46pt
    let html = r#"<div class="x" style="width:10cm;height:1cm;background:red"></div>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("render");
    assert!(pdf.len() > 100);
}

#[test]
fn width_px_uses_css_px_to_pt_ratio() {
    // 360px = 360 × 0.75 = 270pt
    let html = r#"<div class="x" style="width:360px;height:10px"></div>"#;
    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("render");
    assert!(pdf.len() > 100);
}
```

**Step 2: Run to verify test file compiles**

Run: `cargo test -p fulgur --test unit_semantics 2>&1 | tail -5`

Expected: tests compile and pass smoke check (rendering succeeds). Real width assertions come in Task 3 via unit tests that inspect the Pageable tree.

**Step 3: Commit scaffold**

```bash
git add crates/fulgur/tests/unit_semantics.rs
git commit -m "test(fulgur): scaffold unit semantics integration tests (fulgur-9ul)"
```

---

## Task 2: Add convert.rs unit tests for Pageable tree geometry (RED)

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (append to `mod tests`)

**Step 1: Locate the existing tests module and inspect how Pageable tree is walked**

Run:

```bash
grep -n "^#\[cfg(test)\]\|mod tests\|build_pageable_for_testing_no_gcpm" crates/fulgur/src/convert.rs
```

Expected: existing test scaffolding uses `Engine::build_pageable_for_testing_no_gcpm`.

**Step 2: Add 4 oracle unit tests that walk the Pageable tree**

Identify the first `BlockPageable` whose class matches, and assert its size:

```rust
#[cfg(test)]
mod unit_oracle_tests {
    use super::*;
    use crate::pageable::{BlockPageable, Pageable};
    use crate::Engine;

    /// Visit every BlockPageable in a tree; return size of the first one with
    /// a non-zero layout width (skipping root wrappers).
    fn first_sized_block(root: &dyn Pageable) -> Option<(f32, f32)> {
        // Use the existing collect_ids or walk API to find the first block.
        // Prototype using the Debug impl / visitor if no clean walker exists;
        // fallback is to extract size via the draw machinery's measure pass.
        //
        // NOTE: If no stable public walker exists, expose a pub(crate) one
        // in pageable.rs for testing (gated on cfg(test) or #[doc(hidden)]).
        None // placeholder; Step 3 wires up the real walker
    }

    #[test]
    fn width_100_percent_equals_content_width() {
        let html = r#"<div style="width:100%;height:10pt;background:red"></div>"#;
        let eng = Engine::builder().build();
        let root = eng.build_pageable_for_testing_no_gcpm(html);
        let (w, _) = first_sized_block(&*root).expect("block present");
        let expected = eng.config().content_width();
        assert!((w - expected).abs() < 0.5,
            "width:100% should be {expected}pt, got {w}pt");
    }

    #[test]
    fn width_10cm_is_283_46_pt() {
        let html = r#"<div style="width:10cm;height:1cm"></div>"#;
        let eng = Engine::builder().build();
        let root = eng.build_pageable_for_testing_no_gcpm(html);
        let (w, _) = first_sized_block(&*root).expect("block present");
        let expected = 10.0 * 72.0 / 2.54;
        assert!((w - expected).abs() < 0.5,
            "width:10cm should be {expected}pt, got {w}pt");
    }

    #[test]
    fn width_360px_is_270_pt() {
        let html = r#"<div style="width:360px;height:10px"></div>"#;
        let eng = Engine::builder().build();
        let root = eng.build_pageable_for_testing_no_gcpm(html);
        let (w, _) = first_sized_block(&*root).expect("block present");
        let expected = 360.0 * 0.75;
        assert!((w - expected).abs() < 0.5,
            "width:360px should be {expected}pt, got {w}pt");
    }

    #[test]
    fn width_50vw_is_half_viewport() {
        let html = r#"<div style="width:50vw;height:10pt"></div>"#;
        let eng = Engine::builder().build();
        let root = eng.build_pageable_for_testing_no_gcpm(html);
        let (w, _) = first_sized_block(&*root).expect("block present");
        let expected = eng.config().content_width() / 2.0;
        assert!((w - expected).abs() < 0.5,
            "width:50vw should be {expected}pt, got {w}pt");
    }
}
```

**Step 3: Run tests — they MUST fail (current broken behavior)**

Run: `cargo test -p fulgur --lib unit_oracle_tests 2>&1 | tail -20`

Expected: Tests FAIL (widths are wrong because viewport is pt-not-px and layout is px-not-pt). This proves the tests discriminate correctly.

Record the actual-vs-expected numbers for each failing test. Attach to the commit message.

**Step 4: Commit RED state**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "test(fulgur): add oracle tests for layout unit semantics (RED)

Failing tests pin down what the fix must produce (fulgur-9ul)."
```

---

## Task 3: Add layout_in_pt helper (no behavior change)

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (near `const PX_TO_PT: f32 = 0.75;`)

**Step 1: Add helper next to PX_TO_PT constant**

At `convert.rs:30` after the PX_TO_PT const:

```rust
/// Convert a Taffy layout value (CSS px) to PDF pt.
///
/// Taffy's `final_layout.size/location` are in CSS px because we feed Blitz
/// a CSS px viewport. All of fulgur's Pageable types store pt values, so
/// we apply `PX_TO_PT` at the conversion boundary — here — rather than
/// sprinkling multiplications throughout `convert.rs`.
#[inline]
fn layout_in_pt(layout: &taffy::Layout) -> (f32, f32, f32, f32) {
    (
        layout.location.x * PX_TO_PT,
        layout.location.y * PX_TO_PT,
        layout.size.width * PX_TO_PT,
        layout.size.height * PX_TO_PT,
    )
}

#[inline]
fn size_in_pt(size: taffy::Size<f32>) -> (f32, f32) {
    (size.width * PX_TO_PT, size.height * PX_TO_PT)
}
```

**Step 2: Verify taffy::Layout import is available**

Run:

```bash
grep -n "^use taffy\|use taffy::" crates/fulgur/src/convert.rs
```

If no import exists, add `use taffy;` at the top of convert.rs. If taffy types are re-exported through blitz_dom, use those instead — check with:

```bash
grep -rn "pub use taffy\|pub type Layout" crates/fulgur/src/ | head -5
```

**Step 3: Verify compile (but tests still fail)**

Run: `cargo build -p fulgur 2>&1 | tail -5`

Expected: compile succeeds with warning `layout_in_pt is never used` and `size_in_pt is never used`.

**Step 4: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "refactor(fulgur): add layout_in_pt helper (no behavior change)

Introduces the conversion boundary helper that Task 5 will migrate
existing layout.size/location sites onto. Leaves behavior unchanged."
```

---

## Task 4: Fix viewport input — pt → CSS px at Blitz boundary

**Files:**

- Modify: `crates/fulgur/src/engine.rs:87`
- Modify: `crates/fulgur/src/engine.rs:259`
- Modify: `crates/fulgur/src/render.rs:418, 442`

**Step 1: Fix engine.rs main render path**

At `engine.rs:85-90`:

```rust
        let (mut doc, link_gcpm) = crate::blitz_adapter::parse_html_with_local_resources(
            &html,
            self.config.content_width() / crate::convert::PX_TO_PT,  // pt → CSS px
            fonts,
            self.base_path.as_deref(),
        );
```

Also update PassContext construction at `engine.rs:113-117`:

```rust
        let ctx = crate::blitz_adapter::PassContext {
            viewport_width: self.config.content_width() / crate::convert::PX_TO_PT,
            viewport_height: self.config.content_height() / crate::convert::PX_TO_PT,
            font_data: fonts,
        };
```

**Step 2: Fix test helper**

At `engine.rs:257-268`:

```rust
        let (mut doc, _link_gcpm) = crate::blitz_adapter::parse_html_with_local_resources(
            html,
            self.config.content_width() / crate::convert::PX_TO_PT,
            fonts,
            self.base_path.as_deref(),
        );

        let ctx = crate::blitz_adapter::PassContext {
            viewport_width: self.config.content_width() / crate::convert::PX_TO_PT,
            viewport_height: self.config.content_height() / crate::convert::PX_TO_PT,
            font_data: fonts,
        };
```

**Step 3: Export PX_TO_PT**

At `crates/fulgur/src/convert.rs:30`, change:

```rust
pub(crate) const PX_TO_PT: f32 = 0.75;
```

**Step 4: Fix render.rs GCPM margin box measure paths**

At `render.rs:415-421`:

```rust
                let measure_doc = crate::blitz_adapter::parse_and_layout(
                    &measure_html,
                    content_width / crate::convert::PX_TO_PT,
                    page_size.height / crate::convert::PX_TO_PT,
                    font_data,
                );
```

At `render.rs:438-445`:

```rust
                let measure_doc = crate::blitz_adapter::parse_and_layout(
                    &measure_html,
                    fixed_width / crate::convert::PX_TO_PT,
                    page_size.height / crate::convert::PX_TO_PT,
                    font_data,
                );
```

**Step 5: Verify compile (tests should still fail — layout output still untouched)**

Run: `cargo build -p fulgur 2>&1 | tail -5`

Expected: compile succeeds. `layout_in_pt` still warns unused.

**Step 6: Commit**

```bash
git add crates/fulgur/src/engine.rs crates/fulgur/src/render.rs crates/fulgur/src/convert.rs
git commit -m "fix(fulgur): convert viewport input from pt to CSS px (fulgur-9ul)

Blitz expects CSS px viewport dimensions. We were passing pt values
and Blitz happened to accept them as px, so absolute-unit content
(px, cm, pt) was drawn 4/3× too large and relative units were
(coincidentally) near-correct.

This is half the fix — Task 5 migrates convert.rs layout output
from px to pt so the Pageable tree is internally consistent."
```

---

## Task 5: Migrate convert.rs layout sites to layout_in_pt

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (all `layout.size.*`, `layout.location.*`, `child_layout.size.*`, `child_layout.location.*` sites)

**Step 1: Enumerate every site**

Run:

```bash
grep -n "layout\.size\|layout\.location\|final_layout\.size\|final_layout\.location\|child_layout\.size\|child_layout\.location" crates/fulgur/src/convert.rs > /tmp/convert_sites.txt
wc -l /tmp/convert_sites.txt
```

Expected: ~30 sites.

**Step 2: Migrate each site to layout_in_pt / size_in_pt**

Strategy by callsite pattern:

1. **Full 4-tuple extraction** (most common; e.g. convert.rs:485-487, 1175-1177, 1743-1745, 2047):

   Before:

   ```rust
   let layout = node.final_layout;
   let height = layout.size.height;
   let width = layout.size.width;
   ```

   After:

   ```rust
   let (x, y, width, height) = layout_in_pt(&node.final_layout);
   // x / y may be unused; prefix with _ if so
   ```

3. **compute_transform call (convert.rs:194)**:

   Before:

   ```rust
   match crate::blitz_adapter::compute_transform(&styles, layout.size.width, layout.size.height) {
   ```

   After: convert transform input to CSS px if `compute_transform` expects CSS px (check its body — it operates on CSS pixel values from styles, which are already px-native, so pass `layout.size.width` and `.size.height` WITHOUT conversion here). Document this: transform computes a CSS-space matrix, not a PDF-space matrix. The transform is later applied to pt-coords via scale-invariant operations (rotate/skew) or `× PX_TO_PT` is baked into translate. **Verify by reading compute_transform body.**

4. **child_layout in loops** (convert.rs:1017-1085, 1839-1870):

   Before:

   ```rust
   let child_layout = child_node.final_layout;
   if child_layout.size.height == 0.0 && child_layout.size.width == 0.0 { ... }
   ```

   After:

   ```rust
   let (cx, cy, cw, ch) = layout_in_pt(&child_node.final_layout);
   if ch == 0.0 && cw == 0.0 { ... }
   // pass cx, cy, cw, ch downstream
   ```

5. **Border box extraction (convert.rs:1381-1382)**:

   Before:

   ```rust
   let border_w = node.final_layout.size.width;
   let border_h = node.final_layout.size.height;
   ```

   After:

   ```rust
   let (_, _, border_w, border_h) = layout_in_pt(&node.final_layout);
   ```

6. **Existing `PositionedChild { x, y, width, height, ... }` construction** (convert.rs:1036-1126) — these currently pass raw layout values that will now flow through layout_in_pt at the source.

7. **Tests convert.rs:3032, 3069**: `let parent_layout = doc.get_node(h1_id).unwrap().final_layout.size;` — update to use `size_in_pt(...)`.

**Step 3: Verify compile**

Run: `cargo build -p fulgur 2>&1 | tail -10`

Expected: compile succeeds. No unused helper warnings.

**Step 4: Verify compute_transform is unit-correct**

Read `blitz_adapter::compute_transform` and confirm:

- If it only consumes `width`/`height` to center rotation/scale origin, the unit (px vs pt) doesn't matter for rotation/scale matrices (dimensionless ratios). Translation components need pt-space input.
- If translations are baked in, pass pt values: `let (_, _, w, h) = layout_in_pt(&node.final_layout); compute_transform(&styles, w, h)`.

**Step 5: Run oracle tests**

Run: `cargo test -p fulgur --lib unit_oracle_tests 2>&1 | tail -20`

Expected: **all 4 oracle tests PASS** (100%, 10cm, 360px, 50vw).

If a test still fails, grep for missed sites:

```bash
grep -n "layout\.size\|layout\.location" crates/fulgur/src/convert.rs
```

Should return 0 matches outside of `layout_in_pt` definition itself.

**Step 6: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "fix(fulgur): convert Taffy layout output from CSS px to pt (fulgur-9ul)

Migrates ~30 call sites in convert.rs to go through layout_in_pt, so
Pageable tree sizes/positions are in pt. Combined with viewport input
fix from prior commit, absolute (px, cm) and relative (%, vw) units
now draw at the same scale Chrome / WeasyPrint / Prince produce.

Supersedes PR #90 (enricoschaaf:pr/fix-layout-px-to-pt) which
addressed only the output half.

Oracle tests from Task 2 now pass."
```

---

## Task 6: Fix render.rs get_body_child_dimension callers

**Files:**

- Modify: `crates/fulgur/src/render.rs:422, 446`

**Step 1: Multiply returned dimension by PX_TO_PT**

`get_body_child_dimension` returns CSS px. Callers use it as pt. Either:

- **Option A**: Multiply at call site

  ```rust
  get_body_child_dimension(&measure_doc, true) * crate::convert::PX_TO_PT
  ```

- **Option B**: Bake conversion into helper

  ```rust
  fn get_body_child_dimension(doc: &..., use_width: bool) -> f32 {
      // ... as before, but multiply by PX_TO_PT before returning
  }
  ```

Pick **Option B** so the helper is self-contained and consistent with `layout_in_pt` semantics.

**Step 2: Apply**

Modify `render.rs:184-210` to multiply the final returned value by `crate::convert::PX_TO_PT`:

```rust
fn get_body_child_dimension(doc: &blitz_html::HtmlDocument, use_width: bool) -> f32 {
    use std::ops::Deref;
    let root = doc.root_element();
    let base_doc = doc.deref();
    let px = /* existing logic returning raw CSS px */;
    px * crate::convert::PX_TO_PT
}
```

Add a doc comment noting the unit contract.

**Step 3: Run GCPM-using tests**

Run: `cargo test -p fulgur --lib gcpm 2>&1 | tail -10`

Expected: all GCPM unit tests pass. Baseline values may have shifted; if ANY gcpm unit test now fails on numeric expectations, treat it as a test update (record expected-vs-actual, commit the numeric adjustment in the same pass).

**Step 4: Commit**

```bash
git add crates/fulgur/src/render.rs
git commit -m "fix(fulgur): convert get_body_child_dimension to pt (fulgur-9ul)

Keeps the GCPM 2-pass margin-box path unit-correct after the viewport
input/output fixes."
```

---

## Task 7: Audit PassContext viewport field — remove if dead

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs:49-54`
- Modify: `crates/fulgur/src/engine.rs:113-117, 264-268`
- Modify: all test call sites of `PassContext { viewport_width: 400.0, viewport_height: 10000.0, ... }` (about 9 sites per earlier grep)

**Step 1: Confirm no pass reads viewport fields**

Run:

```bash
grep -rn "ctx\.viewport_width\|ctx\.viewport_height\|context\.viewport_width\|context\.viewport_height" crates/fulgur/src/
```

Expected: zero matches (confirmed during audit).

**Step 2: Remove fields**

At `blitz_adapter.rs:49-54`, delete the two fields:

```rust
pub struct PassContext<'a> {
    pub font_data: &'a [Arc<Vec<u8>>],
}
```

**Step 3: Remove construction in engine.rs (×2)**

At `engine.rs:113-117` and `engine.rs:264-268`:

```rust
let ctx = crate::blitz_adapter::PassContext { font_data: fonts };
```

**Step 4: Remove from every test site**

Run:

```bash
grep -rln "viewport_width: 400.0\|viewport_width:400.0" crates/fulgur/src/ crates/fulgur/tests/
```

For each file, replace the 3-field struct literal with the 1-field version.

**Step 5: Run full unit tests**

Run: `cargo test -p fulgur --lib 2>&1 | tail -10`

Expected: all tests pass (441 previously, likely 445 after Task 2 oracle additions).

**Step 6: Commit**

```bash
git add -A
git commit -m "refactor(fulgur): remove dead viewport fields from PassContext

No DomPass reads viewport_width/height. Removing the dead fields
simplifies the pass harness and removes the temptation to use them
with now-ambiguous units."
```

---

## Task 8: Regenerate VRT goldens

**Files:**

- Modify: `crates/fulgur-vrt/goldens/**/*.png` (regenerate)

**Step 1: Find the VRT regeneration command**

Run:

```bash
cat mise.toml | grep -A 5 vrt
```

Expected: a task like `mise run vrt-regen` or a command in README.

**Step 2: Regenerate all goldens**

Run the identified command. Example:

```bash
cargo test -p fulgur-vrt --test '*' -- --ignored --nocapture 2>&1 | tail -20
# or
mise run vrt-update
```

**Step 3: Diff inspect one golden to verify 0.75× shift is plausible**

Visually inspect a before/after PNG if possible. `grid-simple.html` has `width:360px` — expect all content to have shrunk to 0.75× width (now 270pt instead of 360pt in PDF units).

**Step 4: Verify VRT tests pass on new goldens**

Run:

```bash
cargo test -p fulgur-vrt 2>&1 | tail -10
```

Expected: all pass.

**Step 5: Commit goldens**

```bash
git add crates/fulgur-vrt/goldens/
git commit -m "test(vrt): regenerate goldens after pt/px unit fix (fulgur-9ul)

Per fulgur-9ul design: VRT fixtures are authored in px. Fixing the
pt/px boundary drops every golden by 0.75× (CSS px → PDF pt). This
is the correct scale and matches Chrome/WeasyPrint/Prince output."
```

---

## Task 9: Run full regression and lint suite

**Step 1: Run full test suite**

Run:

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: all pass.

**Step 2: Clippy**

Run:

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```

Expected: zero warnings.

**Step 3: Format check**

Run:

```bash
cargo fmt --check 2>&1 | tail -10
```

Expected: no output (all formatted).

**Step 4: CLI smoke test**

Run:

```bash
echo '<html><body><div style="width:10cm;height:2cm;background:#00f"></div></body></html>' > /tmp/smoke.html
cargo run --bin fulgur -- render /tmp/smoke.html -o /tmp/smoke.pdf 2>&1 | tail -5
pdftocairo -png -r 100 -f 1 -l 1 /tmp/smoke.pdf /tmp/smoke 2>&1
```

Expected: `/tmp/smoke-1.png` shows a blue rectangle that measures 10cm × 2cm at 100 DPI. Verify by eye: at 100 DPI, 10cm = 393.7 px. PNG should show a ~394px wide blue box.

**Step 5: Commit if any fixup from clippy/fmt**

```bash
git add -A
git commit -m "chore(fulgur): clippy/fmt fixups after fulgur-9ul"
```

---

## Task 10: Changelog and handoff

**Files:**

- Modify: `crates/fulgur/CHANGELOG.md`

**Step 1: Add a CHANGELOG entry under an Unreleased / v0.5.0 heading**

```markdown
## [Unreleased]

### Fixed

- **Breaking (visual):** Layout geometry now uses the correct CSS px → PDF pt
  ratio. Previously `width:360px` rendered at `360pt` (127mm) because the
  viewport and the Taffy layout output both mis-labeled units; it now
  renders at `270pt` (95mm), matching Chrome, WeasyPrint, and Prince.
  Absolute and relative units (%, vw, vh, cm, px) are all affected.
  (#fulgur-9ul, supersedes #90)
```

**Step 2: Commit**

```bash
git add crates/fulgur/CHANGELOG.md
git commit -m "docs(fulgur): CHANGELOG entry for pt/px unit fix"
```

---

## Done

- Oracle tests pass (100%, 10cm, 360px, 50vw all produce Chrome-matching widths)
- Full test suite green
- VRT goldens regenerated and consistent with 0.75× shift
- Clippy / fmt clean
- CHANGELOG updated
