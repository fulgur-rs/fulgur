# fulgur-x92a: convert/ の blitz_dom 参照を adapter 経由に段階移行 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `crates/fulgur/src/convert/` 配下の `blitz_dom::*` 直接参照（実測 50 箇所）を `blitz_adapter` 経由に置き換え、Blitz 0.3 系への bump 時の修正範囲を adapter 内に閉じ込める。

**Architecture:** advisor 推奨の **hybrid approach** を採用。型は re-export で透過、enum pattern match だけ helper 化する。具体的には:

1. **型 (BaseDocument / Node / NodeData / Marker / ListItemLayoutPosition)** → `blitz_adapter` で `pub use` 再エクスポート（型 alias なので呼び出し元から区別不能）。call site は `use crate::blitz_adapter::{Node, ...}` に切り替えるだけ。
2. **enum pattern match (Marker / ListItemLayoutPosition / NodeData の variant)** → adapter 側に「中間 enum（または helper 関数）」を作り、pattern match を adapter 内に閉じる。これで Blitz 0.3 で variant 追加・rename されても call site が壊れない。

**Non-goals:**

- adapter 自身の public 引数型（`extract_content_image_url(node: &blitz_dom::Node)` 等）は書き換えない。re-export は型 alias なので call site が `use crate::blitz_adapter::Node` と書けばそのまま通る。
- `convert/` 外の参照（column_css.rs / gcpm/running.rs / multicol_layout.rs / gcpm/string_set.rs / render.rs）は本 plan のスコープ外。完了後に follow-up issue として登録する。
- 機能変更・バグ修正なし。pure refactor。
- Blitz 0.3 への bump も別 issue。

**Tech Stack:** Rust, blitz-dom 0.2.4, fulgur workspace, cargo test/clippy/fmt, fulgur-vrt（PDF byte-level VRT）。

**Verification strategy（全 phase 共通）:**

各 commit 直前に以下を実行し、すべて green であることを確認する:

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
cargo clippy -p fulgur --all-targets -- -D warnings
cargo fmt --check
```

phase 終端では追加で:

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
```

を 1 回流して PDF byte-wise regression がないことを確認する。

**実測 grep 分布（着手時点）:**

| ファイル | 件数 | 内訳 |
|---|---|---|
| `convert/mod.rs` | 13 | `use blitz_dom::{Node, NodeData}` 1 + `&blitz_dom::BaseDocument` 12 |
| `convert/list_marker.rs` | 12 | `&blitz_dom::BaseDocument` 1 + `&blitz_dom::node::Marker` 2 + `Marker::*` match arm 6 + `ListItemLayoutPosition::*` match arm 3 |
| `convert/pseudo.rs` | 6 | `&blitz_dom::BaseDocument` 6 |
| `convert/inline_root.rs` | 4 | `&blitz_dom::BaseDocument` 4 |
| `convert/positioned.rs` | 4 | `&blitz_dom::BaseDocument` 4 |
| `convert/table.rs` | 4 | `&blitz_dom::BaseDocument` 4 |
| `convert/list_item.rs` | 4 | `&blitz_dom::BaseDocument` 2 + `ListItemLayoutPosition::*` arm 2 |
| `convert/replaced.rs` | 1 | `&blitz_dom::BaseDocument` 1 |
| `convert/block.rs` | 1 | `&blitz_dom::BaseDocument` 1 |
| `convert/style/mod.rs` | 1 | `use blitz_dom::Node` |
| `convert/style/opacity.rs` | 1 | `use blitz_dom::Node` |
| **合計** | **51** | (issue 起票時の 47 から微増) |

加えて、`use blitz_dom::{Node, NodeData}` 経由で展開された **prefix なしの pattern match** が以下にある（Phase 3 候補）:

- `convert/mod.rs:166-168`: `NodeData::Element/Text/Comment` 3-arm match
- `convert/table.rs:120`: `NodeData::Comment`
- `convert/inline_root.rs:307`: `NodeData::Element`
- `convert/positioned.rs:27`: `NodeData::Comment`

---

## Phase 0: Adapter に型 re-export を追加

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs` (再エクスポート用 `pub use` を追加)

### Task 0.1: re-export ブロックを追加

**Step 1: `blitz_adapter.rs` の先頭 import セクション（`use blitz_dom::DocumentConfig;` の直下）に re-export ブロックを追加**

```rust
// Type re-exports for adapter isolation (fulgur-x92a)
//
// `blitz_dom` 型は alias として再公開する。call site が `use crate::blitz_adapter::Node`
// と書けば、Blitz 内部での move/rename を adapter 内 1 箇所で吸収できる。
// 同じ alias を adapter 自身の public API（例: `extract_content_image_url(node: &blitz_dom::Node)`）
// が引数で受けるかは無関係——alias なので呼び出し元には透過。
pub use blitz_dom::{BaseDocument, Node, NodeData};
pub use blitz_dom::node::{ListItemLayoutPosition, Marker};
```

**Step 2: ビルド・テストで non-regression を確認**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
cargo clippy -p fulgur --all-targets -- -D warnings
cargo fmt --check
```

Expected: 全 green（追加した `pub use` は新規参照を作らないので既存挙動には影響なし）。

**Step 3: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "refactor(adapter): re-export blitz_dom core types for convert/ migration"
```

---

## Phase 1: 型参照を adapter 経由に置換

51 箇所の `blitz_dom::*` 参照のうち、**型として現れているもの**（function parameter / `use` import / pattern arm の type prefix）を `crate::blitz_adapter::*` 経由に置換する。pattern match 自体は次 Phase で helper 化するため、本 phase ではまだ `Marker::Char(c)` のような variant パターンが残ってよい（type prefix だけ adapter 経由になっていればよい）。

各サブタスクは「1 ファイル単位」で commit する（rollback 可能性 / レビュー容易性）。

### Task 1.1: convert/mod.rs

**Files:**

- Modify: `crates/fulgur/src/convert/mod.rs`

**Step 1: import 行を書き換え**

`use blitz_dom::{Node, NodeData};` (line 19) を:

```rust
use crate::blitz_adapter::{BaseDocument, Node, NodeData};
```

に置き換える。

**Step 2: function signature 内の `&blitz_dom::BaseDocument` を `&BaseDocument` に置換**

該当行（ベースは grep 起票時点の行番号、編集中にズレる可能性あり）:

- 155, 186, 273, 322, 452, 530, 590, 650, 660, 718, 769, 964

すべて `doc: &blitz_dom::BaseDocument,` → `doc: &BaseDocument,` に。

**Step 3: ビルド・テスト**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
cargo clippy -p fulgur --all-targets -- -D warnings
cargo fmt --check
```

Expected: 全 green。

**Step 4: Commit**

```bash
git add crates/fulgur/src/convert/mod.rs
git commit -m "refactor(convert): route mod.rs blitz_dom types via blitz_adapter"
```

### Task 1.2: convert/block.rs / replaced.rs / positioned.rs / table.rs / pseudo.rs / inline_root.rs

**Files:**

- Modify: `crates/fulgur/src/convert/block.rs`
- Modify: `crates/fulgur/src/convert/replaced.rs`
- Modify: `crates/fulgur/src/convert/positioned.rs`
- Modify: `crates/fulgur/src/convert/table.rs`
- Modify: `crates/fulgur/src/convert/pseudo.rs`
- Modify: `crates/fulgur/src/convert/inline_root.rs`

各ファイル先頭に:

```rust
use crate::blitz_adapter::BaseDocument;
```

（既存の `use` セクションに追加。既存に `use crate::blitz_adapter::...` がある場合は import を統合）

そして body 内の `&blitz_dom::BaseDocument` を `&BaseDocument` に一括置換。

**Step 1: 1 ファイル変更 → ビルド → コミット を 6 ファイル分繰り返す**

例: block.rs:

```bash
# 編集（&blitz_dom::BaseDocument → &BaseDocument、import 追加）
cargo build -p fulgur
cargo test -p fulgur --lib
cargo fmt --check
git add crates/fulgur/src/convert/block.rs
git commit -m "refactor(convert): route block.rs blitz_dom types via blitz_adapter"
```

同様に replaced.rs, positioned.rs, table.rs, pseudo.rs, inline_root.rs について実施。

**ヒント:** 6 ファイルの修正が機械的にほぼ同じなので、1 つの commit にまとめてもよい（レビュー時の認知負荷とのトレードオフで判断）。Plan 上は「1 ファイル 1 commit」を推奨するが、実装者の判断で `git commit -m "refactor(convert): route block.rs/replaced.rs/.../inline_root.rs blitz_dom types via blitz_adapter"` にまとめても可。

### Task 1.3: convert/list_marker.rs と list_item.rs

**Files:**

- Modify: `crates/fulgur/src/convert/list_marker.rs`
- Modify: `crates/fulgur/src/convert/list_item.rs`

**list_marker.rs:**

先頭に:

```rust
use crate::blitz_adapter::{BaseDocument, ListItemLayoutPosition, Marker};
```

を追加。以下を置換:

- `&blitz_dom::BaseDocument` → `&BaseDocument` (1 箇所: line 146)
- `&blitz_dom::node::Marker` → `&Marker` (2 箇所: line 249, 330)
- `blitz_dom::node::Marker::Char(c)` → `Marker::Char(c)` (3 箇所)
- `blitz_dom::node::Marker::String(s)` → `Marker::String(s)` (3 箇所)
- `blitz_dom::node::ListItemLayoutPosition::Inside` → `ListItemLayoutPosition::Inside` (2 箇所)
- `blitz_dom::node::ListItemLayoutPosition::Outside(layout)` → `ListItemLayoutPosition::Outside(layout)` (1 箇所)

**list_item.rs:**

先頭に:

```rust
use crate::blitz_adapter::{BaseDocument, ListItemLayoutPosition};
```

を追加。以下を置換:

- `&blitz_dom::BaseDocument` → `&BaseDocument` (2 箇所: line 18, 320)
- `blitz_dom::node::ListItemLayoutPosition::Outside(_)` → `ListItemLayoutPosition::Outside(_)` (line 43)
- `blitz_dom::node::ListItemLayoutPosition::Inside` → `ListItemLayoutPosition::Inside` (line 155)

**Step 1: 編集**

**Step 2: ビルド・テスト**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
cargo clippy -p fulgur --all-targets -- -D warnings
cargo fmt --check
```

**Step 3: Commit**

```bash
git add crates/fulgur/src/convert/list_marker.rs crates/fulgur/src/convert/list_item.rs
git commit -m "refactor(convert): route list_marker/list_item blitz_dom types via blitz_adapter"
```

### Task 1.4: convert/style/{mod.rs,opacity.rs}

**Files:**

- Modify: `crates/fulgur/src/convert/style/mod.rs`
- Modify: `crates/fulgur/src/convert/style/opacity.rs`

各ファイルの:

```rust
use blitz_dom::Node;
```

を:

```rust
use crate::blitz_adapter::Node;
```

に置換。

**Step 1: 編集 + ビルド**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
cargo clippy -p fulgur --all-targets -- -D warnings
cargo fmt --check
```

**Step 2: Commit**

```bash
git add crates/fulgur/src/convert/style/mod.rs crates/fulgur/src/convert/style/opacity.rs
git commit -m "refactor(convert): route convert/style blitz_dom types via blitz_adapter"
```

### Task 1.5: Phase 1 完了後の grep 検証

**Step 1: convert/ 配下の `blitz_dom::` 参照が 0 になったか確認**

```bash
grep -rnE "\bblitz_dom\b" --include='*.rs' crates/fulgur/src/convert/ | grep -vE "^[^:]+:[0-9]+:\s*//" | wc -l
```

Expected: `0`

もし残っていたら原因を特定して個別対応。

**Step 2: VRT（PDF byte-level）regression 検証**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
```

Expected: 全 pass。

---

## Phase 2: Marker / ListItemLayoutPosition pattern を adapter helper 化

Phase 1 完了時点では `Marker::Char(c) => ...` のような variant pattern が convert/list_marker.rs と list_item.rs に残っている（型 alias 経由で参照しているだけ）。Blitz 0.3 系で variant 追加・rename されると call site が壊れるため、**helper 関数で pattern match を adapter 内に閉じる**。

### Task 2.1: adapter に Marker helper を追加

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`

**実コード調査結果（plan 作成時に確認済み）:**

`crates/fulgur/src/convert/list_marker.rs` には 3 つの `match marker` ブロックがある：

| 場所 | `Marker::Char(c)` arm | `Marker::String(s)` arm |
|---|---|---|
| `extract_marker_lines` (line 163-169) | UTF-8 encode 経由で String 化 | `s.clone()` |
| `find_marker_font` (line 253-260) | `String::push(*c)` で 1 文字 String | `s.clone()` |
| `shape_marker_with_skrifa` (line 336-339) | **`format!("{c} ")` (末尾空白あり)** | `s.clone()` (空白なし) |

→ 1 番目と 2 番目は等価（どちらも「c → String」「s → s.clone()」）。3 番目は **Char arm にだけ末尾空白を付ける非対称な挙動**。Blitz の `build_inline_layout` と整合性を取るためのもの（既存コメント line 322-324 参照）。

helper はこの 3 通りを **2 種類に集約**する:

- `marker_to_string(&Marker) -> String`: 「Marker → 空白なしテキスト」(case 1, 2 で使う)
- `marker_skrifa_text(&Marker) -> String`: 「Char には末尾空白付与、String はそのまま」(case 3 で使う)

**Step 1: failing test を追加**

`blitz_adapter.rs` の末尾（`#[cfg(test)] mod tests` セクションがあればその中、なければ新設）に以下のテストを追加:

```rust
#[cfg(test)]
mod marker_helper_tests {
    use super::*;

    #[test]
    fn marker_to_string_char_returns_single_char_string() {
        let m = Marker::Char('•');
        assert_eq!(marker_to_string(&m), "•");
    }

    #[test]
    fn marker_to_string_string_returns_owned_clone() {
        let m = Marker::String("1.".to_string());
        assert_eq!(marker_to_string(&m), "1.");
    }

    #[test]
    fn marker_skrifa_text_char_appends_trailing_space() {
        let m = Marker::Char('•');
        assert_eq!(marker_skrifa_text(&m), "• ");
    }

    #[test]
    fn marker_skrifa_text_string_keeps_as_is_no_trailing_space() {
        // Marker::String は既に "1. " のように trailing space を含むケースを想定するため、
        // helper では追加のスペースを付けない（list_marker.rs:336-339 と同等）。
        let m = Marker::String("1.".to_string());
        assert_eq!(marker_skrifa_text(&m), "1.");
    }
}
```

`marker_to_string` / `marker_skrifa_text` はまだ存在しないので fail する。

**Step 2: テストを走らせて FAIL を確認**

```bash
cargo test -p fulgur --lib marker_helper_tests
```

Expected: コンパイルエラー（helper 未実装）。

**Step 3: helper 実装を追加**

`blitz_adapter.rs` の `Marker` re-export 直下に追加:

```rust
/// `Marker` を空白追加なしの `String` に変換する。
///
/// `Marker::Char(c)` → `c.to_string()`、`Marker::String(s)` → `s.clone()`。
/// `extract_marker_lines` と `find_marker_font` で使う。
///
/// Blitz 0.3 系で variant が増えた場合は adapter 内でハンドリングを追加すれば
/// 呼び出し側 (convert/list_marker.rs) は無変更。
pub fn marker_to_string(marker: &Marker) -> String {
    match marker {
        Marker::Char(c) => c.to_string(),
        Marker::String(s) => s.clone(),
    }
}

/// `Marker` を skrifa shape 入力用テキストに変換する（**非対称な空白付与**）。
///
/// - `Marker::Char(c)` → `format!("{c} ")`（**末尾空白あり**: Blitz の
///   `build_inline_layout` が `format!("{char} ")` で生成するのと整合）
/// - `Marker::String(s)` → `s.clone()`（空白なし: 通常 `"1. "` のように
///   既に trailing space を含む形式が来る前提）
///
/// `shape_marker_with_skrifa` でのみ使用する。
pub fn marker_skrifa_text(marker: &Marker) -> String {
    match marker {
        Marker::Char(c) => format!("{c} "),
        Marker::String(s) => s.clone(),
    }
}
```

> **注:** `Marker::Char('•')` / `Marker::String("1.".to_string())` は外部から直接構築可能（blitz-dom 0.2.4 `src/node/element.rs:509-512` 確認済み、`#[non_exhaustive]` なし、variant が `pub`）。

**Step 4: テストが pass するか確認**

```bash
cargo test -p fulgur --lib marker_helper_tests
```

Expected: 4 件 pass。

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(adapter): add marker_to_string / marker_skrifa_text helpers"
```

### Task 2.2: list_marker.rs の Marker pattern を helper 経由に書き換え

**Files:**

- Modify: `crates/fulgur/src/convert/list_marker.rs`

**Step 1: 既存の 3 つの match block を helper 呼び出しに置換**

実コード調査済み。3 箇所すべて「文字列化のみ」なので、helper 1 行で置換可能：

**(a) `extract_marker_lines` 内の match (line 163-169):**

```rust
// before
let marker_text = match &list_item_data.marker {
    blitz_dom::node::Marker::Char(c) => {
        let mut buf = [0u8; 4];
        c.encode_utf8(&mut buf).to_string()
    }
    blitz_dom::node::Marker::String(s) => s.clone(),
};

// after
let marker_text = crate::blitz_adapter::marker_to_string(&list_item_data.marker);
```

> **注:** `c.encode_utf8(...).to_string()` と `c.to_string()` は等価（`Display::fmt` が内部で encode_utf8 を呼ぶ）。VRT byte-level で確認可能。

**(b) `find_marker_font` 内の match (line 253-260):**

```rust
// before
let marker_text = match marker {
    blitz_dom::node::Marker::Char(c) => {
        let mut s = String::new();
        s.push(*c);
        s
    }
    blitz_dom::node::Marker::String(s) => s.clone(),
};

// after
let marker_text = crate::blitz_adapter::marker_to_string(marker);
```

**(c) `shape_marker_with_skrifa` 内の match (line 336-339):**

```rust
// before
let text = match marker {
    blitz_dom::node::Marker::Char(c) => format!("{c} "),
    blitz_dom::node::Marker::String(s) => s.clone(),
};

// after
let text = crate::blitz_adapter::marker_skrifa_text(marker);
```

> Phase 1 で `use crate::blitz_adapter::Marker` が import 済みなら、`crate::blitz_adapter::marker_to_string` を `marker_to_string` に短縮するため、ファイル先頭の `use` 文に `marker_to_string, marker_skrifa_text` を追加する。

**Step 2: ビルド・テスト**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
cargo clippy -p fulgur --all-targets -- -D warnings
cargo fmt --check
```

**Step 3: VRT で byte-level regression を確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
```

Expected: 全 pass（PDF 出力が同一）。

**Step 4: Commit**

```bash
git add crates/fulgur/src/convert/list_marker.rs
git commit -m "refactor(convert): use adapter::marker_text helpers in list_marker.rs"
```

### Task 2.3: adapter に ListItemLayoutPosition helper を追加

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`

**実コード調査結果（plan 作成時に確認済み）:**

`ListItemLayoutPosition::Outside` variant の中身は `Box<parley::Layout<TextBrush>>`（`blitz-dom-0.2.4/src/node/element.rs:516-519`）。`TextBrush` は `blitz_dom::node::TextBrush` で再公開されている。call site (`list_marker.rs:158-161`) は `match` で `Outside(layout)` arm を取り、`parley_layout.lines()` で auto-deref しているので、helper も `&parley::Layout<TextBrush>` を返す形で揃える。

**Step 1: failing test を追加**

`blitz_adapter.rs` のテストセクションに:

```rust
#[cfg(test)]
mod list_position_helper_tests {
    use super::*;

    #[test]
    fn list_position_outside_layout_returns_none_for_inside() {
        // ListItemLayoutPosition::Outside の生成は parley::Layout<TextBrush> を Box で持つため
        // ユニットテストで構築するのが煩雑（parley::Layout は private な builder 経由）。
        // ここでは "Inside の場合 None を返す" 性質だけ確認し、Outside の挙動は VRT に任せる。
        assert!(list_position_outside_layout(&ListItemLayoutPosition::Inside).is_none());
    }

    #[test]
    fn is_list_position_inside_returns_true_for_inside() {
        assert!(is_list_position_inside(&ListItemLayoutPosition::Inside));
    }
}
```

**Step 2: テスト fail を確認**

```bash
cargo test -p fulgur --lib list_position_helper_tests
```

Expected: コンパイルエラー（helper 未実装）。

**Step 3: helper 実装を追加**

`blitz_adapter.rs` の `ListItemLayoutPosition` re-export 直下に:

```rust
/// `ListItemLayoutPosition::Outside(layout)` の場合に `Some(&layout)`、
/// `Inside` の場合に `None` を返す。
///
/// `Outside` variant は `Box<parley::Layout<TextBrush>>` を持つが、call site が
/// `.lines()` で auto-deref することを前提に `&parley::Layout<TextBrush>` を返す。
///
/// Blitz 0.3 系で variant 追加・rename された場合は adapter 内で吸収する。
/// 呼び出し側 (convert/list_marker.rs / convert/list_item.rs) は本 helper 経由にすることで
/// pattern match に直接さらされない。
pub fn list_position_outside_layout(
    pos: &ListItemLayoutPosition,
) -> Option<&parley::Layout<blitz_dom::node::TextBrush>> {
    match pos {
        ListItemLayoutPosition::Outside(layout) => Some(layout.as_ref()),
        ListItemLayoutPosition::Inside => None,
    }
}

/// `ListItemLayoutPosition::Inside` であるかを返す boolean accessor。
pub fn is_list_position_inside(pos: &ListItemLayoutPosition) -> bool {
    matches!(pos, ListItemLayoutPosition::Inside)
}
```

> **注:** `parley::Layout<TextBrush>` の型 path は実装時に Read で確認すること。`blitz_dom::node::TextBrush` は `blitz-dom-0.2.4/src/node/element.rs:529` で定義され、`blitz-dom-0.2.4/src/node/mod.rs:11` で `pub use` 経由で公開されている。
> `parley` は blitz-dom 経由で transitively 含まれるため `Cargo.toml` 直接依存追加は不要。`cargo build` で確認する。

**Step 4: テストが pass するか確認**

```bash
cargo test -p fulgur --lib list_position_helper_tests
```

Expected: pass。

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(adapter): add list_position_outside_layout / is_list_position_inside helpers"
```

### Task 2.4: list_marker.rs と list_item.rs の ListItemLayoutPosition pattern を helper 化

**Files:**

- Modify: `crates/fulgur/src/convert/list_marker.rs`
- Modify: `crates/fulgur/src/convert/list_item.rs`

**Step 1: list_marker.rs:114 (Inside だけ check) を `is_list_position_inside(...)` に置換**

```rust
// before
if matches!(layout_position, ListItemLayoutPosition::Inside) { ... }

// after
if crate::blitz_adapter::is_list_position_inside(layout_position) { ... }
```

実際の前後コンテキストは編集前に Read で確認。

**Step 2: list_marker.rs:159-160 (Outside で layout を取り出すパス) を helper 化**

```rust
// before
let layout = match position {
    ListItemLayoutPosition::Outside(layout) => layout,
    ListItemLayoutPosition::Inside => return (Vec::new(), 0.0, 0.0),
};

// after
let Some(layout) = crate::blitz_adapter::list_position_outside_layout(position) else {
    return (Vec::new(), 0.0, 0.0);
};
```

**Step 3: list_item.rs:43 と list_item.rs:155 の同様の pattern を helper 化**

ファイルを Read で確認した上で適切な helper に置換。

**Step 4: ビルド・テスト**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
cargo clippy -p fulgur --all-targets -- -D warnings
cargo fmt --check
```

**Step 5: VRT 検証**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
```

Expected: 全 pass。

**Step 6: Commit**

```bash
git add crates/fulgur/src/convert/list_marker.rs crates/fulgur/src/convert/list_item.rs
git commit -m "refactor(convert): use adapter::list_position_* helpers"
```

### Task 2.5: Phase 2 完了後の grep 検証

```bash
grep -rnE "Marker::Char|Marker::String|ListItemLayoutPosition::" --include='*.rs' crates/fulgur/src/convert/ | grep -vE "^[^:]+:[0-9]+:\s*//" | wc -l
```

Expected: `0`（pattern match が convert/ から消えている。コメント行は除外）。

---

## Phase 3 (Optional): NodeData pattern helper

Phase 2 完了時点で:

- mod.rs:166-168: `NodeData::Element/Text/Comment` の 3-arm match（debug 出力用）
- table.rs:120: `NodeData::Comment` だけの判定
- inline_root.rs:307: `NodeData::Element` だけの判定
- positioned.rs:27: `NodeData::Comment` だけの判定

が残る。これらは Phase 1 で型 import を adapter 経由に変更済みだが、**variant pattern 自体は call site に残っている**。Blitz 0.3 で `NodeData` に variant が追加されるリスクは中程度（既存 `Element` / `Text` / `Comment` / `Document` などはほぼ stable）なので、**本 phase は optional 扱い**。実装者が時間に余裕があれば実施、そうでなければ完了後 follow-up issue で別途。

### Task 3.1: adapter に NodeData helper を追加（実施する場合のみ）

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`

```rust
/// Convert/ で使う NodeData の discriminator。Blitz 0.3 で variant が増えても
/// `Other` で吸収できる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Element,
    Text,
    Comment,
    Other,
}

pub fn node_kind(node: &Node) -> NodeKind {
    match &node.data {
        NodeData::Element(_) => NodeKind::Element,
        NodeData::Text(_) => NodeKind::Text,
        NodeData::Comment => NodeKind::Comment,
        _ => NodeKind::Other,
    }
}

/// 表示用の short label（"div", "#text", "#comment", "#other"）を返す。
/// debug_print_tree で使うことを想定。
pub fn node_display_label(node: &Node) -> String {
    match &node.data {
        NodeData::Element(e) => e.name.local.to_string(),
        NodeData::Text(_) => "#text".to_string(),
        NodeData::Comment => "#comment".to_string(),
        _ => "#other".to_string(),
    }
}
```

unit test を追加し、`cargo test -p fulgur --lib` で pass を確認後 commit。

```bash
git commit -m "feat(adapter): add NodeKind discriminator + node_display_label helper"
```

### Task 3.2: convert/ 内の NodeData pattern を helper 化（実施する場合のみ）

- `convert/mod.rs:166-168`: `node_display_label(node)` に置換
- `convert/table.rs:120`, `convert/positioned.rs:27`: `node_kind(node) == NodeKind::Comment` に置換
- `convert/inline_root.rs:307`: pattern が `if let NodeData::Element(el) = ...` で `el` を使うため、本 helper では十分でない。**この箇所は本 phase の対象外** とし、別途 `adapter::element_data(node) -> Option<&ElementData>` helper を別 issue / 別 phase で導入する。

ビルド・テスト・VRT 確認後 commit。

```bash
git commit -m "refactor(convert): use adapter::NodeKind / node_display_label helpers"
```

---

## Phase 4: 完了確認 + follow-up issue 起票

### Task 4.1: 完了確認

**Step 1: 最終的な grep スキャン**

```bash
grep -rnE "\bblitz_dom\b" --include='*.rs' crates/fulgur/src/convert/ | grep -vE "^[^:]+:[0-9]+:\s*//" | wc -l
```

Expected:

- Phase 1+2 のみ実施: `0`
- Phase 3 も実施: `0`

**Step 2: 全 verification を流す**

```bash
cargo build -p fulgur
cargo test -p fulgur --lib
cargo clippy -p fulgur --all-targets -- -D warnings
cargo fmt --check
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
```

Expected: 全 green。

**Step 3: Convert/ 外の参照を改めて grep（follow-up issue 起票用）**

```bash
grep -rnE "blitz_dom" crates/fulgur/src/ | grep -v "convert/" | grep -v "blitz_adapter.rs"
```

期待される現在の参照箇所:

- `column_css.rs` (4 件: 779, 799, 1263, 1266)
- `gcpm/running.rs` (3 件: 85, 91, 93)
- `gcpm/string_set.rs` (4 件: 55, 61, 82)
- `multicol_layout.rs` (5 件: 3, 13, 17, 1366, 1369)
- `render.rs` (1 件: 199)

これらの結果を follow-up issue の design に貼る。

### Task 4.2: follow-up issue を起票

```bash
bd create \
  --title="refactor(adapter): blitz_dom 参照を adapter helper 経由に段階移行 (convert/ 外)" \
  --type=task \
  --priority=3 \
  --description="fulgur-x92a で convert/ の blitz_dom 参照を adapter 経由に移行完了。次フェーズとして convert/ 外の以下のファイルを対象にする: column_css.rs, gcpm/running.rs, gcpm/string_set.rs, multicol_layout.rs, render.rs。同じ hybrid 方針 (型 re-export + pattern match helper 化) を適用。"
```

issue ID を控えて、本 issue を close するときに「next: <id>」として記録する。

### Task 4.3: PR の準備

`finishing-a-development-branch` skill を使って、PR 作成 / 直接 push / merge を user に決めさせる。

PR 分割の推奨: Phase ごとに 2-3 PR に分けてもよいし、refactor だけなので 1 PR にまとめてもよい（合計 commit 数は 10-15 個程度の見込み）。

---

## Risk & Rollback

- **Risk: Phase 1 の機械的置換で型エラー** — re-export は型 alias なので原理的にエラーは出ないが、`blitz_dom::node::Marker` のような **module path 経由の型** は path 構造が adapter 上で再現されない。adapter 側が `pub use blitz_dom::node::{ListItemLayoutPosition, Marker};` で再公開するためフラット化されるが、もし `blitz_dom::node::node_specifics::Foo` のような深いパスが convert/ で参照されていたら追加で個別対応が必要。
  - **Mitigation:** Phase 1 着手前に grep で `blitz_dom::node::` 系の全パスを再列挙 → adapter の re-export ブロックに過不足なくミラーされているか確認する。

- **Risk: Phase 2 の helper で `taffy::Layout` の型 path が違う** — blitz-dom 0.2.4 の実型を Read で確認。`crates/fulgur/src/multicol_layout.rs` 内で `taffy::*` が使われているのでそこを参考にする。

- **Rollback:** 各 commit が独立しており、`git revert` 1 個で 1 ファイル分の変更だけ巻き戻せる構造。Phase 0 のみ revert すれば Phase 1+ の commit が連鎖的にコンパイルエラーになるが、Phase 0 を残したまま Phase 1+ を revert することで「型 alias は導入済みだが convert/ は未移行」状態に戻すことができる。

---

## 推奨実行方式

advisor 推奨に従い:

- **Phase 0 / 1**: 機械的な type 置換が中心。1 ファイル 1 commit の単純作業。Subagent-Driven で task 単位に分けるか、まとめて手動で進めるかは実装者判断。
- **Phase 2**: helper 実装 + pattern 置換。設計判断（helper シグネチャ）を伴うので Subagent でやる場合は task 内で 1 つの helper に集中させる（Task 2.1 → 2.2 → 2.3 → 2.4 の順）。
- **Phase 3**: optional。時間に余裕があれば本セッションで、なければ follow-up。

verification は **各 commit 直前に必ず 4 コマンド (build / test / clippy / fmt) を流す**こと。VRT は phase 終端で 1 回。
