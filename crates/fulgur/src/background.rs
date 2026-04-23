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

    let (img_w, img_h) = resolve_size(layer, ow, oh);
    if img_w <= 0.0 || img_h <= 0.0 {
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

    match &layer.content {
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
