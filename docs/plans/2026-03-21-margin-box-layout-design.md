# Margin Box Width Distribution Design

## Overview

CSS Paged Media 仕様に準拠したマージンボックスの幅配分を実装する。上辺・下辺の3箇所（left/center/right）が定義されている場合、max-content width に基づく flex 配分で幅を決定する。

## Algorithm

### flex 配分（2者間）

```text
flex_distribute(a_max, b_max, available):
  total = a_max + b_max
  if total <= available:
    flex_space = available - total
    a_factor = a_max / total
    a = a_max + flex_space * a_factor
    b = b_max + flex_space * (1 - a_factor)
  else:
    a_factor = a_max / total
    a = available * a_factor
    b = available * (1 - a_factor)
  return (a, b)
```

### 3箇所の幅決定

**C あり:**

1. C の max-content と 仮想 AC (L+R 合計) の max-content で flex 配分
2. `L の幅 = R の幅 = (available - C の幅) / 2`

**C なし、L + R:**

- L と R の max-content で flex 配分

**1箇所のみ:**

- 全幅

コーナーは常に固定サイズ（margin 幅 × margin 高さ）。

### `width` 明示指定

指定値を固定として扱い、残りを他のボックスで配分。MVP では後回し。

## Data Flow (render.rs)

```text
各ページ:
  1. effective_boxes 解決（セレクタ優先度）
  2. resolved_html 生成（counter/element 解決）
  3. 各 resolved_html を content_width で Blitz レイアウト → max-content 取得
  4. 辺ごとに compute_edge_layout で flex 幅配分 → 確定 rect
  5. 確定 rect で描画
```

### max-content の取得

Blitz で content_width を viewport にレイアウトし、ルート子要素の `final_layout.size.width` を max-content として使用。マージンボックスの中身は短いテキストなので折り返しは起きない。

### キャッシュ構造

```rust
// Measure cache: html → max-content width (measured once with inline-block)
type MeasureCache = HashMap<String, f32>;
// Render cache: (html, width_bits) → Pageable (laid out at confirmed width)
type RenderCache = HashMap<(String, u32), Box<dyn Pageable>>;
```

## API

### margin_box.rs

```rust
pub enum Edge { Top, Bottom, Left, Right }

pub fn compute_edge_layout(
    edge: Edge,
    defined: &BTreeMap<MarginBoxPosition, f32>,  // position → max-content width
    page_size: PageSize,
    margin: Margin,
) -> HashMap<MarginBoxPosition, MarginBoxRect>
```

### render.rs

計測→配分→描画の3段階。計測キャッシュと描画キャッシュを分離。

## Files

- Modify: `crates/fulgur/src/gcpm/margin_box.rs` — `Edge`, `compute_edge_layout`, `distribute_widths`, `flex_distribute`
- Modify: `crates/fulgur/src/render.rs` — マージンボックス描画ループを3段階に
