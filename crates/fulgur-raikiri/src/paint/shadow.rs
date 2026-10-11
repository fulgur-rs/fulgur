//! Text shadows (CSS Text Decoration 3 §4).
//!
//! Each shadow is the glyphs filled with the shadow color and moved
//! by the shadow offset. A sharp shadow is drawn as the glyph outlines, so
//! that extracted or searched text holds the run's text once. PDF has no blur,
//! so a blurred shadow is rasterized: the glyph outlines are filled into a
//! coverage mask, blurred, and drawn as an image whose alpha is the blurred
//! coverage, as browsers do when they print blurred shadows to PDF.

use super::fill;
use super::text_clip::{PathSink, Pen, append_run};
use krilla::geom::{PathBuilder, Size, Transform};
use krilla::image::Image;
use krilla::surface::Surface;
use raikiri_html::computed::CssColor;
use raikiri_html::{PositionedGlyphRun, TextShadow};

/// Raster pixels per CSS px of a blurred shadow (288 dpi on paper).
const RASTER_SCALE: f32 = 3.0;

/// Upper bound on the pixels of one blurred shadow's raster. A larger
/// shadow is rasterized at a lower scale.
const MAX_RASTER_PIXELS: f32 = 4_000_000.0;

/// Upper bound on the pixels of all blurred shadow rasters of a document,
/// which bounds the memory and blur work a short style sheet can ask for.
const DOCUMENT_RASTER_PIXELS: f32 = 64_000_000.0;

/// The lowest scale a blurred shadow is rasterized at. A shadow that does
/// not fit the remaining budget at this scale is not drawn.
const MIN_RASTER_SCALE: f32 = 0.5;

/// Raster pixels left for the blurred shadows of a document.
pub(super) struct Budget {
    pixels: f32,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            pixels: DOCUMENT_RASTER_PIXELS,
        }
    }
}

/// Draw `shadow` of the glyphs of `runs`, which share it. The caller
/// paints the layers last declared first, so that the first shadow is on
/// top (CSS Text Decoration 3 §4).
pub(super) fn paint(
    surface: &mut Surface<'_>,
    runs: &[&PositionedGlyphRun<'_>],
    shadow: &TextShadow,
    budget: &mut Budget,
) {
    // An overflowing length leaves a non-finite offset or blur radius,
    // which has no drawable shadow.
    let finite = shadow.offset.0.is_finite()
        && shadow.offset.1.is_finite()
        && shadow.blur_radius.is_finite();
    if shadow.color.a == 0 || !finite {
        return;
    }
    if shadow.blur_radius > 0.0 {
        let mut pen = Pen::new(SkiaPath(tiny_skia::PathBuilder::new()));
        append_runs(&mut pen, runs);
        if let Some(path) = pen.builder.0.finish() {
            paint_blurred(surface, &path, shadow, budget);
        }
    } else {
        let mut pen = Pen::new(PathBuilder::new());
        append_runs(&mut pen, runs);
        if let Some(path) = pen.builder.finish() {
            surface.push_transform(&Transform::from_translate(shadow.offset.0, shadow.offset.1));
            surface.set_fill(Some(fill(shadow.color)));
            surface.draw_path(&path);
            surface.pop();
        }
    }
}

/// Append the glyph outlines of `runs`. The glyphs are drawn without
/// synthetic oblique, so their shadow is too.
fn append_runs<B: PathSink>(pen: &mut Pen<B>, runs: &[&PositionedGlyphRun<'_>]) {
    for run in runs {
        if !run.glyphs.is_empty() && run.font_size > 0.0 {
            append_run(pen, run, false);
        }
    }
}

/// Draw the outlines in `path`, blurred and moved by the shadow offset, as
/// an image.
fn paint_blurred(
    surface: &mut Surface<'_>,
    path: &tiny_skia::Path,
    shadow: &TextShadow,
    budget: &mut Budget,
) {
    let params = ShadowParams {
        offset: shadow.offset,
        blur_radius: shadow.blur_radius,
        color: shadow.color,
    };
    let Some(image) = blurred_raster(path, params, budget) else {
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

/// The values of a [`TextShadow`] a raster is made from.
#[derive(Clone, Copy)]
struct ShadowParams {
    offset: (f32, f32),
    blur_radius: f32,
    color: CssColor,
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

/// Rasterize the outlines in `path` with the blurred `shadow`, or `None`
/// when it has no ink or does not fit the remaining budget.
///
/// The blur is a Gaussian with a standard deviation of half the blur radius
/// (CSS Backgrounds 3 §7.1), approximated by three box blurs. The raster
/// extends three standard deviations past the outlines, where the Gaussian
/// has fallen below one percent.
fn blurred_raster(
    path: &tiny_skia::Path,
    shadow: ShadowParams,
    budget: &mut Budget,
) -> Option<ShadowRaster> {
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
    let pixels = MAX_RASTER_PIXELS.min(budget.pixels);
    let scale = RASTER_SCALE.min(fitting_scale(width, height, pixels));
    if scale.is_nan() || scale < MIN_RASTER_SCALE {
        return None;
    }
    // The raster starts on a whole pixel so the scale stays exact.
    let x = (left * scale).floor();
    let y = (top * scale).floor();
    let pixel_width = ((left + width) * scale).ceil() - x;
    let pixel_height = ((top + height) * scale).ceil() - y;
    let (pixel_width, pixel_height) = (pixel_width as u32, pixel_height as u32);
    let area = u64::from(pixel_width) * u64::from(pixel_height);
    if area as f32 > pixels {
        return None;
    }
    let mut mask = tiny_skia::Mask::new(pixel_width, pixel_height)?;
    budget.pixels -= area as f32;
    let transform = tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        shadow.offset.0 * scale - x,
        shadow.offset.1 * scale - y,
    );
    mask.fill_path(path, tiny_skia::FillRule::Winding, true, transform);
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

/// The largest scale at which a `width` by `height` rectangle, with its
/// edges rounded out to whole pixels, has at most `pixels` pixels.
///
/// Rounding out adds less than one pixel at each edge, so each side is
/// below `side * scale + 2`. The scale solves
/// `(width * scale + 2) * (height * scale + 2) = pixels`, which bounds
/// the rounded raster even when one side is far smaller than a pixel.
fn fitting_scale(width: f32, height: f32, pixels: f32) -> f32 {
    let spare = pixels - 4.0;
    if spare.is_nan() || spare <= 0.0 {
        return f32::NAN;
    }
    let edges = 2.0 * (width + height);
    // The root of `width * height * s² + edges * s - spare`, written so
    // that it stays exact when `width * height` is tiny.
    2.0 * spare / (edges + (edges * edges + 4.0 * width * height * spare).sqrt())
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
