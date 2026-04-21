# fulgur-ftp: multicol A-5 `break-inside: avoid` Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CSS `break-inside: avoid` / `avoid-page` / `avoid-column` を fulgur のページ送りで honour する。ブロックがページ境界をまたぐなら次ページへ promote。1ページより大きい avoid block は silent overflow / 無限ループを避けるため通常 split へ fall back。

**Architecture:** (1) `column_css.rs` の `ColumnStyleProps` に `break_inside` を追加し、既存の selector + cascade 基盤で拾う。(2) `convert.rs` の全 `BlockPageable::with_positioned_children` サイト（12箇所）に `.with_pagination(...)` を配線。(3) `BlockPageable` に `page_height` を持たせ、`find_split_point` で oversized-avoid を detect して通常 split に fall through。`multicol_layout.rs` への変更は不要（`distribute` の whole placement が ColumnGroup 内 avoid-child を自動保護する）。

**Tech Stack:** Rust, cssparser 0.35, fulgur `ColumnStyleTable` / `BlockPageable` / `Pagination`.

**Worktree:** `.worktrees/fulgur-ftp-break-inside-avoid` (branch `feature/fulgur-ftp-break-inside-avoid`, baseline: 549 lib tests green)

**Issue:** `fulgur-ftp`. Follow-up for `break-before` / `break-after` is `fulgur-4zje`.

---

### Task 1: Probe integration test (straddling avoid block)

**Files:**

- Create: `crates/fulgur/tests/break_inside_avoid.rs`

**Step 1: Write failing test**

```rust
//! Integration tests for CSS `break-inside: avoid` (fulgur-ftp).

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

/// avoid block がページ境界にまたがる → 次ページへ promote。
#[test]
fn avoid_block_straddling_boundary_promotes_to_next_page() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .spacer { height: 160pt; background: #eee; }
        .keep { height: 60pt; background: #c00; break-inside: avoid; }
    </style></head><body>
      <div class="spacer"></div>
      <div class="keep"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom_pt(200.0, 200.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected avoid block to promote to page 2, got {} pages",
        page_count(&pdf)
    );
}
```

> **Note:** `PageSize::custom_pt` が未定義なら `PageSize::custom(70.56, 70.56)` (200pt ≈ 70.56mm) に置換。`grep -n "custom_pt\|pub fn custom" crates/fulgur/src/config.rs` で確認。

**Step 2: Run test to verify it fails**

Run: `cargo test -p fulgur --test break_inside_avoid -- --nocapture`
Expected: FAIL — 現状 `break-inside` は pipeline に流れていないので 1 ページだけ生成される。

> **Probe test の検証力についての note:** この probe は `break-inside` 配線が効いていることを弱くしか検証しない。spacer(160pt) + keep(60pt) = 220pt > page(200pt) のため、配線の有無にかかわらず `page_count >= 2` は満たされる（keep の開始位置だけが変わる）。CSS仕様上 `page_count` だけで avoid 効果を区別する HTML は作れないため、本 PR の最終 acceptance は Task 5 の oversized-avoid test (`avoid_block_taller_than_page_falls_back_to_split`) が、配線 + fallback を強く検証することで達成される。Tasks 1-3 単体ではなく、Tasks 1-5 を合わせて merge 可能性を判断すること。

**Step 3: Do not commit yet.** Tasks 2–3 の acceptance check。

---

### Task 2: column_css.rs 拡張 — `break_inside` フィールド + parser

**Files:**

- Modify: `crates/fulgur/src/column_css.rs`

**Step 1: `ColumnStyleProps` に `break_inside` を追加**

`crates/fulgur/src/column_css.rs:100` の struct に1フィールド追加:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColumnStyleProps {
    pub rule: Option<ColumnRuleSpec>,
    pub fill: Option<ColumnFill>,
    pub break_inside: Option<crate::pageable::BreakInside>,
}
```

`merge()` (line 108) と `is_empty()` (line 117) も拡張:

```rust
fn merge(&mut self, other: ColumnStyleProps) {
    if other.rule.is_some() { self.rule = other.rule; }
    if other.fill.is_some() { self.fill = other.fill; }
    if other.break_inside.is_some() { self.break_inside = other.break_inside; }
}

fn is_empty(&self) -> bool {
    self.rule.is_none() && self.fill.is_none() && self.break_inside.is_none()
}
```

**Step 2: DeclarationParser に `break-inside` ブランチを追加**

`impl DeclarationParser for ColumnDeclParser` の `parse_value` (line 434) の elif chain 末尾に追加:

```rust
} else if name.eq_ignore_ascii_case("break-inside") {
    let ident = input.expect_ident()?;
    let bi = match ident.as_ref().to_ascii_lowercase().as_str() {
        "avoid" | "avoid-page" | "avoid-column" => crate::pageable::BreakInside::Avoid,
        "auto" => crate::pageable::BreakInside::Auto,
        _ => return Err(input.new_unexpected_token_error(
            cssparser::Token::Ident(ident.clone())
        )),
    };
    self.props.break_inside = Some(bi);
    Ok(())
}
```

> **Note:** 既存の分岐で `self.props.*` に直接代入しているか、それとも mutable ref を使うかは既存コードの書き方に合わせる。`column-rule-width` (line 447) の実装を参考にする。

**Step 3: Unit tests を追加**

`column_css.rs` の既存 `#[cfg(test)] mod tests` ブロック末尾に追加:

```rust
#[test]
fn parse_break_inside_avoid_inline() {
    let props = parse_declaration_block("break-inside: avoid;");
    assert_eq!(props.break_inside, Some(crate::pageable::BreakInside::Avoid));
}

#[test]
fn parse_break_inside_avoid_page_and_column_collapse_to_avoid() {
    let p1 = parse_declaration_block("break-inside: avoid-page;");
    let p2 = parse_declaration_block("break-inside: avoid-column;");
    assert_eq!(p1.break_inside, Some(crate::pageable::BreakInside::Avoid));
    assert_eq!(p2.break_inside, Some(crate::pageable::BreakInside::Avoid));
}

#[test]
fn parse_break_inside_auto_is_auto_variant() {
    let props = parse_declaration_block("break-inside: auto;");
    assert_eq!(props.break_inside, Some(crate::pageable::BreakInside::Auto));
}

#[test]
fn parse_break_inside_invalid_value_is_silently_dropped() {
    let props = parse_declaration_block("break-inside: banana;");
    assert_eq!(props.break_inside, None);
}

#[test]
fn parse_break_inside_via_selector() {
    let rules = parse_stylesheet(".keep { break-inside: avoid; }");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].props.break_inside, Some(crate::pageable::BreakInside::Avoid));
}
```

**Step 4: Run unit tests**

Run: `cargo test -p fulgur --lib column_css::`
Expected: PASS (5 new tests + all pre-existing).

**Step 5: Module doc を更新**

`crates/fulgur/src/column_css.rs:9` の「extend this module with `break-inside` — deliberately out of scope here」を「`break-inside` is handled here as of fulgur-ftp」に差し替え。

**Step 6: Commit**

```bash
git add crates/fulgur/src/column_css.rs crates/fulgur/tests/break_inside_avoid.rs
git commit -m "feat(fulgur-ftp): parse break-inside in column_css"
```

---

### Task 3: convert.rs 配線 — 全 BlockPageable サイトに `.with_pagination(...)`

**Files:**

- Modify: `crates/fulgur/src/convert.rs`

**Step 1: ヘルパー関数を追加**

`crates/fulgur/src/convert.rs` の既存ヘルパー（`extract_block_id` 近辺）の隣に追加:

```rust
fn extract_pagination_from_column_css(
    ctx: &ConvertContext,
    node: &blitz_dom::Node,
) -> crate::pageable::Pagination {
    use crate::pageable::{BreakInside, Pagination};
    let props = ctx.column_styles.get(&node.id).copied().unwrap_or_default();
    Pagination {
        break_inside: props.break_inside.unwrap_or(BreakInside::Auto),
        ..Pagination::default()
    }
}
```

> **Note:** `ConvertContext` の正確な借用方法、`blitz_dom::Node` の正確な型は既存の `extract_*` ヘルパー（例: `extract_block_id`、`has_column_span_all`）と同じスタイルで揃える。

**Step 2: 全 12 サイトに `.with_pagination(...)` を挿入**

grep で全サイトを列挙:

```bash
grep -n "BlockPageable::with_positioned_children" crates/fulgur/src/convert.rs
```

想定サイト（現在の tree）: lines 498, 543, 566, 584, 791, 847, 978, 1028, 1057, 1087, 1270（加えて `BlockPageable::new` サイトがあれば同様に処理）

各サイトで builder chain に `.with_pagination(extract_pagination_from_column_css(ctx, node))` を挿入:

```rust
// Before
let mut block = BlockPageable::with_positioned_children(children)
    .with_style(style)
    .with_visible(visible)
    .with_id(extract_block_id(node));

// After
let mut block = BlockPageable::with_positioned_children(children)
    .with_pagination(extract_pagination_from_column_css(ctx, node))
    .with_style(style)
    .with_visible(visible)
    .with_id(extract_block_id(node));
```

> **Note:** サイトによって `ctx` / `node` のスコープ名が違う可能性あり (`cx`, `parent_node` など)。まわりの `extract_block_id(node)` と同じ引数を渡せば基本的には OK。もし `node` binding がないサイトがあれば、直近の caller から渡す — ヘルパー追加ではなく inline で。

**Step 3: Run Task 1's integration probe**

Run: `cargo test -p fulgur --test break_inside_avoid`
Expected: PASS — `avoid_block_straddling_boundary_promotes_to_next_page` が2ページ生成する。

**Step 4: 全 fulgur スイート**

Run: `cargo test -p fulgur`
Expected: PASS — 既存テストも green を維持（`BreakInside::Auto` のデフォルトで `Pagination::default()` と完全一致）。

**Step 5: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(fulgur-ftp): wire break-inside through convert.rs"
```

---

### Task 4: Oversized-avoid の failing test

**Files:**

- Modify: `crates/fulgur/tests/break_inside_avoid.rs`

**Step 1: テストを追記**

```rust
/// 1ページより大きい avoid block は無限ループせず通常 split へ fallback。
#[test]
fn avoid_block_taller_than_page_falls_back_to_split() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .huge { height: 500pt; background: #036; break-inside: avoid; }
    </style></head><body>
      <div class="huge"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom_pt(200.0, 200.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected oversized avoid block to still paginate, got {} pages",
        page_count(&pdf)
    );
}
```

**Step 2: Run — Failure mode を確認**

Run: `cargo test -p fulgur --test break_inside_avoid avoid_block_taller_than_page -- --nocapture`
Expected: FAIL（1 ページで silent overflow、または paginate の Err(unsplit) パスで single-page として押し込まれる）。

**Step 3: Do not commit.** Task 5 の acceptance check。

---

### Task 5: oversized-avoid fallback 実装

**Files:**

- Modify: `crates/fulgur/src/pageable.rs`

**Step 1: `BlockPageable` に `page_height` フィールドを追加**

`pageable.rs:796` の struct に追加:

```rust
pub struct BlockPageable {
    // ... existing fields ...
    /// 最後の `wrap()` で受け取った `avail_height`。`find_split_point` で
    /// 「block 自体がどのページにも収まらない」ことを検出し、
    /// `break-inside: avoid` を無視して通常 split へ fall back するのに使う。
    page_height: Pt,
}
```

すべての constructor (`new`, `with_positioned_children`, `..Self` literal) で `page_height: 0.0` を初期化。

**Step 2: `wrap()` で `page_height` を記録**

`impl Pageable for BlockPageable { fn wrap(...)` (line 1528) の `_avail_height` を `avail_height` に戻し、body 冒頭で記録:

```rust
fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
    self.page_height = avail_height;
    // ... existing body ...
}
```

**Step 3: `find_split_point` の guard を差し替え**

`pageable.rs:880` 付近:

```rust
// Before
if self.pagination.break_inside == BreakInside::Avoid {
    return SplitDecision::NoSplit;
}

// After
if self.pagination.break_inside == BreakInside::Avoid {
    let total_height = self.cached_size.map(|s| s.height).unwrap_or(0.0);
    // page_height == 0.0 means wrap() was never called with a real page
    // budget (defensive) — treat as "honour avoid, no fallback available".
    if self.page_height <= 0.0 || total_height <= self.page_height {
        return SplitDecision::NoSplit;
    }
    // Fall through to normal splitting logic.
}
```

**Step 4: 両 integration test を run**

Run: `cargo test -p fulgur --test break_inside_avoid`
Expected: 2 tests PASS (`avoid_block_straddling_boundary_promotes_to_next_page` と `avoid_block_taller_than_page_falls_back_to_split`)。

**Step 5: 全 fulgur スイート**

Run: `cargo test -p fulgur`
Expected: 全 green。既存の `test_break_inside_avoid` (pageable.rs:3374 付近) が既に `block.wrap(200, 1000)` を呼んでいるので fallback invariant は保たれる。もし regress したら `block.wrap(...)` を追加。

**Step 6: Commit**

```bash
git add crates/fulgur/src/pageable.rs crates/fulgur/tests/break_inside_avoid.rs
git commit -m "feat(fulgur-ftp): fall back to split when avoid block exceeds page"
```

---

### Task 6: multicol ColumnGroup 内の avoid-child regression test

**Files:**

- Modify: `crates/fulgur/tests/break_inside_avoid.rs`

**Step 1: テストを追記**

```rust
/// ColumnGroup 内の avoid-child は `distribute` の whole placement で
/// 自動保護される。この挙動を regression-proof する。
#[test]
fn avoid_child_inside_multicol_fits_whole_column() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 300pt 400pt; margin: 10pt; }
        .mc { column-count: 2; column-gap: 10pt; }
        .block { height: 120pt; margin-bottom: 10pt; background: #ddd; }
        .keep { break-inside: avoid; }
    </style></head><body>
      <div class="mc">
        <div class="block"></div>
        <div class="block keep"></div>
        <div class="block"></div>
        <div class="block keep"></div>
      </div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom_pt(300.0, 400.0))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(page_count(&pdf) >= 1);
    assert!(page_count(&pdf) <= 2);
    assert!(pdf.len() > 500, "PDF looks truncated");
}
```

**Step 2: Run**

Run: `cargo test -p fulgur --test break_inside_avoid avoid_child_inside_multicol`
Expected: PASS（コード変更不要 — 現在の挙動を lock in するテスト）。

**Step 3: Commit**

```bash
git add crates/fulgur/tests/break_inside_avoid.rs
git commit -m "test(fulgur-ftp): lock in break-inside: avoid in multicol"
```

---

### Task 7: blitz_adapter の extract_column_style_table テストに break-inside ケースを追加

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 1: 既存 `extract_column_style_table` test (line 2580 付近) の隣に追加**

`column_css.rs` の parser 単体は Task 2 でテスト済み。ここでは blitz 統合経由で node.id にマップされることを保証:

```rust
#[test]
fn extract_column_style_table_populates_break_inside() {
    let html = r#"<!doctype html><html><head><style>
        .k { break-inside: avoid; }
    </style></head><body>
      <div class="k" id="k"></div>
    </body></html>"#;
    let mut doc = parse(html, 400.0, &[]);
    resolve(&mut doc);
    use std::ops::Deref;
    let keep_id = find_element_by_attr_id(doc.deref(), "k");
    let table = extract_column_style_table(&doc);
    let props = table.get(&keep_id).expect("keep node in table");
    assert_eq!(props.break_inside, Some(crate::pageable::BreakInside::Avoid));
}
```

> **Note:** 周辺の test の parse/resolve 呼び方と揃える（line 2580 の既存 test を参考）。`find_element_by_attr_id(doc.deref(), "k")` は sibling test (`extract_column_style_table_picks_up_inline_and_stylesheet`) と同じ helper パターン。

**Step 2: Run**

Run: `cargo test -p fulgur --lib blitz_adapter::tests::extract_column_style_table_populates_break_inside`
Expected: PASS。

**Step 3: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "test(fulgur-ftp): break-inside roundtrip via extract_column_style_table"
```

---

### Task 8: Lint, format, final verification

**Files:** none (verification only)

**Step 1: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -40`
Expected: No warnings.

**Step 2: Formatting**

Run: `cargo fmt --check`
Expected: Clean。汚れていれば `cargo fmt` → `git diff` 確認。

**Step 3: 全 workspace テスト**

Run: `cargo test`
Expected: fulgur / fulgur-cli / fulgur-vrt すべて green。

**Step 4: Markdown lint**

Run: `npx markdownlint-cli2 '**/*.md'`
Expected: No errors。

**Step 5: Final commit（fmt / clippy が動かしたもの）**

```bash
git add -u
git commit -m "chore(fulgur-ftp): clippy + fmt polish"
```

---

## Out of scope for this PR

- **`break-before` / `break-after` 配線** — `fulgur-4zje` で別途起票済み。本 PR のヘルパー `extract_pagination_from_column_css` を拡張する形で実装される。
- **`avoid-page` vs `avoid-column` 軸分離** — Phase B。現在は collapse。
- **Cross-page ColumnGroup pagination** — `fulgur-6q5` / `fulgur-wfd` でカバー済み。
- **External `<link rel=stylesheet>` harvesting** — `fulgur-s5ro` でカバー済み。

## Acceptance checklist

- [ ] `cargo test -p fulgur --test break_inside_avoid` — 3 tests pass
- [ ] `cargo test -p fulgur` — 全スイート green（既存 regression なし）
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — clean
- [ ] `cargo fmt --check` — clean
- [ ] `column_css.rs` のモジュールコメントが「`break-inside` is handled here」に更新されている
- [ ] `bd close fulgur-ftp` — worktree merge 後
