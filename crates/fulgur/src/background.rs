//! Background rendering: color fills and image layers.

use std::sync::Arc;

use crate::pageable::{
    BackgroundLayer, BgBox, BgClip, BgLengthPercentage, BgRepeat, BgSize, BlockStyle, Canvas,
};

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

    let data: krilla::Data = Arc::clone(&layer.image_data).into();
    let Ok(image) = layer.format.to_krilla_image(data) else {
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
            image_data: Arc::new(vec![]),
            format: ImageFormat::Png,
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
    fn test_repeat_alignment_with_offset_position() {
        // Tiles must align with position: tiles at position ± n*image_size.
        // position=25, clip_start=10, image_size=20 → tiles at 5, 25, 45, ...
        let (size, _space, start, _end) =
            resolve_repeat_axis(BgRepeat::Repeat, 25.0, 20.0, 10.0, 100.0);
        assert_eq!(size, 20.0);
        assert_eq!(start, 5.0);
    }
}
