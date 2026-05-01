# Phase 4 PR 8i: convert → Drawables 直接生成 (dom_to_pageable 廃止) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** convert 層を「`Box<dyn Pageable>` を返して後段で `extract_drawables_from_pageable` するスキャフォールド構造」から「DOM walk 中に直接 `Drawables` の per-NodeId マップへ insert する flat 構造」へ移行し、`dom_to_pageable` / `extract_drawables_from_pageable` を削除する。

**Architecture:** `convert_node` / `convert_node_inner` の戻り値を `Box<dyn Pageable>` から `()` に変え、`out: &mut Drawables` 引数で side effect 化する。各 convert サブモジュール (block, replaced, inline_root, list_item, table, positioned, pseudo) も同様にシグネチャ変更。`transform` は `out.transforms` に before/after node_id スナップショット差分で descendants を埋める方式で直接 insert。`bookmark` / `string-set` / `counter-ops` / `running-element` の Pageable wrapper は v2 path で完全 vestigial であることが判明したので convert 層からは全削除（snapshot/side-channel が独立して機能している）。

**Tech Stack:** Rust, fulgur core (`crates/fulgur/src/convert/*.rs`, `drawables.rs`), Blitz/Taffy (上流データ), VRT (`crates/fulgur-vrt`) で byte-identical 検証。

---

## Pre-flight 状況メモ

- **branch base**: `feat/phase4-pr8i-convert-drawables`（`origin/feat/phase4-pr8c-pagination-delete` から派生、PR 8h まで含む）
- **worktree**: `/home/ubuntu/fulgur/.worktrees/phase4-pr8i`
- **baseline**: `cargo test -p fulgur --lib` = 811 tests pass
- **依存先**: fulgur-pzu1 (PR 8h) merged
- **後続**: fulgur-3z6n (PR 8j: pageable.rs 全削除) は本 PR 完了後

## 重要な発見事項

1. **marker wrapper 群は v2 で完全 vestigial**:
   - `BookmarkMarkerWrapperPageable`: `dom_to_drawables` line 198 が `bookmark_by_node` を **clone してから** convert を呼んでいるので、convert の中での drain も wrap も v2 出力 (`bookmark_anchors`) には影響しない
   - `StringSetWrapperPageable` / `CounterOpWrapperPageable`: `string_set_by_node` / `counter_ops_by_node` は `engine.rs` から `pagination_layout::run_pass_with_break_styles` に直接渡される（fragmenter side-channel）。convert 経由は不要
   - `RunningElementWrapperPageable`: `RunningElementStore` は別 channel
   → convert 層の `maybe_prepend_string_set` / `maybe_prepend_counter_ops` / `convert_node` 末尾の bookmark wrap / `positioned.rs` の running wrap / `emit_orphan_*` / `take_running_marker` を **全削除**
2. **`PositionedChild` は v2 で不要**: Drawables は per-NodeId フラット、render は `pagination_geometry` / `multicol_geometry` を見る → in-flow / out-of-flow の区別 / 子座標保持はすべて削除可能。convert は単純に DOM walk してすべての NodeId を Drawables に登録
3. **`extract_paragraph`** は `Box<ParagraphPageable>` を返している → `ParagraphEntry` を直接返す形にリファクタ
4. **inline-box 内容の型は本 PR で変更しない**: `LineItem::InlineBox.content` は `Box<dyn Pageable>` (`paragraph.rs:147 type InlineBoxContent`) のまま。
   - 理由: `paragraph.rs::draw_shaped_lines` の InlineBox arm は v2 path (`InlineBoxRenderCtx` 提供時) に `dispatch_inline_box_content(content_id, ...)` で **Drawables 経由 dispatch** する設計が PR 8g で導入済。`ib.content.draw()` フォールバックは `ParagraphPageable::draw` (Pageable trait method) 経由の legacy path 用で、現在 production caller は無い
   - したがって `convert_inline_box_node` は `Box::new(SpacerPageable::new(0.0))` 等の **placeholder Pageable** を返せばよい。ただし副作用として `convert_node(child_id, ..., out)` を呼んで inline-box subtree を Drawables に登録 → v2 dispatch が機能
   - inline-box content の型変更 (Box<dyn Pageable> → Option / NodeId) は PR 8j (pageable.rs 全削除) のスコープ
5. **`LineItem::InlineBox` の draw 経路 2 系統**:
   - production: `dispatch_inline_box_content(node_id, ...)` (`render.rs:618`) → 従って Drawables に inline-box subtree が必要
   - legacy: `ib.content.draw()` (`paragraph.rs:913` 付近) → Pageable trait method、PR 8j で削除予定

---

## Task 0: テストヘルパ `Engine::build_drawables_for_testing` を追加

**Files:**
- Modify: `crates/fulgur/src/engine.rs` (`build_pageable_for_testing_no_gcpm` の隣に併設)

**Step 0-1: 新ヘルパ追加（`build_pageable_for_testing_no_gcpm` を雛形に）**

```rust
#[doc(hidden)]
#[cfg(any(test, feature = "test-helpers"))]
pub fn build_drawables_for_testing(&self, html: &str) -> crate::drawables::Drawables {
    use crate::convert::{ConvertContext, dom_to_drawables};
    use crate::gcpm::running::RunningElementStore;
    use std::collections::HashMap;
    let mut doc = crate::blitz_adapter::parse(
        html,
        crate::convert::pt_to_px(self.config.content_width()),
        &[],
    );
    crate::blitz_adapter::resolve(&mut doc);
    let running_store = RunningElementStore::new();
    let mut ctx = ConvertContext {
        running_store: &running_store,
        assets: None,
        font_cache: HashMap::new(),
        string_set_by_node: HashMap::new(),
        counter_ops_by_node: HashMap::new(),
        bookmark_by_node: HashMap::new(),
        column_styles: crate::column_css::ColumnStyleTable::new(),
        multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
        pagination_geometry: crate::pagination_layout::PaginationGeometryTable::new(),
        link_cache: Default::default(),
        viewport_size_px: None,
    };
    dom_to_drawables(&doc, &mut ctx)
}
```

(↑ ここでは旧 `dom_to_drawables` 経路を使う。Task 1 以降で本体が書き換わる。)

**Step 0-2: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | tail -5
```
Expected: 0 errors / 0 warnings on this addition.

**Step 0-3: コミット**

```bash
git add crates/fulgur/src/engine.rs
git commit -m "test(engine): add build_drawables_for_testing helper for PR 8i tests"
```

---

## Task 1: convert/replaced.rs — Drawables 化（最小依存サブモジュール）

`replaced` は他のサブモジュールに依存せず、テストも局所的なので最初に着手。

**Files:**
- Modify: `crates/fulgur/src/convert/replaced.rs` (全面書き換え)
- Modify: `crates/fulgur/src/convert/mod.rs` (try_convert 呼び出し側 only。本体は Task 7 でまとめて書き換えるが、ここでは shim を入れる)

**Step 1-1: `try_convert` シグネチャを `-> bool, out: &mut Drawables` に変更**

旧:
```rust
pub(super) fn try_convert(
    doc: &BaseDocument, node_id: usize, ctx: &mut ConvertContext<'_>,
) -> Option<Box<dyn Pageable>>
```
新:
```rust
pub(super) fn try_convert(
    doc: &BaseDocument, node_id: usize, ctx: &mut ConvertContext<'_>,
    out: &mut crate::drawables::Drawables,
) -> bool
```

`make_image_pageable` → `make_image_entry` に rename し、`ImagePageable` 構築の代わりに `ImageEntry` を直接 insert。`SvgPageable` 同様。`wrap_replaced_in_block_style` で BlockPageable に巻く処理は削除し、代わりに `BlockEntry` も同時 insert（border/background paint を維持するため）。

**Step 1-2: テスト 3 件を Drawables 検証に**

`tests` モジュール内の `dom_to_pageable` 呼び出しを `Engine::build_drawables_for_testing` に変えて、`drawables.images` / `drawables.svgs` を検証する形へ。

**Step 1-3: convert/mod.rs の dispatch 経由でも replaced が呼ばれるように shim**

これは Task 7 (mod.rs 書き換え) でまとめてやるので、Task 1 単体ではビルドが通らない。`#[cfg(test)]` ガードでテスト隔離して Task 7 までは旧 path を維持する手もあるが、Big-bang 方式を採用するため、ここでは **commit を Task 7 完了時にまとめる**。Task 1 ローカルブランチで進めるが push しない。

**Step 1-4: ビルド確認は Task 7 と一緒に**

→ Task 1 単体での test 実行はスキップ。Task 7 完了後に `cargo test -p fulgur --lib` で総合検証。

---

## Task 2-6: 残りサブモジュール書き換え (Big-bang Phase)

複数モジュール間の循環依存（block ↔ positioned ↔ pseudo, list_item → block, table → block, inline_root → pseudo）により、**個別 commit では compile が通らない**。Task 1〜6 を 1 つの作業ブロックとして進め、Task 7 (mod.rs) と一緒に大コミットする。

各タスクは subagent に並列発注可能だが、ファイル間整合（共通シグネチャ・共通ヘルパ）に注意。

### Task 2: convert/inline_root.rs

- `try_convert` シグネチャ変更 (`-> bool, &mut Drawables`)
- `extract_paragraph` を `ParagraphEntry` (lines + opacity + visible + id) を直接生成する形にリファクタ。`ParagraphPageable` instance の構築は inline-box content 用に必要なら残す（`Box::new(ParagraphPageable {...})` で空 lines の placeholder 等）が、最終的な格納先は `out.paragraphs[node_id]`
- `convert_inline_box_node` は **placeholder Pageable を返す**（例: `Box::new(SpacerPageable::new(0.0))`）+ `convert_node(node_id, ctx, depth+1, out)` で inline-box subtree を Drawables に登録する側路を残す。content フィールドの実体は v2 dispatch では不要
- absolutely-positioned pseudo の早期 return (`SpacerPageable` placeholder) は既存挙動どおり
- `recalculate_paragraph_line_boxes` 等のヘルパは `&mut [ShapedLine]` のまま流用可

### Task 3: convert/list_marker.rs + convert/list_item.rs

- list_marker.rs: `extract_marker_lines` 等は paragraph.rs と同じ shaping ロジックなので返り値型は今のまま `Vec<ShapedLine>` 等で OK
- list_item.rs: `try_convert` シグネチャ変更。`ListItemEntry` を `out.list_items` に insert、body は `convert_node` 再帰で Drawables に展開
- インラインマーカー注入 (`inject_inside_marker_item_into_children`) は paragraph 内で完結

### Task 4: convert/table.rs

- `try_convert` シグネチャ変更
- `TableEntry` を `out.tables` に insert
- `clipping` のとき `before = collect_drawables_node_ids(out)`、cells を `convert_node` 再帰、`after - before - {node_id}` で `clip_descendants` 確定

### Task 5: convert/pseudo.rs

- `build_pseudo_image` / `build_block_pseudo_images` / `build_inline_pseudo_image` 等を `Drawables` に直接 insert する形に書き換え
- `wrap_with_pseudo_content` / `wrap_with_block_pseudo_images` / `inject_inline_pseudo_images` は「pseudo 部分の image/svg/paragraph を Drawables に追加で insert する」ヘルパに変更（実体を生成するだけで、ラッピングはしない）
- `attach_link_to_inline_image` は paragraph LineItem 内の link 注入なので影響小
- テスト 5 件移行

### Task 6: convert/positioned.rs + convert/block.rs

- positioned: `collect_positioned_children` → `walk_children_into_drawables(doc, parent_id, ctx, depth, out)` に書き換え。in-flow / out-of-flow の区別を撤廃し、すべての子 NodeId を Drawables に登録（`convert_node` 再帰呼び出し）。`build_absolute_pseudo_children` / `build_absolute_non_pseudo_children` / `build_absolute_children` も Drawables 直接 insert に
- block: `convert` シグネチャ変更。`BlockEntry { style, ..., layout_size: ... }` を `out.block_styles` に insert。clipping / opacity_scope のとき before snapshot → children 再帰 → after 差分で descendants 確定

---

## Task 7: convert/mod.rs エントリ書き換え + 旧関数削除

**Files:** `crates/fulgur/src/convert/mod.rs`

**Step 7-1: `dom_to_drawables` を「直接 DOM walk」に書き換え**

```rust
pub fn dom_to_drawables(
    doc: &HtmlDocument,
    ctx: &mut ConvertContext<'_>,
) -> crate::drawables::Drawables {
    let bookmark_snapshot = ctx.bookmark_by_node.clone();
    let mut drawables = crate::drawables::Drawables::new();
    let root = doc.root_element();
    if std::env::var("FULGUR_DEBUG").is_ok() {
        debug_print_tree(doc.deref(), root.id, 0);
    }
    convert_node(doc.deref(), root.id, ctx, 0, &mut drawables);
    drawables.bookmark_anchors = extract_bookmark_anchors(doc, &bookmark_snapshot, ctx.assets);
    drawables.body_offset_pt = extract_body_offset_pt(doc);
    drawables.root_id = Some(root.id);
    drawables.body_id = find_body_id_in_dom(doc);
    drawables
}
```

**Step 7-2: `convert_node` 新シグネチャ**

```rust
fn convert_node(
    doc: &BaseDocument, node_id: usize, ctx: &mut ConvertContext<'_>,
    depth: usize, out: &mut crate::drawables::Drawables,
) {
    if depth >= MAX_DOM_DEPTH { return; }
    let before = collect_drawables_node_ids(out);
    convert_node_inner(doc, node_id, ctx, depth, out);
    record_multicol_rule(doc, node_id, ctx, out);
    record_transform(doc, node_id, &before, out);
    // bookmark / string-set / counter-ops / running の wrap/drain は削除
    // (v2 では vestigial; bookmark_by_node は dom_to_drawables 冒頭で snapshot 済)
}
```

**Step 7-3: `convert_node_inner` 新シグネチャ**

```rust
fn convert_node_inner(
    doc: &BaseDocument, node_id: usize, ctx: &mut ConvertContext<'_>,
    depth: usize, out: &mut crate::drawables::Drawables,
) {
    if list_item::try_convert(doc, node_id, ctx, depth, out) { return; }
    if table::try_convert(doc, node_id, ctx, depth, out) { return; }
    if replaced::try_convert(doc, node_id, ctx, out) { return; }
    if inline_root::try_convert(doc, node_id, ctx, depth, out) { return; }
    block::convert(doc, node_id, ctx, depth, out);
}
```

**Step 7-4: `record_transform` ヘルパ**

```rust
fn record_transform(
    doc: &BaseDocument, node_id: usize,
    before: &std::collections::BTreeSet<usize>,
    out: &mut crate::drawables::Drawables,
) {
    let Some(node) = doc.get_node(node_id) else { return; };
    let Some(styles) = node.primary_styles() else { return; };
    let (w, h) = size_in_pt(node.final_layout.size);
    let Some((matrix, origin)) = crate::blitz_adapter::compute_transform(&styles, w, h) else { return; };
    let after = collect_drawables_node_ids(out);
    let descendants: Vec<usize> = after.difference(before).copied().filter(|&id| id != node_id).collect();
    out.transforms.insert(node_id, crate::drawables::TransformEntry { matrix, origin, descendants });
}
```

**Step 7-5: `record_multicol_rule` ヘルパ**

`maybe_wrap_multicol_rule` のロジックを Drawables 直接 insert に変えたもの。`out.multicol_rules.insert(node_id, MulticolRuleEntry { rule, groups })` のみ。

**Step 7-6: 削除**

- `dom_to_pageable` 関数全体
- `extract_drawables_from_pageable` 関数全体
- `extract_drawables_tests` モジュール全体（Drawables を直接 insert するので `extract_*` 経由テストは無意味）
- `maybe_wrap_transform` / `maybe_wrap_multicol_rule` 旧版
- `maybe_prepend_string_set` / `maybe_prepend_counter_ops`
- `emit_orphan_string_set_markers` / `emit_counter_op_markers` / `emit_orphan_bookmark_marker`
- `take_running_marker`
- `convert_node` 末尾の `BookmarkMarkerWrapperPageable` ラップ
- `convert_node_inner` の旧本体
- `pageable` import から不要 wrapper 型 (`BookmarkMarkerWrapperPageable`, `CounterOpMarkerPageable`, `CounterOpWrapperPageable`, `RunningElementMarkerPageable`, `RunningElementWrapperPageable`, `StringSetPageable`, `StringSetWrapperPageable`, `TransformWrapperPageable`, `MulticolRulePageable`, `Pageable`, `PositionedChild`, `Size`) を整理

`collect_drawables_node_ids` は残す（`convert_node` / `block::convert` / `table::try_convert` で使用）。

**Step 7-7: convert/mod.rs 内の bookmark テスト群 + unit_oracle_tests を Drawables 化**

- `h1_wraps_block_with_bookmark_marker`, `h3_produces_level_3_marker`: これらは「bookmark wrap が起きるか」を Pageable tree 経由で確認していた。新世界では `dom_to_drawables` を呼んで `drawables.bookmark_anchors[node_id].label` を確認する形に書き換える
- `orphan_bookmark_marker_survives_empty_element` (line 1442-1513): 同様に書き換え。**重要**: 既存の `assert!(ctx.bookmark_by_node.is_empty(), "bookmark_by_node entry must be removed by the orphan-emit path")` (line ~1483) は **削除**する。新世界では convert は `bookmark_by_node` を drain しない（`dom_to_drawables` 冒頭で snapshot 済）ので、この assertion は意味を失う。代わりに `drawables.bookmark_anchors.contains_key(&div_id)` を assert
- `unit_oracle_tests::find_block_by_id`: Drawables 版に書き換え。`drawables.block_styles` を iterate して `id == "target"` を探す。`block.layout_size` の代わりに `BlockEntry.layout_size` を確認

---

## Task 8: 残テスト移行 (`pageable.rs`, `paragraph.rs`)

**Files:**
- Modify: `crates/fulgur/src/pageable.rs` (3箇所)
- Modify: `crates/fulgur/src/paragraph.rs` (1箇所)

**Step 8-1: pageable.rs:3023, 3074, 3119 の `dom_to_pageable` 呼び出し**

これらはおそらく Pageable tree の構造検証テスト。新世界ではテスト意義を失う可能性が高い。

選択肢:
- (a) `Engine::build_drawables_for_testing` 経由で Drawables を生成し、検証対象を「entry が存在するか」「style が期待値か」に変更
- (b) PR 8j で pageable.rs ごと消える前提なので、(a) が手間なら `#[ignore]` + コメントで PR 8j 削除予告

判断: テスト本数は 3 件と少ないので (a) で書き換える。具体的にどんな検証なのかは Task 開始時に Read で確認。

**Step 8-2: paragraph.rs:1649 の `dom_to_pageable` 呼び出し**

おそらく paragraph の line shaping を検証する test。`Engine::build_drawables_for_testing` 経由で `drawables.paragraphs[node_id].lines` を検証する形に。

**Step 8-3: ビルド + テスト**

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
```
Expected: 全 pass。

---

## Task 9: 検証

**Step 9-1: lint**

```bash
cargo clippy -p fulgur --no-deps -- -D warnings 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -10
```
Expected: 0 warnings, 0 diff.

**Step 9-2: VRT byte-identical**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -20
```
Expected: 56 fixture all pass (no `FULGUR_VRT_UPDATE` needed). 1 byte でも違ったら convert 層のロジック誤りなので Task 1〜8 のどこかに戻る。

**Step 9-3: smoke (engine end-to-end)**

```bash
cargo test -p fulgur --test render_smoke 2>&1 | tail -10
```
Expected: 全 pass.

**Step 9-4: 不要 import / dead code 確認**

```bash
cargo build -p fulgur 2>&1 | grep -i warning | head -20
```
Expected: 0 warnings.

**Step 9-5: コミット & プッシュ**

big-bang block を 1 つ目のコミット、テスト移行を 2 つ目のコミットくらいに分けるのが review 上望ましい。具体的には:

```bash
git add crates/fulgur/src/convert/ crates/fulgur/src/drawables.rs
git commit -m "refactor(convert): produce Drawables directly from DOM walk (Phase 4 PR 8i)"

git add crates/fulgur/src/{pageable.rs,paragraph.rs,engine.rs}
git commit -m "test: migrate dom_to_pageable callers to build_drawables_for_testing (PR 8i)"

# (もし残れば)
git add crates/fulgur/src/convert/mod.rs
git commit -m "chore(convert): drop dom_to_pageable / extract_drawables_from_pageable scaffolding (PR 8i)"
```

---

## 受け入れ条件 (issue 既定 + 追加)

- [ ] `dom_to_pageable` 関数が source に存在しない (`grep -rn "fn dom_to_pageable" crates/fulgur/src` で 0 件)
- [ ] `extract_drawables_from_pageable` 関数が source に存在しない
- [ ] `cargo test -p fulgur --lib` 全 pass
- [ ] `cargo test -p fulgur --test render_smoke` 全 pass
- [ ] `cargo clippy -p fulgur --no-deps -- -D warnings` clean
- [ ] `cargo fmt --check` pass
- [ ] VRT 56 fixture 全 byte-identical（`FONTCONFIG_FILE=...examples/.fontconfig/fonts.conf cargo test -p fulgur-vrt` pass、goldens 更新なし）

---

## リスクと注意点

1. **`extract_paragraph` 内で `convert_node` を再帰呼び出ししているか要確認**: もし inline-box content 用に `convert_node` を呼んでいるなら、その先で Drawables へ insert される。inline-box は paragraph 内で描画する設計なので Drawables に追加 insert するべきか要判断
2. **`positioned` / `pseudo` のリンク**: `attach_link_to_inline_image` 等が paragraph LineItem を変更している。Drawables 化しても LineItem 経由の link 注入は不変
3. **`PaginationGeometryTable` との整合**: fragmenter が body の各子要素 NodeId に依存している。convert で out-of-flow を walk せず in-flow のみ登録するロジックを廃止しても、fragmenter は独自に DOM を歩いているので影響なし（要 grep 確認）
4. **`viewport_size_px` の利用箇所**: `position: fixed` 用に `compute_fixed_pos_cb_size` で使われる。convert 層はもう fixed pos の特別扱いをしないので、この field は残すが convert からは参照されなくなる可能性あり（render_v2 側で参照しているか要確認）
