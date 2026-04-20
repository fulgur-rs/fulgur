# fulgur-v7a: multicol A-4 `column-rule-*` + `column-fill` Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Land the custom CSS parser that blitz/stylo cannot give us (engine-gated longhands), then render `column-rule-*` between adjacent non-empty columns and honour `column-fill: auto | balance` in the multicol layout hook.

**Architecture:**

`stylo 0.8.0` gates `column-rule-{width,style,color}`, `column-fill`, and `break-inside` behind `engines="gecko"`, but blitz ships with the `servo` feature only — these longhands never reach `ComputedValues`. The plan adds a small, opinionated CSS sniffer in `crates/fulgur/src/column_css.rs` that:

1. Harvests declarations from every element's inline `style="…"` attribute and from every top-level `<style>` block's qualified rules.
2. Matches a **minimal** selector grammar: type (`div`), class (`.foo`), id (`#bar`), compound (`div.foo#bar` — simple selectors ANDed), and comma-separated lists. No combinators, no pseudo-classes, no attribute selectors. Source-order wins (no specificity / no `!important`).
3. Produces a side-table `BTreeMap<NodeId, ColumnStyleProps>` consumed downstream by the multicol hook and by the Pageable tree.

The Taffy multicol hook stashes per-`ColumnGroup` geometry (column-width, gap, column count, per-column filled height) on a new `ColumnGeometry` record keyed by the container's NodeId. In `convert.rs` the multicol container is wrapped in a new `MulticolRulePageable` carrying the rule spec and the geometry records; its `draw()` paints vertical lines between adjacent non-empty columns with the vertical extent clamped to `min(col_heights[i], col_heights[i+1])`. `column-fill: auto` switches `layout_column_group` from `balance_budget` to a greedy sequential fill.

Scope discipline (spec / Phase alignment):

- **Parser Phase A**: inline style + top-level `<style>` only. `@media`, external CSS (`<link rel=stylesheet>` already parses CSS via Blitz for the properties Blitz supports, but our sniffer scans **only** `<style>` blocks to stay scoped), and selector combinators are out of scope.
- **Properties Phase A**: `column-rule-width`, `column-rule-style` (`solid | dashed | dotted` only), `column-rule-color`, `column-rule` shorthand, `column-fill` (`auto | balance`). `double | groove | ridge | inset | outset` deferred to Phase C per the epic design doc.
- **No `break-inside` in this PR.** fulgur-ftp depends on this parser and will add that property as a trivial extension once v7a lands.

**Tech Stack:** Rust, `cssparser` (already a dep via `crates/fulgur/src/gcpm/parser.rs`), `blitz_dom::Node` attr access (`element_data().attr(...)`), fulgur's existing Taffy hook pipeline.

---

## Task summary

| # | Task | Worktree impact |
|---|------|-----------------|
| 1 | Parser scaffold + property/selector TDD | new `column_css.rs`, cssparser impls, unit tests |
| 2 | Harvester pass over DOM | extend `blitz_adapter.rs`, walk inline + `<style>` |
| 3 | Column geometry record on the Taffy hook | modify `multicol_layout.rs`, new `ColumnGeometry` struct |
| 4 | `MulticolRulePageable` wrapper | new type in `pageable.rs` |
| 5 | Wire in `convert.rs`, handle `column-fill: auto` | touch `convert.rs` + `multicol_layout.rs` |
| 6 | Integration test with pixel-sampling | new test in `crates/fulgur/tests/` |
| 7 | Clippy / fmt / markdownlint | polish |

---

### Task 1: Parser scaffold + property TDD

**Files:**

- Create: `crates/fulgur/src/column_css.rs`
- Modify: `crates/fulgur/src/lib.rs` (add `mod column_css;` behind `pub(crate)`)

**Step 1: Sketch the public types — no tests yet, just compile**

```rust
//! Minimal CSS sniffer for the properties stylo 0.8.0 gates to its gecko
//! engine (and thus the `servo`-flavoured blitz does not expose on
//! `ComputedValues`). Phase A covers `column-rule-*`, `column-fill`, and
//! leaves room for `break-inside` (fulgur-ftp).

use std::collections::BTreeMap;
use cssparser::RGBA;
use taffy::NodeId;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnRuleStyle {
    #[default]
    None,
    Solid,
    Dashed,
    Dotted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColumnRuleSpec {
    pub width: f32,             // pt
    pub style: ColumnRuleStyle,
    pub color: [u8; 4],         // RGBA, matches existing `Color` convention
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnFill {
    #[default]
    Balance,
    Auto,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColumnStyleProps {
    pub rule: Option<ColumnRuleSpec>,
    pub fill: Option<ColumnFill>,
}

pub type ColumnStyleTable = BTreeMap<usize, ColumnStyleProps>;
```

> **Note on types:** `RGBA` handling mirrors whatever `crates/fulgur/src/background.rs` or `paragraph.rs` uses. Grep for an existing `Color` type first — if one exists, re-use it instead of `[u8; 4]`. `Pt` versus `f32` similarly: match the surrounding convention.

**Step 2: Failing test — parse a single declaration string**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_column_rule_longhand_triplet() {
        let css = "column-rule-width: 2pt; column-rule-style: solid; column-rule-color: red;";
        let props = parse_declaration_block(css);
        let rule = props.rule.expect("rule");
        assert!((rule.width - 2.0).abs() < 1e-3);
        assert_eq!(rule.style, ColumnRuleStyle::Solid);
        assert_eq!(rule.color, [255, 0, 0, 255]);
    }
}
```

**Step 3: Implement `parse_declaration_block`**

Wire a cssparser `DeclarationParser` similar to the GCPM `GcpmDeclarationParser` (read `crates/fulgur/src/gcpm/parser.rs:~200` for the pattern). Keys:

- Accept `column-rule-width`, `column-rule-style`, `column-rule-color`, `column-rule` (shorthand), `column-fill`, in any order.
- Use `Parser::parse_entirely` for each declaration and discard on error (continue parsing the block).
- For length parsing reuse whatever helper `gcpm/parser.rs` already uses for `px/pt/em` (grep `parse_length` / `parse_dimension`). If nothing reusable exists, inline a small converter: `px → pt` by `value * 72.0 / 96.0`; `pt` pass-through; `em` deferred (treat as invalid for now and document the limitation in a doc-comment on the parser).
- For color reuse cssparser's `Color::parse` (via `cssparser::Color`). Only accept the resolved `Color::Rgba` variant — `currentcolor` and system colors are returned as `None` (log a `trace!` at most).

Add more failing tests incrementally and implement to pass:

```rust
#[test]
fn parses_column_rule_shorthand() {
    let props = parse_declaration_block("column-rule: 1px dashed #0a0;");
    let rule = props.rule.expect("rule");
    assert!((rule.width - 0.75).abs() < 1e-2); // 1px → 0.75pt
    assert_eq!(rule.style, ColumnRuleStyle::Dashed);
    assert_eq!(rule.color, [0x00, 0xAA, 0x00, 0xFF]);
}

#[test]
fn parses_column_fill_auto() {
    let props = parse_declaration_block("column-fill: auto;");
    assert_eq!(props.fill, Some(ColumnFill::Auto));
}

#[test]
fn ignores_unsupported_rule_style() {
    // double is Phase C — must be silently dropped, not error-out the block.
    let props = parse_declaration_block("column-rule: 1pt double red;");
    assert!(props.rule.is_none() || matches!(props.rule.unwrap().style, ColumnRuleStyle::None));
}
```

**Step 4: Selector matcher — tests + impl**

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
enum SimpleSelector {
    Type(String),         // lowercased tag name ("div")
    Class(String),        // ".foo" → "foo"
    Id(String),           // "#bar" → "bar"
    Universal,            // "*"
}

#[derive(Clone, Debug, Default)]
pub struct CompoundSelector {
    pub parts: Vec<SimpleSelector>,
}

pub fn parse_selector_list(input: &str) -> Vec<CompoundSelector> { /* ... */ }

pub fn matches_node(sel: &CompoundSelector, node: &blitz_dom::Node) -> bool { /* ... */ }
```

Tests (failing first, then impl):

```rust
#[test]
fn selector_matches_type_class_id_compound() {
    // dummy struct facade for matching; in-code we use blitz_dom::Node but
    // tests use a minimal fake. Alternatively, build a small HTML fixture
    // via `blitz_adapter::parse` and exercise the matcher end-to-end.
}
```

> **Note on test strategy:** it is cleaner to exercise `matches_node` end-to-end by building a tiny `blitz_dom` document via `crate::blitz_adapter::parse` + `resolve`, looking up nodes by id/class, and asserting match/no-match. Follow whatever pattern the existing blitz_adapter test module uses (grep `mod tests` in that file).

**Step 5: Stylesheet aggregator — tests + impl**

```rust
pub struct StyleRule {
    pub selectors: Vec<CompoundSelector>,
    pub props: ColumnStyleProps,
}

pub fn parse_stylesheet(source: &str) -> Vec<StyleRule> { /* ... */ }
```

`parse_stylesheet` uses cssparser's `StyleSheetParser` + `QualifiedRuleParser`. Skip `@` rules (GCPM already harvests `@page`; we don't need them here). For each qualified rule parse the prelude as a selector list; if any simple selector contains an unsupported token (`:`, `[`, `>`, `+`, `~`, ` `), discard the whole rule silently. Parse the declaration block via `parse_declaration_block`.

**Step 6: Cascade resolver — tests + impl**

```rust
pub fn build_column_style_table(
    doc: &blitz_dom::HtmlDocument,
    stylesheet_rules: &[StyleRule],
) -> ColumnStyleTable;
```

Algorithm per node: walk stylesheet rules in source order; for each rule, if **any** of its compound selectors matches, fold its `ColumnStyleProps` into the node's entry (`Some` overwrites `None`; later `Some` overwrites earlier `Some` — last-wins, no specificity). Then parse inline `style` attribute and fold last.

**Step 7: Run**

`cargo test -p fulgur --lib column_css` — expect every unit test added above to pass. About 6–10 tests total.

**Step 8: Commit**

```bash
git add crates/fulgur/src/column_css.rs crates/fulgur/src/lib.rs
git commit -m "feat(fulgur-v7a): custom CSS sniffer for column-rule / column-fill"
```

---

### Task 2: DOM harvester — build the side-table once per document

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs` — add `extract_column_style_table(doc) -> ColumnStyleTable`
- Modify: `crates/fulgur/src/engine.rs` — call it after `resolve()` and thread the table through to conversion / layout

**Step 1: Sketch the extractor**

```rust
pub fn extract_column_style_table(
    doc: &blitz_dom::HtmlDocument,
) -> crate::column_css::ColumnStyleTable {
    // 1. Concatenate every top-level <style> block's text content.
    // 2. Parse it via column_css::parse_stylesheet.
    // 3. Call column_css::build_column_style_table.
}
```

Walk helper can reuse the same recursion shape as `walk_for_inline_styles` (blitz_adapter.rs:239) — lift `walk_for_inline_styles` into a more general private visitor that yields text content, or duplicate the pattern if the original one has strong GCPM-specific coupling. Prefer duplication over a risky refactor; mark a `// TODO(phase-b): unify with walk_for_inline_styles` at the boundary.

**Step 2: Thread the table through**

`Engine::render_html` already holds a `BaseDocument`. After `blitz_adapter::resolve(...)` but before `convert::to_pageable(...)`, call `extract_column_style_table` and pass the result through both:

1. `FulgurLayoutTree::new(doc, column_styles)` (new field), so `compute_multicol_layout` can read `column-fill` to switch balance strategies.
2. `convert::to_pageable`'s context so the BlockPageable construction can see the rule spec.

If `FulgurLayoutTree` currently takes only `&mut BaseDocument`, extend its signature — there are few callers. Search: `grep -rn "FulgurLayoutTree::new" crates/fulgur`.

**Step 3: Test**

Add one test at `crates/fulgur/src/blitz_adapter.rs` (tests module):

```rust
#[test]
fn extract_column_style_table_picks_up_inline_and_stylesheet() {
    let html = r#"<html><head><style>
        .mc { column-fill: auto; column-rule: 1pt solid blue; }
    </style></head><body>
        <div class="mc" id="a"></div>
        <div class="mc" id="b" style="column-rule: 2pt dashed red"></div>
    </body></html>"#;
    let mut doc = /* parse + resolve as in sibling tests */;
    let table = extract_column_style_table(&doc);
    let a = find_by_id(&doc, "a");
    let b = find_by_id(&doc, "b");
    // A: picks up stylesheet.
    let a_props = table.get(&a).unwrap();
    assert_eq!(a_props.fill, Some(crate::column_css::ColumnFill::Auto));
    assert!(a_props.rule.unwrap().color == [0, 0, 255, 255]);
    // B: inline overrides.
    let b_props = table.get(&b).unwrap();
    assert!((b_props.rule.unwrap().width - 2.0).abs() < 1e-3);
    assert_eq!(b_props.rule.unwrap().style, crate::column_css::ColumnRuleStyle::Dashed);
}
```

**Step 4: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs crates/fulgur/src/engine.rs crates/fulgur/src/multicol_layout.rs
git commit -m "feat(fulgur-v7a): harvest column styles from DOM into side-table"
```

---

### Task 3: Stash per-ColumnGroup geometry out of the Taffy hook

**Files:**

- Modify: `crates/fulgur/src/multicol_layout.rs`

**Step 1: New geometry types**

```rust
#[derive(Clone, Debug, Default)]
pub struct ColumnGroupGeometry {
    /// y-offset within the multicol container's content box.
    pub y_offset: f32,
    pub col_w: f32,
    pub gap: f32,
    pub n: u32,
    /// Per-column cumulative filled height. Length == n. Zero means empty.
    pub col_heights: Vec<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct MulticolGeometry {
    pub groups: Vec<ColumnGroupGeometry>,
}

pub type MulticolGeometryTable = BTreeMap<usize, MulticolGeometry>;
```

**Step 2: Record geometry in `layout_column_group`**

Currently returns `(placements, seg_h)`. Extend the return to `(placements, ColumnGroupGeometry)`. Compute `col_heights` by iterating placements and grouping by `col_idx` (derivable from `location.x / (col_w + gap)`). Update the one caller in `compute_multicol_layout`.

**Step 3: Propagate to `FulgurLayoutTree`**

Store `MulticolGeometryTable` on `FulgurLayoutTree`, extend `layout_multicol_subtrees` to record one `MulticolGeometry` per container. Add a public getter: `pub fn take_geometry(&mut self) -> MulticolGeometryTable`.

Unit tests: keep the existing `layout_column_group_matches_legacy_flat_balance` case green, and add one new test asserting `col_heights` sum equals `total_h / n ± balance_step` for a trivial fixture.

**Step 4: Commit**

```bash
git add crates/fulgur/src/multicol_layout.rs
git commit -m "feat(fulgur-v7a): record per-ColumnGroup geometry from the Taffy hook"
```

---

### Task 4: `MulticolRulePageable` wrapper

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` — new type, trait impl, unit tests

**Step 1: Define the wrapper**

```rust
pub struct MulticolRulePageable {
    pub child: Box<dyn Pageable>,
    pub rule: crate::column_css::ColumnRuleSpec,
    pub groups: Vec<crate::multicol_layout::ColumnGroupGeometry>,
}
```

All `Pageable` methods delegate to `self.child`, **except** `draw()`: call `child.draw()` first, then for every `(group, i, i+1)` with both `col_heights[i] > 0.0` and `col_heights[i+1] > 0.0`, paint a vertical line at `x = group.col_w + i * (group.col_w + group.gap) + group.gap / 2.0` from `y_offset` to `y_offset + min(col_heights[i], col_heights[i+1])`.

Stroke width, dash pattern, and colour come from `self.rule`. For `Dotted` use a dash pattern of `[width, width]`; for `Dashed` use `[width * 3, width * 2]`; for `Solid` no dash. Krilla stroke APIs: mirror whatever `render.rs` already uses for borders (grep `stroke_path` / `Stroke`).

**Step 2: Ensure split plumbing works**

The wrapper must survive pagination. `split_boxed` — delegate to `child.split_boxed` and reconstruct wrappers around both halves, preserving groups that fall on the first half vs the second half. Groups whose `y_offset + max(col_heights) <= first_half_height` stay on the first; the rest shift to the second (subtract `first_half_height` from `y_offset`). Groups that straddle the boundary split their `col_heights` by clamping — document this as approximate because cross-page ColumnGroup pagination is a known gap (see out-of-scope below).

Add a unit test: wrap a simple BlockPageable with two groups at y 0 and y 400, split at 300pt, assert group 0 ends up on page 1 and group 1 on page 2.

**Step 3: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(fulgur-v7a): MulticolRulePageable draws rules + survives split"
```

---

### Task 5: Wire side-table + geometry into convert.rs; honour `column-fill: auto`

**Files:**

- Modify: `crates/fulgur/src/convert.rs` — detect multicol containers, wrap in `MulticolRulePageable`
- Modify: `crates/fulgur/src/multicol_layout.rs` — branch on `ColumnFill::Auto` before `balance_budget`
- Modify: `crates/fulgur/src/engine.rs` — pass the side-table + geometry into convert

**Step 1: `column-fill: auto` branch**

In `layout_column_group` (before `balance_budget`), if the container's `ColumnStyleProps.fill == Some(ColumnFill::Auto)`, replace the budget selection with a greedy fill: `budget = avail_h` (each column fills to the top up to the container height). No further changes downstream.

Unit test: fixture with `column-fill: auto` where the total content fits in a single column — assert the second column remains empty (`col_heights[1] == 0.0`).

**Step 2: Wrap multicol containers in convert.rs**

Identify the 1–2 sites where a multicol container becomes a `BlockPageable`. If the side-table has a `ColumnStyleProps.rule`, wrap the block in `MulticolRulePageable::new(block, rule, groups)` using the geometry from the table. If `rule.style == ColumnRuleStyle::None` or `rule.width <= 0.0`, skip the wrapper — no visual change.

**Step 3: Run tests**

`cargo test -p fulgur` — 493 + new tests all pass. No existing test should have changed behaviour for non-multicol or for multicol without `column-rule`.

**Step 4: Commit**

```bash
git add -u
git commit -m "feat(fulgur-v7a): render column rules and honour column-fill: auto"
```

---

### Task 6: Integration test with pixel sampling

**Files:**

- Create: `crates/fulgur/tests/column_rule_rendering.rs`

**Step 1: Failing test**

```rust
//! Pixel-sampling probe for `column-rule` rendering (fulgur-v7a).
//!
//! Renders a simple 2-column layout with a thick solid red rule, converts
//! the PDF to PNG via `pdftocairo` (skip if unavailable), and asserts a red
//! pixel exists in the column-gap region. `page_count` cannot discriminate
//! rule-present vs rule-absent, so we sample pixels directly.

use fulgur::{Engine, PageSize};

fn pdftocairo_available() -> bool {
    std::process::Command::new("pdftocairo")
        .arg("-v")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn column_rule_solid_red_is_visible_between_columns() {
    if !pdftocairo_available() {
        eprintln!("skipping: pdftocairo not installed");
        return;
    }
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 10pt; }
        body { margin: 0; color: black; font-family: sans-serif; }
        .mc {
            column-count: 2;
            column-gap: 20pt;
            column-rule: 4pt solid red;
        }
        .mc p { margin: 0 0 8pt 0; }
    </style></head><body>
      <div class="mc">
        <p>Aaa aaa aaa aaa aaa aaa aaa.</p>
        <p>Bbb bbb bbb bbb bbb bbb bbb.</p>
        <p>Ccc ccc ccc ccc ccc ccc ccc.</p>
      </div>
    </body></html>"#;

    let engine = Engine::builder()
        .page_size(PageSize { width: 200.0, height: 200.0 })
        .build();
    let pdf = engine.render_html(html).expect("render");

    // Write PDF to a temp file, render with pdftocairo -png -r 200, load the
    // PNG, and look for a pixel at the expected gap centre (approx x=100pt →
    // 200*200/72 ≈ 277px at 200 DPI).
    // Use tempfile crate (already a dev-dep via other tests — grep for it).
    // ... (see existing VRT pipeline in fulgur-vrt for helper patterns).

    // Assert: at least one pixel in the rectangle x ∈ [gap_center - 5, +5]
    // is visually "red" (R>200, G<64, B<64).
}
```

> **Note:** Replicate the pdftocairo invocation used by `fulgur-vrt` (see `crates/fulgur-vrt/src/pdf_render.rs`). If `fulgur-vrt` is not a dev-dep of `fulgur`, inline the shell invocation via `std::process::Command`. Read the PNG via `image` crate (already transitively available — check `Cargo.lock`); otherwise fall back to raw PNG decoding with `png` crate.

**Step 2: Run and iterate until the red pixel appears**

Expected first run: fails because Tasks 1–5 must all be landed — if the pixel isn't red, walk backwards: is the wrapper attached? does geometry populate? does the parser pick up the rule? Use `RUST_LOG=trace` and/or targeted `eprintln!` to diagnose.

**Step 3: Add a second test for `column-fill: auto`**

```rust
#[test]
fn column_fill_auto_leaves_second_column_empty_for_short_content() {
    // One short paragraph inside `column-fill: auto` should land in column
    // 1 entirely; column 2 should be empty. Sample a pixel where column 2's
    // text would land — assert white (R>240, G>240, B>240).
}
```

**Step 4: Commit**

```bash
git add crates/fulgur/tests/column_rule_rendering.rs
git commit -m "test(fulgur-v7a): pixel-sampling probe for column-rule + column-fill"
```

---

### Task 7: Lint, format, markdownlint, full-suite green

**Files:** none (verification-only unless clippy requires touches)

**Step 1: Clippy**

`cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -40` — fix any warnings.

**Step 2: Formatting**

`cargo fmt --check` — must be clean.

**Step 3: Full test run**

`cargo test` — fulgur, fulgur-cli, fulgur-vrt all green.

**Step 4: Markdown lint of this plan**

`npx markdownlint-cli2 'docs/plans/2026-04-21-fulgur-v7a-column-rule.md'`

**Step 5: Final commit if anything shifted**

```bash
git add -u
git commit -m "chore(fulgur-v7a): clippy + fmt polish"
```

---

## Out of scope (follow-ups / Phase C)

- Selector combinators (` `, `>`, `+`, `~`), pseudo-classes / pseudo-elements, attribute selectors.
- CSS specificity / `!important` — this parser is **source-order wins**.
- Column-rule styles beyond `solid | dashed | dotted` (double/groove/ridge/inset/outset).
- Cross-page pagination of a `ColumnGroup` — rule geometry is approximated when a group straddles a page. File as part of the existing Phase B gap (fulgur-6q5 / fulgur-wfd).
- `break-inside` — reserved for fulgur-ftp, which re-uses the parser from this PR.
- `em` / `rem` length units — resolved against a fixed 16px default (`DEFAULT_EM_PX` in `column_css.rs`), not the container's computed font-size. Phase B will thread the real basis through the parser so authored sizes honour inherited `font-size`.
- Linked stylesheets (`<link rel="stylesheet">`) — the column side-table is populated from inline `style="..."` attributes and top-level `<style>` blocks only. External CSS still drives stylo but never reaches the sniffer; tracked as fulgur-s5ro.
- `<style media="screen">` and other non-print media queries — sheets with a media attribute that excludes `all` / `print` are skipped by the harvester. Full media-query evaluation (size ranges, logical operators) is deferred.

## Acceptance checklist

- [ ] `cargo test -p fulgur --lib column_css` — all parser unit tests pass.
- [ ] `cargo test -p fulgur --test column_rule_rendering` — pixel probes pass (or skip cleanly when `pdftocairo` is missing).
- [ ] `cargo test -p fulgur` — full suite green (493 prior tests still pass).
- [ ] `cargo clippy --workspace --all-targets` — no warnings.
- [ ] `cargo fmt --check` — clean.
- [ ] Plan passes `npx markdownlint-cli2`.
- [ ] fulgur-ftp unblocker: once this branch merges, the side-table struct exposes enough shape for fulgur-ftp to add `break_inside` with a ~20 LOC diff to `column_css.rs`.
