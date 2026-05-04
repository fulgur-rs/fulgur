# grid/flex row cross-page co-split Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** grid/flex の並列 sibling cell が page boundary をまたぐとき、全 cell を同じ page から開始して co-split する（recursion・非 recursion 両パス）。

**Architecture:** `fragment_block_subtree` の子ループ先頭に `RowState` 構造体を追加。flex/grid コンテナ内の並列 sibling（Taffy `location.y` が同じ）を検出し、2 番目以降の cell を row 開始時の `(page_index, cursor_y, page_start_y, page_taffy_origin)` から復元する。parent fragment 重複は `emitted_parent_pages: BTreeSet<u32>` で抑制。row 終了後は全 cell の `max(end_page, end_cursor_y)` に統合する。

**Tech Stack:** Rust, `crates/fulgur/src/pagination_layout.rs`, `crates/fulgur-vrt/fixtures/bugs/grid-row-promote-background.html`

---

### Task 1: 非 recursion grid cross-page co-split の failing test を追加

**Files:**
- Modify: `crates/fulgur/src/pagination_layout.rs` (テストモジュール末尾 `4917` 行の `}` の前)

**Step 1: failing test を書く**

`4917` 行目の `}` の直前（最後のテスト `position_absolute_body_direct_beyond_page_budget_extends_pages` の後）に追加：

```rust
    #[test]
    fn grid_row_leaf_cells_cosplit_across_page_boundary() {
        // 2-col grid, each leaf cell 60px tall.
        // spacer 80px pushes grid to y=80 on a 100px page.
        // pre-fix: cell 2 pushed to page 1 at y=0 (whole cell).
        // post-fix: both cells split — page 0 y=80..100 (20px),
        //           page 1 y=0..40 (40px).
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 80px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div style="height: 60px; width: 100px"></div>
                <div style="height: 60px; width: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0, &table);

        // collect all 60px-tall, 100px-wide fragments (the two leaf cells)
        let mut frags: Vec<(u32, f32, f32)> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| (f.height - 60.0).abs() < 0.5 && (f.width - 100.0).abs() < 0.5)
            .map(|f| (f.page_index, f.x, f.y))
            .collect();
        frags.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // pre-fix: only 2 frags (cell 1 on page 0, cell 2 on page 1 whole)
        // post-fix: 4 frags — each cell split into page-0 and page-1 slices
        // For now assert both cells appear on page 0 (top slice exists)
        let on_page0: Vec<_> = frags.iter().filter(|(p, _, _)| *p == 0).collect();
        assert_eq!(
            on_page0.len(),
            2,
            "both leaf cells must have a fragment on page 0 (co-split); frags={frags:?}"
        );
        // Both page-0 slices start at the same y (row top on page 0)
        let ys: Vec<f32> = on_page0.iter().map(|(_, _, y)| *y).collect();
        assert!(
            (ys[0] - ys[1]).abs() < 0.5,
            "page-0 fragments must share the same y (parallel row); ys={ys:?}"
        );
    }

    #[test]
    fn flex_row_leaf_cells_cosplit_across_page_boundary() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 80px"></div>
              <div style="display: flex; width: 200px;">
                <div style="height: 60px; width: 100px; flex: 0 0 100px"></div>
                <div style="height: 60px; width: 100px; flex: 0 0 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0, &table);

        let mut frags: Vec<(u32, f32, f32)> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| (f.height - 60.0).abs() < 0.5 && (f.width - 100.0).abs() < 0.5)
            .map(|f| (f.page_index, f.x, f.y))
            .collect();
        frags.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let on_page0: Vec<_> = frags.iter().filter(|(p, _, _)| *p == 0).collect();
        assert_eq!(
            on_page0.len(),
            2,
            "both leaf cells must have a fragment on page 0 (co-split); frags={frags:?}"
        );
        let ys: Vec<f32> = on_page0.iter().map(|(_, _, y)| *y).collect();
        assert!(
            (ys[0] - ys[1]).abs() < 0.5,
            "page-0 fragments must share the same y; ys={ys:?}"
        );
    }
```

**Step 2: テストが fail することを確認**

```bash
cd /home/ubuntu/fulgur/.worktrees/fix/fulgur-ysms-grid-cosplit
cargo test -p fulgur --lib grid_row_leaf_cells_cosplit 2>&1 | tail -20
cargo test -p fulgur --lib flex_row_leaf_cells_cosplit 2>&1 | tail -20
```

Expected: FAIL（`assertion failed: on_page0.len() == 2`）

**Step 3: commit (red)**

```bash
git add crates/fulgur/src/pagination_layout.rs
git commit -m "test(pagination): add failing cross-page co-split tests for leaf grid/flex cells (fulgur-ysms)"
```

---

### Task 2: recursion cross-page co-split の failing test を追加

**Files:**
- Modify: `crates/fulgur/src/pagination_layout.rs`

**Step 1: failing test を書く**（Task 1 の 2 テストの後に追加）

```rust
    #[test]
    fn grid_row_recursive_cells_cosplit_across_page_boundary() {
        // 2-col grid, each cell contains 2 inner divs (40px each) = 80px total.
        // spacer 70px, page_height 100px → grid starts at y=70.
        // row bottom = 70+80 = 150 > 100 → cross-page.
        // pre-fix: cell 2 recursion starts at page 1 cursor=0 → inner divs at y=0,40 on page 1.
        // post-fix: both cells start recursion at page 0 cursor=70 → split at page boundary.
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 70px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div style="width: 100px">
                  <div style="height: 40px; width: 100px"></div>
                  <div style="height: 40px; width: 100px"></div>
                </div>
                <div style="width: 100px">
                  <div style="height: 40px; width: 100px"></div>
                  <div style="height: 40px; width: 100px"></div>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0, &table);

        // inner divs: 40px tall, 100px wide
        let mut inner: Vec<(u32, f32, f32)> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| (f.height - 40.0).abs() < 0.5 && (f.width - 100.0).abs() < 0.5)
            .map(|f| (f.page_index, f.x, f.y))
            .collect();
        inner.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // Each column has 2 inner divs. First inner div of each column
        // starts at y=70 on page 0 (the row's start).
        let first_divs_page0: Vec<_> = inner.iter().filter(|(p, _, y)| *p == 0 && *y > 60.0).collect();
        assert_eq!(
            first_divs_page0.len(),
            2,
            "both columns' first inner divs must appear on page 0; inner={inner:?}"
        );
    }

    #[test]
    fn flex_row_recursive_cells_cosplit_across_page_boundary() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 70px"></div>
              <div style="display: flex; width: 200px;">
                <div style="width: 100px; flex: 0 0 100px">
                  <div style="height: 40px; width: 100px"></div>
                  <div style="height: 40px; width: 100px"></div>
                </div>
                <div style="width: 100px; flex: 0 0 100px">
                  <div style="height: 40px; width: 100px"></div>
                  <div style="height: 40px; width: 100px"></div>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0, &table);

        let mut inner: Vec<(u32, f32, f32)> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| (f.height - 40.0).abs() < 0.5 && (f.width - 100.0).abs() < 0.5)
            .map(|f| (f.page_index, f.x, f.y))
            .collect();
        inner.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let first_divs_page0: Vec<_> = inner.iter().filter(|(p, _, y)| *p == 0 && *y > 60.0).collect();
        assert_eq!(
            first_divs_page0.len(),
            2,
            "both columns' first inner divs must appear on page 0; inner={inner:?}"
        );
    }
```

**Step 2: fail 確認**

```bash
cargo test -p fulgur --lib grid_row_recursive_cells_cosplit 2>&1 | tail -15
cargo test -p fulgur --lib flex_row_recursive_cells_cosplit 2>&1 | tail -15
```

Expected: FAIL

**Step 3: commit**

```bash
git add crates/fulgur/src/pagination_layout.rs
git commit -m "test(pagination): add failing cross-page co-split tests for recursive grid/flex cells (fulgur-ysms)"
```

---

### Task 3: RowState 構造体と row group 検出を実装

**Files:**
- Modify: `crates/fulgur/src/pagination_layout.rs`

`fragment_block_subtree` 関数本体（`fn fragment_block_subtree(` の定義ブロック内）を修正する。

**Step 1: use 追加**

ファイル先頭の `use` ブロックに追加（`use std::collections::BTreeMap;` の行付近）：

```rust
use std::collections::BTreeSet;
```

**Step 2: RowState 構造体を関数の直前に追加**

`fn fragment_block_subtree(` の直前（`/// In-place mid-element split` コメントより前）：

```rust
/// Row-level state for grid/flex parallel-sibling co-split (fulgur-ysms).
///
/// Saved once at the first cell of each row; subsequent cells in the same
/// row restore from `start_*` so their recursion begins at the same
/// (page, cursor) as the first cell did. After all cells in the row are
/// processed the outer cursor advances to `max_end_*`.
struct RowState {
    start_page:              u32,
    start_cursor_y:          f32,
    start_page_start_y:      f32,
    start_page_taffy_origin: f32,
    max_end_page:            u32,
    max_end_cursor_y:        f32,
    /// Taffy `location.y` of the first cell in this row.
    row_top:    f32,
    /// Max `location.y + height` seen across cells in this row.
    row_bottom: f32,
    /// Pages for which a parent fragment has already been emitted this row,
    /// to avoid duplicate entries when multiple cells cross the same boundary.
    emitted_parent_pages: BTreeSet<u32>,
}
```

**Step 3: `let mut row_state: Option<RowState> = None;` を追加**

`fragment_block_subtree` 関数内のローカル変数初期化ブロック（`let mut prev_used_page` あたり）に追加：

```rust
    // fulgur-ysms: row-level co-split state for flex/grid containers.
    let mut row_state: Option<RowState> = None;
```

**Step 4: ループ先頭（`origin_pending_target_y.take()` の前）に row group 検出を追加**

`let this_top_in_parent = layout.location.y;` の直後、`if let Some(mut target_y) = origin_pending_target_y.take()` の直前：

```rust
        // fulgur-ysms: row-level co-split for flex/grid containers.
        // Must run BEFORE origin_pending_target_y is consumed so that
        // restoring page_taffy_origin is consistent.
        if allow_same_row_rebase {
            if let Some(ref rs) = row_state {
                if this_top_in_parent < rs.row_bottom - 0.5 {
                    // Same row: restore to row-start state.
                    page_index = rs.start_page;
                    cursor_y = rs.start_cursor_y;
                    page_start_y = rs.start_page_start_y;
                    page_taffy_origin = rs.start_page_taffy_origin;
                    // Discard pending origin adjustments from the previous
                    // cell — restored page_taffy_origin makes them stale.
                    origin_pending_target_y = None;
                    origin_pending_same_row = None;
                } else {
                    // New row: advance outer cursor to the row's max end.
                    let rs = row_state.take().unwrap();
                    page_index = rs.max_end_page;
                    cursor_y = rs.max_end_cursor_y;
                    page_start_y = if rs.max_end_page > rs.start_page { 0.0 } else { rs.start_page_start_y };
                }
            }
            if row_state.is_none() {
                // First cell in a new row: snapshot current state.
                row_state = Some(RowState {
                    start_page: page_index,
                    start_cursor_y: cursor_y,
                    start_page_start_y: page_start_y,
                    start_page_taffy_origin: page_taffy_origin,
                    max_end_page: page_index,
                    max_end_cursor_y: cursor_y,
                    row_top: this_top_in_parent,
                    row_bottom: this_top_in_parent + child_h,
                    emitted_parent_pages: BTreeSet::new(),
                });
            } else if let Some(ref mut rs) = row_state {
                rs.row_bottom = rs.row_bottom.max(this_top_in_parent + child_h);
            }
        }
```

**Step 5: compile 確認**

```bash
cargo check -p fulgur 2>&1 | tail -20
```

Expected: 警告のみ、エラーなし

**Step 6: commit**

```bash
git add crates/fulgur/src/pagination_layout.rs
git commit -m "feat(pagination): add RowState struct and row-group detection for grid/flex co-split (fulgur-ysms)"
```

---

### Task 4: parent fragment dedup を実装

**Files:**
- Modify: `crates/fulgur/src/pagination_layout.rs`

`fragment_block_subtree` 内で `geometry.entry(parent_id)` に push する箇所が 3 つある（recursion パスの pre_recursion_page emit、intermediate pages ループ、非 recursion パスの strip-overflow 直前）。これらを dedup する。

**Step 1: dedup ヘルパーマクロを定義**（関数直前またはファイル先頭の適切な場所）

ヘルパー関数として、dedup 付き push を実装する。`fragment_block_subtree` 内のローカルクロージャとして定義する（`let mut row_state` の下）：

```rust
    // fulgur-ysms: dedup-aware parent fragment push.
    // Uses row_state.emitted_parent_pages when inside a row; otherwise
    // always emits (block-flow parents never duplicate on the same page).
    // Defined as a macro-like inline via a closure that borrows both
    // geometry and row_state — but since Rust closures can't borrow both
    // mutably, we use a standalone helper invoked with explicit args.
```

実際には closure で両方 mutably borrow できないため、インライン `if` で書く。各 push 箇所を以下のパターンで置き換える：

```rust
// Before (example):
geometry.entry(parent_id).or_default().fragments.push(Fragment {
    page_index: pre_recursion_page,
    ...
});

// After:
let should_emit_parent = row_state.as_mut()
    .map(|rs| rs.emitted_parent_pages.insert(pre_recursion_page))
    .unwrap_or(true);
if should_emit_parent {
    geometry.entry(parent_id).or_default().fragments.push(Fragment {
        page_index: pre_recursion_page,
        ...
    });
}
```

**修正対象の 3 箇所**（行番号は現在のファイルに基づく概算、`git grep` で正確に特定すること）：

1. **recursion パス: pre_recursion_page フラグメント**（`if page_index > pre_recursion_page` ブロック内、`let logical_height = ...` の後の push）
2. **recursion パス: intermediate pages ループ**（`for p in (pre_recursion_page + 1)..page_index` の中の push）— ループ変数 `p` を使って dedup
3. **非 recursion パス: strip-overflow 直前フラグメント**（`if child_page_y > page_start_y && child_page_y + child_h > page_height_px` ブロック内の push）

**Step 2: 各箇所を正確に特定**

```bash
grep -n "entry(parent_id)" crates/fulgur/src/pagination_layout.rs
```

行番号を確認して上記パターンで置き換える。

**Step 3: compile**

```bash
cargo check -p fulgur 2>&1 | tail -20
```

**Step 4: commit**

```bash
git add crates/fulgur/src/pagination_layout.rs
git commit -m "feat(pagination): dedup parent fragments per page within grid/flex row (fulgur-ysms)"
```

---

### Task 5: row end 集約（max end accumulation）を実装

**Files:**
- Modify: `crates/fulgur/src/pagination_layout.rs`

**Step 1: 各 cell 処理後に max 更新**

子ループ内の `if !is_float { prev_used_page = ... }` の直前（`continue` の前）に追加（recursion パスの `continue` と非 recursion パスの `cursor_y = cursor_y.max(...)` の後）：

recursion パスの `continue` 直前：

```rust
            // fulgur-ysms: update row max-end after this cell's recursion.
            if let Some(ref mut rs) = row_state {
                if page_index > rs.max_end_page
                    || (page_index == rs.max_end_page && cursor_y > rs.max_end_cursor_y)
                {
                    rs.max_end_page = page_index;
                    rs.max_end_cursor_y = cursor_y;
                }
            }
```

非 recursion パスの `cursor_y = cursor_y.max(child_page_y + child_h);` の直後にも同じブロックを追加。

**Step 2: ループ後（trailing close の前）に row を確定**

`// Close the parent's fragment for the final page span.` コメントの直前に追加：

```rust
    // fulgur-ysms: finalize any open row — advance to the max end state
    // reached across all parallel sibling cells.
    if let Some(rs) = row_state.take() {
        page_index = rs.max_end_page;
        cursor_y = rs.max_end_cursor_y;
        if rs.max_end_page > rs.start_page {
            page_start_y = 0.0;
        }
    }
```

**Step 3: テスト実行**

```bash
cargo test -p fulgur --lib grid_row_leaf_cells_cosplit 2>&1 | tail -10
cargo test -p fulgur --lib flex_row_leaf_cells_cosplit 2>&1 | tail -10
cargo test -p fulgur --lib grid_row_recursive_cells_cosplit 2>&1 | tail -10
cargo test -p fulgur --lib flex_row_recursive_cells_cosplit 2>&1 | tail -10
```

Expected: 4 本すべて PASS

**Step 4: 既存テスト全件確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 全件 pass（0 failed）

**Step 5: commit**

```bash
git add crates/fulgur/src/pagination_layout.rs
git commit -m "feat(pagination): accumulate row max-end and finalize after last parallel sibling (fulgur-ysms)"
```

---

### Task 6: VRT fixture 修正と golden 再生成

**Files:**
- Modify: `crates/fulgur-vrt/fixtures/bugs/grid-row-promote-background.html`

**Step 1: spacer を 800pt → 660pt に変更**

```html
<!-- Before -->
.spacer { height: 800pt; background: #eef; }

<!-- After -->
.spacer { height: 660pt; background: #eef; }
```

A4 content ≈ 728pt、grid margin 12pt → grid start ≈ 672pt、card ≈ 78pt → bottom ≈ 750pt（page boundary をまたぐ）。

**Step 2: VRT を update モードで実行**

```bash
cd /home/ubuntu/fulgur/.worktrees/fix/fulgur-ysms-grid-cosplit
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt 2>&1 | tail -20
```

Expected: golden が更新される

**Step 3: VRT を通常モードで再実行して pass 確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
cargo test -p fulgur-vrt 2>&1 | tail -10
```

Expected: all pass

**Step 4: commit**

```bash
git add crates/fulgur-vrt/fixtures/bugs/grid-row-promote-background.html
git add crates/fulgur-vrt/goldens/
git commit -m "fix(vrt): update grid-row-promote-background fixture for cross-page co-split (fulgur-ysms)"
```

---

### Task 7: 全テスト最終確認

**Step 1: unit tests**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 0 failed

**Step 2: integration tests**

```bash
cargo test -p fulgur 2>&1 | tail -5
```

**Step 3: clippy**

```bash
cargo clippy -p fulgur 2>&1 | tail -10
```

Expected: no errors (warnings ok)

**Step 4: VRT**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
cargo test -p fulgur-vrt 2>&1 | tail -5
```

Expected: all pass
