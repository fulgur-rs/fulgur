# Phase 3.2 stacked PR design — fulgur-g9e3.2

> Parent: `docs/plans/2026-04-29-phase3-paginate-deletion.md` §3.2
> Predecessor branch: `feat/fulgur-7hf5-in-place-split` (PR #286 / Phase 3.1.5c)
> Successor: Phase 3.3 (`Pageable::split` 削除 / fulgur-g9e3.3)

## ゴール

`render.rs` の `paginate(root, w, h)` 呼び出しを fragmenter geometry を使った per-page Pageable 抽出に置き換える。Phase 3.2 を 3 本の stacked PR に分割して段階的にマージする。

## 共通: 新 trait method `slice_for_page`

```rust
trait Pageable {
    /// Extract the slice of self that falls on `page_index` according
    /// to `geometry`. Returns `None` when no fragment of self (or its
    /// descendants) is on that page. Implementations recurse into
    /// children. `geometry.y` is already page-local in fragmenter
    /// output (see `fragment_pagination_root` / `fragment_block_subtree`),
    /// so implementations read fragment.y directly without a body→page
    /// y conversion.
    fn slice_for_page(
        &self,
        page_index: u32,
        geometry: &PaginationGeometryTable,
    ) -> Option<Box<dyn Pageable>>;
}
```

PR 1 では `default impl { unimplemented!() }` で追加し plain 系のみ実装。PR 2 で残りを埋める。Phase 3.3 で `split` / `find_split_point` / `clone_box` (役目を終える) と一緒に整理する。

後方互換は不要 — paginate() 経路は PR 3 で除却するため、PR 1/2 段階で `slice_for_page` と `split` が並存していてよい (default `unimplemented!` は PR 3 で削除されると同時に required = 実装網羅、と段階を踏める)。

## PR 1 (g9e3.2.a): helper core + plain Pageable

### 実装範囲

- `Pageable::slice_for_page` を required method として trait 定義に追加 (default = `unimplemented!()`)
- impl: `BlockPageable`, `ParagraphPageable`, `SpacerPageable`, `ImagePageable`
- 新 helper `partition_pageable_by_geometry(root: &dyn Pageable, geometry: &PaginationGeometryTable) -> Vec<Box<dyn Pageable>>`
  - `implied_page_count(geometry)` で page 数決定
  - 各 page p について `root.slice_for_page(p, geometry)` を呼ぶ
  - `None` の page は empty placeholder Pageable で埋める (page count は維持)
- `render.rs` には触らない (paginate() は健在)

### Pageable per-impl 実装ノート

- `BlockPageable::slice_for_page`:
  - `self.id` で `geometry.get(node_id)` し、`page_index` 一致の fragment があるか確認
  - 一致 fragment の `(x, y, w, h)` で新 BlockPageable を組む (cached_size = (w, h))
  - children を順に `slice_for_page` 再帰、`Some` のみ in-place で残す
  - fragment が無く全 children も None → 自身も None
- `ParagraphPageable::slice_for_page`:
  - geometry 上に複数 fragment があれば line range で分けるが、Phase 2.1 (widow/orphan) でフラグメント済み — fragment ごとに line subset を割り当て
  - 1 fragment に含まれる line を `lines[range]` で抽出して新 Paragraph を組む
- `SpacerPageable` / `ImagePageable`: 自身の fragment が page_index に存在 → clone (geometry の y で位置調整)、無ければ None

### 受け入れ条件

- `cargo test -p fulgur --lib` 全 pass (新 unit test 含む)
- 新規 unit test: plain Block の 2-page split / Paragraph の line split / 単一 page fixture / 0-fragment subtree → None
- clippy / fmt clean

### リスク

- 既存 26 impl のうち plain 系 4 種を実装、残り 22 種は `unimplemented!()` のまま — production 経路 (paginate()) は健在なので問題ないが、`slice_for_page` を呼ぶ test code を実行すると panic する。test gate は plain 系のみに留める。

## PR 2 (g9e3.2.b): wrapper / 特殊型対応

### 実装範囲

#### Wrapper 系 (first-half marker semantics)

- `CounterOpWrapper`, `StringSetWrapper`, `BookmarkWrapper`, `RunningElementWrapper`:
  - wrapped 子の `slice_for_page(page_index, geometry)` を呼ぶ
  - 子が `Some(sliced_child)` を返した場合:
    - 自身が emit される最小 page_index を geometry から求める (wrapped 子の geometry entries から min(page_index))
    - `page_index == min_page` → marker 込みで wrapper 再構築
    - それ以外 → wrapper 外して `sliced_child` を直接返す (透過)
  - 子が `None` → `None`

#### Out-of-flow 系

- `FixedPosPageable`: `position: fixed` は全 page に複製。`slice_for_page` は wrapped 子の任意 page slice を返す (geometry に依存せず always Some、ただし子が空なら None)
- `ColumnGroupPageable`: column 単位の split は Phase 2.5 で fragmenter 側に移行済み — 各 column を `slice_for_page` 再帰すれば良い

#### 特殊 split 系

- `TablePageable`: header rows を全 page に repeat。slice_for_page では header を always 含め、body rows を geometry でフィルタ
- `ListItemPageable`: marker は first page のみ (CounterOpWrapper と同じ semantics)
- `RepeatRowsPageable`: header と同じ repeat semantics

### 受け入れ条件

- `cargo test -p fulgur --lib` 全 pass
- 新規 unit test:
  - counter increment が first page にのみ出る (CounterOpWrapper)
  - bookmark anchor が first page にのみ出る (BookmarkWrapper)
  - fixed pos が全 page に複製される (FixedPosPageable)
  - table header が全 page に repeat される (TablePageable)
  - column group の column 単位 slice (ColumnGroupPageable)
- 全 26 impl が `slice_for_page` を実装 (`unimplemented!()` 残無し)

### リスク

- `propagate_page_height` の `break-inside: avoid` フォールバック判定が partition 側で必要か不要か — Phase 3.1 (fulgur-g9e3.1) で fragmenter 側に移行済みなら不要。実装中に確認、必要なら helper 内で reproduce
- `clone_pc_with_offset` の y_offset 計算が `slice_for_page` で再現必要 — geometry y は page-local なので不要のはずだが、fixed pos / out-of-flow の anchor 計算は要確認

## PR 3 (g9e3.2.c): render.rs 切り替え

### 実装範囲

- `render.rs::render_to_pdf` (line 25) の `paginate()` 呼び出しを `partition_pageable_by_geometry` に置換
- `render.rs::render_to_pdf_with_gcpm` (line 441) も同様
- `propagate_page_height` の dead code (Phase 3.1 で fragmenter 側に移行済みのフォールバック) を削除
- `paginate()` の caller が render.rs から完全に消えたことを確認 (test 経由の caller は Phase 3.3 で整理)

### 受け入れ条件

- `cargo test -p fulgur` 全 pass (lib + integration)
- `cargo test -p fulgur-vrt` 33 fixtures byte-equal
- `cargo test -p fulgur-cli --test examples_determinism` 11 fixtures byte-equal
- `cargo test -p fulgur-wpt` regression 数が Phase 2.6 baseline と同じ
- `cargo clippy -p fulgur` clean
- `cargo fmt --check` clean
- `render.rs` 内に `paginate(` 文字列が残っていない

### リスク

- byte-equal が崩れた場合、原因は (a) wrapper marker 順序 (b) page-local y 変換 (c) fixed pos の page 複製 のいずれか。byte diff を pdftocairo で画像化して目視確認 → 該当の `slice_for_page` 実装を直す

## サブタスク依存

```text
g9e3.2.a (PR 1) → g9e3.2.b (PR 2) → g9e3.2.c (PR 3)
```

PR 1 は `feat/fulgur-7hf5-in-place-split` (PR #286) を base に。PR #286 がマージされ次第、後続 PR は順次 rebase。

## 不確実性 / 要検討

- `Pageable::slice_for_page` を required method にすると 26 impl すべてで `unimplemented!()` を書く必要があり、PR 1 の diff が膨らむ。代案: trait method を `Option<Box<dyn Pageable>>` を返す default 付き method にし、plain 系のみ override (default は `None`) → ただし PR 2 の漏れに気付きにくい。required の方が PR 2 の網羅判定が楽 (compile error)
- helper の test fixture は `pagination_layout::tests::parse` パターンを再利用できるか — fragmenter geometry の build まで test 内でやればよい
