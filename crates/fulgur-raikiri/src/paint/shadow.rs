//! Text shadows (CSS Text Decoration 3 §4).
//!
//! Each shadow is the run's glyphs filled with the shadow color and moved
//! by the shadow offset. A sharp shadow is drawn as glyphs. PDF has no blur,
//! so a blurred shadow is rasterized: the glyph outlines are filled into a
//! coverage mask, blurred, and drawn as an image whose alpha is the blurred
//! coverage, as browsers do when they print blurred shadows to PDF.

use super::text_clip::{PathSink, Pen, append_run};
use super::{FontCache, draw_glyphs};
use krilla::geom::{Size, Transform};
use krilla::image::Image;
use krilla::surface::Surface;
use raikiri_html::computed::CssColor;
use raikiri_html::{PositionedGlyphRun, TextShadow};

/// Raster pixels per CSS px of a blurred shadow (288 dpi on paper).
const RASTER_SCALE: f32 = 3.0;

/// Upper bound on the pixels of one blurred shadow's raster. A larger
/// shadow is rasterized at a lower scale.
const MAX_RASTER_PIXELS: f32 = 4_000_000.0;

/// Draw the shadows of `run`, last declared first so that the first shadow
/// is on top (CSS Text Decoration 3 §4).
pub(super) fn paint(
    surface: &mut Surface<'_>,
    run: &PositionedGlyphRun<'_>,
    fonts: &mut FontCache,
) {
    if run.glyphs.is_empty() || run.font_size <= 0.0 {
        return;
    }
    for shadow in run.shadows.iter().rev() {
        if shadow.color.a == 0 {
            continue;
        }
        if shadow.blur_radius > 0.0 {
            paint_blurred(surface, run, shadow);
        } else {
            let origin = (
                run.origin.0 + shadow.offset.0,
                run.origin.1 + shadow.offset.1,
            );
            draw_glyphs(surface, run, fonts, origin, shadow.color);
        }
    }
}

/// Draw one blurred shadow of `run` as an image.
fn paint_blurred(surface: &mut Surface<'_>, run: &PositionedGlyphRun<'_>, shadow: &TextShadow) {
    let Some(image) = blurred_raster(run, shadow) else {
        return;
    };
    let Some(size) = Size::from_wh(image.width, image.height) else {
        return;
    };
    surface.push_transform(&Transform::from_translate(image.x, image.y));
    surface.draw_image(
        Image::from_rgba8(image.rgba, image.pixel_width, image.pixel_height),
        size,
    );
    surface.pop();
}

/// A blurred shadow rasterized for drawing: RGBA pixels and the rectangle
/// they cover, in CSS px.
struct ShadowRaster {
    rgba: Vec<u8>,
    pixel_width: u32,
    pixel_height: u32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// Rasterize the blurred `shadow` of `run`, or `None` when it has no ink.
///
/// The blur is a Gaussian with a standard deviation of half the blur radius
/// (CSS Backgrounds 3 §7.1), approximated by three box blurs. The raster
/// extends three standard deviations past the outlines, where the Gaussian
/// has fallen below one percent.
fn blurred_raster(run: &PositionedGlyphRun<'_>, shadow: &TextShadow) -> Option<ShadowRaster> {
    let mut pen = Pen::new(SkiaPath(tiny_skia::PathBuilder::new()));
    // The glyphs are drawn without synthetic oblique, so their shadow is too.
    append_run(&mut pen, run, false);
    let path = pen.builder.0.finish()?;
    let bounds = path.bounds();
    let sigma = shadow.blur_radius / 2.0;
    let extent = sigma * 3.0;
    let left = bounds.left() + shadow.offset.0 - extent;
    let top = bounds.top() + shadow.offset.1 - extent;
    let width = bounds.width() + extent * 2.0;
    let height = bounds.height() + extent * 2.0;
    if !(width.is_finite() && height.is_finite()) {
        return None;
    }
    let scale = RASTER_SCALE.min((MAX_RASTER_PIXELS / (width * height)).sqrt());
    // The raster starts on a whole pixel so the scale stays exact.
    let x = (left * scale).floor();
    let y = (top * scale).floor();
    let pixel_width = ((left + width) * scale).ceil() - x;
    let pixel_height = ((top + height) * scale).ceil() - y;
    let (pixel_width, pixel_height) = (pixel_width as u32, pixel_height as u32);
    let mut mask = tiny_skia::Mask::new(pixel_width, pixel_height)?;
    let transform = tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        shadow.offset.0 * scale - x,
        shadow.offset.1 * scale - y,
    );
    mask.fill_path(&path, tiny_skia::FillRule::Winding, true, transform);
    let mut coverage: Vec<f32> = mask.data().iter().map(|&value| f32::from(value)).collect();
    gaussian_blur(
        &mut coverage,
        pixel_width as usize,
        pixel_height as usize,
        sigma * scale,
    );
    let alpha = f32::from(shadow.color.a) / 255.0;
    let mut ink = false;
    let mut rgba = Vec::with_capacity(coverage.len() * 4);
    for value in coverage {
        let a = (value * alpha).round().clamp(0.0, 255.0) as u8;
        ink |= a != 0;
        let CssColor { r, g, b, .. } = shadow.color;
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    ink.then_some(ShadowRaster {
        rgba,
        pixel_width,
        pixel_height,
        x: x / scale,
        y: y / scale,
        width: pixel_width as f32 / scale,
        height: pixel_height as f32 / scale,
    })
}

/// Blur `pixels` (row-major, `width` by `height`) with a Gaussian of
/// standard deviation `sigma` pixels, approximated by three box blurs in
/// each direction.
fn gaussian_blur(pixels: &mut [f32], width: usize, height: usize, sigma: f32) {
    let mut scratch = vec![0.0; pixels.len()];
    for radius in box_radii(sigma) {
        if radius == 0 {
            continue;
        }
        for row in 0..height {
            let start = row * width;
            box_blur(
                &pixels[start..start + width],
                &mut scratch[start..start + width],
                1,
                radius,
            );
        }
        // Columns are blurred from the row pass's output back into `pixels`.
        for column in 0..width {
            box_blur(&scratch[column..], &mut pixels[column..], width, radius);
        }
    }
}

/// Radii of three successive box blurs whose combined variance is
/// `sigma²`: the widths are the two odd integers around the ideal width,
/// chosen so that the variances add up as closely as possible.
fn box_radii(sigma: f32) -> [usize; 3] {
    if !(sigma.is_finite() && sigma > 0.0) {
        return [0; 3];
    }
    const PASSES: f32 = 3.0;
    let ideal = (12.0 * sigma * sigma / PASSES + 1.0).sqrt();
    let mut lower = ideal.floor() as i64;
    if lower % 2 == 0 {
        lower -= 1;
    }
    let lower = lower.max(1);
    let upper = lower + 2;
    let lower_f = lower as f32;
    let small = ((12.0 * sigma * sigma
        - PASSES * lower_f * lower_f
        - 4.0 * PASSES * lower_f
        - 3.0 * PASSES)
        / (-4.0 * lower_f - 4.0))
        .round()
        .clamp(0.0, PASSES) as usize;
    let radius = |width: i64| ((width - 1) / 2) as usize;
    std::array::from_fn(|pass| {
        if pass < small {
            radius(lower)
        } else {
            radius(upper)
        }
    })
}

/// One centered box blur of `radius` over the line of `input` whose
/// elements are `stride` apart, written to the same positions of `output`.
/// Pixels outside the line count as zero.
fn box_blur(input: &[f32], output: &mut [f32], stride: usize, radius: usize) {
    let len = input
        .len()
        .div_ceil(stride)
        .min(output.len().div_ceil(stride));
    let at = |index: usize| input[index * stride];
    let scale = 1.0 / (radius * 2 + 1) as f32;
    let mut sum: f32 = (0..=radius.min(len.saturating_sub(1))).map(at).sum();
    for index in 0..len {
        output[index * stride] = sum * scale;
        let enter = index + radius + 1;
        if enter < len {
            sum += at(enter);
        }
        if let Some(leave) = index.checked_sub(radius) {
            sum -= at(leave);
        }
    }
}

/// Collects glyph outlines into a tiny-skia path for rasterizing.
struct SkiaPath(tiny_skia::PathBuilder);

impl PathSink for SkiaPath {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }

    fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
mod tests;
