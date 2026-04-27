//! Background rendering: color fills and image layers.

use std::sync::Arc;

use crate::pageable::{
    BackgroundLayer, BgBox, BgClip, BgImageContent, BgLengthPercentage, BgRepeat, BgSize,
    BlockStyle, Canvas,
};

/// Draw outer box-shadows behind the element's background.
///
/// Per CSS Backgrounds §7.2, shadows are painted in reverse declaration order
/// (last-declared shadow at the bottom of the paint stack, first-declared on top).
/// Outer shadows are drawn _below_ the element's background and border.
/// `inset` shadows are currently unsupported and excluded upstream in
/// `convert.rs::extract_block_style` (never pushed into `box_shadows`).
pub fn draw_box_shadows(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    if style.box_shadows.is_empty() {
        return;
    }
    for shadow in style.box_shadows.iter().rev() {
        if shadow.inset {
            continue; // defensive; inset shadows are excluded upstream in convert.rs
        }
        draw_single_box_shadow(canvas, style, shadow, x, y, w, h);
    }
}

fn draw_single_box_shadow(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    shadow: &crate::pageable::BoxShadow,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    // NOTE: when blur rendering is implemented (fulgur-4ie follow-up), this rect
    // must also be expanded by the blur radius, and the blur extent drawn via
    // rasterization + gaussian blur + image embed.
    let sx = x + shadow.offset_x - shadow.spread;
    let sy = y + shadow.offset_y - shadow.spread;
    let sw = w + 2.0 * shadow.spread;
    let sh = h + 2.0 * shadow.spread;
    if sw <= 0.0 || sh <= 0.0 {
        return;
    }

    // Build the (expanded) shadow shape.
    let shadow_path = if style.has_radius() {
        let radii = expand_radii(&style.border_radii, shadow.spread);
        crate::pageable::build_rounded_rect_path(sx, sy, sw, sh, &radii)
    } else {
        build_rect_path(sx, sy, sw, sh)
    };
    let Some(shadow_path) = shadow_path else {
        return;
    };

    // Per CSS Backgrounds §7.2, outer shadows are only visible *outside* the
    // element's border-box. If we painted the expanded shape directly, elements
    // with transparent or semi-transparent backgrounds would show the shadow
    // bleeding through the interior. To prevent this we clip the shadow by
    // excluding the border-box using an EvenOdd clip path: the clip region
    // covers the shadow's bounding box minus the border-box.
    let clip_path = {
        let mut pb = krilla::geom::PathBuilder::new();
        let Some(bbox) = krilla::geom::Rect::from_xywh(sx, sy, sw, sh) else {
            return;
        };
        pb.push_rect(bbox);
        if style.has_radius() {
            crate::pageable::append_rounded_rect_subpath(&mut pb, x, y, w, h, &style.border_radii);
        } else if let Some(box_rect) = krilla::geom::Rect::from_xywh(x, y, w, h) {
            pb.push_rect(box_rect);
        } else {
            return;
        }
        pb.finish()
    };
    let Some(clip_path) = clip_path else { return };

    canvas
        .surface
        .push_clip_path(&clip_path, &krilla::paint::FillRule::EvenOdd);

    let [r, g, b, a] = shadow.color;
    canvas.surface.set_fill(Some(krilla::paint::Fill {
        paint: krilla::color::rgb::Color::new(r, g, b).into(),
        opacity: krilla::num::NormalizedF32::new(a as f32 / 255.0)
            .unwrap_or(krilla::num::NormalizedF32::ONE),
        rule: Default::default(),
    }));
    canvas.surface.set_stroke(None);
    canvas.surface.draw_path(&shadow_path);

    canvas.surface.pop();
}

/// Expand border radii by `spread`. Negative `spread` clamps to zero per CSS spec
/// (shadow corners become sharp when spread < -radius). Corners with zero radius
/// stay sharp regardless of spread, per CSS Backgrounds and Borders Level 3.
fn expand_radii(outer: &[[f32; 2]; 4], spread: f32) -> [[f32; 2]; 4] {
    let expand = |r: f32| {
        if r == 0.0 {
            0.0
        } else {
            f32::max(r + spread, 0.0)
        }
    };
    [
        [expand(outer[0][0]), expand(outer[0][1])],
        [expand(outer[1][0]), expand(outer[1][1])],
        [expand(outer[2][0]), expand(outer[2][1])],
        [expand(outer[3][0]), expand(outer[3][1])],
    ]
}

/// Draw all background layers for a block element.
pub fn draw_background(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    draw_background_color(canvas, style, x, y, w, h);
    // Draw layers in reverse order (last declared = bottom-most)
    for layer in style.background_layers.iter().rev() {
        draw_background_layer(canvas, style, layer, x, y, w, h);
    }
}

fn build_rect_path(x: f32, y: f32, w: f32, h: f32) -> Option<krilla::geom::Path> {
    let rect = krilla::geom::Rect::from_xywh(x, y, w, h)?;
    let mut pb = krilla::geom::PathBuilder::new();
    pb.push_rect(rect);
    pb.finish()
}

fn draw_background_color(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let Some(bg) = &style.background_color else {
        return;
    };
    let path = if style.has_radius() {
        crate::pageable::build_rounded_rect_path(x, y, w, h, &style.border_radii)
    } else {
        build_rect_path(x, y, w, h)
    };

    if let Some(path) = path {
        canvas.surface.set_fill(Some(krilla::paint::Fill {
            paint: krilla::color::rgb::Color::new(bg[0], bg[1], bg[2]).into(),
            opacity: krilla::num::NormalizedF32::new(bg[3] as f32 / 255.0)
                .unwrap_or(krilla::num::NormalizedF32::ONE),
            rule: Default::default(),
        }));
        canvas.surface.set_stroke(None);
        canvas.surface.draw_path(&path);
    }
}

/// Draw a single background image layer.
fn draw_background_layer(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    layer: &BackgroundLayer,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let (ox, oy, ow, oh) = compute_origin_rect(style, &layer.origin, x, y, w, h);
    let (cx, cy, cw, ch) = compute_clip_rect(style, &layer.clip, x, y, w, h);
    if cw <= 0.0 || ch <= 0.0 {
        return;
    }

    let clip_path = if style.has_radius() {
        let clip_radii = compute_inner_radii(&style.border_radii, style, &layer.clip);
        crate::pageable::build_rounded_rect_path(cx, cy, cw, ch, &clip_radii)
    } else {
        build_rect_path(cx, cy, cw, ch)
    };
    let Some(clip_path) = clip_path else {
        return;
    };
    canvas
        .surface
        .push_clip_path(&clip_path, &krilla::paint::FillRule::default());

    let (img_w, img_h) = match &layer.content {
        BgImageContent::LinearGradient { .. } | BgImageContent::RadialGradient { .. } => {
            resolve_gradient_size(&layer.size, ow, oh)
        }
        BgImageContent::Raster { .. } | BgImageContent::Svg { .. } => resolve_size(layer, ow, oh),
    };
    if img_w <= 0.0 || img_h <= 0.0 {
        canvas.surface.pop();
        return;
    }

    let pos_x = ox + resolve_position(&layer.position_x, ow, img_w);
    let pos_y = oy + resolve_position(&layer.position_y, oh, img_h);

    let tiles = compute_tile_positions(
        layer.repeat_x,
        layer.repeat_y,
        pos_x,
        pos_y,
        img_w,
        img_h,
        cx,
        cy,
        cw,
        ch,
    );
    if tiles.is_empty() {
        canvas.surface.pop();
        return;
    }

    match &layer.content {
        BgImageContent::LinearGradient {
            direction,
            stops,
            repeating,
        } => {
            // Try to detect a uniform tile grid and emit a single Tiling Pattern
            // resource (one Function 2 + Shading 2 + Pattern triplet) rather
            // than N independent gradient draws. Falls back to the per-tile
            // loop for irregular tile geometry (e.g. uneven space repeat).
            if let Some(grid) = try_uniform_grid(&tiles) {
                let angle = match direction {
                    crate::pageable::LinearGradientDirection::Angle(a) => *a,
                    crate::pageable::LinearGradientDirection::Corner(corner) => {
                        // uniform grid → all tiles share the same (cell_w, cell_h)
                        // aspect, so a single corner-derived angle suffices.
                        corner_to_angle_rad(*corner, grid.cell.0, grid.cell.1)
                    }
                };
                draw_gradient_tiling_pattern(canvas, grid, |surface, _tw, _th| {
                    draw_linear_gradient(
                        surface,
                        angle,
                        stops,
                        *repeating,
                        0.0,
                        0.0,
                        grid.cell.0,
                        grid.cell.1,
                    );
                });
            } else {
                // Fallback: per-tile loop. Match before the loop (Angle hoists,
                // Corner needs per-tile recomputation because the angle depends
                // on tile aspect — CSS Images §3.1.1).
                match direction {
                    crate::pageable::LinearGradientDirection::Angle(a) => {
                        let angle = *a;
                        for (tx, ty, tw, th) in &tiles {
                            draw_linear_gradient(
                                canvas.surface,
                                angle,
                                stops,
                                *repeating,
                                *tx,
                                *ty,
                                *tw,
                                *th,
                            );
                        }
                    }
                    crate::pageable::LinearGradientDirection::Corner(corner) => {
                        for (tx, ty, tw, th) in &tiles {
                            let angle = corner_to_angle_rad(*corner, *tw, *th);
                            draw_linear_gradient(
                                canvas.surface,
                                angle,
                                stops,
                                *repeating,
                                *tx,
                                *ty,
                                *tw,
                                *th,
                            );
                        }
                    }
                }
            }
        }
        BgImageContent::RadialGradient {
            shape,
            size,
            position_x,
            position_y,
            stops,
            repeating,
        } => {
            // Same dedup story as Linear: uniform grids share (cell_w, cell_h)
            // so cx/cy/rx/ry are identical across tiles → a single Pattern
            // resource is sound.
            if let Some(grid) = try_uniform_grid(&tiles) {
                draw_gradient_tiling_pattern(canvas, grid, |surface, tw, th| {
                    draw_radial_gradient(
                        surface, *shape, size, position_x, position_y, stops, *repeating, 0.0, 0.0,
                        tw, th,
                    );
                });
            } else {
                // Per-tile shape geometry — uses each tile's own (tw, th)
                // for cx/cy/rx/ry. No uniformity assumption needed.
                for (tx, ty, tw, th) in &tiles {
                    draw_radial_gradient(
                        canvas.surface,
                        *shape,
                        size,
                        position_x,
                        position_y,
                        stops,
                        *repeating,
                        *tx,
                        *ty,
                        *tw,
                        *th,
                    );
                }
            }
        }
        BgImageContent::Raster { data, format } => {
            let data: krilla::Data = Arc::clone(data).into();
            let Ok(image) = format.to_krilla_image(data) else {
                canvas.surface.pop();
                return;
            };
            for (tx, ty, tw, th) in &tiles {
                let Some(size) = krilla::geom::Size::from_wh(*tw, *th) else {
                    continue;
                };
                let transform = krilla::geom::Transform::from_translate(*tx, *ty);
                canvas.surface.push_transform(&transform);
                canvas.surface.draw_image(image.clone(), size);
                canvas.surface.pop();
            }
        }
        BgImageContent::Svg { tree } => {
            use krilla_svg::{SurfaceExt, SvgSettings};
            for (tx, ty, tw, th) in &tiles {
                let Some(size) = krilla::geom::Size::from_wh(*tw, *th) else {
                    continue;
                };
                let transform = krilla::geom::Transform::from_translate(*tx, *ty);
                canvas.surface.push_transform(&transform);
                if canvas
                    .surface
                    .draw_svg(tree, size, SvgSettings::default())
                    .is_none()
                {
                    log::warn!("failed to draw SVG background tile");
                }
                canvas.surface.pop();
            }
        }
    }

    canvas.surface.pop();
}

/// Resolve a `to <corner>` direction to a CSS gradient angle (radians)
/// for a `width × height` gradient box.
///
/// Per CSS Images 3 §3.1.1, the gradient line is perpendicular to the
/// diagonal connecting the two corners NOT in the start/end pair, so the
/// angle depends on the box's aspect ratio. In Y-down coordinates the
/// gradient direction is `(H · h_sign, W · v_sign)`, then
/// `θ = atan2(H · h_sign, −W · v_sign)` because CSS measures clockwise from
/// the +Y-up axis (`direction(θ) = (sin θ, −cos θ)` in Y-down).
fn corner_to_angle_rad(corner: crate::pageable::LinearGradientCorner, w: f32, h: f32) -> f32 {
    use crate::pageable::LinearGradientCorner::*;
    let (h_sign, v_sign) = match corner {
        TopLeft => (-1.0_f32, -1.0_f32),
        TopRight => (1.0, -1.0),
        BottomLeft => (-1.0, 1.0),
        BottomRight => (1.0, 1.0),
    };
    (h * h_sign).atan2(-w * v_sign)
}

/// 範囲外 fraction を CSS Images 3 §3.5.1 準拠で `[0, 1]` 内表現に renormalize する。
///
/// 入力: monotonically non-decreasing position の `(pos, rgba)` ベクタ。
/// `pos` は ℝ で、範囲外 (`-0.5` や `1.5` など) も許容する。
///
/// 出力: `pos` が `[0, 1]` に収まる `(pos, rgba)` ベクタ。Krilla の
/// `NormalizedF32` 制約をそのまま満たす。
///
/// アルゴリズム:
/// - fast path: 全 stop が `[0, 1]` 内なら無変換
/// - 範囲外を含む場合: `color_at(0)` / `color_at(1)` を隣接 stop 線形補間
///   (前方は first stop の色で pad、後方は last stop の色で pad) で合成し、
///   `(0, 1)` 内既存 stop と組み合わせる
/// - 既に in-range stop が境界 (`0.0` / `1.0`) ぴったりに座っている場合は
///   重複合成しない
fn renormalize_stops_to_unit_range(stops: Vec<(f32, [u8; 4])>) -> Vec<(f32, [u8; 4])> {
    debug_assert!(stops.len() >= 2, "caller guaranteed len >= 2");

    if stops.iter().all(|(p, _)| (0.0..=1.0).contains(p)) {
        return stops;
    }

    let last_idx = stops.len() - 1;
    let color_at = |t: f32| -> [u8; 4] {
        if t <= stops[0].0 {
            return stops[0].1;
        }
        if t >= stops[last_idx].0 {
            return stops[last_idx].1;
        }
        for w in stops.windows(2) {
            let (p0, c0) = w[0];
            let (p1, c1) = w[1];
            if p0 <= t && t <= p1 {
                let span = p1 - p0;
                if span <= 0.0 {
                    // hard stop boundary (coincident positions): 後方の色を採用
                    return c1;
                }
                let alpha = (t - p0) / span;
                return [
                    lerp_u8(c0[0], c1[0], alpha),
                    lerp_u8(c0[1], c1[1], alpha),
                    lerp_u8(c0[2], c1[2], alpha),
                    lerp_u8(c0[3], c1[3], alpha),
                ];
            }
        }
        unreachable!("stops are monotonic and t is in [stops[0].0, stops[last].0]")
    };

    // 上流 (`resolve_gradient_stops` の Auto fill / fr(0.0) / px(0.0) 等) は
    // exact 0.0 / 1.0 を保証するため、浮動小数 `==` 比較で十分。
    let has_stop_at_zero = stops.iter().any(|(p, _)| *p == 0.0);
    let has_stop_at_one = stops.iter().any(|(p, _)| *p == 1.0);

    let mut result = Vec::with_capacity(stops.len() + 2);

    if stops[0].0 != 0.0 && !has_stop_at_zero {
        result.push((0.0, color_at(0.0)));
    }

    for (p, rgba) in stops.iter() {
        if (0.0..=1.0).contains(p) {
            result.push((*p, *rgba));
        }
    }

    if stops[last_idx].0 != 1.0 && !has_stop_at_one {
        result.push((1.0, color_at(1.0)));
    }

    result
}

#[inline]
fn lerp_u8(a: u8, b: u8, alpha: f32) -> u8 {
    let av = a as f32;
    let bv = b as f32;
    (av + (bv - av) * alpha).round().clamp(0.0, 255.0) as u8
}

/// Resolve `Vec<GradientStop>` (length / fraction / auto 混在) を krilla の
/// `Stop` 列に変換する。
///
/// **Unit contract:** `line_length` must be in **CSS px** (matching
/// `GradientStopPosition::LengthPx`). Callers in pt-space (Krilla draw
/// surface) convert with `crate::convert::pt_to_px` before invoking.
///
/// CSS Images Level 3 §3.5.1 の color-stop fixup に従って:
///   1. `LengthPx(px)` を `px / line_length` で fraction 化
///   2. 先頭/末尾の `Auto` を 0.0 / 1.0 に確定
///   3. monotonic clamp (前 fixed より小さければ前 fixed に合わせる)
///   4. 中間の `Auto` 群を前後 fixed の等間隔補間で埋める
///   5. `repeating=true` なら fixup 後の `(p_first, p_last)` を周期に取り、
///      `[0, 1]` を覆うだけ stop 列を平行コピー (CSS Images 3 §3.6)
///   6. 最終 fraction が `[0, 1]` 外なら `renormalize_stops_to_unit_range` で
///      端点合成し正規化する (CSS Images 3 §3.5.1; fulgur-n3zk)
///
/// `line_length <= 0` の場合は length stop が解決不能なので None。
fn resolve_gradient_stops(
    stops: &[crate::pageable::GradientStop],
    line_length: f32,
    repeating: bool,
) -> Option<Vec<krilla::paint::Stop>> {
    use crate::pageable::GradientStopPosition;

    if stops.len() < 2 {
        return None;
    }
    if line_length <= 0.0 {
        // length-typed stop が一つでもあれば解決不能。fraction-only でも
        // 退化した gradient なので Layer drop 相当 (caller で先に early
        // return しているため通常到達しない)。
        return None;
    }

    let n = stops.len();
    let mut positions: Vec<Option<f32>> = stops
        .iter()
        .map(|s| match s.position {
            GradientStopPosition::Auto => None,
            GradientStopPosition::Fraction(f) => Some(f),
            GradientStopPosition::LengthPx(px) => Some(px / line_length),
        })
        .collect();

    // 先頭/末尾の Auto を 0/1 に
    if positions[0].is_none() {
        positions[0] = Some(0.0);
    }
    if positions[n - 1].is_none() {
        positions[n - 1] = Some(1.0);
    }

    // monotonic clamp.
    // 初期値が NEG_INFINITY (not 0.0) なのは意図的: renormalize は負値の
    // 入力位置をそのまま受け取って端点合成する必要があるため、
    // 0.0 初期値だと Fraction(-0.1) が暗黙に 0.0 に押し上げられて
    // out-of-range 入力として扱われなくなる。
    let mut last_resolved = f32::NEG_INFINITY;
    for v in positions.iter_mut().flatten() {
        if *v < last_resolved {
            *v = last_resolved;
        }
        last_resolved = *v;
    }

    // 中間 Auto を前後 fixed の等間隔補間で埋める
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

    let resolved: Vec<(f32, [u8; 4])> = stops
        .iter()
        .zip(positions)
        .map(|(s, p)| (p.expect("all slots resolved"), s.rgba))
        .collect();

    // 周期展開: repeating-* gradient は first/last stop 間の差を周期に取り、
    // `[0, 1]` を覆うように copy を平行移動して並べる。renormalize 段で
    // 範囲外 stop は端点補間にクリップされるので、ここでは [0, 1] を
    // 充分カバーするだけ生成すればよい。
    let expanded = if repeating {
        expand_repeating_stops(resolved)?
    } else {
        resolved
    };

    // renormalize: 範囲外 fraction は端点合成で [0, 1] 内表現に変換 (fulgur-n3zk)
    let renormalized = renormalize_stops_to_unit_range(expanded);
    debug_assert!(
        renormalized.len() >= 2,
        "renormalize_stops_to_unit_range guarantees len >= 2"
    );

    Some(
        renormalized
            .into_iter()
            .map(|(p, rgba)| krilla::paint::Stop {
                offset: krilla::num::NormalizedF32::new(p).expect("renormalize guarantees [0, 1]"),
                color: krilla::color::rgb::Color::new(rgba[0], rgba[1], rgba[2]).into(),
                opacity: crate::pageable::alpha_to_opacity(rgba[3]),
            })
            .collect(),
    )
}

/// `repeating-linear-gradient` / `repeating-radial-gradient` の周期展開。
///
/// 入力: fixup 後の monotonically non-decreasing `(pos, rgba)` ベクタ (`len >= 2`)。
/// 出力: `[0, 1]` 範囲を覆うように first/last 差を周期として平行コピーした列。
/// `renormalize_stops_to_unit_range` 段で out-of-range stop が端点補間に丸められる
/// 前提なので、ここでは充分なオーバーラップを与える。
///
/// `period <= 0` (degenerate) は CSS Images 3 §3.6 に従い「最終 stop の色で
/// 塗りつぶす単色」として扱う。krilla は最低 2 stop 必要なので、`(0, last)`
/// と `(1, last)` の 2 stop を返す。`None` ではなく単色 fill を返すことで
/// `repeating-linear-gradient(red 0%, blue 0%)` のような入力もブラウザと
/// 同じ blue 単色描画になる (PR #244 coderabbit review より)。
/// `period > 0` でも安全上限 `MAX_PERIODS` を超えたら最終周期で打ち切り
/// (`repeating-linear-gradient(red 0%, blue 0.01%)` のような病的入力でも有界化)。
fn expand_repeating_stops(stops: Vec<(f32, [u8; 4])>) -> Option<Vec<(f32, [u8; 4])>> {
    debug_assert!(stops.len() >= 2, "caller guaranteed len >= 2");

    let p_first = stops.first().expect("len >= 2").0;
    let p_last = stops.last().expect("len >= 2").0;
    let period = p_last - p_first;

    // CSS Images 3 §3.6: 周期が 0 または負 (monotonic clamp 通過後に first/last が
    // 同位置に潰れた場合) は degenerate。最終 stop の色で塗りつぶす単色 fill を返す。
    // NaN/Inf も同じ扱いで安全側に倒す。
    if period <= 0.0 || !period.is_finite() {
        let last_color = stops.last().expect("len >= 2").1;
        return Some(vec![(0.0, last_color), (1.0, last_color)]);
    }

    // 安全上限。256 周期あれば line_length 比 0.4% の周期まで描き切れ、
    // それ以下は実質連続色なので品質劣化しない。`stops.len() = 100` 等の
    // pathological 入力 × MAX_PERIODS でも合計 5 万 stop 以下に留まる。
    const MAX_PERIODS: i32 = 256;

    // forward: p_last + k*period >= 1 となる最小の k
    // backward: p_first + k*period <= 0 となる最小の |k|  (k は負方向)
    let forward = if p_last >= 1.0 {
        0
    } else {
        ((1.0 - p_last) / period).ceil() as i32
    };
    let backward = if p_first <= 0.0 {
        0
    } else {
        (p_first / period).ceil() as i32
    };

    let forward = forward.min(MAX_PERIODS);
    let backward = backward.min(MAX_PERIODS);

    let total_copies = (forward + backward + 1) as usize;
    let mut out: Vec<(f32, [u8; 4])> = Vec::with_capacity(total_copies * stops.len());

    // k = -backward .. = forward の順に shift 量 k*period を加えて並べる。
    // 同位置で隣接する周期境界の color が違う場合 (e.g. period 末尾 blue →
    // 次周期先頭 red) は hard stop として出力に残し、renormalize の線形補間
    // (`p0 == p1` 時は後者の色を採用) と整合する。
    for k in -backward..=forward {
        let shift = (k as f32) * period;
        for &(p, rgba) in &stops {
            out.push((p + shift, rgba));
        }
    }

    Some(out)
}

/// Draw a CSS linear-gradient over the origin rect.
///
/// CSS angle convention (radians): 0 = "to top", π/2 = "to right",
/// π = "to bottom", 3π/2 = "to left", clockwise. Krilla / fulgur use Y-down
/// (top-left origin) so "to top" means decreasing Y.
///
/// The gradient line passes through the center of the origin rect with length
/// `|W·sin θ| + |H·cos θ|` — this is the projection of both diagonals onto the
/// gradient axis, ensuring the line spans corner-to-corner regardless of angle
/// (CSS Images §3.1).
#[allow(clippy::too_many_arguments)]
fn draw_linear_gradient(
    surface: &mut krilla::surface::Surface<'_>,
    angle_rad: f32,
    stops: &[crate::pageable::GradientStop],
    repeating: bool,
    ox: f32,
    oy: f32,
    ow: f32,
    oh: f32,
) {
    if ow <= 0.0 || oh <= 0.0 || stops.len() < 2 {
        return;
    }

    let sin = angle_rad.sin();
    // CSS y-axis points up; our Y-down system flips it. "to top" (angle=0)
    // must produce a line ending at the top of the box (low Y), so we
    // negate cos to express the direction in Y-down space.
    let cos_neg = -angle_rad.cos();

    let length = (ow * sin).abs() + (oh * cos_neg).abs();
    if length <= 0.0 {
        return;
    }

    let cx_box = ox + ow * 0.5;
    let cy_box = oy + oh * 0.5;
    let half = length * 0.5;
    let x1 = cx_box - sin * half;
    let y1 = cy_box - cos_neg * half;
    let x2 = cx_box + sin * half;
    let y2 = cy_box + cos_neg * half;

    // length は pt 単位 (ow, oh が pt) だが、`GradientStopPosition::LengthPx` は
    // CSS px で保持されている。`resolve_gradient_stops` は同一単位空間での
    // `px / line_length` で fraction 化するので、line_length も CSS px に揃える。
    // (例: 400px box → length = 300pt → px 換算で 400 → 50px / 400 = 0.125)
    let length_px = crate::convert::pt_to_px(length);
    let Some(krilla_stops) = resolve_gradient_stops(stops, length_px, repeating) else {
        return;
    };

    let lg = krilla::paint::LinearGradient {
        x1,
        y1,
        x2,
        y2,
        transform: krilla::geom::Transform::default(),
        spread_method: krilla::paint::SpreadMethod::Pad,
        stops: krilla_stops,
        anti_alias: false,
    };

    surface.set_fill(Some(krilla::paint::Fill {
        paint: lg.into(),
        rule: Default::default(),
        opacity: krilla::num::NormalizedF32::ONE,
    }));
    surface.set_stroke(None);

    // Per CSS Images §3, the gradient image has the size of the positioning
    // (origin) area; areas inside `clip` but outside `origin` should be
    // transparent for this layer. With `SpreadMethod::Pad`, painting the
    // clip rect would extend the first/last stop colors as solid bands into
    // those areas. Draw the origin rect — the caller's already-pushed
    // clip_path bounds it, so what's rendered is `origin ∩ clip`, which is
    // the spec-correct visible region.
    let Some(rect_path) = build_rect_path(ox, oy, ow, oh) else {
        surface.set_fill(None);
        return;
    };
    surface.draw_path(&rect_path);
    // Don't leak the gradient paint to the next draw on this surface.
    surface.set_fill(None);
}

/// Draw a CSS radial-gradient over the origin rect.
///
/// CSS Images 3 §3.6 の式に従い (cx, cy, rx, ry) を計算し、Krilla の
/// `RadialGradient` (円のみサポート) に楕円を transform で表現する。
#[allow(clippy::too_many_arguments)]
fn draw_radial_gradient(
    surface: &mut krilla::surface::Surface<'_>,
    shape: crate::pageable::RadialGradientShape,
    size: &crate::pageable::RadialGradientSize,
    position_x: &BgLengthPercentage,
    position_y: &BgLengthPercentage,
    stops: &[crate::pageable::GradientStop],
    repeating: bool,
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
            // circle は parser 段階で rx == ry なので rx だけ使う (% 不可だが念のため resolve)
            let r = resolve_length(rx, ow);
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

    // Radial gradient line length = rx (CSS Images §3.6.1, ellipse でも +X 軸)。
    // rx は pt 単位 (ow/oh が pt) なので、`LengthPx` (CSS px) との比較のために
    // CSS px に揃える。
    let rx_px = crate::convert::pt_to_px(rx);
    let Some(krilla_stops) = resolve_gradient_stops(stops, rx_px, repeating) else {
        return;
    };

    // Krilla の RadialGradient は円のみ。楕円は cr=rx + transform で y 軸を ry/rx に scale。
    // 合成 T(cx,cy) · S(1, ry/rx) · T(-cx,-cy) を直接展開:
    //   x → x
    //   y → sy*y + cy*(1 - sy)  (sy = ry/rx)
    // tiny_skia の Transform 行列 |sx kx tx; ky sy ty; 0 0 1| に当てはめると
    //   sx=1, kx=0, tx=0, ky=0, sy=sy, ty=cy*(1-sy)
    // krilla::geom::Transform の `pre_concat` は pub(crate) で外部から chain できないため、
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

    surface.set_fill(Some(krilla::paint::Fill {
        paint: rg.into(),
        rule: Default::default(),
        opacity: krilla::num::NormalizedF32::ONE,
    }));
    surface.set_stroke(None);

    let Some(rect_path) = build_rect_path(ox, oy, ow, oh) else {
        surface.set_fill(None);
        return;
    };
    surface.draw_path(&rect_path);
    surface.set_fill(None);
}

/// uniform-grid 検出時の Tiling Pattern 描画ヘルパー。
///
/// 1. `surface.stream_builder().surface()` で sub-surface を取得し、
///    `paint_in_cell` クロージャで gradient を `(0, 0, tile_w, tile_h)` に描画。
/// 2. `Pattern { stream, transform: Translate(origin), width: step_x, height: step_y }`
///    を構築 (PDF /Matrix · /XStep · /YStep に対応)。
/// 3. `set_fill(pattern)` + `draw_path(union_rect)` で塗りつぶし。
///    既存の `clip_path` がレイヤーの可視領域を bound する。
///
/// Krilla の `Pattern` は `Cacheable` なので、同じ stream / transform / step を持つ
/// Pattern は resource 層で dedupe される (`grid` 全体で 1 個の Pattern resource)。
fn draw_gradient_tiling_pattern(
    canvas: &mut Canvas<'_, '_>,
    grid: UniformGrid,
    paint_in_cell: impl FnOnce(&mut krilla::surface::Surface<'_>, f32, f32),
) {
    let mut sb = canvas.surface.stream_builder();
    {
        let mut ps = sb.surface();
        paint_in_cell(&mut ps, grid.cell.0, grid.cell.1);
        ps.finish();
    }
    let stream = sb.finish();

    let pattern = krilla::paint::Pattern {
        stream,
        transform: krilla::geom::Transform::from_translate(grid.origin.0, grid.origin.1),
        width: grid.step.0,
        height: grid.step.1,
    };

    canvas.surface.set_fill(Some(krilla::paint::Fill {
        paint: pattern.into(),
        rule: Default::default(),
        opacity: krilla::num::NormalizedF32::ONE,
    }));
    canvas.surface.set_stroke(None);

    let total_w = grid.step.0 * grid.count.0 as f32;
    let total_h = grid.step.1 * grid.count.1 as f32;
    let Some(rect_path) = build_rect_path(grid.origin.0, grid.origin.1, total_w, total_h) else {
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
#[allow(clippy::too_many_arguments)]
fn ellipse_corner_scale(
    cx: f32,
    cy: f32,
    ox: f32,
    oy: f32,
    ow: f32,
    oh: f32,
    rx0: f32,
    ry0: f32,
    farthest: bool,
) -> (f32, f32) {
    if rx0 <= 0.0 || ry0 <= 0.0 {
        return (rx0, ry0);
    }
    let corners = [(ox, oy), (ox + ow, oy), (ox, oy + oh), (ox + ow, oy + oh)];
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

/// Resolve `background-size` for a gradient layer.
///
/// Per CSS Images §3.3 / §5.5, gradients have no intrinsic dimensions and no
/// intrinsic aspect ratio. The default concrete object size is the positioning
/// area, so `auto` / `cover` / `contain` all return `(origin_w, origin_h)`.
/// `Explicit` with one axis `None` falls back to the corresponding origin axis
/// (still no aspect to derive from).
fn resolve_gradient_size(size: &BgSize, origin_w: f32, origin_h: f32) -> (f32, f32) {
    match size {
        BgSize::Auto | BgSize::Cover | BgSize::Contain => (origin_w, origin_h),
        BgSize::Explicit(w_opt, h_opt) => {
            let rw = w_opt
                .as_ref()
                .map(|v| resolve_lp(v, origin_w))
                .unwrap_or(origin_w);
            let rh = h_opt
                .as_ref()
                .map(|v| resolve_lp(v, origin_h))
                .unwrap_or(origin_h);
            (rw, rh)
        }
    }
}

/// Resolve `background-size` for a layer relative to the origin area.
fn resolve_size(layer: &BackgroundLayer, origin_w: f32, origin_h: f32) -> (f32, f32) {
    let iw = layer.intrinsic_width;
    let ih = layer.intrinsic_height;
    if iw <= 0.0 || ih <= 0.0 {
        return (0.0, 0.0);
    }
    let aspect = iw / ih;
    match &layer.size {
        BgSize::Auto => (iw, ih),
        BgSize::Cover => {
            let scale = (origin_w / iw).max(origin_h / ih);
            (iw * scale, ih * scale)
        }
        BgSize::Contain => {
            let scale = (origin_w / iw).min(origin_h / ih);
            (iw * scale, ih * scale)
        }
        BgSize::Explicit(w_opt, h_opt) => {
            let rw = w_opt.as_ref().map(|v| resolve_lp(v, origin_w));
            let rh = h_opt.as_ref().map(|v| resolve_lp(v, origin_h));
            match (rw, rh) {
                (Some(rw), Some(rh)) => (rw, rh),
                (Some(rw), None) => (rw, rw / aspect),
                (None, Some(rh)) => (rh * aspect, rh),
                (None, None) => (iw, ih),
            }
        }
    }
}

fn resolve_lp(lp: &BgLengthPercentage, basis: f32) -> f32 {
    match lp {
        BgLengthPercentage::Length(v) => *v,
        BgLengthPercentage::Percentage(p) => basis * p,
    }
}

/// CSS spec: position = (container - image) * percentage, or just length.
fn resolve_position(lp: &BgLengthPercentage, container: f32, image: f32) -> f32 {
    match lp {
        BgLengthPercentage::Length(v) => *v,
        BgLengthPercentage::Percentage(p) => (container - image) * p,
    }
}

fn compute_origin_rect(
    style: &BlockStyle,
    origin: &BgBox,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> (f32, f32, f32, f32) {
    let bw = &style.border_widths;
    let pad = &style.padding;
    match origin {
        BgBox::BorderBox => (x, y, w, h),
        BgBox::PaddingBox => (x + bw[3], y + bw[0], w - bw[1] - bw[3], h - bw[0] - bw[2]),
        BgBox::ContentBox => (
            x + bw[3] + pad[3],
            y + bw[0] + pad[0],
            w - bw[1] - bw[3] - pad[1] - pad[3],
            h - bw[0] - bw[2] - pad[0] - pad[2],
        ),
    }
}

fn compute_clip_rect(
    style: &BlockStyle,
    clip: &BgClip,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> (f32, f32, f32, f32) {
    let bw = &style.border_widths;
    let pad = &style.padding;
    match clip {
        BgClip::BorderBox => (x, y, w, h),
        // Text clip: Stylo doesn't expose background-clip: text; falls back to padding-box.
        // See issue fulgur-5gb for future implementation.
        BgClip::PaddingBox | BgClip::Text => {
            (x + bw[3], y + bw[0], w - bw[1] - bw[3], h - bw[0] - bw[2])
        }
        BgClip::ContentBox => (
            x + bw[3] + pad[3],
            y + bw[0] + pad[0],
            w - bw[1] - bw[3] - pad[1] - pad[3],
            h - bw[0] - bw[2] - pad[0] - pad[2],
        ),
    }
}

/// Compute inner border-radii for an inset clip rectangle.
///
/// Per CSS Backgrounds §5.3, inner radii are `max(outer_radius - inset, 0)` where
/// the inset depends on the background-clip box.
fn compute_inner_radii(outer: &[[f32; 2]; 4], style: &BlockStyle, clip: &BgClip) -> [[f32; 2]; 4] {
    let bw = &style.border_widths;
    let pad = &style.padding;
    // Insets: (top, right, bottom, left)
    let (top, right, bottom, left) = match clip {
        BgClip::BorderBox => (0.0, 0.0, 0.0, 0.0),
        BgClip::PaddingBox | BgClip::Text => (bw[0], bw[1], bw[2], bw[3]),
        BgClip::ContentBox => (
            bw[0] + pad[0],
            bw[1] + pad[1],
            bw[2] + pad[2],
            bw[3] + pad[3],
        ),
    };
    // Each corner is adjacent to two edges:
    // top-left: (top, left), top-right: (top, right),
    // bottom-right: (bottom, right), bottom-left: (bottom, left)
    [
        [
            f32::max(outer[0][0] - left, 0.0),
            f32::max(outer[0][1] - top, 0.0),
        ],
        [
            f32::max(outer[1][0] - right, 0.0),
            f32::max(outer[1][1] - top, 0.0),
        ],
        [
            f32::max(outer[2][0] - right, 0.0),
            f32::max(outer[2][1] - bottom, 0.0),
        ],
        [
            f32::max(outer[3][0] - left, 0.0),
            f32::max(outer[3][1] - bottom, 0.0),
        ],
    ]
}

#[allow(clippy::too_many_arguments)]
fn compute_tile_positions(
    repeat_x: BgRepeat,
    repeat_y: BgRepeat,
    pos_x: f32,
    pos_y: f32,
    img_w: f32,
    img_h: f32,
    clip_x: f32,
    clip_y: f32,
    clip_w: f32,
    clip_h: f32,
) -> Vec<(f32, f32, f32, f32)> {
    // NoRepeat × NoRepeat short-circuit: the slow path's NoRepeat branch
    // unconditionally emits exactly one tile at (pos, pos, img, img),
    // regardless of clip overlap. Skip the resolve_repeat_axis indirection
    // entirely — pure simplification, no correctness change.
    if repeat_x == BgRepeat::NoRepeat && repeat_y == BgRepeat::NoRepeat {
        if img_w <= 0.0 || img_h <= 0.0 {
            return Vec::new();
        }
        return vec![(pos_x, pos_y, img_w, img_h)];
    }

    // Degenerate fast-path: a single image already fully covers the clip rect
    // from its position. Without this, the boundary tile loop in `repeat`
    // mode emits up to 4 tiles for the common "image fills box" case (e.g.
    // default repeat with `image == clip` exactly) where 3 are entirely
    // outside the clip and add nothing visible but bloat the PDF stream and
    // can perturb sub-pixel rasterization. Excluded for `round`, which
    // deliberately resizes tiles to fit an integer count and must not
    // collapse to a single image-sized tile.
    //
    // Epsilon choice: `1e-3` here, vs. the slow-path loop's `+ 0.01` (1e-2).
    // The two epsilons answer different questions: the slow-path's `+ 0.01`
    // is a loop-overshoot tolerance asking "should we emit one more tile at
    // the boundary?", while the fast-path's `1e-3` is a coverage tolerance
    // asking "does the image cover the clip within float precision?".
    // Using a tighter epsilon here keeps the fast-path conservative — if the
    // image only marginally covers the clip (e.g., 5e-3 short on the right),
    // the fast-path declines and the slow-path's larger epsilon emits a
    // second tile to fill the residual gap. This asymmetry is intentional.
    //
    // Parity with the slow path is enforced by the
    // `tile_positions_fast_slow_parity_*` tests below, which call
    // `compute_tile_positions_slow` directly to compare and assert that any
    // extra slow-path tiles lie entirely outside the clip rect.
    // Per-axis cover predicate: Repeat axes require *strict* containment
    // because any uncovered sliver on the cover side is filled by the
    // adjacent repeated tile in the slow path. Without strict, the
    // fast path silently drops that sliver (e.g., pos=0.0005, img=99.9995,
    // clip=(0,100): the [0, 0.0005) strip is covered by the slow path's
    // boundary-overlap tile but not by a single fast-path tile at pos).
    // NoRepeat / Space axes have no adjacent tile to fall back on, so the
    // 1e-3 coverage tolerance is safe — it only collapses already-covered
    // cases.
    let covers_x = match repeat_x {
        BgRepeat::Repeat => pos_x <= clip_x && pos_x + img_w >= clip_x + clip_w,
        _ => pos_x <= clip_x + 1e-3 && pos_x + img_w + 1e-3 >= clip_x + clip_w,
    };
    let covers_y = match repeat_y {
        BgRepeat::Repeat => pos_y <= clip_y && pos_y + img_h >= clip_y + clip_h,
        _ => pos_y <= clip_y + 1e-3 && pos_y + img_h + 1e-3 >= clip_y + clip_h,
    };
    if repeat_x != BgRepeat::Round
        && repeat_y != BgRepeat::Round
        && img_w > 0.0
        && img_h > 0.0
        && covers_x
        && covers_y
    {
        return vec![(pos_x, pos_y, img_w, img_h)];
    }

    compute_tile_positions_slow(
        repeat_x, repeat_y, pos_x, pos_y, img_w, img_h, clip_x, clip_y, clip_w, clip_h,
    )
}

/// Slow path: emit tiles via the `resolve_repeat_axis`-driven loop.
/// Extracted from `compute_tile_positions` so tests can compare fast-path
/// output against this path for the same input.
#[allow(clippy::too_many_arguments)]
fn compute_tile_positions_slow(
    repeat_x: BgRepeat,
    repeat_y: BgRepeat,
    pos_x: f32,
    pos_y: f32,
    img_w: f32,
    img_h: f32,
    clip_x: f32,
    clip_y: f32,
    clip_w: f32,
    clip_h: f32,
) -> Vec<(f32, f32, f32, f32)> {
    let mut tiles = Vec::new();
    let (tile_w, space_x, start_x, end_x) =
        resolve_repeat_axis(repeat_x, pos_x, img_w, clip_x, clip_w);
    let (tile_h, space_y, start_y, end_y) =
        resolve_repeat_axis(repeat_y, pos_y, img_h, clip_y, clip_h);

    if tile_w <= 0.0 || tile_h <= 0.0 {
        return tiles;
    }

    // Cap tile count to prevent memory/CPU explosion with tiny images on large containers.
    const MAX_TILES: usize = 10_000;

    let mut ty = start_y;
    while ty < end_y + 0.01 {
        let mut tx = start_x;
        while tx < end_x + 0.01 {
            tiles.push((tx, ty, tile_w, tile_h));
            if tiles.len() >= MAX_TILES {
                return tiles;
            }
            tx += tile_w + space_x;
            if repeat_x == BgRepeat::NoRepeat {
                break;
            }
        }
        ty += tile_h + space_y;
        if repeat_y == BgRepeat::NoRepeat {
            break;
        }
    }
    tiles
}

/// 一様タイルグリッドのジオメトリ。
#[derive(Debug, Clone, Copy, PartialEq)]
struct UniformGrid {
    /// グリッドの最小座標 — ソート後の `(xs[0], ys[0])` であり、
    /// `tiles[0].0..1` ではない (入力順序に依存しない)。
    origin: (f32, f32),
    /// セル (タイル) のサイズ。
    cell: (f32, f32),
    /// グリッドのステップ (cell + 任意の gap)。`cell == step` で repeat / round、
    /// `step > cell` で space (タイル間 gap あり)。
    step: (f32, f32),
    /// X 方向のタイル数, Y 方向のタイル数。`count.0 * count.1 == tiles.len()`。
    count: (usize, usize),
}

/// 全タイルが (cell サイズ一致 + ステップ一様 + count.x×count.y == tiles.len()) を
/// 満たすなら `UniformGrid` を返す。Pattern dedup 経路の適用判定に使う。
/// `tiles.len() < 2` のときは Pattern 構築コストが無駄なので `None`。
fn try_uniform_grid(tiles: &[(f32, f32, f32, f32)]) -> Option<UniformGrid> {
    if tiles.len() < 2 {
        return None;
    }
    // eps は PDF user-space pt 座標における sub-pixel jitter 許容値。
    let eps = 1e-3_f32;

    // セルサイズ一致チェック
    let (tw0, th0) = (tiles[0].2, tiles[0].3);
    if !tiles
        .iter()
        .all(|t| (t.2 - tw0).abs() < eps && (t.3 - th0).abs() < eps)
    {
        return None;
    }

    // ユニークな x / y 座標を昇順で収集 (epsilon マージ)
    let mut xs: Vec<f32> = Vec::new();
    for t in tiles {
        if !xs.iter().any(|&x| (x - t.0).abs() < eps) {
            xs.push(t.0);
        }
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut ys: Vec<f32> = Vec::new();
    for t in tiles {
        if !ys.iter().any(|&y| (y - t.1).abs() < eps) {
            ys.push(t.1);
        }
    }
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // count.x × count.y がタイル数と一致 (=完全グリッド) であること
    if xs.len() * ys.len() != tiles.len() {
        return None;
    }

    // X / Y 方向のステップ一様性をチェック
    let step_x = if xs.len() >= 2 {
        let s = xs[1] - xs[0];
        for w in xs.windows(2) {
            if (w[1] - w[0] - s).abs() > eps {
                return None;
            }
        }
        s
    } else {
        // 単行 (count.x == 1): ステップは未定義だが、cell サイズで埋める
        tw0
    };
    let step_y = if ys.len() >= 2 {
        let s = ys[1] - ys[0];
        for w in ys.windows(2) {
            if (w[1] - w[0] - s).abs() > eps {
                return None;
            }
        }
        s
    } else {
        th0
    };

    Some(UniformGrid {
        origin: (xs[0], ys[0]),
        cell: (tw0, th0),
        step: (step_x, step_y),
        count: (xs.len(), ys.len()),
    })
}

fn resolve_repeat_axis(
    repeat: BgRepeat,
    position: f32,
    image_size: f32,
    clip_start: f32,
    clip_size: f32,
) -> (f32, f32, f32, f32) {
    let clip_end = clip_start + clip_size;
    match repeat {
        BgRepeat::NoRepeat => (image_size, 0.0, position, position),
        BgRepeat::Repeat => {
            if image_size <= 0.0 {
                return (image_size, 0.0, position, position);
            }
            let offset = ((clip_start - position) % image_size + image_size) % image_size;
            let start = clip_start - offset;
            (image_size, 0.0, start, clip_end)
        }
        BgRepeat::Space => {
            if image_size <= 0.0 || image_size > clip_size {
                return (image_size, 0.0, position, position);
            }
            let count = (clip_size / image_size).floor() as usize;
            if count <= 1 {
                return (image_size, 0.0, position, position);
            }
            let spacing = (clip_size - count as f32 * image_size) / (count - 1) as f32;
            (image_size, spacing, clip_start, clip_end)
        }
        BgRepeat::Round => {
            if image_size <= 0.0 {
                return (image_size, 0.0, position, position);
            }
            let count = (clip_size / image_size).round().max(1.0);
            let adjusted = clip_size / count;
            (adjusted, 0.0, clip_start, clip_end)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ImageFormat;

    fn make_layer(iw: f32, ih: f32, size: BgSize) -> BackgroundLayer {
        BackgroundLayer {
            content: BgImageContent::Raster {
                data: Arc::new(vec![]),
                format: ImageFormat::Png,
            },
            intrinsic_width: iw,
            intrinsic_height: ih,
            size,
            position_x: BgLengthPercentage::Percentage(0.0),
            position_y: BgLengthPercentage::Percentage(0.0),
            repeat_x: BgRepeat::NoRepeat,
            repeat_y: BgRepeat::NoRepeat,
            origin: BgBox::PaddingBox,
            clip: BgClip::BorderBox,
        }
    }

    #[test]
    fn test_size_auto() {
        let layer = make_layer(100.0, 50.0, BgSize::Auto);
        let (w, h) = resolve_size(&layer, 200.0, 200.0);
        assert_eq!(w, 100.0);
        assert_eq!(h, 50.0);
    }

    #[test]
    fn test_size_cover() {
        let layer = make_layer(100.0, 50.0, BgSize::Cover);
        let (w, h) = resolve_size(&layer, 200.0, 200.0);
        assert_eq!(w, 400.0);
        assert_eq!(h, 200.0);
    }

    #[test]
    fn test_size_contain() {
        let layer = make_layer(100.0, 50.0, BgSize::Contain);
        let (w, h) = resolve_size(&layer, 200.0, 200.0);
        assert_eq!(w, 200.0);
        assert_eq!(h, 100.0);
    }

    #[test]
    fn test_position_percentage() {
        let offset = resolve_position(&BgLengthPercentage::Percentage(0.5), 200.0, 100.0);
        assert_eq!(offset, 50.0);
    }

    #[test]
    fn test_position_length() {
        let offset = resolve_position(&BgLengthPercentage::Length(30.0), 200.0, 100.0);
        assert_eq!(offset, 30.0);
    }

    #[test]
    fn test_repeat_space() {
        let (size, space, start, _end) =
            resolve_repeat_axis(BgRepeat::Space, 0.0, 90.0, 0.0, 300.0);
        assert_eq!(size, 90.0);
        assert_eq!(space, 15.0);
        assert_eq!(start, 0.0);
    }

    #[test]
    fn test_repeat_round() {
        let (size, space, start, _end) =
            resolve_repeat_axis(BgRepeat::Round, 0.0, 110.0, 0.0, 300.0);
        assert_eq!(size, 100.0);
        assert_eq!(space, 0.0);
        assert_eq!(start, 0.0);
    }

    #[test]
    fn test_svg_layer_resolve_size_contain() {
        let svg_data = br#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="blue"/></svg>"#;
        let opts = usvg::Options::default();
        let tree = usvg::Tree::from_data(svg_data, &opts).unwrap();
        let layer = BackgroundLayer {
            content: BgImageContent::Svg {
                tree: std::sync::Arc::new(tree),
            },
            intrinsic_width: 200.0,
            intrinsic_height: 100.0,
            size: BgSize::Contain,
            position_x: BgLengthPercentage::Percentage(0.0),
            position_y: BgLengthPercentage::Percentage(0.0),
            repeat_x: BgRepeat::NoRepeat,
            repeat_y: BgRepeat::NoRepeat,
            origin: BgBox::PaddingBox,
            clip: BgClip::BorderBox,
        };
        let (w, h) = resolve_size(&layer, 300.0, 300.0);
        assert_eq!(w, 300.0);
        assert_eq!(h, 150.0);
    }

    #[test]
    fn test_repeat_alignment_with_offset_position() {
        // Tiles must align with position: tiles at position ± n*image_size.
        // position=25, clip_start=10, image_size=20 → tiles at 5, 25, 45, ...
        let (size, _space, start, _end) =
            resolve_repeat_axis(BgRepeat::Repeat, 25.0, 20.0, 10.0, 100.0);
        assert_eq!(size, 20.0);
        assert_eq!(start, 5.0);
    }

    #[test]
    fn expand_radii_positive_spread_increases_each_corner() {
        let outer = [[10.0, 10.0]; 4];
        let got = expand_radii(&outer, 5.0);
        for corner in &got {
            assert_eq!(corner[0], 15.0);
            assert_eq!(corner[1], 15.0);
        }
    }

    #[test]
    fn expand_radii_negative_spread_clamps_to_zero() {
        let outer = [[2.0, 2.0]; 4];
        let got = expand_radii(&outer, -5.0);
        for corner in &got {
            assert_eq!(corner[0], 0.0);
            assert_eq!(corner[1], 0.0);
        }
    }

    /// Sharp corners (r == 0) must stay sharp even when spread is positive,
    /// per CSS Backgrounds and Borders Level 3 §7.2.
    #[test]
    fn expand_radii_zero_radii_unchanged() {
        let outer = [[0.0, 0.0]; 4];
        let got = expand_radii(&outer, 5.0);
        for corner in &got {
            assert_eq!(corner[0], 0.0);
            assert_eq!(corner[1], 0.0);
        }
    }

    // Helper: BlockStyle with given border widths and padding, all else default.
    fn make_style(border_widths: [f32; 4], padding: [f32; 4]) -> BlockStyle {
        BlockStyle {
            border_widths,
            padding,
            ..BlockStyle::default()
        }
    }

    // ─── resolve_lp ──────────────────────────────────────────────────────────

    #[test]
    fn resolve_lp_length_returns_value() {
        assert_eq!(resolve_lp(&BgLengthPercentage::Length(42.0), 200.0), 42.0);
    }

    #[test]
    fn resolve_lp_percentage_multiplies_basis() {
        assert_eq!(
            resolve_lp(&BgLengthPercentage::Percentage(0.25), 200.0),
            50.0
        );
    }

    // ─── resolve_size Explicit variants ──────────────────────────────────────

    #[test]
    fn resolve_size_explicit_both_axes() {
        let layer = make_layer(
            100.0,
            50.0,
            BgSize::Explicit(
                Some(BgLengthPercentage::Length(80.0)),
                Some(BgLengthPercentage::Length(40.0)),
            ),
        );
        let (w, h) = resolve_size(&layer, 200.0, 200.0);
        assert_eq!(w, 80.0);
        assert_eq!(h, 40.0);
    }

    #[test]
    fn resolve_size_explicit_width_only_derives_height_from_aspect() {
        // iw=100, ih=50, aspect=2; explicit width=80 → height=80/2=40
        let layer = make_layer(
            100.0,
            50.0,
            BgSize::Explicit(Some(BgLengthPercentage::Length(80.0)), None),
        );
        let (w, h) = resolve_size(&layer, 200.0, 200.0);
        assert_eq!(w, 80.0);
        assert_eq!(h, 40.0);
    }

    #[test]
    fn resolve_size_explicit_height_only_derives_width_from_aspect() {
        // iw=100, ih=50, aspect=2; explicit height=40 → width=40*2=80
        let layer = make_layer(
            100.0,
            50.0,
            BgSize::Explicit(None, Some(BgLengthPercentage::Length(40.0))),
        );
        let (w, h) = resolve_size(&layer, 200.0, 200.0);
        assert_eq!(w, 80.0);
        assert_eq!(h, 40.0);
    }

    #[test]
    fn resolve_size_explicit_neither_falls_back_to_intrinsic() {
        let layer = make_layer(100.0, 50.0, BgSize::Explicit(None, None));
        let (w, h) = resolve_size(&layer, 200.0, 200.0);
        assert_eq!(w, 100.0);
        assert_eq!(h, 50.0);
    }

    #[test]
    fn resolve_size_zero_intrinsic_returns_zero() {
        let layer = make_layer(0.0, 50.0, BgSize::Auto);
        let (w, h) = resolve_size(&layer, 200.0, 200.0);
        assert_eq!(w, 0.0);
        assert_eq!(h, 0.0);
    }

    // ─── resolve_gradient_size (no intrinsic dimensions) ─────────────────────

    #[test]
    fn resolve_gradient_size_auto_returns_origin() {
        let (w, h) = resolve_gradient_size(&BgSize::Auto, 200.0, 100.0);
        assert!((w - 200.0).abs() < 1e-6);
        assert!((h - 100.0).abs() < 1e-6);
    }

    #[test]
    fn resolve_gradient_size_cover_returns_origin() {
        let (w, h) = resolve_gradient_size(&BgSize::Cover, 200.0, 100.0);
        assert!((w - 200.0).abs() < 1e-6);
        assert!((h - 100.0).abs() < 1e-6);
    }

    #[test]
    fn resolve_gradient_size_contain_returns_origin() {
        let (w, h) = resolve_gradient_size(&BgSize::Contain, 200.0, 100.0);
        assert!((w - 200.0).abs() < 1e-6);
        assert!((h - 100.0).abs() < 1e-6);
    }

    #[test]
    fn resolve_gradient_size_explicit_both_resolves() {
        let size = BgSize::Explicit(
            Some(BgLengthPercentage::Length(50.0)),
            Some(BgLengthPercentage::Percentage(0.25)),
        );
        let (w, h) = resolve_gradient_size(&size, 200.0, 100.0);
        assert!((w - 50.0).abs() < 1e-6);
        assert!((h - 25.0).abs() < 1e-6);
    }

    #[test]
    fn resolve_gradient_size_explicit_asymmetric_percentages() {
        // Each axis resolves against its own origin dimension independently,
        // so `(50%, 25%)` on a 200×100 origin yields (100, 25), not a
        // uniform scale. Locks the percentage basis.
        let size = BgSize::Explicit(
            Some(BgLengthPercentage::Percentage(0.5)),
            Some(BgLengthPercentage::Percentage(0.25)),
        );
        let (w, h) = resolve_gradient_size(&size, 200.0, 100.0);
        assert!((w - 100.0).abs() < 1e-6);
        assert!((h - 25.0).abs() < 1e-6);
    }

    #[test]
    fn resolve_gradient_size_explicit_one_auto_uses_origin() {
        // width specified, height auto → height fills origin (no aspect)
        let size = BgSize::Explicit(Some(BgLengthPercentage::Length(80.0)), None);
        let (w, h) = resolve_gradient_size(&size, 200.0, 100.0);
        assert!((w - 80.0).abs() < 1e-6);
        assert!((h - 100.0).abs() < 1e-6);

        // height specified, width auto → width fills origin
        let size = BgSize::Explicit(None, Some(BgLengthPercentage::Percentage(0.5)));
        let (w, h) = resolve_gradient_size(&size, 200.0, 100.0);
        assert!((w - 200.0).abs() < 1e-6);
        assert!((h - 50.0).abs() < 1e-6);
    }

    // ─── compute_origin_rect ─────────────────────────────────────────────────

    // Layout used below: x=10, y=20, w=100, h=200
    // border_widths: top=5, right=10, bottom=15, left=20
    // padding:       top=2, right=4,  bottom=6,  left=8

    #[test]
    fn origin_rect_border_box_is_identity() {
        let style = make_style([5.0, 10.0, 15.0, 20.0], [2.0, 4.0, 6.0, 8.0]);
        let (ox, oy, ow, oh) =
            compute_origin_rect(&style, &BgBox::BorderBox, 10.0, 20.0, 100.0, 200.0);
        assert_eq!((ox, oy, ow, oh), (10.0, 20.0, 100.0, 200.0));
    }

    #[test]
    fn origin_rect_padding_box_insets_by_border() {
        let style = make_style([5.0, 10.0, 15.0, 20.0], [2.0, 4.0, 6.0, 8.0]);
        // x + left_border, y + top_border, w - right_border - left_border, h - top_border - bottom_border
        // = 10+20=30, 20+5=25, 100-10-20=70, 200-5-15=180
        let (ox, oy, ow, oh) =
            compute_origin_rect(&style, &BgBox::PaddingBox, 10.0, 20.0, 100.0, 200.0);
        assert_eq!((ox, oy, ow, oh), (30.0, 25.0, 70.0, 180.0));
    }

    #[test]
    fn origin_rect_content_box_insets_by_border_and_padding() {
        let style = make_style([5.0, 10.0, 15.0, 20.0], [2.0, 4.0, 6.0, 8.0]);
        // x + left_border + left_pad = 10+20+8=38
        // y + top_border + top_pad   = 20+5+2=27
        // w - right_border - left_border - right_pad - left_pad = 100-10-20-4-8=58
        // h - top_border - bottom_border - top_pad - bottom_pad = 200-5-15-2-6=172
        let (ox, oy, ow, oh) =
            compute_origin_rect(&style, &BgBox::ContentBox, 10.0, 20.0, 100.0, 200.0);
        assert_eq!((ox, oy, ow, oh), (38.0, 27.0, 58.0, 172.0));
    }

    // ─── compute_clip_rect ───────────────────────────────────────────────────

    #[test]
    fn clip_rect_border_box_is_identity() {
        let style = make_style([5.0, 10.0, 15.0, 20.0], [2.0, 4.0, 6.0, 8.0]);
        let (cx, cy, cw, ch) =
            compute_clip_rect(&style, &BgClip::BorderBox, 10.0, 20.0, 100.0, 200.0);
        assert_eq!((cx, cy, cw, ch), (10.0, 20.0, 100.0, 200.0));
    }

    #[test]
    fn clip_rect_padding_box_insets_by_border() {
        let style = make_style([5.0, 10.0, 15.0, 20.0], [2.0, 4.0, 6.0, 8.0]);
        let (cx, cy, cw, ch) =
            compute_clip_rect(&style, &BgClip::PaddingBox, 10.0, 20.0, 100.0, 200.0);
        assert_eq!((cx, cy, cw, ch), (30.0, 25.0, 70.0, 180.0));
    }

    #[test]
    fn clip_rect_text_equals_padding_box() {
        let style = make_style([5.0, 10.0, 15.0, 20.0], [2.0, 4.0, 6.0, 8.0]);
        let padding_box = compute_clip_rect(&style, &BgClip::PaddingBox, 10.0, 20.0, 100.0, 200.0);
        let text_clip = compute_clip_rect(&style, &BgClip::Text, 10.0, 20.0, 100.0, 200.0);
        assert_eq!(padding_box, text_clip);
    }

    #[test]
    fn clip_rect_content_box_insets_by_border_and_padding() {
        let style = make_style([5.0, 10.0, 15.0, 20.0], [2.0, 4.0, 6.0, 8.0]);
        let (cx, cy, cw, ch) =
            compute_clip_rect(&style, &BgClip::ContentBox, 10.0, 20.0, 100.0, 200.0);
        assert_eq!((cx, cy, cw, ch), (38.0, 27.0, 58.0, 172.0));
    }

    // ─── compute_inner_radii ─────────────────────────────────────────────────

    // outer corners all = 10pt; bw=(top=5,right=10,bottom=15,left=20); pad=(top=2,right=4,bottom=6,left=8)

    #[test]
    fn inner_radii_border_box_clip_unchanged() {
        let style = make_style([5.0, 10.0, 15.0, 20.0], [2.0, 4.0, 6.0, 8.0]);
        let outer = [[10.0f32; 2]; 4];
        let got = compute_inner_radii(&outer, &style, &BgClip::BorderBox);
        assert_eq!(got, [[10.0; 2]; 4]);
    }

    #[test]
    fn inner_radii_padding_box_clip_shrinks_by_border() {
        // insets: top=5, right=10, bottom=15, left=20
        // corner 0 (top-left):   [max(10-20,0), max(10-5,0)] = [0, 5]
        // corner 1 (top-right):  [max(10-10,0), max(10-5,0)] = [0, 5]
        // corner 2 (bot-right):  [max(10-10,0), max(10-15,0)] = [0, 0]
        // corner 3 (bot-left):   [max(10-20,0), max(10-15,0)] = [0, 0]
        let style = make_style([5.0, 10.0, 15.0, 20.0], [2.0, 4.0, 6.0, 8.0]);
        let outer = [[10.0f32; 2]; 4];
        let got = compute_inner_radii(&outer, &style, &BgClip::PaddingBox);
        assert_eq!(got[0], [0.0, 5.0]);
        assert_eq!(got[1], [0.0, 5.0]);
        assert_eq!(got[2], [0.0, 0.0]);
        assert_eq!(got[3], [0.0, 0.0]);
    }

    #[test]
    fn inner_radii_content_box_clip_shrinks_by_border_and_padding() {
        // insets: top=5+2=7, right=10+4=14, bottom=15+6=21, left=20+8=28
        // corner 0 (top-left):   [max(10-28,0), max(10-7,0)] = [0, 3]
        // corner 1 (top-right):  [max(10-14,0), max(10-7,0)] = [0, 3]
        // corner 2 (bot-right):  [max(10-14,0), max(10-21,0)] = [0, 0]
        // corner 3 (bot-left):   [max(10-28,0), max(10-21,0)] = [0, 0]
        let style = make_style([5.0, 10.0, 15.0, 20.0], [2.0, 4.0, 6.0, 8.0]);
        let outer = [[10.0f32; 2]; 4];
        let got = compute_inner_radii(&outer, &style, &BgClip::ContentBox);
        assert_eq!(got[0], [0.0, 3.0]);
        assert_eq!(got[1], [0.0, 3.0]);
        assert_eq!(got[2], [0.0, 0.0]);
        assert_eq!(got[3], [0.0, 0.0]);
    }

    // ─── compute_tile_positions ───────────────────────────────────────────────

    #[test]
    fn tile_positions_no_repeat_yields_single_tile() {
        let tiles = compute_tile_positions(
            BgRepeat::NoRepeat,
            BgRepeat::NoRepeat,
            50.0,
            30.0, // pos_x, pos_y
            80.0,
            60.0, // img_w, img_h
            0.0,
            0.0,
            400.0,
            300.0, // clip
        );
        assert_eq!(tiles, vec![(50.0, 30.0, 80.0, 60.0)]);
    }

    #[test]
    fn tile_positions_image_equals_clip_repeat_collapses_to_one_tile() {
        // image == clip exactly with default repeat: the boundary epsilon
        // would otherwise emit 4 tiles (3 fully outside clip). Fast-path
        // collapses to a single tile.
        let tiles = compute_tile_positions(
            BgRepeat::Repeat,
            BgRepeat::Repeat,
            0.0,
            0.0,
            100.0,
            100.0, // image == clip
            0.0,
            0.0,
            100.0,
            100.0,
        );
        assert_eq!(tiles, vec![(0.0, 0.0, 100.0, 100.0)]);
    }

    #[test]
    fn tile_positions_image_larger_than_clip_repeat_collapses_to_one_tile() {
        // image strictly larger than clip with repeat: still a single tile
        // since the image already covers the clip from its position.
        let tiles = compute_tile_positions(
            BgRepeat::Repeat,
            BgRepeat::Repeat,
            -10.0,
            -5.0,
            150.0,
            120.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        assert_eq!(tiles, vec![(-10.0, -5.0, 150.0, 120.0)]);
    }

    #[test]
    fn tile_positions_fast_slow_parity_repeat_image_equals_clip() {
        // Direct fast-path vs slow-path comparison: same input, the fast
        // path must produce the same tile set as the slow path would.
        let fast = compute_tile_positions(
            BgRepeat::Repeat,
            BgRepeat::Repeat,
            0.0,
            0.0,
            100.0,
            100.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        let slow = compute_tile_positions_slow(
            BgRepeat::Repeat,
            BgRepeat::Repeat,
            0.0,
            0.0,
            100.0,
            100.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        // The slow path emits 4 tiles (the +0.01 boundary epsilon); the
        // fast path emits 1. They cover the same visible area: tile[0] of
        // slow == fast[0], and the other 3 slow tiles lie entirely outside
        // the clip rect (right, below, and bottom-right of clip end).
        assert_eq!(fast.len(), 1);
        assert_eq!(slow[0], fast[0]);
        for &(tx, ty, _, _) in slow.iter().skip(1) {
            assert!(
                tx >= 100.0 - 1e-3 || ty >= 100.0 - 1e-3,
                "slow-path extra tile ({tx}, {ty}) should be outside clip rect"
            );
        }
    }

    #[test]
    fn tile_positions_fast_slow_parity_space_image_equals_clip() {
        // For Space×Space with image == clip, the slow path's count <= 1
        // branch produces a single tile at position. Fast path matches.
        let fast = compute_tile_positions(
            BgRepeat::Space,
            BgRepeat::Space,
            0.0,
            0.0,
            100.0,
            100.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        let slow = compute_tile_positions_slow(
            BgRepeat::Space,
            BgRepeat::Space,
            0.0,
            0.0,
            100.0,
            100.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        assert_eq!(fast, slow, "fast and slow paths must agree for Space");
        assert_eq!(fast.len(), 1);
    }

    #[test]
    fn tile_positions_fast_slow_parity_negative_position_repeat() {
        // Reviewer's concern: for Repeat with negative pos_x where the
        // image covers the clip (pos_x + img_w >= clip_x + clip_w), does
        // the slow path's `start_x` equal `pos_x`? Mathematical analysis:
        // when image covers clip from pos_x, img_w >= clip_w + (clip_x -
        // pos_x), so `clip_x - pos_x < img_w` and the slow-path's
        // `(clip_x - pos_x) % img_w` reduces to `clip_x - pos_x` exactly,
        // making `start_x = pos_x` algebraically.
        //
        // Empirically there is a sub-ulp drift through the modulo when
        // pos_x is not exactly representable in f32 (e.g. -99.999), so we
        // assert agreement within 1e-3 — well below sub-pixel rendering
        // precision. The fast path uses the literal pos_x and is in fact
        // strictly more accurate than the slow path here.
        for &(pos_x, img_w) in &[
            (-50.0_f32, 200.0_f32),
            (-99.999, 250.0),
            (-150.0, 250.0),
            (-1.0, 110.0),
            // Reviewer concern (job 442 Medium): pos_x = -150, img_w = 260
            // claim: slow start_x = 0, fast tile at -150 → mismatch.
            // Actual slow: offset = 150 % 260 = 150, start_x = 0 - 150 = -150.
            // Both fast and slow yield -150. Lock this case explicitly.
            (-150.0, 260.0),
            // Larger absolute pos_x where image still covers clip:
            // pos = -250, img = 360, clip = (0, 100). Slow offset =
            // 250 % 360 = 250, start_x = 0 - 250 = -250 (still equals pos_x
            // because 250 < 360, i.e. clip_x - pos_x < img_w as required by
            // the cover-clip predicate).
            (-250.0, 360.0),
        ] {
            let fast = compute_tile_positions(
                BgRepeat::Repeat,
                BgRepeat::Repeat,
                pos_x,
                pos_x,
                img_w,
                img_w,
                0.0,
                0.0,
                100.0,
                100.0,
            );
            let slow = compute_tile_positions_slow(
                BgRepeat::Repeat,
                BgRepeat::Repeat,
                pos_x,
                pos_x,
                img_w,
                img_w,
                0.0,
                0.0,
                100.0,
                100.0,
            );
            assert_eq!(
                fast.len(),
                1,
                "fast-path must emit 1 tile for pos={pos_x} img={img_w}"
            );
            let (sx, sy, sw, sh) = slow[0];
            let (fx, fy, fw, fh) = fast[0];
            assert!(
                (sx - fx).abs() < 1e-3
                    && (sy - fy).abs() < 1e-3
                    && (sw - fw).abs() < 1e-3
                    && (sh - fh).abs() < 1e-3,
                "tile[0] mismatch for pos={pos_x} img={img_w}: \
                 fast={:?} slow={:?}",
                fast[0],
                slow[0],
            );
            // Any extra slow-path tile must lie entirely outside the clip
            // rect (above/below/left/right of the clip box) for the
            // fast-path collapse to be safe.
            for &(tx, ty, tw, th) in slow.iter().skip(1) {
                let outside_left = tx + tw <= 0.0 + 1e-3;
                let outside_right = tx >= 100.0 - 1e-3;
                let outside_top = ty + th <= 0.0 + 1e-3;
                let outside_bottom = ty >= 100.0 - 1e-3;
                assert!(
                    outside_left || outside_right || outside_top || outside_bottom,
                    "slow extra tile ({tx}, {ty}, {tw}, {th}) inside clip for \
                     pos={pos_x} img={img_w}",
                );
            }
        }
    }

    #[test]
    fn tile_positions_fast_slow_parity_space_image_slightly_less_than_clip() {
        // Reviewer concern (job 439 Medium): Space mode might "center" a
        // single tile when image is slightly less than clip. Per
        // resolve_repeat_axis::Space:
        //   - image_size > clip_size? false (image is less)
        //   - count = floor(clip / image) = floor(1.000005) = 1
        //   - count <= 1 → return single tile at *position* (NOT centered)
        // So Space does not center for count=1. Verify fast and slow agree
        // for image just under clip where the fast-path 1e-3 epsilon still
        // triggers.
        let img = 99.9995_f32;
        let clip = 100.0_f32;
        let fast = compute_tile_positions(
            BgRepeat::Space,
            BgRepeat::Space,
            0.0,
            0.0,
            img,
            img,
            0.0,
            0.0,
            clip,
            clip,
        );
        let slow = compute_tile_positions_slow(
            BgRepeat::Space,
            BgRepeat::Space,
            0.0,
            0.0,
            img,
            img,
            0.0,
            0.0,
            clip,
            clip,
        );
        assert_eq!(fast.len(), 1);
        assert_eq!(slow.len(), 1);
        let (sx, sy, _, _) = slow[0];
        let (fx, fy, _, _) = fast[0];
        assert!((sx - fx).abs() < 1e-3 && (sy - fy).abs() < 1e-3);
    }

    #[test]
    fn tile_positions_fast_slow_parity_no_repeat_various_inputs() {
        // The NoRepeat × NoRepeat short-circuit emits a single tile at
        // (pos, pos, img, img). Verify it matches the slow path across
        // positive, negative, and image-larger-than-clip positions, and
        // for image-smaller-than-clip (where the broader fast-path
        // coverage check declines but NoRepeat still single-tiles).
        for &(pos_x, pos_y, img_w, img_h) in &[
            (0.0_f32, 0.0_f32, 100.0_f32, 100.0_f32), // image == clip
            (50.0, 30.0, 80.0, 60.0),                 // image inside clip
            (-10.0, -5.0, 150.0, 120.0),              // image larger, neg pos
            (20.0, 20.0, 200.0, 200.0),               // image extends past clip
        ] {
            let fast = compute_tile_positions(
                BgRepeat::NoRepeat,
                BgRepeat::NoRepeat,
                pos_x,
                pos_y,
                img_w,
                img_h,
                0.0,
                0.0,
                100.0,
                100.0,
            );
            let slow = compute_tile_positions_slow(
                BgRepeat::NoRepeat,
                BgRepeat::NoRepeat,
                pos_x,
                pos_y,
                img_w,
                img_h,
                0.0,
                0.0,
                100.0,
                100.0,
            );
            assert_eq!(
                fast, slow,
                "NoRepeat fast-slow parity broken for pos=({pos_x}, {pos_y}) img=({img_w}, {img_h})"
            );
        }
    }

    #[test]
    fn tile_positions_no_repeat_zero_axis_returns_empty() {
        // Degenerate axis (img_w == 0) under NoRepeat: must emit no tiles
        // (the slow path's resolve_repeat_axis guards image_size <= 0).
        let tiles = compute_tile_positions(
            BgRepeat::NoRepeat,
            BgRepeat::NoRepeat,
            10.0,
            10.0,
            0.0,
            50.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        assert!(tiles.is_empty());
    }

    #[test]
    fn tile_positions_repeat_strict_cover_preserves_sliver() {
        // Regression for coderabbit job 442 Major: with Repeat × Repeat
        // and pos = 0.0005, img = 99.9995, clip = (0, 100), the slow
        // path's boundary-overlap tile covers the [0.0, 0.0005) strip
        // via the offset modulo. The pre-fix fast-path collapsed to a
        // single tile at pos=0.0005 and silently dropped the sliver.
        // After the fix (strict cover for Repeat axes), the fast-path
        // declines and the slow path runs.
        let fast = compute_tile_positions(
            BgRepeat::Repeat,
            BgRepeat::Repeat,
            0.0005,
            0.0005,
            99.9995,
            99.9995,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        // fast-path must NOT collapse to 1 tile here — it should fall
        // through to the slow path which emits multiple boundary tiles
        // covering the sliver.
        assert!(
            fast.len() > 1,
            "Repeat axis with sliver-uncovered strip must not collapse: \
             got {} tiles, expected slow-path multi-tile",
            fast.len()
        );
        // Verify at least one tile starts at or before clip_x = 0 (the
        // boundary tile that covers the sliver).
        assert!(
            fast.iter()
                .any(|&(tx, _, tw, _)| tx <= 0.0 && tx + tw >= 0.0),
            "no boundary tile covers the [0, 0.0005) strip: {fast:?}"
        );
    }

    #[test]
    fn tile_positions_repeat_strict_cover_with_no_sliver() {
        // Sanity: Repeat axes still get the fast-path when image truly
        // covers the clip from pos. (pos=0, img=100, clip=(0,100)) →
        // 1 tile.
        let fast = compute_tile_positions(
            BgRepeat::Repeat,
            BgRepeat::Repeat,
            0.0,
            0.0,
            100.0,
            100.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        assert_eq!(fast.len(), 1);
    }

    #[test]
    fn tile_positions_fast_slow_parity_image_larger_than_clip() {
        // Image strictly larger than clip with negative position: fast path
        // returns single image-sized tile at position. Slow path may emit
        // additional boundary-epsilon tiles for Repeat — those must lie
        // entirely outside the clip rect for the fast-path collapse to be
        // safe. Verify: tile[0] matches AND every extra tile is outside
        // [clip_x, clip_x+clip_w) × [clip_y, clip_y+clip_h).
        let clip_x = 0.0_f32;
        let clip_y = 0.0_f32;
        let clip_w = 100.0_f32;
        let clip_h = 100.0_f32;
        for repeat in [BgRepeat::Repeat, BgRepeat::Space, BgRepeat::NoRepeat] {
            let fast = compute_tile_positions(
                repeat, repeat, -10.0, -5.0, 150.0, 120.0, clip_x, clip_y, clip_w, clip_h,
            );
            let slow = compute_tile_positions_slow(
                repeat, repeat, -10.0, -5.0, 150.0, 120.0, clip_x, clip_y, clip_w, clip_h,
            );
            assert_eq!(fast.len(), 1, "{repeat:?}: fast-path must be 1 tile");
            assert_eq!(slow[0], fast[0], "{repeat:?}: tile[0] must match");
            // Every extra slow-path tile must be fully outside the clip rect
            // (tile_x >= clip end OR tile_y >= clip end OR tile_right <= clip_x
            // OR tile_bottom <= clip_y). Since slow-path NEVER emits tiles to
            // the left/above the start, the relevant checks are tx >= clip_end
            // and ty >= clip_end.
            for &(tx, ty, _, _) in slow.iter().skip(1) {
                assert!(
                    tx >= clip_x + clip_w - 1e-3 || ty >= clip_y + clip_h - 1e-3,
                    "{repeat:?}: slow extra tile ({tx}, {ty}) must be outside clip",
                );
            }
        }
    }

    #[test]
    fn tile_positions_image_equals_clip_round_does_not_fast_path() {
        // Round must not collapse to a single tile even when image == clip:
        // round's contract is to resize tiles to fit an integer count, and
        // the caller may rely on tile-size adjustment for the "round" effect.
        let tiles = compute_tile_positions(
            BgRepeat::Round,
            BgRepeat::Round,
            0.0,
            0.0,
            100.0,
            100.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        // Round with image == clip resolves to tile_size = clip_size / round(clip/image) = 100/1 = 100
        // and emits boundary tiles per the existing loop (≥1). The point of
        // this test is that the fast-path was NOT taken.
        assert!(
            tiles.iter().all(|&(_, _, w, h)| w == 100.0 && h == 100.0),
            "round must not change tile size when image fits exactly"
        );
    }

    #[test]
    fn tile_positions_repeat_both_axes_fills_clip() {
        // 50×50 image, 100×100 clip at origin, position=(0,0)
        // Tiles at x∈{0,50,100}, y∈{0,50,100} = 9 tiles (partial tiles included)
        let tiles = compute_tile_positions(
            BgRepeat::Repeat,
            BgRepeat::Repeat,
            0.0,
            0.0,
            50.0,
            50.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        assert!(
            tiles.len() >= 4,
            "expected at least 4 tiles, got {}",
            tiles.len()
        );
        assert!(tiles.iter().all(|&(_, _, w, h)| w == 50.0 && h == 50.0));
    }

    #[test]
    fn tile_positions_no_repeat_x_repeat_y_yields_single_column() {
        // x: NoRepeat → 1 tile per row; y: Repeat → multiple rows
        let tiles = compute_tile_positions(
            BgRepeat::NoRepeat,
            BgRepeat::Repeat,
            0.0,
            0.0,
            50.0,
            50.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        // All tiles share the same x position
        assert!(tiles.len() >= 2);
        assert!(tiles.iter().all(|&(tx, _, _, _)| tx == 0.0));
    }

    #[test]
    fn tile_positions_max_tiles_cap() {
        // 1×1 image on 200×200 clip → would produce 200*200=40000 tiles without cap
        let tiles = compute_tile_positions(
            BgRepeat::Repeat,
            BgRepeat::Repeat,
            0.0,
            0.0,
            1.0,
            1.0,
            0.0,
            0.0,
            200.0,
            200.0,
        );
        assert_eq!(tiles.len(), 10_000);
    }

    #[test]
    fn tile_positions_zero_img_size_returns_empty() {
        let tiles = compute_tile_positions(
            BgRepeat::Repeat,
            BgRepeat::Repeat,
            0.0,
            0.0,
            0.0,
            50.0,
            0.0,
            0.0,
            100.0,
            100.0,
        );
        assert!(tiles.is_empty());
    }

    // ─── resolve_repeat_axis edge cases ──────────────────────────────────────

    #[test]
    fn resolve_repeat_axis_no_repeat_returns_position_as_start_end() {
        let (size, space, start, end) =
            resolve_repeat_axis(BgRepeat::NoRepeat, 25.0, 50.0, 0.0, 200.0);
        assert_eq!(size, 50.0);
        assert_eq!(space, 0.0);
        assert_eq!(start, 25.0);
        assert_eq!(end, 25.0);
    }

    #[test]
    fn resolve_repeat_axis_space_image_larger_than_clip_no_tiling() {
        // image_size=250 > clip_size=200 → falls back to no-repeat at position
        let (size, space, start, end) =
            resolve_repeat_axis(BgRepeat::Space, 0.0, 250.0, 0.0, 200.0);
        assert_eq!(size, 250.0);
        assert_eq!(space, 0.0);
        assert_eq!(start, 0.0);
        assert_eq!(end, 0.0);
    }

    #[test]
    fn resolve_repeat_axis_space_count_one_no_gap() {
        // image_size=200, clip_size=200 → count=1, falls back to no-repeat
        let (size, space, start, end) =
            resolve_repeat_axis(BgRepeat::Space, 0.0, 200.0, 0.0, 200.0);
        assert_eq!(size, 200.0);
        assert_eq!(space, 0.0);
        assert_eq!(start, 0.0);
        assert_eq!(end, 0.0);
    }

    #[test]
    fn resolve_repeat_axis_repeat_zero_image_size_degenerate() {
        // image_size=0 → returns (0, 0, position, position)
        let (size, space, start, end) = resolve_repeat_axis(BgRepeat::Repeat, 5.0, 0.0, 0.0, 100.0);
        assert_eq!(size, 0.0);
        assert_eq!(space, 0.0);
        assert_eq!(start, 5.0);
        assert_eq!(end, 5.0);
    }

    #[test]
    fn resolve_repeat_axis_round_zero_image_size_degenerate() {
        let (size, space, start, end) = resolve_repeat_axis(BgRepeat::Round, 5.0, 0.0, 0.0, 100.0);
        assert_eq!(size, 0.0);
        assert_eq!(space, 0.0);
        assert_eq!(start, 5.0);
        assert_eq!(end, 5.0);
    }

    #[test]
    fn resolve_repeat_axis_space_zero_image_size_degenerate() {
        let (size, space, start, end) = resolve_repeat_axis(BgRepeat::Space, 5.0, 0.0, 0.0, 100.0);
        assert_eq!(size, 0.0);
        assert_eq!(space, 0.0);
        assert_eq!(start, 5.0);
        assert_eq!(end, 5.0);
    }

    // ─── try_uniform_grid ─────────────────────────────────────────────────────

    #[test]
    fn uniform_grid_single_tile_returns_none() {
        // Single tile (no-repeat) is not worth the Pattern overhead.
        let tiles = vec![(10.0, 20.0, 30.0, 40.0)];
        assert!(try_uniform_grid(&tiles).is_none());
    }

    #[test]
    fn uniform_grid_repeat_round_8x6_detected() {
        // 8 columns × 6 rows, cell = step = (10, 15)
        let mut tiles = Vec::new();
        for j in 0..6 {
            for i in 0..8 {
                tiles.push((i as f32 * 10.0, j as f32 * 15.0, 10.0, 15.0));
            }
        }
        let g = try_uniform_grid(&tiles).expect("detect 8×6 grid");
        assert_eq!(g.count, (8, 6));
        assert!((g.cell.0 - 10.0).abs() < 1e-3);
        assert!((g.cell.1 - 15.0).abs() < 1e-3);
        assert!((g.step.0 - 10.0).abs() < 1e-3);
        assert!((g.step.1 - 15.0).abs() < 1e-3);
        assert!((g.origin.0 - 0.0).abs() < 1e-3);
        assert!((g.origin.1 - 0.0).abs() < 1e-3);
    }

    #[test]
    fn uniform_grid_space_with_gaps_detected() {
        // 3 columns × 2 rows, cell = (10, 10), step = (15, 20)
        let tiles = vec![
            (0.0, 0.0, 10.0, 10.0),
            (15.0, 0.0, 10.0, 10.0),
            (30.0, 0.0, 10.0, 10.0),
            (0.0, 20.0, 10.0, 10.0),
            (15.0, 20.0, 10.0, 10.0),
            (30.0, 20.0, 10.0, 10.0),
        ];
        let g = try_uniform_grid(&tiles).expect("detect 3×2 spaced grid");
        assert_eq!(g.count, (3, 2));
        assert!((g.cell.0 - 10.0).abs() < 1e-3);
        assert!((g.step.0 - 15.0).abs() < 1e-3);
        assert!((g.step.1 - 20.0).abs() < 1e-3);
    }

    #[test]
    fn uniform_grid_mismatched_cell_size_returns_none() {
        // Second tile has different height → not uniform.
        let tiles = vec![(0.0, 0.0, 10.0, 10.0), (10.0, 0.0, 10.0, 12.0)];
        assert!(try_uniform_grid(&tiles).is_none());
    }

    #[test]
    fn uniform_grid_irregular_step_returns_none() {
        // X steps: 10, 11 → not uniform.
        let tiles = vec![
            (0.0, 0.0, 5.0, 5.0),
            (10.0, 0.0, 5.0, 5.0),
            (21.0, 0.0, 5.0, 5.0),
        ];
        assert!(try_uniform_grid(&tiles).is_none());
    }

    #[test]
    fn uniform_grid_single_row_repeat_x() {
        // 4 tiles in a single row → 4×1 grid.
        let tiles = vec![
            (0.0, 5.0, 10.0, 10.0),
            (10.0, 5.0, 10.0, 10.0),
            (20.0, 5.0, 10.0, 10.0),
            (30.0, 5.0, 10.0, 10.0),
        ];
        let g = try_uniform_grid(&tiles).expect("detect 4×1 grid");
        assert_eq!(g.count, (4, 1));
    }

    #[test]
    fn uniform_grid_count_mismatch_returns_none() {
        // 3 tiles claimed but only 2 unique x positions × 2 unique y → 4 expected.
        let tiles = vec![
            (0.0, 0.0, 5.0, 5.0),
            (10.0, 0.0, 5.0, 5.0),
            (0.0, 10.0, 5.0, 5.0),
        ];
        assert!(try_uniform_grid(&tiles).is_none());
    }

    #[test]
    fn uniform_grid_negative_origin_detected() {
        // Negative origin (e.g. background-position pulling tiles into negative coords).
        let tiles = vec![
            (-50.0, -30.0, 10.0, 10.0),
            (-40.0, -30.0, 10.0, 10.0),
            (-50.0, -20.0, 10.0, 10.0),
            (-40.0, -20.0, 10.0, 10.0),
        ];
        let g = try_uniform_grid(&tiles).expect("detect 2×2 grid with negative origin");
        assert_eq!(g.count, (2, 2));
        assert!((g.origin.0 + 50.0).abs() < 1e-3);
        assert!((g.origin.1 + 30.0).abs() < 1e-3);
    }

    #[test]
    fn uniform_grid_input_order_independent() {
        // try_uniform_grid sorts xs/ys internally; shuffled input must give same result.
        let canonical = vec![
            (0.0, 0.0, 5.0, 5.0),
            (5.0, 0.0, 5.0, 5.0),
            (0.0, 5.0, 5.0, 5.0),
            (5.0, 5.0, 5.0, 5.0),
        ];
        let shuffled = vec![
            (5.0, 5.0, 5.0, 5.0),
            (0.0, 0.0, 5.0, 5.0),
            (5.0, 0.0, 5.0, 5.0),
            (0.0, 5.0, 5.0, 5.0),
        ];
        assert_eq!(try_uniform_grid(&canonical), try_uniform_grid(&shuffled));
    }

    // ─── corner_to_angle_rad ──────────────────────────────────────────────────

    #[test]
    fn corner_to_angle_rad_top_right_square_is_pi_over_4() {
        use crate::pageable::LinearGradientCorner;
        // "to top right" on a square → 45° = π/4 clockwise from "to top"
        let angle = corner_to_angle_rad(LinearGradientCorner::TopRight, 100.0, 100.0);
        assert!(
            (angle - std::f32::consts::FRAC_PI_4).abs() < 1e-5,
            "expected π/4, got {angle}"
        );
    }

    #[test]
    fn corner_to_angle_rad_top_left_square_is_neg_pi_over_4() {
        use crate::pageable::LinearGradientCorner;
        let angle = corner_to_angle_rad(LinearGradientCorner::TopLeft, 100.0, 100.0);
        assert!(
            (angle + std::f32::consts::FRAC_PI_4).abs() < 1e-5,
            "expected -π/4, got {angle}"
        );
    }

    #[test]
    fn corner_to_angle_rad_bottom_right_square_is_3pi_over_4() {
        use crate::pageable::LinearGradientCorner;
        let angle = corner_to_angle_rad(LinearGradientCorner::BottomRight, 100.0, 100.0);
        let expected = 3.0 * std::f32::consts::FRAC_PI_4;
        assert!(
            (angle - expected).abs() < 1e-5,
            "expected 3π/4, got {angle}"
        );
    }

    #[test]
    fn corner_to_angle_rad_bottom_left_square_is_neg_3pi_over_4() {
        use crate::pageable::LinearGradientCorner;
        let angle = corner_to_angle_rad(LinearGradientCorner::BottomLeft, 100.0, 100.0);
        let expected = -3.0 * std::f32::consts::FRAC_PI_4;
        assert!(
            (angle - expected).abs() < 1e-5,
            "expected -3π/4, got {angle}"
        );
    }

    #[test]
    fn corner_to_angle_rad_top_right_and_top_left_are_negations_for_square() {
        // Symmetry: "to top right" and "to top left" are equal-magnitude on a square.
        use crate::pageable::LinearGradientCorner;
        let right = corner_to_angle_rad(LinearGradientCorner::TopRight, 200.0, 200.0);
        let left = corner_to_angle_rad(LinearGradientCorner::TopLeft, 200.0, 200.0);
        assert!(
            (right + left).abs() < 1e-5,
            "angles should cancel: right={right} left={left}"
        );
    }

    #[test]
    fn corner_to_angle_rad_non_square_top_right_uses_aspect_ratio() {
        // For w=300, h=400: "to top right" = atan2(h, w) = atan2(400, 300).
        // On a non-square box the corner direction depends on the aspect ratio.
        use crate::pageable::LinearGradientCorner;
        let angle = corner_to_angle_rad(LinearGradientCorner::TopRight, 300.0, 400.0);
        let expected = f32::atan2(400.0_f32, 300.0_f32);
        assert!(
            (angle - expected).abs() < 1e-5,
            "expected atan2(400,300)={expected}, got {angle}"
        );
    }

    // ─── ellipse_corner_scale ─────────────────────────────────────────────────

    #[test]
    fn ellipse_corner_scale_zero_rx_returns_early() {
        // Degenerate: rx0 == 0 → early return of (rx0, ry0) unchanged.
        let (rx, ry) = ellipse_corner_scale(50.0, 50.0, 0.0, 0.0, 100.0, 100.0, 0.0, 25.0, true);
        assert_eq!(rx, 0.0);
        assert_eq!(ry, 25.0);
    }

    #[test]
    fn ellipse_corner_scale_zero_ry_returns_early() {
        let (rx, ry) = ellipse_corner_scale(50.0, 50.0, 0.0, 0.0, 100.0, 100.0, 30.0, 0.0, false);
        assert_eq!(rx, 30.0);
        assert_eq!(ry, 0.0);
    }

    #[test]
    fn ellipse_corner_scale_centered_square_all_corners_equidistant() {
        // Center exactly at box center (50,50); all corners are 50√2 away.
        // Farthest == closest since all ratios equal; result = (50√2, 50√2).
        let sqrt2: f32 = std::f32::consts::SQRT_2;
        let (rx_f, ry_f) = ellipse_corner_scale(50.0, 50.0, 0.0, 0.0, 100.0, 100.0, 1.0, 1.0, true);
        assert!((rx_f - 50.0 * sqrt2).abs() < 1e-3, "farthest rx={rx_f}");
        assert!((ry_f - 50.0 * sqrt2).abs() < 1e-3, "farthest ry={ry_f}");

        let (rx_c, _ry_c) =
            ellipse_corner_scale(50.0, 50.0, 0.0, 0.0, 100.0, 100.0, 1.0, 1.0, false);
        assert!(
            (rx_f - rx_c).abs() < 1e-3,
            "farthest={rx_f} and closest={rx_c} should match for symmetric center"
        );
    }

    #[test]
    fn ellipse_corner_scale_farthest_off_center() {
        // Center at (30,30); box (0,0,100,100); rx0=ry0=1.
        // Corner distances: (0,0)→30√2, (100,0)→√5800, (0,100)→√5800, (100,100)→70√2.
        // Farthest ratio = 70√2 → result = (70√2, 70√2).
        let sqrt2: f32 = std::f32::consts::SQRT_2;
        let (rx, ry) = ellipse_corner_scale(30.0, 30.0, 0.0, 0.0, 100.0, 100.0, 1.0, 1.0, true);
        assert!((rx - 70.0 * sqrt2).abs() < 1e-3, "rx={rx}");
        assert!((ry - 70.0 * sqrt2).abs() < 1e-3, "ry={ry}");
    }

    #[test]
    fn ellipse_corner_scale_closest_off_center() {
        // Same setup; closest corner = (0,0) at 30√2 → result = (30√2, 30√2).
        let sqrt2: f32 = std::f32::consts::SQRT_2;
        let (rx, ry) = ellipse_corner_scale(30.0, 30.0, 0.0, 0.0, 100.0, 100.0, 1.0, 1.0, false);
        assert!((rx - 30.0 * sqrt2).abs() < 1e-3, "rx={rx}");
        assert!((ry - 30.0 * sqrt2).abs() < 1e-3, "ry={ry}");
    }

    #[test]
    fn ellipse_corner_scale_non_uniform_radii_farthest() {
        // Center at (0,0); box (100,0,100,100); rx0=50, ry0=25.
        // Per-corner ratios: (100,0)→2, (200,0)→4, (100,100)→√20, (200,100)→4√2.
        // Farthest ratio = 4√2 → result = (50·4√2, 25·4√2) = (200√2, 100√2).
        let sqrt2: f32 = std::f32::consts::SQRT_2;
        let (rx, ry) = ellipse_corner_scale(0.0, 0.0, 100.0, 0.0, 100.0, 100.0, 50.0, 25.0, true);
        assert!((rx - 200.0 * sqrt2).abs() < 0.01, "rx={rx}");
        assert!((ry - 100.0 * sqrt2).abs() < 0.01, "ry={ry}");
    }

    #[test]
    fn ellipse_corner_scale_non_uniform_radii_closest() {
        // Same setup; closest corner (100,0) has ratio=2 → result = (100, 50).
        let (rx, ry) = ellipse_corner_scale(0.0, 0.0, 100.0, 0.0, 100.0, 100.0, 50.0, 25.0, false);
        assert!((rx - 100.0).abs() < 1e-3, "rx={rx}");
        assert!((ry - 50.0).abs() < 1e-3, "ry={ry}");
    }

    // ─── resolve_point and resolve_length ─────────────────────────────────────

    #[test]
    fn resolve_point_length_returns_value() {
        assert_eq!(
            resolve_point(&BgLengthPercentage::Length(42.0), 200.0),
            42.0
        );
    }

    #[test]
    fn resolve_point_percentage_multiplies_container() {
        assert_eq!(
            resolve_point(&BgLengthPercentage::Percentage(0.25), 200.0),
            50.0
        );
    }

    #[test]
    fn resolve_length_length_returns_value() {
        assert_eq!(
            resolve_length(&BgLengthPercentage::Length(15.0), 300.0),
            15.0
        );
    }

    #[test]
    fn resolve_length_percentage_multiplies_container() {
        assert_eq!(
            resolve_length(&BgLengthPercentage::Percentage(0.5), 300.0),
            150.0
        );
    }
}

#[cfg(test)]
mod resolve_gradient_stops_tests {
    use super::*;
    use crate::pageable::GradientStop;
    use crate::pageable::GradientStopPosition::{self, *};

    fn fr(f: f32) -> GradientStopPosition {
        Fraction(f)
    }
    fn px(f: f32) -> GradientStopPosition {
        LengthPx(f)
    }
    fn stop(p: GradientStopPosition, rgba: [u8; 4]) -> GradientStop {
        GradientStop { position: p, rgba }
    }

    #[test]
    fn fraction_only_passes_through() {
        let stops = vec![
            stop(fr(0.0), [255, 0, 0, 255]),
            stop(fr(1.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, false).unwrap();
        assert_eq!(out.len(), 2);
        assert!((out[0].offset.get() - 0.0).abs() < 1e-6);
        assert!((out[1].offset.get() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn length_px_resolved_by_line_length() {
        let stops = vec![
            stop(px(0.0), [255, 0, 0, 255]),
            stop(px(50.0), [0, 0, 255, 255]),
        ];
        // line_length = 100 → 50px = 0.5
        let out = resolve_gradient_stops(&stops, 100.0, false).unwrap();
        assert_eq!(out.len(), 2);
        assert!((out[1].offset.get() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn auto_position_filled_at_endpoints() {
        let stops = vec![stop(Auto, [255, 0, 0, 255]), stop(Auto, [0, 0, 255, 255])];
        let out = resolve_gradient_stops(&stops, 100.0, false).unwrap();
        assert!((out[0].offset.get() - 0.0).abs() < 1e-6);
        assert!((out[1].offset.get() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn auto_position_filled_in_middle() {
        let stops = vec![
            stop(fr(0.0), [255, 0, 0, 255]),
            stop(Auto, [0, 255, 0, 255]),
            stop(fr(1.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, false).unwrap();
        assert!((out[1].offset.get() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn mixed_length_fraction_auto() {
        // linear-gradient(red, blue 50px, green) on line_length=100
        // → red Auto (→0), blue 0.5, green Auto (→1)
        let stops = vec![
            stop(Auto, [255, 0, 0, 255]),
            stop(px(50.0), [0, 0, 255, 255]),
            stop(Auto, [0, 255, 0, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, false).unwrap();
        assert_eq!(out.len(), 3);
        assert!((out[0].offset.get() - 0.0).abs() < 1e-6);
        assert!((out[1].offset.get() - 0.5).abs() < 1e-6);
        assert!((out[2].offset.get() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn out_of_range_length_renormalized() {
        // 50px on line_length=30 → 50/30 ≈ 1.667 > 1 → renormalize で
        // 端点合成。renormalize 入力 = [(0.0, red), (1.667, blue)]:
        //   0.0 はちょうど red 位置なので左合成スキップ
        //   1.667 を 1.0 で右合成 → 2 stops [(0.0, red), (1.0, synthesized)]
        let stops = vec![
            stop(fr(0.0), [255, 0, 0, 255]),
            stop(px(50.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 30.0, false).unwrap();
        assert_eq!(out.len(), 2, "renormalize keeps 2 stops");
        assert!((out[0].offset.get() - 0.0).abs() < 1e-6);
        assert!((out[1].offset.get() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn negative_fraction_renormalized() {
        // [(-0.1, red), (1.0, blue)]: 左合成で 0.0 を pad、1.0 はちょうど
        // blue 位置なので右合成スキップ → 2 stops
        let stops = vec![
            stop(fr(-0.1), [255, 0, 0, 255]),
            stop(fr(1.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, false).unwrap();
        assert_eq!(out.len(), 2, "renormalize keeps 2 stops");
        assert!((out[0].offset.get() - 0.0).abs() < 1e-6);
        assert!((out[1].offset.get() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn monotonic_clamp_applied() {
        // [0.6, 0.3] → [0.6, 0.6] (monotonic fix)
        let stops = vec![
            stop(fr(0.6), [255, 0, 0, 255]),
            stop(fr(0.3), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, false).unwrap();
        assert!((out[0].offset.get() - 0.6).abs() < 1e-6);
        assert!((out[1].offset.get() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn line_length_zero_returns_none() {
        let stops = vec![
            stop(px(50.0), [255, 0, 0, 255]),
            stop(fr(1.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 0.0, false);
        assert!(out.is_none());
    }

    /// `repeating-linear-gradient(red 0%, blue 25%)`: period = 0.25。
    /// 4 周期分 + 端点合成で `[0, 1]` を覆う。0% / 25% / 50% / 75% / 100% は
    /// hard stop boundary を保ち、隣接 (red→blue→red→...) のパターンが見える。
    #[test]
    fn repeating_simple_period() {
        let stops = vec![
            stop(fr(0.0), [255, 0, 0, 255]),
            stop(fr(0.25), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, true).unwrap();
        // 期待: forward = ceil(0.75/0.25) = 3, backward = 0
        // → k = 0, 1, 2, 3 で各周期の (red, blue) を配置
        // (0, red), (0.25, blue), (0.25, red), (0.5, blue), (0.5, red),
        // (0.75, blue), (0.75, red), (1.0, blue) の 8 stops
        assert_eq!(out.len(), 8, "got offsets: {:?}", offsets(&out));
        let expected = [
            (0.0, [255, 0, 0, 255]),
            (0.25, [0, 0, 255, 255]),
            (0.25, [255, 0, 0, 255]),
            (0.5, [0, 0, 255, 255]),
            (0.5, [255, 0, 0, 255]),
            (0.75, [0, 0, 255, 255]),
            (0.75, [255, 0, 0, 255]),
            (1.0, [0, 0, 255, 255]),
        ];
        for (i, (eo, ec)) in expected.iter().enumerate() {
            assert!(
                (out[i].offset.get() - eo).abs() < 1e-5,
                "stop[{i}] offset got {} expected {eo}",
                out[i].offset.get()
            );
            // krilla::paint::Stop にコンポーネントアクセスがないので
            // RGB は output 個数だけ確認 (色の検証は VRT 側で行う)
            let _ = ec;
        }
    }

    /// `repeating-linear-gradient(red 25%, blue 50%)`: period = 0.25、
    /// 周期境界が box の端点に揃わない。前後両方向に展開され、renormalize で
    /// 端点合成される。
    #[test]
    fn repeating_offset_period_extends_both_directions() {
        let stops = vec![
            stop(fr(0.25), [255, 0, 0, 255]),
            stop(fr(0.5), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, true).unwrap();
        // backward = ceil(0.25/0.25) = 1, forward = ceil(0.5/0.25) = 2
        // 展開列: k=-1 (0.0 red, 0.25 blue), k=0 (0.25 red, 0.5 blue),
        //         k=1 (0.5 red, 0.75 blue), k=2 (0.75 red, 1.0 blue)
        // = (0.0, red), (0.25, blue), (0.25, red), (0.5, blue), (0.5, red),
        //   (0.75, blue), (0.75, red), (1.0, blue) → all in [0, 1] なので
        // renormalize fast path で 8 stops そのまま。
        assert_eq!(out.len(), 8);
        let first = out.first().unwrap().offset.get();
        let last = out.last().unwrap().offset.get();
        assert!((first - 0.0).abs() < 1e-5, "first offset {first}");
        assert!((last - 1.0).abs() < 1e-5, "last offset {last}");
    }

    /// `repeating-linear-gradient(red 0%, blue 0%)`: period = 0 (degenerate)。
    /// CSS Images 3 §3.6 に従い最終 stop の色 (blue) で塗りつぶす単色 fill になる。
    #[test]
    fn repeating_period_zero_renders_solid_last_color() {
        let stops = vec![
            stop(fr(0.0), [255, 0, 0, 255]),
            stop(fr(0.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, true).expect("solid fill, not None");
        // 単色 fill: 2 stops at 0.0/1.0 で同色 (blue)。renormalize の fast path
        // (全 stop in [0, 1]) を通って 2 stops のまま。
        assert_eq!(out.len(), 2);
        assert!((out[0].offset.get() - 0.0).abs() < 1e-5);
        assert!((out[1].offset.get() - 1.0).abs() < 1e-5);
    }

    /// `repeating-linear-gradient(red 0%, blue 100%)`: period = 1.0。
    /// 1 周期で `[0, 1]` を完全カバー、non-repeating と同じ結果。
    #[test]
    fn repeating_full_period_equals_single_pass() {
        let stops = vec![
            stop(fr(0.0), [255, 0, 0, 255]),
            stop(fr(1.0), [0, 0, 255, 255]),
        ];
        let repeat = resolve_gradient_stops(&stops, 100.0, true).unwrap();
        let plain = resolve_gradient_stops(&stops, 100.0, false).unwrap();
        // どちらも 2 stops で同じ位置。
        assert_eq!(repeat.len(), plain.len());
        for (r, p) in repeat.iter().zip(plain.iter()) {
            assert!((r.offset.get() - p.offset.get()).abs() < 1e-5);
        }
    }

    /// `repeating-linear-gradient(red, blue, green)`: 3 stops, Auto fixup 後
    /// (0.0, 0.5, 1.0) → period = 1.0。1 周期で完結。
    #[test]
    fn repeating_three_color_stops() {
        let stops = vec![
            stop(Auto, [255, 0, 0, 255]),
            stop(Auto, [0, 255, 0, 255]),
            stop(Auto, [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, true).unwrap();
        // forward = backward = 0 → 1 copy = 3 stops
        assert_eq!(out.len(), 3);
        assert!((out[0].offset.get() - 0.0).abs() < 1e-5);
        assert!((out[1].offset.get() - 0.5).abs() < 1e-5);
        assert!((out[2].offset.get() - 1.0).abs() < 1e-5);
    }

    /// `repeating-radial-gradient(red 0px, blue 25px)` を radius=100px で評価。
    /// 0/25 px → 0.0/0.25 fraction で linear と同じ展開。
    #[test]
    fn repeating_with_length_px_stops() {
        let stops = vec![
            stop(px(0.0), [255, 0, 0, 255]),
            stop(px(25.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, true).unwrap();
        // line_length=100, 25px → 0.25 fraction → repeating_simple_period と
        // 同じ展開: 8 stops
        assert_eq!(out.len(), 8);
    }

    fn offsets(stops: &[krilla::paint::Stop]) -> Vec<f32> {
        stops.iter().map(|s| s.offset.get()).collect()
    }
}

#[cfg(test)]
mod renormalize_stops_to_unit_range_tests {
    use super::renormalize_stops_to_unit_range;

    fn s(offset: f32, r: u8, g: u8, b: u8) -> (f32, [u8; 4]) {
        (offset, [r, g, b, 255])
    }

    fn expect(stops: &[(f32, [u8; 4])], expected: &[(f32, [u8; 4])]) {
        assert_eq!(stops.len(), expected.len(), "stop count mismatch");
        for (i, (got, exp)) in stops.iter().zip(expected).enumerate() {
            assert!(
                (got.0 - exp.0).abs() < 1e-5,
                "stop[{i}].offset: got {} expected {}",
                got.0,
                exp.0
            );
            assert_eq!(got.1, exp.1, "stop[{i}].rgba");
        }
    }

    #[test]
    fn no_op_when_all_in_range() {
        let stops = vec![s(0.0, 255, 0, 0), s(1.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [255, 0, 0, 255]), (1.0, [0, 0, 255, 255])]);
    }

    #[test]
    fn synthesize_left_endpoint() {
        // red at -50%, blue at 100%: at offset 0, t = 0.5 / 1.5 = 1/3
        // r = 255 + 1/3 * (0 - 255) = 170
        // g = 0
        // b = 0   + 1/3 * (255 - 0) = 85
        let stops = vec![s(-0.5, 255, 0, 0), s(1.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(
            &result,
            &[(0.0, [170, 0, 85, 255]), (1.0, [0, 0, 255, 255])],
        );
    }

    #[test]
    fn synthesize_right_endpoint() {
        // red at 50%, blue at 200%: at offset 1, t = 0.5 / 1.5 = 1/3
        let stops = vec![s(0.5, 255, 0, 0), s(2.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(
            &result,
            &[
                (0.0, [255, 0, 0, 255]),
                (0.5, [255, 0, 0, 255]),
                (1.0, [170, 0, 85, 255]),
            ],
        );
    }

    #[test]
    fn all_below_zero_pads_with_last_color() {
        // stops at -50%, -25%: both out of range; pad after last (blue)
        let stops = vec![s(-0.5, 255, 0, 0), s(-0.25, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [0, 0, 255, 255]), (1.0, [0, 0, 255, 255])]);
    }

    #[test]
    fn all_above_one_pads_with_first_color() {
        // stops at 150%, 200%: both out of range; pad before first (red)
        let stops = vec![s(1.5, 255, 0, 0), s(2.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [255, 0, 0, 255]), (1.0, [255, 0, 0, 255])]);
    }

    #[test]
    fn boundary_stop_at_zero_no_left_synthesis() {
        // -50% red, 0% blue, 100% green: 0.0 はちょうど blue で合成不要
        let stops = vec![s(-0.5, 255, 0, 0), s(0.0, 0, 0, 255), s(1.0, 0, 255, 0)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [0, 0, 255, 255]), (1.0, [0, 255, 0, 255])]);
    }

    #[test]
    fn alpha_channel_is_interpolated() {
        // red(alpha=0) at -50%, blue(alpha=255) at 100%
        // at offset 0, t = 1/3 → alpha = 0 + 1/3 * 255 ≈ 85
        let stops = vec![(-0.5_f32, [255, 0, 0, 0]), (1.0_f32, [0, 0, 255, 255])];
        let result = renormalize_stops_to_unit_range(stops);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1[3], 85, "alpha at offset 0");
        assert_eq!(result[1].1[3], 255, "alpha at offset 1");
    }

    /// Hard stop (coincident positions) は CSS Images 3 §3.5.1 で valid。
    /// 範囲外 stop の合成中に hard stop が含まれていても、in-range の
    /// hard stop は preserve され、合成側は隣接区間を正しく拾う。
    #[test]
    fn hard_stop_in_range_is_preserved_during_synthesis() {
        // -50% red, 50% blue, 50% green, 150% yellow
        // 左端合成: t=0 は (-0.5, red)/(0.5, blue) 区間で alpha=0.5 → [128, 0, 128]
        // hard stop preserved: blue at 0.5, green at 0.5
        // 右端合成: t=1 は (0.5, green)/(1.5, yellow) 区間で alpha=0.5 → [128, 255, 0]
        let stops = vec![
            s(-0.5, 255, 0, 0),
            s(0.5, 0, 0, 255),
            s(0.5, 0, 255, 0),
            s(1.5, 255, 255, 0),
        ];
        let result = renormalize_stops_to_unit_range(stops);
        expect(
            &result,
            &[
                (0.0, [128, 0, 128, 255]),
                (0.5, [0, 0, 255, 255]),
                (0.5, [0, 255, 0, 255]),
                (1.0, [128, 255, 0, 255]),
            ],
        );
    }
}
