# PR 8j: pageable.rs 全削除 + primitives 移動 実装プラン

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `pageable.rs` を完全削除し、primitive 型を `draw_primitives.rs` に移動する。`Pageable` 識別子が source 全体から消える。

**Architecture:** (1) primitives を `draw_primitives.rs` に切り出し → (2) 全 use site を更新 → (3) `image.rs`/`svg.rs`/`paragraph.rs` から `impl Pageable` を除去してリネーム → (4) `pageable.rs` 削除。

**Tech Stack:** Rust, Cargo。コンパイルが通ることが全ステップの検証基準。

---

## 前提条件確認

```bash
cd /home/ubuntu/fulgur/.worktrees/feat/pr8j-pageable-cleanup
cargo test -p fulgur --lib 2>&1 | tail -3
# 期待: ok. 761 passed
```

---

## Task 1: `draw_primitives.rs` 新設 + lib.rs に追加

**Files:**

- Create: `crates/fulgur/src/draw_primitives.rs`
- Modify: `crates/fulgur/src/lib.rs`
- Modify: `crates/fulgur/src/pageable.rs`

**Step 1: `draw_primitives.rs` を作成する**

`pageable.rs` から以下の範囲をそのままコピーして新ファイルを作る:

- ファイル先頭の `use` ブロック（imports）
- `DestinationRegistry` 〜 `draw_with_opacity`（primitive 幾何型・Canvas・描画基盤）
- `BoxShadow` 〜 `BackgroundLayer`（スタイル型）
- `build_rounded_rect_path` 〜 `clamp_marker_size` の自由関数群（行 1037〜2338 付近）
- `BookmarkEntry` + `BookmarkCollector`（outline.rs が使用）
- `matrix_test_util` モジュール（blitz_adapter.rs テストが使用）

ファイル先頭に `//! Primitive geometry, canvas, style, and drawing-helper types.` を追加すること。

元の pageable.rs の imports を確認してコピー:

```bash
head -80 crates/fulgur/src/pageable.rs
```

`draw_primitives.rs` の先頭はこのようになる:

```rust
//! Primitive geometry, canvas, style, and drawing-helper types.
use std::sync::Arc;
// (pageable.rs の use ブロックをそのままコピー)
```

**Step 2: `lib.rs` に `pub mod draw_primitives;` を追加する**

`crates/fulgur/src/lib.rs` の `pub mod pageable;` の直前に追加:

```rust
pub mod draw_primitives;
```

**Step 3: `pageable.rs` に re-export を追加して primitive 定義を削除する**

`pageable.rs` の冒頭（`use` ブロックの後）に追加:

```rust
pub use crate::draw_primitives::*;
```

そして、`draw_primitives.rs` にコピーした定義群（`DestinationRegistry`〜`draw_with_opacity`、スタイル型、自由関数群、`BookmarkEntry`/`BookmarkCollector`、`matrix_test_util`）を `pageable.rs` から削除する。

`pageable.rs` には残るのは: `pub use crate::draw_primitives::*;` 行 + `Pageable` trait + `PositionedChild` + 全 XxxPageable impl のみ。

**Step 4: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | tail -5
# 期待: Finished
cargo test -p fulgur --lib 2>&1 | tail -3
# 期待: ok. 761 passed
```

**Step 5: コミット**

```bash
git add crates/fulgur/src/draw_primitives.rs crates/fulgur/src/lib.rs crates/fulgur/src/pageable.rs
git commit -m "refactor(primitives): extract draw_primitives.rs from pageable.rs (PR 8j Task 1)"
```

---

## Task 2: 全 use site を `crate::pageable::` → `crate::draw_primitives::` に更新

**Files:**

- Modify: `crates/fulgur/src/background.rs`
- Modify: `crates/fulgur/src/blitz_adapter.rs`
- Modify: `crates/fulgur/src/column_css.rs`
- Modify: `crates/fulgur/src/drawables.rs`
- Modify: `crates/fulgur/src/image.rs`
- Modify: `crates/fulgur/src/link.rs`
- Modify: `crates/fulgur/src/outline.rs`
- Modify: `crates/fulgur/src/pagination_layout.rs`
- Modify: `crates/fulgur/src/paragraph.rs`
- Modify: `crates/fulgur/src/render.rs`
- Modify: `crates/fulgur/src/svg.rs`
- Modify: `crates/fulgur/src/convert/inline_root.rs`
- Modify: `crates/fulgur/src/convert/list_marker.rs`
- Modify: `crates/fulgur/src/convert/mod.rs`
- Modify: `crates/fulgur/src/convert/style/background.rs`
- Modify: `crates/fulgur/src/convert/style/border.rs`
- Modify: `crates/fulgur/src/convert/style/box_metrics.rs`
- Modify: `crates/fulgur/src/convert/style/mod.rs`
- Modify: `crates/fulgur/src/convert/style/overflow.rs`
- Modify: `crates/fulgur/src/convert/style/shadow.rs`

**Step 1: sed で一括置換する（pageable.rs 以外）**

```bash
# crates/fulgur/src/ 以下で pageable.rs 以外の全ファイルに適用
find crates/fulgur/src -name "*.rs" ! -name "pageable.rs" -exec \
  sed -i 's/crate::pageable::/crate::draw_primitives::/g' {} +
```

**Step 2: `use crate::pageable::{...}` 形式の import も更新する**

```bash
find crates/fulgur/src -name "*.rs" ! -name "pageable.rs" -exec \
  sed -i 's/use crate::pageable::/use crate::draw_primitives::/g' {} +
```

**Step 3: paragraph.rs の `Pageable` trait import を除去する**

`paragraph.rs` には `use crate::draw_primitives::{Canvas, Pageable, Pt};` のような行ができているはずなので、`Pageable` を除去する:

```bash
grep -n "draw_primitives.*Pageable" crates/fulgur/src/paragraph.rs
# -> 該当行から Pageable を手動で除去
```

**Step 4: image.rs と svg.rs の `Pageable` import を除去する**

同様に、`use crate::draw_primitives::{Canvas, Pageable, Pt}` から `Pageable` を除去:

```bash
grep -n "draw_primitives.*Pageable" crates/fulgur/src/image.rs crates/fulgur/src/svg.rs
```

**Step 5: ビルド確認（まだ Pageable impl は残っているので OK）**

```bash
cargo build -p fulgur 2>&1 | tail -5
# 期待: Finished (警告はあっても可)
cargo test -p fulgur --lib 2>&1 | tail -3
# 期待: ok. 761 passed
```

**Step 6: `pageable.rs` の `pub use crate::draw_primitives::*;` 行を削除する**

re-export は不要になったので削除（pageable.rs に残るのは Pageable trait + impls のみ）。

```bash
cargo build -p fulgur 2>&1 | tail -5
# 期待: Finished
```

**Step 7: コミット**

```bash
git add -u
git commit -m "refactor(primitives): update all use sites crate::pageable → crate::draw_primitives (PR 8j Task 2)"
```

---

## Task 3: `image.rs` / `svg.rs` — `impl Pageable` 除去 + リネーム

**Files:**

- Modify: `crates/fulgur/src/image.rs`
- Modify: `crates/fulgur/src/svg.rs`
- Modify: `crates/fulgur/src/convert/mod.rs`
- Modify: `crates/fulgur/src/convert/replaced.rs`
- Modify: `crates/fulgur/src/convert/pseudo.rs`
- Modify: `crates/fulgur/src/convert/style/background.rs`
- Modify: `crates/fulgur/src/convert/list_marker.rs`

**Step 1: `image.rs` から `impl Pageable for ImagePageable { ... }` を削除する**

削除対象: `impl Pageable for ImagePageable` ブロック全体（`clone_box`, `as_any`, `node_id`, `draw`, `wrap`, `split` メソッドを含む）。

`Pageable` の `node_id`, `draw` 相当のものが inherent impl として残っているか確認し、なければ何も不要。

削除後、ファイル冒頭の `Pageable` import を除去:

```rust
// Before
use crate::draw_primitives::{Canvas, Pageable, Pt};
// After
use crate::draw_primitives::{Canvas, Pt};
```

**Step 2: `image.rs` で `ImagePageable` → `ImageRender` リネーム**

```bash
sed -i 's/ImagePageable/ImageRender/g' crates/fulgur/src/image.rs
```

**Step 3: 全 use site で `ImagePageable` → `ImageRender` に更新する**

```bash
find crates/fulgur/src -name "*.rs" ! -name "image.rs" -exec \
  sed -i 's/ImagePageable/ImageRender/g' {} +
```

**Step 4: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | grep "^error" | head -10
cargo test -p fulgur --lib 2>&1 | tail -3
```

**Step 5: `svg.rs` から `impl Pageable for SvgPageable { ... }` を削除する**

削除対象: `impl Pageable for SvgPageable` ブロック全体。

削除後、ファイル冒頭の `Pageable` import を除去。

テスト内の `as_any().downcast_ref::<SvgPageable>()` テスト（`as_any_returns_self` テスト）は `impl Pageable` に依存しているので削除する:

```bash
grep -n "as_any\|downcast_ref.*Svg\|clone_box" crates/fulgur/src/svg.rs
# -> これらのテストを削除
```

**Step 6: `svg.rs` で `SvgPageable` → `SvgRender` リネーム**

```bash
sed -i 's/SvgPageable/SvgRender/g' crates/fulgur/src/svg.rs
```

**Step 7: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | grep "^error" | head -10
cargo test -p fulgur --lib 2>&1 | tail -3
# 期待: ok. (N passed, テスト削除分だけ減少)
```

**Step 8: コミット**

```bash
git add crates/fulgur/src/image.rs crates/fulgur/src/svg.rs \
  crates/fulgur/src/convert/mod.rs crates/fulgur/src/convert/replaced.rs \
  crates/fulgur/src/convert/pseudo.rs crates/fulgur/src/convert/style/background.rs \
  crates/fulgur/src/convert/list_marker.rs
git commit -m "refactor(image,svg): remove impl Pageable, rename to ImageRender/SvgRender (PR 8j Task 3)"
```

---

## Task 4: `paragraph.rs` + `convert/inline_root.rs` — InlineBoxItem.content 置換 + ParagraphRender リネーム

これが最も複雑なタスク。手動編集が主体。

**Files:**

- Modify: `crates/fulgur/src/paragraph.rs`
- Modify: `crates/fulgur/src/convert/inline_root.rs`

### 4a: `paragraph.rs` の変更

**Step 1: `InlineBoxContent` 型エイリアスを削除する**

```rust
// 削除する行 (paragraph.rs:147 付近)
pub type InlineBoxContent = Box<dyn crate::draw_primitives::Pageable>;
// または更新済みの名前で
```

**Step 2: v1 ヘルパ関数群を削除する**

以下を丸ごと削除 (paragraph.rs の approximately lines 149〜250):

- `pub(crate) fn inline_box_baseline_offset(...)` とそのコメントブロック
- `fn has_outer_overflow_clip(...)` とそのコメントブロック
- `pub(crate) fn pageable_last_baseline(...)` とそのコメントブロック

**Step 3: `fn inline_box_content_label(...)` を削除する**

paragraph.rs の `inline_box_content_label` 関数（lines 309〜330 付近）を削除する。

**Step 4: `InlineBoxItem.content` フィールドを `node_id: Option<usize>` に変更する**

```rust
// Before
pub struct InlineBoxItem {
    pub content: InlineBoxContent,
    ...
}

// After
#[derive(Clone)]
pub struct InlineBoxItem {
    pub node_id: Option<usize>,
    pub width: f32,
    pub height: f32,
    pub x_offset: f32,
    pub computed_y: f32,
    pub link: Option<Arc<LinkSpan>>,
    pub opacity: f32,
    pub visible: bool,
}
```

**Step 5: `LineItem` の手動 `Debug` impl を簡略化する**

`inline_box_content_label` を参照していた箇所を削除。`InlineBoxItem` は `#[derive(Clone)]` のみ（`Debug` は derive でよい）。`LineItem` の Debug impl は:

```rust
impl std::fmt::Debug for LineItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(t) => f.debug_tuple("Text").field(t).finish(),
            Self::Image(i) => f.debug_tuple("Image").field(i).finish(),
            Self::InlineBox(ib) => f
                .debug_struct("InlineBox")
                .field("node_id", &ib.node_id)
                .field("width", &ib.width)
                .field("height", &ib.height)
                .field("link", &ib.link.is_some())
                .field("visible", &ib.visible)
                .finish(),
        }
    }
}
```

**Step 6: draw code の `ib.content.node_id()` → `ib.node_id` に変更する**

paragraph.rs の draw_shaped_lines 内:

```rust
// Before (line 863)
&& let Some(content_id) = ib.content.node_id()

// After
&& let Some(content_id) = ib.node_id
```

**Step 7: legacy fallback else branch を削除する**

paragraph.rs の lines 915〜927 付近（`// No v2 ctx` コメントのある else ブランチ）を削除:

```rust
// 削除するブロック:
} else {
    // No v2 ctx (legacy `Pageable::draw` invocation path), ...
    crate::draw_primitives::draw_with_opacity(canvas, ib.opacity, |canvas| {
        ib.content.draw(canvas, ox, oy, ib.width, ib.height);
    });
}
```

**Step 8: `impl Pageable for ParagraphPageable` を削除する**

paragraph.rs の `impl Pageable for ParagraphPageable { ... }` ブロック全体を削除（lines 1051〜1072 付近）。

**Step 9: `ParagraphPageable` → `ParagraphRender` にリネームする**

```bash
sed -i 's/ParagraphPageable/ParagraphRender/g' crates/fulgur/src/paragraph.rs
```

**Step 10: テストを更新する**

以下のテストを削除または書き換え:

1. `line_item_inline_box_variant_can_be_constructed` — BlockPageable の代わりに `node_id: Some(42)` を使うように書き換え:

```rust
#[test]
fn line_item_inline_box_variant_can_be_constructed() {
    let item = LineItem::InlineBox(InlineBoxItem {
        node_id: Some(42),
        width: 50.0,
        height: 20.0,
        x_offset: 10.0,
        computed_y: 0.0,
        link: None,
        opacity: 1.0,
        visible: true,
    });
    match item {
        LineItem::InlineBox(ib) => {
            assert_eq!(ib.width, 50.0);
            assert_eq!(ib.node_id, Some(42));
        }
        _ => panic!("expected InlineBox variant"),
    }
}
```

2. `line_item_debug_impl_covers_all_variants` — BlockPageable/ParagraphPageable の代わりに `node_id: Some(1)` / `node_id: None` を使うように書き換え。`Block(..)` / `Paragraph(..)` アサーションを削除し、`node_id` フィールドのアサーションに変更。

3. `recalculate_line_box_skips_inline_box_items` — BlockPageable の代わりに `node_id: None` を使うように書き換え。

4. `pageable_last_baseline_walks_through_wrappers` — 完全に削除。

5. `paragraph_default_has_no_id` / `paragraph_with_id_stores_value` — `ParagraphRender` に名前が変わるだけで内容はそのまま。

### 4b: `convert/inline_root.rs` の変更

**Step 11: `SpacerPageable` import を削除する**

```rust
// 削除
use crate::pageable::SpacerPageable;
```

**Step 12: `ParagraphPageable` import を `ParagraphRender` に更新する**

```rust
// Before
use crate::paragraph::{InlineBoxItem, ParagraphPageable};
// After
use crate::paragraph::{InlineBoxItem, ParagraphRender};
```

**Step 13: `convert_inline_box_node` の戻り値型を変更する**

```rust
// Before
fn convert_inline_box_node(...) -> crate::paragraph::InlineBoxContent {
    ...
    return Box::new(SpacerPageable::new(0.0));
    ...
    Box::new(SpacerPageable::new(0.0).with_node_id(Some(node_id)))
}

// After
fn convert_inline_box_node(...) -> Option<usize> {
    ...
    return None;
    ...
    Some(node_id)
}
```

**Step 14: `InlineBoxItem` の構築を更新する**

```rust
// inline_root.rs line 560 付近
// Before
items.push(LineItem::InlineBox(InlineBoxItem {
    content,
    ...
}));

// After
items.push(LineItem::InlineBox(InlineBoxItem {
    node_id: content,
    ...
}));
```

**Step 15: `extract_paragraph` の戻り値型を更新する**

```rust
// Before
pub(super) fn extract_paragraph(...) -> Option<ParagraphPageable> {
// After
pub(super) fn extract_paragraph(...) -> Option<ParagraphRender> {
```

**Step 16: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | grep "^error" | head -20
```

エラーがある場合は修正する。

**Step 17: テスト確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
# 期待: ok. (N passed, 削除テスト分だけ減少)
```

**Step 18: コミット**

```bash
git add crates/fulgur/src/paragraph.rs crates/fulgur/src/convert/inline_root.rs
git commit -m "refactor(paragraph): replace InlineBoxContent w/ node_id, rename ParagraphRender, remove impl Pageable (PR 8j Task 4)"
```

---

## Task 5: `Pageable` trait + 全 XxxPageable impl を `pageable.rs` から削除

**Files:**

- Modify: `crates/fulgur/src/pageable.rs`

**Step 1: 現時点の pageable.rs の内容を確認する**

```bash
grep -n "^pub\|^impl\|^fn\|^struct\|^enum\|^trait" crates/fulgur/src/pageable.rs | head -40
```

Task 1〜4 完了後、`pageable.rs` に残っているのは:
- `pub use crate::draw_primitives::*;`（Task 2 で削除済みのはず、残っていれば削除）
- `pub trait Pageable: Send + Sync { ... }` とその関連実装
- `pub struct PositionedChild { ... }` + impl
- `pub struct BlockPageable { ... }` + impl + `impl Pageable for BlockPageable`
- `pub struct SpacerPageable { ... }` + `impl Pageable for SpacerPageable`
- `pub struct BookmarkMarkerPageable { ... }` + `impl Pageable`
- ... (残りの XxxPageable 群)
- テストモジュール

**Step 2: `pageable.rs` を空ファイルへ縮小する**

`pageable.rs` の全内容を削除し、ファイルを空にする（後で削除するまでの中間状態）:

```bash
> crates/fulgur/src/pageable.rs
```

**Step 3: ビルド確認（エラーがあれば修正）**

```bash
cargo build -p fulgur 2>&1 | grep "^error" | head -20
```

エラーが出た場合: 残存する `crate::pageable::XxxPageable` 参照を特定して修正。

**Step 4: テスト確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

---

## Task 6: `pageable.rs` 削除 + `lib.rs` から `pub mod pageable;` 除去

**Files:**

- Delete: `crates/fulgur/src/pageable.rs`
- Modify: `crates/fulgur/src/lib.rs`

**Step 1: ファイル削除**

```bash
rm crates/fulgur/src/pageable.rs
```

**Step 2: `lib.rs` から `pub mod pageable;` を削除する**

```rust
// 削除する行
pub mod pageable;
```

**Step 3: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | tail -5
# 期待: Finished
```

**Step 4: `grep` で Pageable 残存確認**

```bash
grep -rn "Pageable" crates/fulgur/src/ | grep -v "//.*Pageable\|//!.*Pageable" | grep -v pageable.rs
# 期待: 0件（コメントのみなら許容）
```

**Step 5: 全テスト**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo clippy -p fulgur --no-deps -- -D warnings 2>&1 | grep "^error" | head -10
cargo fmt --check 2>&1
```

**Step 6: コミット**

```bash
git add crates/fulgur/src/lib.rs
git rm crates/fulgur/src/pageable.rs
git commit -m "refactor(pageable): delete pageable.rs, all Pageable types removed (PR 8j Task 6)"
```

---

## Task 7: 最終検証

**Step 1: `cargo doc` warnings 確認**

```bash
cargo doc --no-deps -p fulgur 2>&1 | grep "^warning\|^error" | head -20
# 期待: 0 件
```

**Step 2: VRT 全 fixture 確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -10
# 期待: 56 passed; 0 failed
```

**Step 3: 最終 grep 確認**

```bash
grep -rn "Pageable\|pageable::" crates/fulgur/src/ | grep -v "//\|pageable\.rs"
# 期待: 0件（comments のみなら許容）
```

**Step 4: 最終コミット（必要な場合）**

VRT golden 更新が必要な場合:

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt
git add goldens/
git commit -m "test(vrt): update goldens after PR 8j pageable removal"
```

---

## チェックリスト（完了確認）

- [ ] `pageable.rs` が存在しない
- [ ] `grep -rn "Pageable" crates/fulgur/src/` が 0 件（コメント除く）
- [ ] `cargo test -p fulgur --lib` 全 pass
- [ ] `cargo clippy -p fulgur --no-deps -- -D warnings` clean
- [ ] `cargo fmt --check` pass
- [ ] `cargo doc --no-deps -p fulgur` warnings 0
- [ ] VRT 56 fixture 全 byte-identical
