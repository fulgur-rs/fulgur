# CSS page-break-after/before Wiring Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Wire CSS `page-break-after: always`, `page-break-before: always`, `break-after: page`, and `break-before: page` through the column_css sniffer into `Pagination`, so forced page breaks actually work.

**Architecture:** stylo 0.8.0 gates `break-after`/`break-before` to `engines="gecko"`, so these properties are invisible in blitz's servo build. The identical problem was solved for `break-inside` in fulgur-ftp (PR #138) by using a custom cssparser sniffer (`column_css.rs`). We extend that same sniffer with `break_before`/`break_after` fields and update `extract_pagination_from_column_css` in `convert.rs` to populate them.

**Tech Stack:** Rust, cssparser (already dep), `crates/fulgur/src/column_css.rs`, `crates/fulgur/src/convert.rs`, `crates/fulgur/tests/page_break_wiring.rs` (new)

---

## Task 1: Add break_before/break_after fields to ColumnStyleProps

**Files:**

- Modify: `crates/fulgur/src/column_css.rs`

**Step 1: Write the failing unit tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `column_css.rs`:

```rust
#[test]
fn parse_break_after_always_inline() {
    let props = parse_inline_style("break-after: always");
    assert_eq!(props.break_after, Some(BreakAfterValue::Page));
}

#[test]
fn parse_page_break_after_always_inline() {
    let props = parse_inline_style("page-break-after: always");
    assert_eq!(props.break_after, Some(BreakAfterValue::Page));
}

#[test]
fn parse_break_before_always_inline() {
    let props = parse_inline_style("break-before: always");
    assert_eq!(props.break_before, Some(BreakBeforeValue::Page));
}

#[test]
fn parse_page_break_before_always_inline() {
    let props = parse_inline_style("page-break-before: always");
    assert_eq!(props.break_before, Some(BreakBeforeValue::Page));
}

#[test]
fn parse_break_after_auto_is_auto() {
    let props = parse_inline_style("break-after: auto");
    assert_eq!(props.break_after, Some(BreakAfterValue::Auto));
}

#[test]
fn parse_break_after_invalid_is_silently_dropped() {
    let props = parse_inline_style("break-after: banana");
    assert_eq!(props.break_after, None);
}

#[test]
fn parse_break_after_via_selector() {
    let rules = parse_stylesheet(".forced { break-after: page; }");
    assert_eq!(rules[0].props.break_after, Some(BreakAfterValue::Page));
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur --lib column_css 2>&1 | tail -20
```

Expected: compilation failure (BreakAfterValue and BreakBeforeValue types don't exist yet, break_after field doesn't exist on ColumnStyleProps)

**Step 3: Add BreakAfterValue and BreakBeforeValue enums and fields**

Near the top of `column_css.rs`, after the existing imports and before `ColumnStyleProps`, add:

```rust
/// The two states fulgur cares about for `break-after` / `page-break-after`.
/// CSS Fragmentation `always` / `page` / `left` / `right` / `recto` / `verso`
/// all collapse to `Page`; `auto` stays `Auto`; unknown values drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakAfterValue {
    Auto,
    Page,
}

/// The two states fulgur cares about for `break-before` / `page-break-before`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakBeforeValue {
    Auto,
    Page,
}
```

Then extend `ColumnStyleProps`:

```rust
pub struct ColumnStyleProps {
    pub rule: Option<ColumnRuleSpec>,
    pub fill: Option<ColumnFill>,
    pub break_inside: Option<BreakInside>,
    /// `break-after` / `page-break-after` resolved by the sniffer.
    pub break_after: Option<BreakAfterValue>,
    /// `break-before` / `page-break-before` resolved by the sniffer.
    pub break_before: Option<BreakBeforeValue>,
}
```

Update `merge()`:

```rust
fn merge(&mut self, other: ColumnStyleProps) {
    if other.rule.is_some() { self.rule = other.rule; }
    if other.fill.is_some() { self.fill = other.fill; }
    if other.break_inside.is_some() { self.break_inside = other.break_inside; }
    if other.break_after.is_some() { self.break_after = other.break_after; }
    if other.break_before.is_some() { self.break_before = other.break_before; }
}
```

Update `is_empty()`:

```rust
fn is_empty(&self) -> bool {
    self.rule.is_none()
        && self.fill.is_none()
        && self.break_inside.is_none()
        && self.break_after.is_none()
        && self.break_before.is_none()
}
```

**Step 4: Run tests again — still fail (no parser yet)**

```bash
cargo test -p fulgur --lib column_css 2>&1 | tail -20
```

Expected: compilation succeeds now, but new tests fail (break_after/break_before parsed as None)

**Step 5: Add parse helpers and wire into declaration parser**

Add two parser functions after `parse_break_inside_value`:

```rust
fn parse_break_after_value<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<BreakAfterValue, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    match ident.as_ref().to_ascii_lowercase().as_str() {
        "always" | "page" | "left" | "right" | "recto" | "verso" => Ok(BreakAfterValue::Page),
        "auto" => Ok(BreakAfterValue::Auto),
        _ => Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
    }
}

fn parse_break_before_value<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<BreakBeforeValue, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    match ident.as_ref().to_ascii_lowercase().as_str() {
        "always" | "page" | "left" | "right" | "recto" | "verso" => Ok(BreakBeforeValue::Page),
        "auto" => Ok(BreakBeforeValue::Auto),
        _ => Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
    }
}
```

In `parse_declaration_block` (inside `ColumnDeclParser`'s impl), after the `break-inside` arm, add:

```rust
} else if name.eq_ignore_ascii_case("break-after")
    || name.eq_ignore_ascii_case("page-break-after")
{
    if let Ok(v) = input.parse_entirely(parse_break_after_value) {
        self.props.break_after = Some(v);
    }
} else if name.eq_ignore_ascii_case("break-before")
    || name.eq_ignore_ascii_case("page-break-before")
{
    if let Ok(v) = input.parse_entirely(parse_break_before_value) {
        self.props.break_before = Some(v);
    }
```

**Step 6: Run tests to verify they pass**

```bash
cargo test -p fulgur --lib column_css 2>&1 | tail -20
```

Expected: all column_css tests pass

**Step 7: Run full suite to check for regressions**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 615+ tests, 0 failed

**Step 8: Commit**

```bash
git -C /home/ubuntu/fulgur/.worktrees/fulgur-lje5 add crates/fulgur/src/column_css.rs
git -C /home/ubuntu/fulgur/.worktrees/fulgur-lje5 commit -m "feat(column_css): add break-after/before sniffer fields (fulgur-lje5)"
```

---

## Task 2: Wire break_before/break_after into extract_pagination_from_column_css

**Files:**

- Modify: `crates/fulgur/src/convert.rs`

**Step 1: Write a unit test for extract_pagination_from_column_css**

In `column_css.rs`'s test module (or in `convert.rs` if easier), add a test that round-trips through `build_column_style_table` with `page-break-after: always` and verifies the table contains `BreakAfterValue::Page`:

```rust
// In column_css.rs tests
#[test]
fn build_table_carries_break_after_page() {
    use blitz_dom::node::Node;
    // We test the parser side; convert.rs wiring is covered by integration tests.
    let rules = parse_stylesheet("div { page-break-after: always; }");
    assert_eq!(rules[0].props.break_after, Some(BreakAfterValue::Page));
}
```

**Step 2: Update extract_pagination_from_column_css in convert.rs**

Find the function at line ~1262 of `convert.rs`:

```rust
fn extract_pagination_from_column_css(
    ctx: &ConvertContext<'_>,
    node: &Node,
) -> crate::pageable::Pagination {
    use crate::pageable::{BreakInside, Pagination};
    let props = ctx.column_styles.get(&node.id).copied().unwrap_or_default();
    Pagination {
        break_inside: props.break_inside.unwrap_or(BreakInside::Auto),
        ..Pagination::default()
    }
}
```

Replace with:

```rust
fn extract_pagination_from_column_css(
    ctx: &ConvertContext<'_>,
    node: &Node,
) -> crate::pageable::Pagination {
    use crate::column_css::{BreakAfterValue, BreakBeforeValue};
    use crate::pageable::{BreakAfter, BreakBefore, BreakInside, Pagination};
    let props = ctx.column_styles.get(&node.id).copied().unwrap_or_default();
    Pagination {
        break_inside: props.break_inside.unwrap_or(BreakInside::Auto),
        break_after: match props.break_after {
            Some(BreakAfterValue::Page) => BreakAfter::Page,
            _ => BreakAfter::Auto,
        },
        break_before: match props.break_before {
            Some(BreakBeforeValue::Page) => BreakBefore::Page,
            _ => BreakBefore::Auto,
        },
        ..Pagination::default()
    }
}
```

**Step 3: Run tests to verify nothing regressed**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 615+ tests, 0 failed

**Step 4: Commit**

```bash
git -C /home/ubuntu/fulgur/.worktrees/fulgur-lje5 add crates/fulgur/src/convert.rs
git -C /home/ubuntu/fulgur/.worktrees/fulgur-lje5 commit -m "feat(convert): wire break-after/before into Pagination (fulgur-lje5)"
```

---

## Task 3: Integration tests for page-break-after and page-break-before

**Files:**

- Create: `crates/fulgur/tests/page_break_wiring.rs`
- Modify: `crates/fulgur/Cargo.toml` (add `[[test]]` entry if needed — check existing pattern)

**Step 1: Check if Cargo.toml needs [[test]] entries**

```bash
grep -n "^\[\[test\]\]" /home/ubuntu/fulgur/.worktrees/fulgur-lje5/crates/fulgur/Cargo.toml | head -5
```

If the file uses `[[test]]` entries, add one. If tests are auto-discovered (no `[[test]]` entries), skip this step.

**Step 2: Write the failing integration test file**

Create `crates/fulgur/tests/page_break_wiring.rs`:

```rust
//! Integration tests for CSS page-break-after / page-break-before wiring (fulgur-lje5).

use fulgur::{Engine, PageSize};

fn page_count(pdf: &[u8]) -> usize {
    let prefix = b"/Type /Page";
    let mut count = 0usize;
    let mut i = 0;
    while i + prefix.len() < pdf.len() {
        if &pdf[i..i + prefix.len()] == prefix {
            let next = pdf[i + prefix.len()];
            if !next.is_ascii_alphanumeric() {
                count += 1;
            }
            i += prefix.len();
        } else {
            i += 1;
        }
    }
    count
}

/// `page-break-after: always` forces a page split after the element.
#[test]
fn page_break_after_always_splits_pages() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .first { height: 80pt; page-break-after: always; }
        .second { height: 80pt; }
    </style></head><body>
      <div class="first"></div>
      <div class="second"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected page-break-after: always to force 2 pages, got {}",
        page_count(&pdf)
    );
}

/// `break-after: page` (Level 3 longhand) also forces a page split.
#[test]
fn break_after_page_splits_pages() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .first { height: 80pt; break-after: page; }
        .second { height: 80pt; }
    </style></head><body>
      <div class="first"></div>
      <div class="second"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected break-after: page to force 2 pages, got {}",
        page_count(&pdf)
    );
}

/// `page-break-before: always` forces a page split before the element.
#[test]
fn page_break_before_always_splits_pages() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .first { height: 80pt; }
        .second { height: 80pt; page-break-before: always; }
    </style></head><body>
      <div class="first"></div>
      <div class="second"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected page-break-before: always to force 2 pages, got {}",
        page_count(&pdf)
    );
}

/// `break-before: page` (Level 3 longhand) also forces a page split.
#[test]
fn break_before_page_splits_pages() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .first { height: 80pt; }
        .second { height: 80pt; break-before: page; }
    </style></head><body>
      <div class="first"></div>
      <div class="second"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected break-before: page to force 2 pages, got {}",
        page_count(&pdf)
    );
}

/// Both fit on one page without the break property — confirms the test is not
/// measuring natural overflow.
#[test]
fn no_break_property_stays_on_one_page() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .first { height: 80pt; }
        .second { height: 80pt; }
    </style></head><body>
      <div class="first"></div>
      <div class="second"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert_eq!(
        page_count(&pdf),
        1,
        "without break properties, both divs should fit on 1 page, got {}",
        page_count(&pdf)
    );
}
```

**Step 3: Run integration tests to verify they fail**

```bash
cargo test -p fulgur --test page_break_wiring 2>&1 | tail -20
```

Expected: 4 out of 5 tests fail (forced-break tests produce 1 page each; `no_break_property_stays_on_one_page` passes)

**Step 4: Verify tests pass after Task 1 & 2 implementation**

(The implementation is already done by this point. Re-run to confirm.)

```bash
cargo test -p fulgur --test page_break_wiring 2>&1 | tail -20
```

Expected: 5/5 pass

**Step 5: Run full suite**

```bash
cargo test -p fulgur 2>&1 | tail -10
```

Expected: all tests pass, 0 failed

**Step 6: Commit**

```bash
git -C /home/ubuntu/fulgur/.worktrees/fulgur-lje5 add crates/fulgur/tests/page_break_wiring.rs
git -C /home/ubuntu/fulgur/.worktrees/fulgur-lje5 commit -m "test(page_break): integration tests for page-break-after/before wiring (fulgur-lje5)"
```

---

## Task 4: Lint and final verification

**Step 1: Run clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```

Expected: clean (no warnings)

**Step 2: Run rustfmt check**

```bash
cargo fmt --check 2>&1
```

If there are formatting issues, run `cargo fmt` and commit.

**Step 3: Run full test suite one more time**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur --test page_break_wiring 2>&1 | tail -5
```

Expected: all pass

**Step 4: Commit fmt fix (if needed)**

```bash
git -C /home/ubuntu/fulgur/.worktrees/fulgur-lje5 add -u
git -C /home/ubuntu/fulgur/.worktrees/fulgur-lje5 commit -m "style: cargo fmt (fulgur-lje5)"
```
