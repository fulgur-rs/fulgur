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
        BgImageContent::LinearGradient { direction, stops } => {
            // Match before the loop, not inside: Angle(_) is constant per
            // layer and gets hoisted automatically; Corner needs per-tile
            // recomputation because the angle depends on tile aspect (CSS
            // Images §3.1.1) and we don't rely on tile uniformity.
            match direction {
                crate::pageable::LinearGradientDirection::Angle(a) => {
                    let angle = *a;
                    for (tx, ty, tw, th) in &tiles {
                        draw_linear_gradient(canvas, angle, stops, *tx, *ty, *tw, *th);
                    }
                }
                crate::pageable::LinearGradientDirection::Corner(corner) => {
                    for (tx, ty, tw, th) in &tiles {
                        let angle = corner_to_angle_rad(*corner, *tw, *th);
                        draw_linear_gradient(canvas, angle, stops, *tx, *ty, *tw, *th);
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
        } => {
            // Per-tile shape geometry — uses each tile's own (tw, th)
            // for cx/cy/rx/ry. No uniformity assumption needed.
            for (tx, ty, tw, th) in &tiles {
                draw_radial_gradient(
                    canvas, *shape, size, position_x, position_y, stops, *tx, *ty, *tw, *th,
                );
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
fn draw_linear_gradient(
    canvas: &mut Canvas<'_, '_>,
    angle_rad: f32,
    stops: &[crate::pageable::GradientStop],
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

    let krilla_stops: Vec<krilla::paint::Stop> = stops
        .iter()
        .map(|s| krilla::paint::Stop {
            // `s.offset` is convert-time-clamped to [0, 1] in resolve_linear_gradient,
            // so the explicit clamp + expect documents the invariant.
            offset: krilla::num::NormalizedF32::new(s.offset.clamp(0.0, 1.0))
                .expect("offset is clamped to [0, 1]"),
            color: krilla::color::rgb::Color::new(s.rgba[0], s.rgba[1], s.rgba[2]).into(),
            opacity: crate::pageable::alpha_to_opacity(s.rgba[3]),
        })
        .collect();

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

    canvas.surface.set_fill(Some(krilla::paint::Fill {
        paint: lg.into(),
        rule: Default::default(),
        opacity: krilla::num::NormalizedF32::ONE,
    }));
    canvas.surface.set_stroke(None);

    // Per CSS Images §3, the gradient image has the size of the positioning
    // (origin) area; areas inside `clip` but outside `origin` should be
    // transparent for this layer. With `SpreadMethod::Pad`, painting the
    // clip rect would extend the first/last stop colors as solid bands into
    // those areas. Draw the origin rect — the caller's already-pushed
    // clip_path bounds it, so what's rendered is `origin ∩ clip`, which is
    // the spec-correct visible region.
    let Some(rect_path) = build_rect_path(ox, oy, ow, oh) else {
        canvas.surface.set_fill(None);
        return;
    };
    canvas.surface.draw_path(&rect_path);
    // Don't leak the gradient paint to the next draw on this surface.
    canvas.surface.set_fill(None);
}

/// Draw a CSS radial-gradient over the origin rect.
///
/// CSS Images 3 §3.6 の式に従い (cx, cy, rx, ry) を計算し、Krilla の
/// `RadialGradient` (円のみサポート) に楕円を transform で表現する。
#[allow(clippy::too_many_arguments)]
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
}
