# GCPM Header/Footer Design

## Overview

CSS Generated Content for Paged Media (GCPM) によるヘッダー/フッター機能を実装する。GCPM 準拠の CSS API を提供し、`cssparser` クレートで GCPM 構文を自前パースする。Stylo/Blitz が GCPM をサポートしていないため、CSS 前処理レイヤーとして実装する。

## CSS API

GCPM 準拠の書式:

```css
.header { position: running(pageHeader); }
.footer { position: running(pageFooter); }

@page {
  @top-center {
    content: element(pageHeader);
  }
  @bottom-center {
    content: element(pageFooter) " - " counter(page) " / " counter(pages);
  }
}
```

```html
<body>
  <div class="header">レポートタイトル</div>
  <div class="footer">Confidential</div>
  <p>本文...</p>
</body>
```

## Pipeline

```
CSS文字列 (AssetBundle)
  → gcpm::parser (cssparser)
     抽出: @page マージンボックス、position: running() 名前一覧
     出力: GcpmContext + cleaned_css
  → cleaned_css を HTML に注入 (position: running() → display: none に置換)
  → Blitz parse_and_layout (本文)
  → convert::dom_to_pageable
     - running() 付き要素を Pageable ツリーから除外
     - 除外した要素の HTML 片を Blitz DOM からシリアライズして保持
  → paginate (1パス目)
     - 本文のページ分割 → 総ページ数確定
  → render (2パス目)
     - ページごとにマージンボックスを処理
     - counter(page/pages) を解決
     - running element の HTML にカウンター値を埋め込み
     - Blitz でマージンボックス幅に合わせて再レイアウト (キャッシュあり)
     - Krilla surface にヘッダー/フッター → 本文の順で描画
  → PDF bytes
```

GCPM 構文が CSS に含まれない場合、前処理のコストは CSS の1パス走査のみ。マージンボックスが未定義なら2パス目のマージンボックス処理をスキップし、現在と同じ挙動になる。

## Module Structure

```
crates/fulgur/src/gcpm/
├── mod.rs          -- GcpmContext, 公開API
├── parser.rs       -- cssparser による GCPM 構文抽出
├── margin_box.rs   -- 16箇所の型定義 + 位置計算
├── running.rs      -- running elements ライフサイクル管理
└── counter.rs      -- counter(page/pages) 解決
```

## Data Structures

### GcpmContext (gcpm/mod.rs)

```rust
pub struct GcpmContext {
    pub margin_boxes: Vec<MarginBoxRule>,
    pub running_names: HashSet<String>,
    pub cleaned_css: String,
}
```

### MarginBoxRule (gcpm/parser.rs)

```rust
pub struct MarginBoxRule {
    pub page_selector: Option<String>,   // :first, :left, :right, named page
    pub position: MarginBoxPosition,     // TopCenter, BottomRight, etc.
    pub content: Vec<ContentItem>,       // element(), counter(), 文字列リテラル等
    pub declarations: String,            // その他の CSS プロパティ (background 等)
}

pub enum ContentItem {
    Element(String),        // element(pageHeader)
    Counter(CounterType),   // counter(page), counter(pages)
    String(String),         // リテラル文字列
}

pub enum CounterType {
    Page,
    Pages,
}
```

### MarginBoxPosition (gcpm/margin_box.rs)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarginBoxPosition {
    TopLeftCorner, TopLeft, TopCenter, TopRight, TopRightCorner,
    LeftTop, LeftMiddle, LeftBottom,
    RightTop, RightMiddle, RightBottom,
    BottomLeftCorner, BottomLeft, BottomCenter, BottomRight, BottomRightCorner,
}
```

### RunningElement (gcpm/running.rs)

```rust
pub struct RunningElement {
    pub name: String,           // "pageHeader"
    pub html: String,           // Blitz DOM からシリアライズした HTML 片
    pub source_page: usize,     // 本文中での出現ページ (element() ポリシー解決用)
}
```

## Key Design Decisions

### GCPM を Stylo/Blitz 外で自前パースする理由

- Stylo は `position: running()` / `element()` (GCPM) を未実装
- Stylo は `@page` マージンボックスをプロトタイプのみ (pref 隠し)
- Blitz は `@page` ルールへのアクセス API を公開していない
- `cssparser` クレートは Stylo 経由で既に依存ツリーにある
- 将来 Stylo が GCPM 対応したら前処理レイヤーを外すだけ

### Running 要素の処理方式

GCPM 仕様準拠。本文 HTML 内の要素に `position: running(name)` を付与し、フローから除外してマージンボックスに配置する。

1. CSS 前処理: `position: running(...)` → `display: none` に置換して Blitz に渡す
2. Blitz レイアウト後、`convert.rs` で該当要素を Pageable ツリーから除外
3. Blitz DOM ノードから HTML 片をシリアライズして RunningElement に保持
4. 2パス目で独立した Blitz レイアウトに渡す

`display: none` に置換することで、Blitz が `position: running()` を理解せずに本文中に表示してしまうのを防ぐ。Blitz のスタイル解決 (継承・カスケード) は受けた上で、レイアウトに影響させない。

### 2パスレンダリング + キャッシュ

1. 1パス目: 本文ページ分割 → 総ページ数確定
2. 2パス目: ページごとにカウンター解決 → ヘッダー/フッター HTML を Blitz で再レイアウト → 描画

キャッシュキーは解決済み HTML 文字列。大半のページでヘッダー内容は同一 (ページ番号の桁が同じ) なのでヒット率が高い。

`running()` + `element()` + `counter(page/pages)` の組み合わせなら2パスでページ数が確定する。`target-counter()` や脚注でページ数が変動するケースは Phase 4 で多パスレンダリングとして対応。

### マージンボックスの位置計算

CSS Paged Media 仕様では 16 箇所のマージンボックスが定義されている:

```
┌──────────┬──────────┬───────────┬──────────┬──────────┐
│ TL-corner│ top-left │ top-center│ top-right│ TR-corner│
├──────────┼──────────┴───────────┴──────────┼──────────┤
│ left-top │                                 │ right-top│
│ left-mid │        コンテンツ領域            │ right-mid│
│ left-btm │                                 │ right-btm│
├──────────┼──────────┬───────────┬──────────┼──────────┤
│ BL-corner│ btm-left │ btm-center│ btm-right│ BR-corner│
└──────────┴──────────┴───────────┴──────────┴──────────┘
```

- コーナー: 固定サイズ (margin 幅 × margin 高さ)
- 辺の3箇所: 残り幅を intrinsic width ベースで分配

パーサーは16箇所すべて対応。描画は段階的に拡張。

## Phased Implementation

### Phase 1 (MVP)

- `gcpm/parser.rs`: `@page` マージンボックス + `position: running()` の抽出
- `gcpm/running.rs`: running element の登録・参照解決
- `gcpm/counter.rs`: `counter(page)` / `counter(pages)` の値注入
- `gcpm/margin_box.rs`: 16箇所の enum、MVP では `@top-center` + `@bottom-center` のみ描画
- `convert.rs`: running 要素の除外 + HTML 片シリアライズ
- `render.rs`: 2パスレンダリング + キャッシュ
- `config.rs`: `header_html` / `footer_html` 削除
- `engine.rs`: GCPM 前処理パイプライン統合

MVP の描画位置計算: `@top-center` / `@bottom-center` はコンテンツ幅全体を使って中央配置。

### Phase 2: Margin Box Layout

- 上辺5箇所 + 下辺5箇所の幅配分レイアウト (コーナー含む)
- `@page :first / :left / :right` セレクタ対応

### Phase 3: Full 16 Positions

- 左辺3箇所 + 右辺3箇所

### Phase 4: Extended GCPM

- `string-set` / `string()` — named strings
- `element()` の4ポリシー (first, start, last, first-except)
- `target-counter()` / `target-text()` — cross-references
- 多パスレンダリング (ページ数変動対応)
