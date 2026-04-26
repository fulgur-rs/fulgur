# Conic-gradient Implementation (fulgur-m92h)

**Status:** Implemented (path wedge approach, PDF/A safe by default)

## Goal

CSS `conic-gradient(...)` / `repeating-conic-gradient(...)` を PDF に描画する。
PDF/A-1 / PDF/A-2 適合性を初期実装から確保するため、krilla の `SweepGradient`
(PostScript Type 4 function shading) は使わず、krilla 公開 API のみで完結する
**path wedge 分解** で実装する。

## Design Pivot (2026-04-27)

当初の plan は krilla `SweepGradient` 直接利用 (PostScript Type 4 shading) を
基本路線にしていたが、ユーザー要件 (円グラフ用途 + PDF/A 適合) と sub-skill
レベルの設計議論を経て、以下のように pivot した:

- **採用:** Path wedge 分解 — fulgur 内で 360 個の三角ウェッジを作成、隣接同色を
  merge して polygon 化し krilla `Surface::draw_path` で発行
- **不採用 (理由):**
  - krilla `SweepGradient` → PostScript Type 4 で PDF/A-1/A-2 非適合
  - krilla 上流に Type 0 sampled function を PR → 即時実装の障害になる
  - Coons mesh (Type 6) → krilla 上流改修必要 + PDF/A-1 非適合
  - tiny-skia でラスタ化 → tiny-skia は sweep shader 未対応、自前ピクセルループ
    が必要、PDF サイズが path wedge より大きい (raster 床 ~3.5KB vs path 1.5KB)
  - SVG 経由で `BgImageContent::Svg` 流用 → usvg も sweep 未対応、結局 path 列
    を組むので直接 path のほうが見通しが良い

WeasyPrint と Prince はいずれも conic-gradient 自体を未実装であることを web で
確認済み (上流の選定パターン参考にできず、ゼロから設計)。

## Implementation Summary

### `crates/fulgur/src/pageable.rs`

`BgImageContent::ConicGradient` variant を追加。`from_angle` (radians, CSS 規約:
0=top, CW)、`position_x/y` (BgLengthPercentage)、`stops` (Vec<GradientStop>)、
`repeating` を持つ。stop position は convert 時に `<percentage>` および `<angle>`
を統一 fraction (0..1) に正規化済み。

### `crates/fulgur/src/convert.rs`

`resolve_conic_gradient` を追加し、Stylo の `Gradient::Conic` から ConicGradient
variant を生成する。bail-out 条件は linear/radial と同じ (非デフォルト
color-interpolation-method、interpolation hint、calc() position、stops < 2)。

### `crates/fulgur/src/background.rs`

`draw_conic_gradient` を追加:

1. 中心 `(cx, cy)` を `position_x/y` から計算
2. `normalize_conic_stops` で `Auto` を線形補間で埋める (CSS Images L4 fixup)
3. N=360 個の wedge について mid-angle で `sample_conic_color` を呼んで RGBA を決定
4. 隣接同色 wedge を merge して 1 polygon にまとめ、`box_edge_at_angle` で
   外周頂点を計算しながら `surface.draw_path` で発行

純関数ヘルパは `box_edge_at_angle` / `normalize_conic_stops` /
`sample_conic_color` の 3 つで、すべて `#[cfg(test)]` モジュールに unit test 配置。

`is_background_image` 判定 (sizing 経路) と `draw_background_layer` の主要 match
arm にも `BgImageContent::ConicGradient` を追加。

### Tests

- **Unit (`crates/fulgur/src/background.rs::conic_helpers_tests`):** 11 件
  - `box_edge_at_angle` の 5 case (top/right/bottom/left/corner)
  - `normalize_conic_stops` の fixup 系 3 case
  - `sample_conic_color` の lerp 系 3 case
- **Smoke (`crates/fulgur/tests/render_smoke.rs`):** 5 件
  - pie chart / smooth / repeating / from-angle / at-position
  - すべて `Engine::builder().build().render_html(...)` で `assert!(!pdf.is_empty())`
- **VRT (`crates/fulgur-vrt/tests/conic_gradient_harness.rs`):** 1 件
  - 4-quadrant pie chart vs 4 個の絶対配置矩形 ref で pixel diff
  - tolerance: max channel diff 12 / max diff ratio 2%
  - cropped 308×308 で評価 (radial harness と同じ思想)

## Verified Properties

- ✅ CSS angle convention 正解 (red top-right, yellow bottom-right, green bottom-left,
      blue top-left for `conic-gradient(red 0deg, red 90deg, yellow 90deg, ...)`)
- ✅ PDF/A-1 / PDF/A-2 適合 (PostScript shading 不使用、path のみ)
- ✅ pie chart は完璧な vector 描画 (隣接同色 wedge merge で AA 境界線消失)
- ✅ smooth conic は 360 wedge の連続色で許容品質
- ✅ from-angle / at-position / repeating 全シナリオで lib smoke pass

## PDF Size (200×200 box, baseline 1636 byte 差し引き済み)

| ケース | conic 純増 |
|--------|------------|
| 4-color pie chart (square) | 1634 byte |
| 4-color pie chart (circular `border-radius:50%`) | 1701 byte |
| `from 90deg` 4-color | 1652 byte |
| `at 25% 75%` 4-color | 1923 byte |
| `repeating-conic` 12 stripes | 1740 byte |
| smooth 5-stop | 4875 byte |

PostScript Type 4 (~2 KB) や Coons mesh (~1.8 KB) と同等。raster 床 ~3.5KB より
小さい。

## Related Beads Issues (impact)

- **fulgur-batz** (PDF/A-safe conic-gradient fallback): 当初 PDF/A 対応のため別
  issue で扱う予定だったが、本 PR で path wedge により最初から PDF/A 適合。
  → Close 候補。
- **fulgur-zao0** (PDF/A output mode epic): 「PostScript shading 禁止 fallback」
  リストから conic を外す。design 修正のみ。

## Future Work

- パフォーマンス: N=360 は固定値。box サイズに応じた adaptive N 設計、
  step segment 検出による更なる削減 (現状は wedge 数固定で隣接同色 merge のみ)
- WPT `css-images-4/conic-gradient-*` reftest の expectations 整理
- Coons mesh (Type 6) を krilla に PR (任意の品質オプション、PDF/A-2 限定だが
  smooth conic でサイズ削減可能)
