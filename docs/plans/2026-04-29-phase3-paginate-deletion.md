# Phase 3: paginate.rs deletion (fragmenter-driven page split) — fulgur-g9e3

> Parent epic: `fulgur-z2mg` (Pageable replacement)
> Predecessor: `fulgur-s67g` (Phase 2 — feature gap closure, completed 2026-04-29)
> Successor: `fulgur-9t3z` (Phase 4 — convert + render replacement, delete Pageable type)

## ゴール

`paginate::paginate` および `BlockPageable::split` / `find_split_point` 系の page split 判定を fragmenter (`pagination_layout`) に集約し、`paginate.rs` を削除する。Phase 2 で feature parity が成立したため、fragmenter が page 数 / 各 node の page 配置を一手に決め、Pageable は per-node の draw list 供給源として残る。

## 現状の構造 (Phase 2 完了時点)

### `paginate.rs` (663 行)

公開 API:

```text
paginate(root, page_width, page_height) -> Vec<Box<dyn Pageable>>
collect_string_set_states(pages) -> Vec<BTreeMap<String, StringSetPageState>>
collect_running_element_states(pages) -> Vec<BTreeMap<String, usize>>
collect_counter_states(pages) -> Vec<BTreeMap<String, i32>>
```

すべて Pageable tree を再帰 walk して per-page state を組み立てる。**fragmenter 側の同名関数 (`collect_*_states` / `collect_bookmark_entries`) は Phase 1.x / 2.x で実装済み**で、parity assertion が DEBUG ビルドで一致を保証している。

### `Pageable::split` / `split_boxed` (pageable.rs 内、26 impl)

- `BlockPageable::split` / `find_split_point` (1136 行) — メインロジック
- `ParagraphPageable::split` (line-by-line + widow/orphan)
- `TablePageable::split_boxed` (header repeat)
- `RepeatRowsPageable::split_boxed`
- 各 wrapper (Counter / StringSet / Bookmark / RunningElement / ColumnGroup / FixedPos) の split
- helper: `clone_pc_with_offset`, `split_children_at_index`, `split_children_for_within`

### `paginate()` の production caller

- `render.rs:25` (`render_to_pdf`)
- `render.rs:441` (`render_to_pdf_with_gcpm`)

それ以外の caller はすべて test (`pageable.rs::tests`, `paragraph.rs::tests`, `pagination_layout.rs::cmp_test`)。

### fragmenter (`pagination_layout`) の現状能力 (Phase 2.6 時点)

- body の direct children を順次 placement
- inline root (Parley) は line-by-line split + widow / orphan
- 子要素が strip より大きい場合 → 現 page に whole emit (mid-element split **未対応**)
- `break-before: page` / `break-after: page` / `break-inside: avoid` 対応
- `position: absolute` / `position: fixed` / `position: running()` 除外
- 子の subtree descendants を再帰記録 (Phase 2.5)
- `@page` size / margin の page-1 解決 (Phase 2.6)

**残るギャップ:** mid-element split (`fulgur-s67g` Phase 2.6 で skip gate を入れた領域)。`avoid_block_taller_than_page_falls_back_to_split` のような「子 block が strip より大きく、その中の splittable children を切らないとフィットしない」ケース。

## サブタスク分割

### 3.1 fragmenter: mid-element split support (fulgur-g9e3-1)

fragmenter が body 直下の child だけでなく、**ブロック型 child の subtree 内も再帰的に split** できるようにする。Pageable `BlockPageable::find_split_point` 相当のロジックを DOM 駆動で再実装。

#### 仕様

- 子 block が strip に収まらない場合:
  1. `break-inside: avoid` 指定なら whole placement を試みる (現状通り)
  2. avoid なし、かつ子の DOM children (table の場合は rows、list の場合は items、汎用 block の場合は flow children) を strip 単位で再帰 split
  3. inline root の場合は既存の line-by-line split (これは既に対応済み)
- 子要素単位の fragment を `geometry` に複数 page 分記録する
- `Fragment.height` は分割された strip の高さ (元の `final_layout.size.height` と異なる)

#### 実装方針

`fragment_pagination_root` の child loop 内で「strip 超え + avoid なし」分岐に新ヘルパ `fragment_block_subtree(child_id, ...)` を呼ぶ。再帰関数で:

- block の children を順に walk
- 各 grandchild について「収まる / 跨ぐ / 全く収まらない」判定
- 跨ぐ場合は cursor を strip 終端まで進めた fragment + 次 page 用 fragment を分割記録
- 全く収まらない (= grandchild も oversized) ならさらに再帰

table / list-item / multicol column / fixed-pos など Pageable 側で特殊 split impl を持つケースは、まずは「whole emit (現状の動作)」でフォールバックし、Phase 4 で個別対応するか別 sub-issue で扱う。

#### 受け入れ条件

- `crates/fulgur/tests/break_inside_avoid.rs::avoid_block_taller_than_page_falls_back_to_split` で `assert_pageable_fragmenter_parity` の skip gate が発火しなくなる (page count が一致)
- `mid_element_split_skipped` helper が production 経路で用いられなくなる (削除可能になる)
- 既存の examples_determinism / VRT / WPT すべて pass
- `cargo test -p fulgur` 全 pass

#### リスク

- table / list / multicol の特殊な split semantics (header repeat、marker 継承) を全て fragmenter 側に再現するのは大きい
- Phase 3.1 では汎用 block の再帰だけ対応し、特殊型は引き続き Pageable::split で扱う妥協案を取ることも可能 (ただし Phase 3.2 で paginate() を削除するなら fragmenter 側で handle 必須)

### 3.2 fragmenter-driven page split (fulgur-g9e3-2)

`render.rs` の `paginate(root, w, h)` 呼び出しを fragmenter geometry を使った per-page Pageable 抽出に置き換える。

#### 仕様

新 helper `partition_pageable_by_geometry(root, geometry) -> Vec<Box<dyn Pageable>>`:

- fragmenter geometry から `implied_page_count(geometry)` で page 数を決定
- 各 page p について root を walk し、page p に該当する fragment のみを保持した Pageable を構築
  - subtree の深さ・wrapper 構造は保持 (counter / bookmark / string-set marker が draw 順を保つため)
  - fragment の y は page-local coordinate のまま使用 (現行 slice 仕様で `Fragment.y` は既に page-local。body-global cumulative ではないので rebase 不要)
  - page p に fragment を持たない subtree は除去 (or empty placeholder)
- 出力は既存 `paginate()` と同じ `Vec<Box<dyn Pageable>>`

`render.rs` 側の変更は minimal:

```rust
// before
let pages = paginate(root, content_width, content_height);

// after
let pages = partition_pageable_by_geometry(root, pagination_geometry);
```

#### 受け入れ条件

- `render_to_pdf` / `render_to_pdf_with_gcpm` の paginate 呼び出しが消える
- examples_determinism (11 fixtures) byte-equal
- VRT 33 fixtures byte-equal
- WPT regression 数が Phase 2.6 baseline と同じ
- `cargo test -p fulgur` 全 pass

#### リスク

- Pageable wrapper (CounterOpWrapper 等) が「first half にだけ marker を残す」というセマンティクスを持つ。partition で wrapper 自体を切るときも同じ semantics を再現する必要がある
- `propagate_page_height` で `break-inside: avoid` のフォールバック判定をしている部分が `partition` 経由でどう reproduce されるか要検討 → 3.1 が完了していれば fragmenter 側ですでに判定済みなので不要

### 3.3 Pageable::split / split_boxed / find_split_point 削除 (fulgur-g9e3-3)

#### 仕様

- `Pageable` trait から `split` / `split_boxed` / `has_forced_break_below` / `propagate_page_height` メソッド削除
- `BlockPageable::find_split_point` / `split_children_at_index` / `split_children_for_within` / `clone_pc_with_offset` を削除
- 全 `impl Pageable for X` から `split` / `split_boxed` / `has_forced_break_below` / `propagate_page_height` の実装を削除
- `paginate::paginate()` 関数 + `pagination_layout::cmp_test` parity harness を削除 (`paginate()` は `split_boxed` に依存するため)。`collect_*_states` / `StringSetPageState` 型は 3.4 で fragmenter 側に移すまで残す
- `render::render_to_pdf` (paginate 駆動の旧 API) を削除し、`render_to_pdf_with_partition` を `render_to_pdf` にリネーム (新 API 名として確定)
- `Engine::render_pageable` / `render_pageable_to_file` を削除 (caller が geometry を持たないため partition 経由にできない)
- 既存の test で split / paginate / `Engine::render_pageable` を直接呼んでいるものは fragmenter / VRT golden カバレッジに任せて削除

#### 受け入れ条件

- `pageable.rs` から split / has_forced_break_below / propagate_page_height / split helpers が消えている
- `cargo build -p fulgur` 0 warning
- `cargo test -p fulgur` 全 pass
- VRT / examples_determinism 全 pass

#### リスク

- 削除する split impl の test カバレッジを失わないよう、fragmenter unit test / VRT golden で十分にカバーされていることを確認

#### 3.4 との境界調整 (実装時メモ)

当初 plan は 3.3 を「split メソッドの削除のみ」、3.4 を「paginate.rs 削除」としていたが、`paginate()` は内部で `split_boxed` を呼ぶため artificial な分割だった。本 PR では 3.3 で:

- `paginate()` 関数本体 + 内部テスト (`paginate.rs` の `mod tests`) を削除
- `cmp_test` parity harness (`pagination_layout.rs` 末尾) を削除
- `render_to_pdf` の rename / 旧 API 削除も同時に実施

を **すべて** 畳み込んだ。残る 3.4 (`fulgur-g9e3-4`) のスコープは:

- `paginate.rs` ファイル自体の削除
- `collect_string_set_states` / `collect_counter_states` / `collect_running_element_states` / `StringSetPageState` 型を `pagination_layout` 配下 (or 専用モジュール) へ移動
- `lib.rs` / GCPM render path / counter modules の import 整理

### 3.4 paginate.rs 削除 (fulgur-g9e3-4)

#### 仕様

- `crates/fulgur/src/paginate.rs` をファイルごと削除
- `crates/fulgur/src/lib.rs` から `pub mod paginate;` 削除
- `paginate::collect_string_set_states` / `collect_counter_states` / `collect_running_element_states` / `StringSetPageState` の caller (parity assertion 用に残っていれば) を fragmenter 側 (`pagination_layout::collect_*_states`) 経由に統一
- 関連 import / re-export を整理

#### 受け入れ条件

- `paginate.rs` が存在しない
- `cargo build -p fulgur` 0 warning
- `cargo test -p fulgur` 全 pass
- examples_determinism / VRT / WPT 全 pass

#### 実装メモ

`StringSetPageState` / `PageRunningState` は `pagination_layout` モジュールに移管。`pagination_layout::collect_running_element_states(geometry, &running_store)` を新設し、`render_to_pdf_with_gcpm` の per-page state 収集をすべて fragmenter geometry 駆動に切り替えた。これにより render.rs の parity assertion helper (`assert_pageable_fragmenter_parity` / `assert_string_set_states_parity` / `assert_counter_states_parity` / `assert_bookmark_entries_parity` / `parity_gate_pageable_excess`) と `pagination_layout::collect_bookmark_entries` / `BookmarkPageEntry` も削除。`render_to_pdf_with_gcpm` の `bookmark_by_node` パラメータも不要になったため削除した。

## サブタスク依存

```text
3.1 (mid-element split)  →  3.2 (fragmenter-driven page split)  →  3.3 (Pageable::split 削除)  →  3.4 (paginate.rs 削除)
```

3.2 は 3.1 が完了していないと examples_determinism で diff が出る (mid-element split を含む fixture がある場合)。3.3 / 3.4 は 3.2 完了後に直列で実施可能。

## マージ戦略

各 sub-PR は前の PR を base とした stacked PR で出す (Phase 2 と同じ流れ)。マージ順は 3.1 → 3.2 → 3.3 → 3.4。

## Phase 4 との境界

Phase 4 (`fulgur-9t3z`) は **Pageable type 自体の削除**。Phase 3 完了時点では:

- `Pageable` trait と各 impl は残る (draw 用)
- fragmenter geometry が page split を駆動する
- paginate.rs / split メソッドは消えている

Phase 4 は draw も fragmenter geometry 駆動に変え、Pageable trait を削除して convert を直接 PDF emission に繋げる。

## 不確実性 / 要検討

- mid-element split で table / list / multicol の特殊 split をどこまで再現するか — Phase 3.1 開始時に WPT / VRT fixture で実態を測ってから方針決定
- partition helper の clone semantics — `Box<dyn Pageable>` のため shallow clone 不可。`Pageable::clone_for_page(page_index)` のような新 trait method が必要かもしれない
