# Grid/Flex Row Co-split Implementation Plan (fulgur-u0p0)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 同じ row が page boundary をまたぐとき、grid/flex の parallel siblings を **両方** 同じ row-relative y で分割描画する (片方を次ページに promote しない)。

**Architecture:** `fragment_block_subtree` の recursion 入り口で、`origin_pending_same_row` 経由で rebase された parallel sibling については、recursion の `cursor_in` 引数に **rebase 後の `child_page_y`** を渡す (現状は parent の `cursor_y` = row max bottom を渡している)。これにより同じ row の各 cell が同じ y から strip 評価され、true co-split が実現する。recursion 復帰後は `cursor_y / page_index` を全 cell の max にまとめる。

**Tech Stack:** Rust workspace (fulgur library + fulgur-vrt reftest crate)。既存の `pagination_layout.rs` 内の `fragment_block_subtree` のみ変更。

---

## Task 1: 失敗 unit test (grid co-split)

**Files:**
- Modify: `crates/fulgur/src/pagination_layout.rs` (テストモジュール末尾、line ~4047 付近)

**Step 1: 失敗テストを追加**

`#[test]` を `pagination_layout::tests` 末尾に追加:

```rust
#[test]
fn fragment_block_subtree_grid_row_co_splits_at_same_y() {
    // 1 row の 2 cell が 1 page strip を超える case。
    // pre-fix: card1 が page 0 内で split → card2 は page 1 へ promote
    //   (page 0 に card2 上半分が出ない / page 1 上部に空白)
    // post-fix: 両 cell とも page 0 + page 1 に同じ y で co-split
    let html = r#"
        <html><body style="margin: 0; padding: 0">
          <div style="height: 100px; width: 200px"></div>
          <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
            <div style="height: 250px; width: 100px"></div>
            <div style="height: 250px; width: 100px"></div>
          </div>
        </body></html>
    "#;
    let mut doc = parse(html, 600.0);
    let table = blitz_adapter::extract_column_style_table(&doc);
    let geom = super::run_pass_with_break_styles(doc.deref_mut(), 250.0, &table);

    // 100×100 もしくは 100×NN の cell fragment を集める。
    // 100pt 幅 × 任意高さ のフラグメントで filter (split で height < 250 になる)。
    let mut cells: Vec<(u32, f32, f32, f32)> = geom
        .values()
        .flat_map(|g| g.fragments.iter())
        .filter(|f| (f.width - 100.0).abs() < 0.5)
        .map(|f| (f.page_index, f.x, f.y, f.height))
        .collect();
    cells.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let page0_cells: Vec<_> = cells.iter().filter(|(p, _, _, _)| *p == 0).collect();
    let page1_cells: Vec<_> = cells.iter().filter(|(p, _, _, _)| *p == 1).collect();
    assert_eq!(
        page0_cells.len(),
        2,
        "expected both grid cells to have a page-0 fragment (co-split), got {cells:?}"
    );
    assert_eq!(
        page1_cells.len(),
        2,
        "expected both grid cells to have a page-1 fragment (co-split), got {cells:?}"
    );
    assert!(
        (page0_cells[0].2 - page0_cells[1].2).abs() < 0.5,
        "page-0 co-split cells must share y, got {cells:?}"
    );
    assert!(
        (page1_cells[0].2 - page1_cells[1].2).abs() < 0.5,
        "page-1 co-split cells must share y, got {cells:?}"
    );
}
```

**Step 2: 失敗を確認**

```bash
cd .worktrees/fulgur-u0p0
cargo test -p fulgur --lib fragment_block_subtree_grid_row_co_splits_at_same_y 2>&1 | tail -20
```

期待: 失敗 (page0_cells.len() = 1 が出るはず)

**Step 3: コミット**

```bash
git add crates/fulgur/src/pagination_layout.rs
git commit -m "test(pagination): add failing test for grid row co-split (fulgur-u0p0)"
```

---

## Task 2: 失敗 unit test (flex co-split)

**Files:**
- Modify: `crates/fulgur/src/pagination_layout.rs` (Task 1 のテスト直後)

**Step 1: flex 版テストを追加**

```rust
#[test]
fn fragment_block_subtree_flex_row_co_splits_at_same_y() {
    let html = r#"
        <html><body style="margin: 0; padding: 0">
          <div style="height: 100px; width: 200px"></div>
          <div style="display: flex; width: 200px;">
            <div style="height: 250px; width: 100px; flex: 0 0 100px"></div>
            <div style="height: 250px; width: 100px; flex: 0 0 100px"></div>
          </div>
        </body></html>
    "#;
    let mut doc = parse(html, 600.0);
    let table = blitz_adapter::extract_column_style_table(&doc);
    let geom = super::run_pass_with_break_styles(doc.deref_mut(), 250.0, &table);

    let mut cells: Vec<(u32, f32, f32, f32)> = geom
        .values()
        .flat_map(|g| g.fragments.iter())
        .filter(|f| (f.width - 100.0).abs() < 0.5)
        .map(|f| (f.page_index, f.x, f.y, f.height))
        .collect();
    cells.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let page0_cells: Vec<_> = cells.iter().filter(|(p, _, _, _)| *p == 0).collect();
    let page1_cells: Vec<_> = cells.iter().filter(|(p, _, _, _)| *p == 1).collect();
    assert_eq!(page0_cells.len(), 2, "{cells:?}");
    assert_eq!(page1_cells.len(), 2, "{cells:?}");
    assert!((page0_cells[0].2 - page0_cells[1].2).abs() < 0.5, "{cells:?}");
    assert!((page1_cells[0].2 - page1_cells[1].2).abs() < 0.5, "{cells:?}");
}
```

**Step 2: 失敗を確認**

```bash
cargo test -p fulgur --lib fragment_block_subtree_flex_row_co_splits_at_same_y 2>&1 | tail -20
```

期待: 失敗 (grid と同じパターン)

**Step 3: コミット**

```bash
git add crates/fulgur/src/pagination_layout.rs
git commit -m "test(pagination): add failing test for flex row co-split (fulgur-u0p0)"
```

---

## Task 3: parallel sibling フラグを recursion 入り口に伝搬

**Files:**
- Modify: `crates/fulgur/src/pagination_layout.rs:1530-1537` (origin_pending_* の take 部分)
- Modify: `crates/fulgur/src/pagination_layout.rs:1684-1699` (recursion 入り口)

**Step 1: same-row 消費フラグを追加**

`pagination_layout.rs:1530-1537` の `if let Some(mut target_y) = origin_pending_target_y.take() { ... }` ブロックを以下に置き換える:

```rust
// fulgur-u0p0: track whether THIS child consumed a same-row rebase
// — if so, the recursion below must enter from `child_page_y`
// (rebased to the row's y on the new page strip), not from the
// parent's `cursor_y` (which still holds the row's max bottom from
// the previous sibling). Without this hand-off, the recursion sees
// a tiny `available_strip = page_height_px - cursor_y` and
// promotes the entire sibling to a fresh page rather than co-
// splitting.
let mut child_is_same_row_sibling = false;
if let Some(mut target_y) = origin_pending_target_y.take() {
    if let Some((row_top, row_bottom, same_row_y)) = origin_pending_same_row.take()
        && this_top_in_parent < row_bottom - 0.5
    {
        target_y = same_row_y + (this_top_in_parent - row_top);
        child_is_same_row_sibling = true;
    }
    page_taffy_origin = this_top_in_parent - (target_y - page_start_y);
}
```

**Step 2: recursion 入り口で cursor_in を切り替え**

`pagination_layout.rs:1685` 付近、`let pre_recursion_page = page_index;` の直後に変更を加える。

現状の recursion 呼び出し:

```rust
let (np, nc) = fragment_block_subtree(
    geometry,
    doc,
    column_styles,
    used_page_names,
    child_id,
    child_w,
    child_x_in_body,
    page_index,
    cursor_y,
    page_height_px,
    depth + 1,
);
```

を以下に変更:

```rust
// fulgur-u0p0: parallel sibling co-split — when this child was
// rebased to a same-row y on the new page, enter the recursion
// from `child_page_y` (the rebased row-local y) rather than the
// outer `cursor_y` (which holds the row's max bottom advanced by
// the previous sibling). The recursion's strip-overflow gate uses
// `cursor_in` as the starting y, so passing the row's max bottom
// would force a premature next-page cut for every co-split cell.
let recursion_cursor_in = if child_is_same_row_sibling {
    child_page_y
} else {
    cursor_y
};
let (np, nc) = fragment_block_subtree(
    geometry,
    doc,
    column_styles,
    used_page_names,
    child_id,
    child_w,
    child_x_in_body,
    page_index,
    recursion_cursor_in,
    page_height_px,
    depth + 1,
);
```

**Step 3: recursion 復帰後の cursor_y を row max にまとめる**

`pagination_layout.rs:1700-1701` の現状:

```rust
page_index = np;
cursor_y = nc;
```

を以下に変更:

```rust
page_index = np;
// fulgur-u0p0: for parallel siblings, the recursion's returned
// cursor sits at the rebased y plane (row-local). The parent's
// outer cursor still tracks the previous sibling's bottom — keep
// the larger value so the next non-row sibling resumes from the
// row's full extent on the latest page reached.
cursor_y = if child_is_same_row_sibling && np == pre_recursion_page {
    cursor_y.max(nc)
} else {
    nc
};
```

**Step 4: テスト実行 (grid)**

```bash
cargo test -p fulgur --lib fragment_block_subtree_grid_row_co_splits_at_same_y 2>&1 | tail -15
```

期待: PASS

**Step 5: テスト実行 (flex)**

```bash
cargo test -p fulgur --lib fragment_block_subtree_flex_row_co_splits_at_same_y 2>&1 | tail -15
```

期待: PASS

**Step 6: 既存テスト regression 確認**

```bash
cargo test -p fulgur --lib pagination_layout::tests 2>&1 | tail -10
```

期待: 36 passed (元 34 + 新規 2)。失敗があれば各テストを個別に走らせて原因確認。

特に注意:
- `fragment_block_subtree_grid_later_row_*` (3902-): later row alignment — same-row フラグは「同じ row 内」のみ立つので影響しないはず
- `fragment_block_subtree_following_block_continues_after_split_*_tail` (3830-): row 後続の block sibling

**Step 7: コミット**

```bash
git add crates/fulgur/src/pagination_layout.rs
git commit -m "fix(pagination): co-split grid/flex parallel siblings at same row y (fulgur-u0p0)"
```

---

## Task 4: VRT fixture (grid-row-co-split.html)

**Files:**
- Create: `crates/fulgur-vrt/fixtures/bugs/grid-row-co-split.html`

**Step 1: fixture を作成**

```bash
cat > crates/fulgur-vrt/fixtures/bugs/grid-row-co-split.html << 'EOF'
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>VRT fixture: bugs/grid-row-co-split (fulgur-u0p0)</title>
<style>
  @page { size: A4; margin: 20mm 18mm; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  html, body { background: #fff; }
  /* Spacer pushes a 2-cell grid row across the page boundary. Both
     cells should appear on page 1 (top half) and page 2 (bottom half)
     at the same y — co-split — instead of card 2 promoting to page 2
     while card 1 splits in place. */
  .spacer { height: 600pt; background: #eef; }
  .grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 10pt;
    margin-top: 12pt;
  }
  .card {
    background: #fafafa;
    border: 1pt solid #888;
    border-radius: 8pt;
    padding: 12pt;
    height: 280pt;
  }
  .icon { width: 18pt; height: 18pt; background: #c33; margin-bottom: 6pt; }
  .body { width: 80%; height: 30pt; background: #999; }
</style>
</head><body>
<div class="spacer"></div>
<div class="grid">
  <div class="card"><div class="icon"></div><div class="body"></div></div>
  <div class="card"><div class="icon"></div><div class="body"></div></div>
</div>
</body></html>
EOF
```

**Step 2: golden 生成**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 \
  cargo test -p fulgur-vrt grid_row_co_split 2>&1 | tail -10
```

期待: golden が生成される (`crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-co-split.pdf`)

**Step 3: 生成された golden を視覚確認**

```bash
pdftocairo -png -r 100 crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-co-split.pdf /tmp/co-split-golden
ls /tmp/co-split-golden*.png
```

PNG を読んで page 1 に両 card の上半分があり、page 2 に両 card の下半分があることを確認 (Read tool で表示)。

**Step 4: 通常実行で byte-wise compare**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -10
```

期待: 全 fixture PASS (新規 fixture 含む)

**Step 5: コミット**

```bash
git add crates/fulgur-vrt/fixtures/bugs/grid-row-co-split.html crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-co-split.pdf
git commit -m "test(fulgur-vrt): add grid-row-co-split fixture (fulgur-u0p0)"
```

---

## Task 5: 既存 fixture grid-row-promote-background の golden 更新

**Files:**
- Modify: `crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-promote-background.pdf`

**Step 1: 現状再生成前に視覚確認**

```bash
pdftocairo -png -r 100 crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-promote-background.pdf /tmp/old-golden
ls /tmp/old-golden*.png
```

PNG を Read tool で確認。pre-fix の挙動 (page 1 に空白、page 2 に両 card) のはず。

**Step 2: 通常実行で fail を確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt grid_row_promote_background 2>&1 | tail -15
```

期待: 失敗 (golden と現行出力が byte 不一致)

**Step 3: golden 再生成**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 \
  cargo test -p fulgur-vrt grid_row_promote_background 2>&1 | tail -10
```

**Step 4: 新 golden を視覚確認 (acceptance #3)**

```bash
pdftocairo -png -r 100 crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-promote-background.pdf /tmp/new-golden
ls /tmp/new-golden*.png
```

PNG を Read tool で確認。**page 1 に Card 2 の上半分が描画されている** ことを確認 (issue acceptance #3)。

**Step 5: コミット**

```bash
git add crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-promote-background.pdf
git commit -m "test(fulgur-vrt): regenerate grid-row-promote-background golden after co-split fix (fulgur-u0p0)"
```

---

## Task 6: 全 lib テスト + WPT regression check

**Step 1: fulgur library 全 lib test**

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
```

期待: 全 PASS (元の通過数 + 2)

**Step 2: WPT bugs リグレッション**

```bash
cargo test -p fulgur-wpt --test wpt_lists -- wpt_list_bugs 2>&1 | tail -10
```

期待: 既存 PASS 数を維持

**Step 3: VRT 全件**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -10
```

期待: 全 PASS

**Step 4: clippy / fmt / markdownlint**

```bash
cargo fmt --check 2>&1 | tail -5
cargo clippy --workspace --all-targets 2>&1 | tail -20
npx markdownlint-cli2 'docs/plans/2026-05-04-fulgur-u0p0-grid-row-cosplit.md' 2>&1 | tail -10
```

期待: いずれも warning/error なし

**Step 5: 何らかの fail があれば修正してコミット**

ない場合は次の Task へ。

---

## Task 7: follow-up issue (table-row / multicol co-split)

**Step 1: beads に follow-up issue を作成**

```bash
bd create --title="grid/flex co-split を table-row と multicol へ拡張する" \
  --type=bug --priority=2 \
  --description="fulgur-u0p0 で grid/flex の row co-split が実装された。同じ問題 (parallel siblings の row 跨ぎ) は table-row (display: table-row, anonymous table boxes) と multi-column container (column-count > 1) でも発生しうる。

table-row は Taffy の table layout 経由なので is_flex_or_grid_container_node の判定が table parent でも有効になるか確認が必要。multicol は multicol_layout.rs に独自経路があり、別アプローチになる可能性が高い。

acceptance: table-row 跨ぎと multicol column 跨ぎで parallel cell が同じ row-relative y で co-split される。" \
  2>&1 | tail -5
```

**Step 2: 依存関係を追加 (本 issue を blocks)**

```bash
bd dep add <new-issue-id> fulgur-u0p0
```

**Step 3: コミット (なし — beads は dolt 管理なので git 不要)**

---

## Final: PR 準備

すべて green になったら:

```bash
git log --oneline main..HEAD
git status
```

## 実行方式

**Subagent-Driven (推奨)** — このセッションで Task 1 から順次 fresh subagent に投げる。Task 間で簡単なレビューを挟む。
