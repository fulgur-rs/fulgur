# radial-gradient() Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` (or `superpowers:subagent-driven-development`) to implement this plan task-by-task.

**Goal:** CSS `radial-gradient()` を Krilla の `RadialGradient` API に配線し、circle/ellipse, extent keyword (closest-side/farthest-side/closest-corner/farthest-corner), 明示半径, 中心位置オフセットを描画できるようにする。

**Architecture:** Phase 1 で確立した linear-gradient のパターン (`BgImageContent::LinearGradient` / `resolve_linear_gradient` / `draw_linear_gradient`) を踏襲する。`pageable.rs` に `BgImageContent::RadialGradient` variant とサポート enum を追加 → `convert.rs` で Stylo `Gradient::Radial` を解決 (stops 解決ロジックは linear と共通化) → `background.rs` で CSS Images 3 §3.6 の式に従って (rx, ry) を計算し、Krilla `RadialGradient` の `transform` フィールドで楕円を scale 表現する。

**Tech Stack:** Rust, Stylo 0.8 (`style::values::computed::image::Gradient`), Krilla 0.7 (`krilla::paint::RadialGradient`), 既存 fulgur テストハーネス (`crates/fulgur-vrt`, `crates/fulgur-wpt`).

**Reference:** beads issue `fulgur-gm56` の design フィールドに完全な設計が保存されている。`bd show fulgur-gm56` で参照のこと。

---

## 事前確認

- working dir: `/home/ubuntu/fulgur/.worktrees/fulgur-gm56-radial-gradient`
- branch: `feature/fulgur-gm56-radial-gradient` (main から分岐)
- 既存 linear-gradient 実装の参照ポイント:
  - `crates/fulgur/src/pageable.rs:773-788` (`BgImageContent` enum + `LinearGradient*`)
  - `crates/fulgur/src/convert.rs:3110-3290` (`resolve_linear_gradient`)
  - `crates/fulgur/src/convert.rs:3045` (`Image::Gradient` dispatch)
  - `crates/fulgur/src/background.rs:207-218, 281, 319-395` (`draw_linear_gradient`)
  - `crates/fulgur-vrt/tests/gradient_harness.rs` (test↔ref harness pattern)
  - `crates/fulgur-vrt/fixtures/paint/linear-gradient-horizontal.html`

---

## Task 1: pageable.rs に radial 用データ型を追加

**Files:**

- Modify: `crates/fulgur/src/pageable.rs:773-788`

**Step 1: 追加する型を pageable.rs に書く**

`BgImageContent` enum の直前 (LinearGradientDirection の下、773行目あたり) に以下を追加:

```rust
/// CSS `radial-gradient(<shape>?, ...)` の shape 部分。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadialGradientShape {
    Circle,
    Ellipse,
}

/// CSS `radial-gradient(... <extent>, ...)` keyword。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadialExtent {
    ClosestSide,
    FarthestSide,
    ClosestCorner,
    FarthestCorner,
}

/// CSS `radial-gradient(<shape>? <size>?, ...)` の size 部分。
///
/// extent keyword は draw 時に gradient box から半径を計算する。
/// 明示半径も length-percentage を含むため draw 時に解決する。
#[derive(Clone, Debug)]
pub enum RadialGradientSize {
    Extent(RadialExtent),
    /// circle の場合は rx == ry とする。ellipse は独立。
    Explicit {
        rx: BgLengthPercentage,
        ry: BgLengthPercentage,
    },
}
```

そして `BgImageContent` enum (775行目あたり) に variant を追加:

```rust
pub enum BgImageContent {
    Raster { ... },
    Svg { ... },
    LinearGradient { ... },
    /// CSS `radial-gradient(...)`. position は origin rect 内の中心。
    RadialGradient {
        shape: RadialGradientShape,
        size: RadialGradientSize,
        position_x: BgLengthPercentage,
        position_y: BgLengthPercentage,
        stops: Vec<GradientStop>,
    },
}
```

**Step 2: コンパイルが通ることを確認**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-gm56-radial-gradient
cargo check -p fulgur 2>&1 | tail -30
```

Expected: 既存コードが `BgImageContent` を網羅 match している箇所で `non-exhaustive patterns` エラーが出る (background.rs:208, 281, convert.rs などで不完全 match)。これは Task 2-3 で埋める。一時的に `#[allow(dead_code)]` などは付けない (次のタスクで使う)。

**Step 3: コミット**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(gradient): add RadialGradient variant and support enums to BgImageContent"
```

---

## Task 2: convert.rs で stops 解決を共通化 + dispatch を radial 対応に

**Files:**

- Modify: `crates/fulgur/src/convert.rs:3045` (`Image::Gradient` dispatch)
- Modify: `crates/fulgur/src/convert.rs:3110-3290` (`resolve_linear_gradient` の stops 部分を切り出し)

**Step 1: `resolve_color_stops` ヘルパーを切り出す**

`convert.rs:3187-3283` (Pass 1 / Pass 2) の処理を独立関数に切り出す:

```rust
/// CSS gradient items から GradientStop ベクタを解決する。linear / radial 共通。
///
/// - length-typed stop position は None を返す (gradient line 長さ依存のため Phase 2)
/// - 範囲外 stop position も None (recompute が必要、Phase 2)
/// - interpolation hint も None (Phase 2)
/// - auto position は CSS Images §3.5.1 に従い等間隔で埋める
fn resolve_color_stops(
    items: &[style::values::generics::image::GenericGradientItem<
        style::values::computed::Color,
        style::values::computed::LengthPercentage,
    >],
    current_color: &style::color::AbsoluteColor,
    gradient_kind: &'static str,
) -> Option<Vec<crate::pageable::GradientStop>> {
    use crate::pageable::GradientStop;
    use style::values::generics::image::GradientItem;

    let mut raw: Vec<(Option<f32>, [u8; 4])> = Vec::with_capacity(items.len());
    for item in items.iter() {
        match item {
            GradientItem::SimpleColorStop(c) => {
                let abs = c.resolve_to_absolute(current_color);
                raw.push((None, absolute_to_rgba(abs)));
            }
            GradientItem::ComplexColorStop { color, position } => {
                let Some(pct) = position.to_percentage().map(|p| p.0) else {
                    log::warn!(
                        "{gradient_kind}: length-typed stop position is not yet \
                         supported (Phase 2). Layer dropped."
                    );
                    return None;
                };
                if !(0.0..=1.0).contains(&pct) {
                    log::warn!(
                        "{gradient_kind}: stop position {pct:.4} is outside [0, 1]. \
                         Negative / >100% positions require recomputing the gradient \
                         line (Phase 2). Layer dropped."
                    );
                    return None;
                }
                let abs = color.resolve_to_absolute(current_color);
                raw.push((Some(pct), absolute_to_rgba(abs)));
            }
            GradientItem::InterpolationHint(_) => {
                log::warn!(
                    "{gradient_kind}: interpolation hints are not yet supported \
                     (Phase 2). Layer dropped."
                );
                return None;
            }
        }
    }

    if raw.len() < 2 {
        return None;
    }

    let n = raw.len();
    let mut positions: Vec<Option<f32>> = raw.iter().map(|(p, _)| *p).collect();
    if positions[0].is_none() {
        positions[0] = Some(0.0);
    }
    if positions[n - 1].is_none() {
        positions[n - 1] = Some(1.0);
    }
    let mut last_resolved = 0.0_f32;
    for v in positions.iter_mut().flatten() {
        if *v < last_resolved {
            *v = last_resolved;
        }
        last_resolved = *v;
    }
    let mut i = 0;
    while i < n {
        if positions[i].is_some() {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = start;
        while end < n && positions[end].is_none() {
            end += 1;
        }
        let p_prev = positions[start - 1].expect("first slot resolved");
        let p_next = positions[end].expect("last slot resolved");
        let span = (end - start + 1) as f32;
        for (k, slot) in positions.iter_mut().enumerate().take(end).skip(start) {
            let t = (k - start + 1) as f32 / span;
            *slot = Some(p_prev + (p_next - p_prev) * t);
        }
        i = end;
    }

    Some(
        raw.into_iter()
            .zip(positions)
            .map(|((_, rgba), pos)| GradientStop {
                offset: pos.unwrap().clamp(0.0, 1.0),
                rgba,
            })
            .collect(),
    )
}
```

そして `resolve_linear_gradient` から該当部分 (Pass 1 / Pass 2) を削除して `resolve_color_stops(items, current_color, "linear-gradient")?` を呼ぶように差し替える。

**Step 2: `resolve_radial_gradient` を追加**

`resolve_linear_gradient` の直後に新規関数:

```rust
/// Convert a Stylo computed `Gradient::Radial` into fulgur's `BgImageContent::RadialGradient`.
///
/// Phase 1: scope は beads issue fulgur-gm56 の design フィールド参照。
fn resolve_radial_gradient(
    g: &style::values::computed::Gradient,
    current_color: &style::color::AbsoluteColor,
) -> Option<(BgImageContent, f32, f32)> {
    use crate::pageable::{
        BgLengthPercentage, RadialExtent, RadialGradientShape, RadialGradientSize,
    };
    use style::values::computed::image::Gradient;
    use style::values::generics::image::{
        Circle, Ellipse, EndingShape, GradientFlags, ShapeExtent,
    };

    let (shape, position, items, flags) = match g {
        Gradient::Radial {
            shape,
            position,
            items,
            flags,
            ..
        } => (shape, position, items, flags),
        Gradient::Linear { .. } | Gradient::Conic { .. } => return None,
    };

    if flags.contains(GradientFlags::REPEATING) {
        return None;
    }
    if !flags.contains(GradientFlags::HAS_DEFAULT_COLOR_INTERPOLATION_METHOD) {
        return None;
    }

    let (out_shape, out_size) = match shape {
        EndingShape::Circle(Circle::Radius(r)) => {
            // r: NonNegativeLength = NonNegative<Length>。.0.px() で CSS px、px_to_pt() で pt 化。
            let len_pt = px_to_pt(r.0.px());
            (
                RadialGradientShape::Circle,
                RadialGradientSize::Explicit {
                    rx: BgLengthPercentage::Length(len_pt),
                    ry: BgLengthPercentage::Length(len_pt),
                },
            )
        }
        EndingShape::Circle(Circle::Extent(ext)) => (
            RadialGradientShape::Circle,
            RadialGradientSize::Extent(map_extent(*ext)?),
        ),
        EndingShape::Ellipse(Ellipse::Radii(rx, ry)) => (
            RadialGradientShape::Ellipse,
            RadialGradientSize::Explicit {
                rx: convert_lp_to_bg(&rx.0),
                ry: convert_lp_to_bg(&ry.0),
            },
        ),
        EndingShape::Ellipse(Ellipse::Extent(ext)) => (
            RadialGradientShape::Ellipse,
            RadialGradientSize::Extent(map_extent(*ext)?),
        ),
    };

    // computed::Position::horizontal / vertical はどちらも LengthPercentage 直接 (wrapper なし)。
    let position_x = convert_lp_to_bg(&position.horizontal);
    let position_y = convert_lp_to_bg(&position.vertical);

    let stops = resolve_color_stops(items, current_color, "radial-gradient")?;

    Some((
        BgImageContent::RadialGradient {
            shape: out_shape,
            size: out_size,
            position_x,
            position_y,
            stops,
        },
        0.0,
        0.0,
    ))
}

fn map_extent(e: style::values::generics::image::ShapeExtent) -> Option<crate::pageable::RadialExtent> {
    use style::values::generics::image::ShapeExtent;
    use crate::pageable::RadialExtent;
    match e {
        ShapeExtent::ClosestSide => Some(RadialExtent::ClosestSide),
        ShapeExtent::FarthestSide => Some(RadialExtent::FarthestSide),
        ShapeExtent::ClosestCorner => Some(RadialExtent::ClosestCorner),
        ShapeExtent::FarthestCorner => Some(RadialExtent::FarthestCorner),
        // Contain は ClosestSide のエイリアス、Cover は FarthestCorner のエイリアス
        // (CSS Images 3 §3.6.1 「Sizing Gradient Rays」)
        ShapeExtent::Contain => Some(RadialExtent::ClosestSide),
        ShapeExtent::Cover => Some(RadialExtent::FarthestCorner),
    }
}
```

注: 同モジュール内なので `px_to_pt(...)` (line 39 の `pub(crate) fn`) をそのまま呼べる。`PX_TO_PT` 定数は private で外部 crate からは見えないが、ここは convert.rs 内なので問題ない。

**Step 3: `Image::Gradient` の dispatch を radial 対応に**

`convert.rs:3045` の `Image::Gradient(g) => resolve_linear_gradient(g, &current_color),` を以下に変更:

```rust
Image::Gradient(g) => {
    use style::values::computed::image::Gradient;
    // g: &Box<Gradient>。as_ref() で &Gradient を取って match。
    match g.as_ref() {
        Gradient::Linear { .. } => resolve_linear_gradient(g, &current_color),
        Gradient::Radial { .. } => resolve_radial_gradient(g, &current_color),
        Gradient::Conic { .. } => None,
    }
}
```

注: `Image::Gradient(Box<G>)` (stylo generics::image.rs:32) なので `g: &Box<Gradient>`。
`as_ref()` で `&Gradient` に降ろして match する。`resolve_linear_gradient` / `resolve_radial_gradient` は `&Gradient` を受け取るシグネチャ (auto-deref 経由で動作するが、明示的に `g.as_ref()` を渡しても同じ)。

**Step 4: cargo check で型エラーを潰す**

```bash
cargo check -p fulgur 2>&1 | tail -50
```

Expected: 残るエラーは `background.rs` の `BgImageContent` non-exhaustive match (Task 3 で対応)。

**Step 5: コミット**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(gradient): add resolve_radial_gradient and share color-stop resolution"
```

---

## Task 3: background.rs に draw_radial_gradient を実装

**Files:**

- Modify: `crates/fulgur/src/background.rs:207-218` (LinearGradient arm の隣に RadialGradient arm)
- Modify: `crates/fulgur/src/background.rs:281` (unreachable! arm に Radial を追加)
- Modify: `crates/fulgur/src/background.rs` 末尾付近 (`draw_radial_gradient` 関数を追加)

**Step 1: `draw_radial_gradient` 関数を追加**

`draw_linear_gradient` (`background.rs:319-395`) の直後に追加:

```rust
/// Draw a CSS radial-gradient over the origin rect.
///
/// CSS Images 3 §3.6 の式に従い (cx, cy, rx, ry) を計算し、Krilla の
/// `RadialGradient` (円のみサポート) に楕円を transform で表現する。
fn draw_radial_gradient(
    canvas: &mut Canvas<'_, '_>,
    shape: crate::pageable::RadialGradientShape,
    size: &crate::pageable::RadialGradientSize,
    position_x: &BgLengthPercentage,
    position_y: &BgLengthPercentage,
    stops: &[crate::pageable::GradientStop],
    ox: f32,
    oy: f32,
    ow: f32,
    oh: f32,
) {
    use crate::pageable::{RadialExtent, RadialGradientShape, RadialGradientSize};

    if ow <= 0.0 || oh <= 0.0 || stops.len() < 2 {
        return;
    }

    // 中心位置 (cx, cy) を origin rect 内の絶対座標に解決
    // CSS では position の percentage は origin rect の幅/高さに対する割合 (image=0)
    let cx = ox + resolve_point(position_x, ow);
    let cy = oy + resolve_point(position_y, oh);

    // 辺までの距離 (符号は問わない、abs で扱う)
    let left = (cx - ox).abs();
    let right = (ox + ow - cx).abs();
    let top = (cy - oy).abs();
    let bottom = (oy + oh - cy).abs();

    let (rx, ry) = match (shape, size) {
        (RadialGradientShape::Circle, RadialGradientSize::Extent(ext)) => {
            let r = match ext {
                RadialExtent::ClosestSide => left.min(right).min(top).min(bottom),
                RadialExtent::FarthestSide => left.max(right).max(top).max(bottom),
                RadialExtent::ClosestCorner => {
                    let dl = left.hypot(top);
                    let dr = right.hypot(top);
                    let dbl = left.hypot(bottom);
                    let dbr = right.hypot(bottom);
                    dl.min(dr).min(dbl).min(dbr)
                }
                RadialExtent::FarthestCorner => {
                    let dl = left.hypot(top);
                    let dr = right.hypot(top);
                    let dbl = left.hypot(bottom);
                    let dbr = right.hypot(bottom);
                    dl.max(dr).max(dbl).max(dbr)
                }
            };
            (r, r)
        }
        (RadialGradientShape::Circle, RadialGradientSize::Explicit { rx, .. }) => {
            // circle は parser 段階で rx == ry なので rx だけ使う
            let r = resolve_length(rx, ow); // 円半径は % 不可だが念のため resolve
            (r, r)
        }
        (RadialGradientShape::Ellipse, RadialGradientSize::Extent(ext)) => match ext {
            RadialExtent::ClosestSide => (left.min(right), top.min(bottom)),
            RadialExtent::FarthestSide => (left.max(right), top.max(bottom)),
            RadialExtent::ClosestCorner => {
                // CSS Images §3.6: closest-corner ellipse は closest-side の ratio スケール
                let (rx0, ry0) = (left.min(right), top.min(bottom));
                ellipse_corner_scale(cx, cy, ox, oy, ow, oh, rx0, ry0, false)
            }
            RadialExtent::FarthestCorner => {
                let (rx0, ry0) = (left.max(right), top.max(bottom));
                ellipse_corner_scale(cx, cy, ox, oy, ow, oh, rx0, ry0, true)
            }
        },
        (RadialGradientShape::Ellipse, RadialGradientSize::Explicit { rx, ry }) => {
            (resolve_length(rx, ow), resolve_length(ry, oh))
        }
    };

    if rx <= 0.0 || ry <= 0.0 {
        return;
    }

    let krilla_stops: Vec<krilla::paint::Stop> = stops
        .iter()
        .map(|s| krilla::paint::Stop {
            offset: krilla::num::NormalizedF32::new(s.offset.clamp(0.0, 1.0))
                .expect("offset is clamped to [0, 1]"),
            color: krilla::color::rgb::Color::new(s.rgba[0], s.rgba[1], s.rgba[2]).into(),
            opacity: crate::pageable::alpha_to_opacity(s.rgba[3]),
        })
        .collect();

    // Krilla の RadialGradient は円のみ。楕円は cr=rx + transform で y 軸を ry/rx に scale。
    // (cx, cy) を center に scale するので合成 T(cx,cy) · S(1, ry/rx) · T(-cx,-cy) を直接展開:
    //   x → x
    //   y → sy*y + cy*(1 - sy)  (sy = ry/rx)
    // tiny_skia の Transform 行列 |sx kx tx; ky sy ty; 0 0 1| に当てはめると
    //   sx=1, kx=0, tx=0, ky=0, sy=sy, ty=cy*(1-sy)
    // krilla の `pre_concat` は pub(crate) なので外部から chain できないため、
    // `from_row(sx, ky, kx, sy, tx, ty)` で行列直接構築する。
    let transform = if (rx - ry).abs() > f32::EPSILON {
        let scale_y = ry / rx;
        krilla::geom::Transform::from_row(1.0, 0.0, 0.0, scale_y, 0.0, cy * (1.0 - scale_y))
    } else {
        krilla::geom::Transform::default()
    };

    let rg = krilla::paint::RadialGradient {
        fx: cx,
        fy: cy,
        fr: 0.0,
        cx,
        cy,
        cr: rx, // 円半径 (楕円は transform で表現)
        transform,
        spread_method: krilla::paint::SpreadMethod::Pad,
        stops: krilla_stops,
        anti_alias: false,
    };

    canvas.surface.set_fill(Some(krilla::paint::Fill {
        paint: rg.into(),
        rule: Default::default(),
        opacity: krilla::num::NormalizedF32::ONE,
    }));
    canvas.surface.set_stroke(None);

    let Some(rect_path) = build_rect_path(ox, oy, ow, oh) else {
        canvas.surface.set_fill(None);
        return;
    };
    canvas.surface.draw_path(&rect_path);
    canvas.surface.set_fill(None);
}

/// `BgLengthPercentage` を origin rect 内の点座標に変換 (radial 中心位置用)。
/// CSS では radial-gradient の position percentage は container の幅/高さに対する単純な割合。
fn resolve_point(lp: &BgLengthPercentage, container: f32) -> f32 {
    match lp {
        BgLengthPercentage::Length(v) => *v,
        BgLengthPercentage::Percentage(p) => container * p,
    }
}

/// `BgLengthPercentage` を半径として解決 (length はそのまま、percentage は container 基準)。
fn resolve_length(lp: &BgLengthPercentage, container: f32) -> f32 {
    match lp {
        BgLengthPercentage::Length(v) => *v,
        BgLengthPercentage::Percentage(p) => container * p,
    }
}

/// CSS Images §3.6: ellipse の closest-corner / farthest-corner は
/// closest-side / farthest-side の (rx0, ry0) を ratio スケールしたもの。
/// `farthest=true` で最も遠い corner、`false` で最も近い corner を選ぶ。
fn ellipse_corner_scale(
    cx: f32, cy: f32, ox: f32, oy: f32, ow: f32, oh: f32,
    rx0: f32, ry0: f32, farthest: bool,
) -> (f32, f32) {
    if rx0 <= 0.0 || ry0 <= 0.0 {
        return (rx0, ry0);
    }
    let corners = [
        (ox, oy),
        (ox + ow, oy),
        (ox, oy + oh),
        (ox + ow, oy + oh),
    ];
    let ratios: Vec<f32> = corners
        .iter()
        .map(|(corx, cory)| {
            let dx = corx - cx;
            let dy = cory - cy;
            ((dx / rx0).powi(2) + (dy / ry0).powi(2)).sqrt()
        })
        .collect();
    let chosen = if farthest {
        ratios.iter().cloned().fold(0.0_f32, f32::max)
    } else {
        ratios.iter().cloned().fold(f32::INFINITY, f32::min)
    };
    (rx0 * chosen, ry0 * chosen)
}
```

**Step 2: `BgImageContent::LinearGradient` arm の隣に RadialGradient arm を追加**

`background.rs:207-218` の linear gradient match arm の直後 (218行目あたり):

```rust
BgImageContent::RadialGradient {
    shape,
    size,
    position_x,
    position_y,
    stops,
} => {
    draw_radial_gradient(
        canvas, *shape, size, position_x, position_y, stops, ox, oy, ow, oh,
    );
}
```

**Step 3: 下流の `unreachable!` arm に Radial を追加**

`background.rs:281` 付近の `BgImageContent::LinearGradient { .. } => unreachable!(...)` 行を以下に変更:

```rust
BgImageContent::LinearGradient { .. }
| BgImageContent::RadialGradient { .. } => unreachable!("handled above"),
```

**Step 4: cargo check + cargo build**

```bash
cargo check -p fulgur 2>&1 | tail -30
cargo build -p fulgur 2>&1 | tail -10
```

Expected: warning なし / エラーなし。

**Step 5: コミット**

```bash
git add crates/fulgur/src/background.rs
git commit -m "feat(gradient): implement draw_radial_gradient with circle/ellipse + extent keywords"
```

---

## Task 4: (skip) convert.rs unit test

Phase 1 の linear-gradient も convert.rs に直接の unit test を持たない (`grep -n "BgImageContent::LinearGradient\|resolve_linear_gradient" crates/fulgur/src/convert.rs` で確認済)。検証は VRT 経由で行う前提で揃っているため、radial も同じく Task 5 (VRT harness) を主な検証手段とする。

代わりに以下を Task 5 で確認:

- `radial-gradient(red, blue)` (CSS デフォルト = ellipse + farthest-corner + center) が PDF 上で円形〜楕円形に描画される
- Task 5 の ring-ref harness が PASS することで convert + draw 両方の正しさが間接的に保証される

**コミットなし — このタスクは skip。**

---

## Task 5: VRT harness 追加 — radial-gradient ring approximation

**Files:**

- Create: `crates/fulgur-vrt/fixtures/paint/radial-gradient-circular.html`
- Create: `crates/fulgur-vrt/tests/radial_gradient_harness.rs`

**Step 1: fixture HTML を作成**

`crates/fulgur-vrt/fixtures/paint/radial-gradient-circular.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT: radial-gradient circular red→blue</title>
<style>
  html, body { margin: 0; padding: 0; background: white; }
  /* 同心リング近似 ref と pixel 座標を揃えるため正方形・整数 raster サイズ。
     200 CSS px = 312.5 raster px @ 150dpi → 整数化のため 192 にする。
     192 CSS px = 300 raster px @ 150dpi (整数)。
     margin 32 CSS px = 50 raster px (整数)。 */
  .g {
    width: 192px;
    height: 192px;
    margin: 32px;
    background: radial-gradient(circle, #e53935 0%, #1e88e5 100%);
  }
</style>
</head>
<body>
  <div class="g"></div>
</body>
</html>
```

**Step 2: ring-ref harness を書く**

`crates/fulgur-vrt/tests/radial_gradient_harness.rs`:

```rust
//! Test↔ref pixel-diff harness for radial-gradient implementation.
//!
//! linear-gradient の strip 近似と同じ思想で、ref は同心リングの離散近似。
//! 各リングは外側に少しずつ大きくなる円 (`border-radius:50%`) で、
//! 中央の `(cx, cy)` から `r` の位置の色 (red→blue を r/R で線形補間) を塗る。
//!
//! リング数: 半径 R = 96 CSS px (= 192/2) なので、4 px ステップで 24 リング。
//! 4 CSS px = 6.25 raster px @ 150dpi (非整数だが、円形 AA は元々あるので
//! strip 同様の seam 問題は起きない)。
//!
//! 採用しなかった案:
//! - SVG `<radialGradient>` ref → fulgur SVG 経路と HTML 経路の両方を verify
//!   したい主目的とずれる
//! - PNG raster ref → CI 再現性で扱いづらい

use fulgur_vrt::diff::{self};
use fulgur_vrt::manifest::Tolerance;
use fulgur_vrt::pdf_render::{RenderSpec, pdf_to_rgba, render_html_to_pdf};
use std::fs;
use std::path::PathBuf;

const GRADIENT_SIZE_PX: u32 = 192;
const GRADIENT_MARGIN_PX: u32 = 32;
const RING_COUNT: u32 = 24;
const RING_STEP_PX: u32 = GRADIENT_SIZE_PX / 2 / RING_COUNT; // 96 / 24 = 4

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let av = a as f32;
    let bv = b as f32;
    (av + (bv - av) * t).round().clamp(0.0, 255.0) as u8
}

/// 同心リング近似 ref。外側 (大半径) の色から内側に向けて塗り重ねる
/// (z-index で内側を上に)。各リングの色は (リング中心半径 / 最大半径) の
/// 線形補間で決定。CSS の `radial-gradient(circle, c0 0%, c1 100%)` は
/// 中心 (r=0) で c0、外周 (r=R) で c1 なので、半径 r のリング色は
/// `lerp(c0, c1, r/R)`。
fn build_ring_ref_html(c0: (u8, u8, u8), c1: (u8, u8, u8)) -> String {
    let max_r = GRADIENT_SIZE_PX / 2;
    let mut rings = String::new();
    // 外側から内側へ描く (z-index は不要 — 後から書いた要素が上)
    for k in (0..RING_COUNT).rev() {
        let outer_r_px = (k + 1) * RING_STEP_PX; // 4, 8, ..., 96
        let mid_r = outer_r_px as f32 - (RING_STEP_PX as f32) / 2.0;
        let t = mid_r / max_r as f32;
        let r = lerp_u8(c0.0, c1.0, t);
        let g = lerp_u8(c0.1, c1.1, t);
        let b = lerp_u8(c0.2, c1.2, t);
        let d = outer_r_px * 2;
        let off = (max_r - outer_r_px) as i32;
        rings.push_str(&format!(
            r#"<div style="position:absolute;left:{off}px;top:{off}px;width:{d}px;height:{d}px;border-radius:50%;background:rgb({r},{g},{b});"></div>"#
        ));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT ref: radial-gradient ring approximation</title>
<style>
  html, body {{ margin: 0; padding: 0; background: white; }}
  .box {{ position: relative; width: {w}px; height: {h}px; margin: {m}px; }}
</style>
</head>
<body>
  <div class="box">{rings}</div>
</body>
</html>"#,
        w = GRADIENT_SIZE_PX,
        h = GRADIENT_SIZE_PX,
        m = GRADIENT_MARGIN_PX,
        rings = rings,
    )
}

#[test]
fn radial_gradient_circular_matches_ring_reference() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_html_path = crate_root.join("fixtures/paint/radial-gradient-circular.html");
    let test_html = fs::read_to_string(&test_html_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", test_html_path.display()));

    let ref_html = build_ring_ref_html((0xe5, 0x39, 0x35), (0x1e, 0x88, 0xe5));

    let spec = RenderSpec {
        page_size: "A4",
        margin_pt: Some(0.0),
        dpi: 150,
    };

    let test_pdf = render_html_to_pdf(&test_html, spec).expect("render test pdf");
    let ref_pdf = render_html_to_pdf(&ref_html, spec).expect("render ref pdf");

    let work_dir = crate_root
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("target/vrt-radial-gradient-harness");
    fs::create_dir_all(&work_dir).expect("create work dir");

    let test_dir = work_dir.join("test");
    let ref_dir = work_dir.join("ref");
    let _ = fs::remove_dir_all(&test_dir);
    let _ = fs::remove_dir_all(&ref_dir);
    fs::create_dir_all(&test_dir).expect("create test dir");
    fs::create_dir_all(&ref_dir).expect("create ref dir");

    let test_pdf_path = test_dir.join("test.pdf");
    let ref_pdf_path = ref_dir.join("ref.pdf");
    fs::write(&test_pdf_path, &test_pdf).expect("write test pdf");
    fs::write(&ref_pdf_path, &ref_pdf).expect("write ref pdf");

    let test_img = pdf_to_rgba(&test_pdf_path, spec.dpi, &test_dir).expect("rasterize test");
    let ref_img = pdf_to_rgba(&ref_pdf_path, spec.dpi, &ref_dir).expect("rasterize ref");

    // Tolerance: linear gradient harness は 10/0.5% で運用中。radial は
    // (a) 円弧境界の AA が strip 境界より広く出る、(b) 中心1点の収束特性、
    // の2点で linear より許容を緩める必要がある。だが 3% 等緩すぎると
    // `cr` を倍にしても通ってしまうので、12 channels / 1.0% に絞る。
    // - 12 channels: ring step (24 ring → 0.5*255/24 ≈ 5) + AA 余裕で 7 程度を見込み +5 の headroom
    // - 1.0%: 192² ≈ 110k pixel × 1% = 1100 pixel ≈ 24 ring の境界 1.5 px 帯と box 縁 AA で収まるはず
    // 失敗したら数値を調整する前に diff 画像を確認すること (生 diff 画像は work_dir に出る)。
    let tol = Tolerance {
        max_channel_diff: 12,
        max_diff_pixels_ratio: 0.01,
    };

    let report = diff::compare(&ref_img, &test_img, tol);

    assert!(
        report.pass,
        "radial gradient test↔ref harness failed: {} of {} pixels differ ({:.3}%), max channel diff = {} (tolerance: max_diff={}, ratio<={:.3}%). \
         test PDF: {}\n  ref PDF: {}\n  test img: {}\n  ref img:  {}",
        report.diff_pixels,
        report.total_pixels,
        report.ratio() * 100.0,
        report.max_channel_diff,
        tol.max_channel_diff,
        tol.max_diff_pixels_ratio * 100.0,
        test_pdf_path.display(),
        ref_pdf_path.display(),
        test_dir.join("page-1.png").display(),
        ref_dir.join("page-1.png").display(),
    );
}
```

**Step 3: テスト実行**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-gm56-radial-gradient
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  cargo test -p fulgur-vrt --test radial_gradient_harness 2>&1 | tail -30
```

Expected: PASS。FAIL の場合は `target/vrt-radial-gradient-harness/{test,ref}/page-1.png` を見て比較。tolerance の調整が必要なら数値だけ調整 (大きすぎる ratio は実装バグを見逃すので慎重に)。

**Step 4: コミット**

```bash
git add crates/fulgur-vrt/fixtures/paint/radial-gradient-circular.html crates/fulgur-vrt/tests/radial_gradient_harness.rs
git commit -m "test(vrt): add radial-gradient ring-approximation harness"
```

---

## Task 6: WPT expectations seed (css-images)

> **実装後追記 (PR #238):** 当初は新規 subdir 専用 list (`expectations/css-images.txt`)
> および対応するランナー (`tests/wpt_css_images.rs`) を作る計画だったが、実装時に
> 既存 `crates/fulgur-wpt/expectations/lists/bugs.txt` が cherry-pick 用 list
> として linear-gradient (fulgur-yax4) を既に管理していることが判明し、同じ
> 仕組みに寄せる方が運用統一できるため **`bugs.txt` への追記に変更** した。
> 新規 subdir 用ランナーは作っていない (`tests/wpt_lists.rs` の build.rs が
> `bugs.txt` を既に拾うため不要)。下記の Step 1〜4 はオリジナルの計画として
> 残す (履歴記録)。実装の最終形は `commit 51167f0` を参照。

**Files:**

- Create: `crates/fulgur-wpt/expectations/css-images.txt` (新規)

**Step 1: WPT ソース取得 + seed 実行**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-gm56-radial-gradient
scripts/wpt/fetch.sh 2>&1 | tail -10
```

WPT が既に取得済みであれば skip される。`target/wpt/css/css-images/` に reftest があることを確認:

```bash
ls target/wpt/css/css-images/ | grep -i radial | head -10
```

**Step 2: seed コマンドで expectations を生成**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  cargo run -p fulgur-wpt --example seed -- \
    --subdir css-images \
    --wpt-root target/wpt \
    --out crates/fulgur-wpt/expectations/css-images.txt 2>&1 | tail -30
```

Expected: `expectations/css-images.txt` が生成される。中身を見て radial-gradient 関連が PASS で何件入っているか確認。少なくとも 1〜2 件 PASS が目標。

**Step 3: ハーネスに新 subdir を組み込む**

既存 `crates/fulgur-wpt/tests/wpt_*.rs` に `wpt_css_page.rs` / `wpt_lists.rs` のパターンがある。新規 `wpt_css_images.rs` を追加:

```bash
cat crates/fulgur-wpt/tests/wpt_lists.rs # 構造を確認
```

それを真似て `wpt_css_images.rs` を作る (subdir 文字列だけ差し替え)。

```bash
cargo test -p fulgur-wpt --test wpt_css_images 2>&1 | tail -20
```

Expected: expectations 通り PASS / FAIL / SKIP が分類される。回帰なし (新規追加なので過去比較対象なし)。

**Step 4: コミット**

```bash
git add crates/fulgur-wpt/expectations/css-images.txt crates/fulgur-wpt/tests/wpt_css_images.rs
git commit -m "test(wpt): seed css-images expectations including radial-gradient reftests"
```

---

## Task 7: 全体検証 + format/lint

**Step 1: format**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-gm56-radial-gradient
cargo fmt
git diff --stat
```

差分があれば format コミット:

```bash
git add -u
git commit -m "style: cargo fmt"
```

**Step 2: clippy**

```bash
cargo clippy -p fulgur -p fulgur-vrt -p fulgur-wpt --tests 2>&1 | tail -30
```

Expected: warning / error 共になし。出たら個別対応。

**Step 3: 全 unit test**

```bash
cargo test -p fulgur --lib 2>&1 | tail -20
```

Expected: 既存 ~340 + 新規 radial test が PASS。

**Step 4: 全 VRT**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  cargo test -p fulgur-vrt 2>&1 | tail -20
```

Expected: linear-gradient + radial-gradient + 既存全 VRT が PASS。回帰なし。

**Step 5: markdownlint**

```bash
npx markdownlint-cli2 'docs/plans/2026-04-26-radial-gradient.md' 2>&1 | tail -10
```

Expected: 警告なし。

**Step 6: 最終コミット (もし残あれば)**

```bash
git status
```

差分がなければ Task 7 はノーコミットで完了。

---

## Task 8: PR 作成 (必要に応じて)

**Step 1: ログ確認**

```bash
git log --oneline main..HEAD
```

5〜7 コミット並んでいるはず。

**Step 2: push + PR**

ユーザーに確認してから push:

```bash
git push -u origin feature/fulgur-gm56-radial-gradient
gh pr create --title "feat(gradient): radial-gradient() support (Phase 2 child of fulgur-yax4)" --body "$(cat <<'EOF'
## 概要

CSS `radial-gradient()` を Krilla の `RadialGradient` API に配線する。
Phase 1 で確立した linear-gradient のパターンを踏襲。

closes fulgur-gm56

## 主な変更

- `pageable.rs`: `BgImageContent::RadialGradient` variant + 関連 enum
- `convert.rs`: `resolve_radial_gradient` + 共通化した `resolve_color_stops`
- `background.rs`: `draw_radial_gradient` (CSS Images 3 §3.6 の式に従う shape/size 解決, ellipse は Krilla transform で scale)
- VRT: 同心リング近似 ref harness
- WPT: `css-images` expectations seed (radial-gradient reftest を組み入れ)

## スコープ外

- `repeating-radial-gradient` (fulgur-12z0)
- 非デフォルト interpolation method / length-typed stop / 範囲外 stop / hint (既存 linear と同じく silent None)

## Test plan

- [ ] `cargo test -p fulgur --lib`: 既存 + radial unit test PASS
- [ ] `cargo test -p fulgur-vrt --test radial_gradient_harness` PASS
- [ ] `cargo test -p fulgur-wpt --test wpt_css_images` 期待通り
- [ ] `cargo fmt --check` / `cargo clippy` clean
EOF
)"
```

---

## Plan summary

| Task | 行数感 | 期待時間 |
|---|---|---|
| 1. pageable.rs 型追加 | ~40 | 5 分 |
| 2. convert.rs (resolve_color_stops + resolve_radial_gradient + dispatch) | ~120 | 20 分 |
| 3. background.rs (draw_radial_gradient + helpers) | ~150 | 25 分 |
| 4. unit test (skip) | -- | -- |
| 5. VRT harness | ~150 | 20 分 |
| 6. WPT seed | ~30 | 15 分 |
| 7. 検証 (fmt/clippy/全テスト) | -- | 10 分 |
| 8. PR | -- | 5 分 |
| **合計** | -- | **~100 分** |
