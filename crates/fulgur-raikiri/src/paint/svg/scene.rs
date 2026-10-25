//! Vector scenes of `raikiri-svg` drawn with Krilla.
//!
//! A scene is plain data in CSS pixels: nested groups of paths, shaped text
//! and decoded pictures. Each construct maps to the Krilla call that draws it,
//! so the output keeps vector paths, shadings, clip paths, opacity groups and
//! selectable text instead of pixels.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::ops::Range;
use std::sync::{Arc, Weak};

use fulgur_core::Result;
use krilla::color::rgb;
use krilla::geom::{Path, PathBuilder, Point, Size, Transform};
use krilla::image::Image;
use krilla::mask::{Mask, MaskType};
use krilla::num::NormalizedF32;
use krilla::paint::{
    Fill, FillRule, LineCap, LineJoin, LinearGradient, Paint, RadialGradient, SpreadMethod, Stop,
    Stroke, StrokeDash,
};
use krilla::surface::Surface;
use krilla::text::{Font, GlyphId, KrillaGlyph, Tag};
use raikiri_traits::vector as scene;
use raikiri_traits::{DecodedImage, PaintColor, PaintTransform};
use skrifa::MetadataProvider;

use crate::paint::raster::krilla_image;

/// Upper bound on the heap bytes of the scenes that stay cached.
const MAX_CACHED_SCENE_BYTES: usize = 32 * 1024 * 1024;

/// The tolerance within which two linear transforms count as the same one.
const LINEAR_EPSILON: f32 = 1e-4;

/// Fonts and pictures converted for Krilla, shared by the scenes of one cache
/// so that a face or a picture is converted once there.
#[derive(Default)]
pub(in crate::paint) struct SceneAssets {
    /// The face is kept with its font, or with `None` for a face Krilla cannot
    /// parse: the key is the address of the face's shared bytes, which must
    /// stay unique while the entry exists.
    fonts: HashMap<FontKey, (scene::FontFace, Option<Font>)>,
    /// Converted pictures by the address of their pixels, which the weak
    /// reference keeps unique.
    images: HashMap<usize, Vec<ConvertedImage>>,
}

type FontKey = (usize, u32, Vec<([u8; 4], u32)>);

struct ConvertedImage {
    _pixels: Weak<DecodedImage>,
    smooth: bool,
    image: Image,
}

impl SceneAssets {
    fn font(
        &mut self,
        face: &scene::FontFace,
        variations: &[scene::FontVariation],
    ) -> Option<Font> {
        let coords = instance_coords(face, variations);
        let key = (
            Arc::as_ptr(face.data()).cast::<()>() as usize,
            face.index(),
            coords
                .iter()
                .map(|(tag, value)| (*tag, value.to_bits()))
                .collect(),
        );
        self.fonts
            .entry(key)
            .or_insert_with(|| {
                let coords: Vec<(Tag, f32)> = coords
                    .iter()
                    .map(|(tag, value)| (Tag::new(tag), *value))
                    .collect();
                // An instance with no coordinates is the font's default; a
                // face Krilla cannot parse is skipped rather than failing
                // the page.
                let font = Font::new_variable(face.data().clone().into(), face.index(), &coords);
                (face.clone(), font)
            })
            .1
            .clone()
    }

    fn image(&mut self, picture: &scene::RasterImage) -> Result<Image> {
        let key = Arc::as_ptr(&picture.image) as usize;
        let converted = self.images.entry(key).or_default();
        if let Some(known) = converted
            .iter()
            .find(|known| known.smooth == picture.smooth)
        {
            return Ok(known.image.clone());
        }
        let image = krilla_image(&picture.image, picture.smooth)?;
        converted.push(ConvertedImage {
            _pixels: Arc::downgrade(&picture.image),
            smooth: picture.smooth,
            image: image.clone(),
        });
        Ok(image)
    }
}

/// The variation coordinates that make `face` differ from its default
/// instance. The parser lists every axis of the text's style, such as the
/// weight of a font that has none; leaving those out lets the text share the
/// embedded font of the page's own text instead of embedding an equal copy.
fn instance_coords(
    face: &scene::FontFace,
    variations: &[scene::FontVariation],
) -> Vec<([u8; 4], f32)> {
    if variations.is_empty() {
        return Vec::new();
    }
    let Ok(font) = skrifa::FontRef::from_index(face.bytes(), face.index()) else {
        return Vec::new();
    };
    let axes = font.axes();
    variations
        .iter()
        .filter(|variation| {
            axes.iter().any(|axis| {
                axis.tag().to_be_bytes() == variation.tag && axis.default_value() != variation.value
            })
        })
        .map(|variation| (variation.tag, variation.value))
        .collect()
}

/// Draws `scene` into the current space of `surface`: `(0, 0)` is the top
/// left corner of the scene's viewport box and one unit is one CSS pixel.
pub(in crate::paint) fn paint(
    surface: &mut Surface<'_>,
    scene: &scene::VectorImage,
    assets: &mut SceneAssets,
) -> Result<()> {
    group(surface, &scene.root, assets)
}

/// Whether [`paint`] can draw `scene` without distorting it. Krilla strokes
/// text in the space the glyph is drawn in, so a glyph whose transform is not
/// a rotation, a reflection and a uniform scale cannot be stroked there; the
/// caller draws such an image another way.
pub(in crate::paint) fn paintable(scene: &scene::VectorImage) -> bool {
    group_paintable(&scene.root)
}

fn group_paintable(group: &scene::Group) -> bool {
    group.children.iter().all(|node| match node {
        scene::Node::Group(group) => group_paintable(group),
        scene::Node::Text(run) => {
            run.stroke.is_none()
                || run
                    .glyphs
                    .iter()
                    .all(|glyph| is_similarity(glyph.transform))
        }
        _ => true,
    })
}

/// Whether `transform` keeps shapes similar: angles stay, and so does the
/// ratio between lengths in every direction.
fn is_similarity(transform: PaintTransform) -> bool {
    let PaintTransform { a, b, c, d, .. } = transform;
    let (x, y) = (a * a + b * b, c * c + d * d);
    (x - y).abs() <= LINEAR_EPSILON * x.max(y) && (a * c + b * d).abs() <= LINEAR_EPSILON * x
}

fn group(surface: &mut Surface<'_>, group: &scene::Group, assets: &mut SceneAssets) -> Result<()> {
    if group.opacity <= 0.0 || group.clip.as_deref().is_some_and(clip_covers_nothing) {
        return Ok(());
    }
    let mut pushed = push_frame(surface, group);
    if group.opacity < 1.0 {
        surface.push_opacity(unit(group.opacity));
        pushed += 1;
    }
    let result = group
        .children
        .iter()
        .try_for_each(|child| node(surface, child, assets));
    for _ in 0..pushed {
        surface.pop();
    }
    result
}

fn node(surface: &mut Surface<'_>, node: &scene::Node, assets: &mut SceneAssets) -> Result<()> {
    match node {
        scene::Node::Group(inner) => group(surface, inner, assets)?,
        scene::Node::Path(shape) => path(surface, shape),
        scene::Node::Text(run) => text(surface, run, assets),
        scene::Node::Image(picture) => {
            let image = assets.image(picture)?;
            let (width, height) = (picture.image.width as f32, picture.image.height as f32);
            if let Some(size) = Size::from_wh(width, height) {
                surface.draw_image(image, size);
            }
        }
        _ => {}
    }
    Ok(())
}

fn path(surface: &mut Surface<'_>, shape: &scene::Path) {
    let Some(outline) = outline(&shape.segments, PaintTransform::IDENTITY) else {
        return;
    };
    let fill = shape
        .fill
        .as_ref()
        .map(|fill| krilla_fill(fill, PaintTransform::IDENTITY));
    let stroke = shape
        .stroke
        .as_ref()
        .and_then(|stroke| krilla_stroke(stroke, PaintTransform::IDENTITY, 1.0));
    match (fill, stroke) {
        (Some(fill), Some(stroke)) if shape.paint_order == scene::PaintOrder::StrokeThenFill => {
            surface.set_stroke(Some(stroke));
            surface.draw_path(&outline);
            surface.set_stroke(None);
            surface.set_fill(Some(fill));
            surface.draw_path(&outline);
            surface.set_fill(None);
        }
        (None, None) => {}
        (fill, stroke) => {
            surface.set_fill(fill);
            surface.set_stroke(stroke);
            surface.draw_path(&outline);
            surface.set_fill(None);
            surface.set_stroke(None);
        }
    }
}

/// Pushes the transform and the clip of `group` and returns how many pushes
/// were made.
fn push_frame(surface: &mut Surface<'_>, group: &scene::Group) -> usize {
    let mut pushed = 0;
    if group.transform != PaintTransform::IDENTITY {
        surface.push_transform(&transform(group.transform));
        pushed += 1;
    }
    if let Some(clip) = group.clip.as_deref() {
        pushed += push_clip(surface, clip);
    }
    pushed
}

/// A Krilla path along `segments` with `to_space` applied to every point.
fn outline(segments: &[scene::PathSegment], to_space: PaintTransform) -> Option<Path> {
    let at = |point: (f32, f32)| to_space.apply(point);
    let mut builder = PathBuilder::new();
    for segment in segments {
        match *segment {
            scene::PathSegment::MoveTo(point) => {
                let (x, y) = at(point);
                builder.move_to(x, y);
            }
            scene::PathSegment::LineTo(point) => {
                let (x, y) = at(point);
                builder.line_to(x, y);
            }
            scene::PathSegment::QuadTo { control, to } => {
                let ((x1, y1), (x, y)) = (at(control), at(to));
                builder.quad_to(x1, y1, x, y);
            }
            scene::PathSegment::CubicTo {
                control1,
                control2,
                to,
            } => {
                let ((x1, y1), (x2, y2), (x, y)) = (at(control1), at(control2), at(to));
                builder.cubic_to(x1, y1, x2, y2, x, y);
            }
            scene::PathSegment::Close => builder.close(),
            _ => return None,
        }
    }
    builder.finish()
}

// Clips

/// Whether nothing can be drawn through `clip`: a link of its chain has no
/// shape that makes a path.
fn clip_covers_nothing(clip: &scene::ClipPath) -> bool {
    let mut link = Some(clip);
    while let Some(current) = link {
        if !has_shape(&current.root) {
            return true;
        }
        link = current.clip.as_deref();
    }
    false
}

fn has_shape(group: &scene::Group) -> bool {
    group.children.iter().any(|node| match node {
        scene::Node::Path(shape) => outline(&shape.segments, PaintTransform::IDENTITY).is_some(),
        scene::Node::Group(inner) => has_shape(inner),
        _ => false,
    })
}

/// Intersects the drawing area with every link of `clip`'s chain and returns
/// how many pushes were made. Each link's area is a union of shapes: a single
/// shape is a clip path, several are a mask.
fn push_clip(surface: &mut Surface<'_>, clip: &scene::ClipPath) -> usize {
    let mut pushed = 0;
    let mut link = Some(clip);
    while let Some(current) = link {
        let single = single_shape(&current.root, current.transform)
            .and_then(|(to_space, shape)| Some((outline(&shape.segments, to_space)?, shape)));
        if let Some((area, shape)) = single {
            let rule = fill_rule(shape.fill.as_ref().map(|fill| fill.rule));
            surface.push_clip_path(&area, &rule);
        } else {
            push_mask(surface, current);
        }
        pushed += 1;
        link = current.clip.as_deref();
    }
    pushed
}

/// The only shape below `group` and the transform from its space to the
/// space `to_space` maps to, when there is exactly one and nothing else
/// limits or hides it.
fn single_shape(
    group: &scene::Group,
    to_space: PaintTransform,
) -> Option<(PaintTransform, &scene::Path)> {
    if group.clip.is_some() {
        return None;
    }
    let to_space = to_space * group.transform;
    match group.children.as_slice() {
        [scene::Node::Path(shape)] => Some((to_space, shape)),
        [scene::Node::Group(inner)] => single_shape(inner, to_space),
        _ => None,
    }
}

fn push_mask(surface: &mut Surface<'_>, clip: &scene::ClipPath) {
    let mut builder = surface.stream_builder();
    {
        let mut inner = builder.surface();
        let moved = clip.transform != PaintTransform::IDENTITY;
        if moved {
            inner.push_transform(&transform(clip.transform));
        }
        clip_group(&mut inner, &clip.root);
        if moved {
            inner.pop();
        }
        inner.finish();
    }
    let stream = builder.finish();
    surface.push_mask(Mask::new(stream, MaskType::Alpha));
}

/// Draws the area of the shapes below `group` in black, opaque and without
/// strokes, into the mask of a clip. Opacity, text and pictures do not
/// change an area, so they do not count.
fn clip_group(surface: &mut Surface<'_>, group: &scene::Group) {
    if group.clip.as_deref().is_some_and(clip_covers_nothing) {
        return;
    }
    let pushed = push_frame(surface, group);
    for child in &group.children {
        match child {
            scene::Node::Group(inner) => clip_group(surface, inner),
            scene::Node::Path(shape) => clip_path(surface, shape),
            _ => {}
        }
    }
    for _ in 0..pushed {
        surface.pop();
    }
}

fn clip_path(surface: &mut Surface<'_>, shape: &scene::Path) {
    let Some(outline) = outline(&shape.segments, PaintTransform::IDENTITY) else {
        return;
    };
    surface.set_fill(Some(Fill {
        paint: rgb::Color::black().into(),
        opacity: NormalizedF32::ONE,
        rule: fill_rule(shape.fill.as_ref().map(|fill| fill.rule)),
    }));
    surface.draw_path(&outline);
    surface.set_fill(None);
}

// Text

fn text(surface: &mut Surface<'_>, run: &scene::TextRun, assets: &mut SceneAssets) {
    if run.glyphs.is_empty()
        || run.font_size.is_nan()
        || run.font_size <= 0.0
        || (run.fill.is_none() && run.stroke.is_none())
    {
        return;
    }
    let mut start = 0;
    while start < run.glyphs.len() {
        let first = &run.glyphs[start];
        let Some(font) = assets.font(&first.font, &run.variations) else {
            start += 1;
            continue;
        };
        // Krilla sizes the glyph itself: the scene's transform followed by a
        // scale of units per em over the font size places it, with the
        // size-dependent part of the transform left to Krilla.
        let scale = font.units_per_em() / run.font_size;
        let linear = linear_part(first.transform, scale);
        let mut end = start + 1;
        while end < run.glyphs.len()
            && run.glyphs[end].font.is_same_face(&first.font)
            && same_linear(linear_part(run.glyphs[end].transform, scale), linear)
        {
            end += 1;
        }
        text_batch(surface, run, &run.glyphs[start..end], &font, linear);
        start = end;
    }
}

/// The `(a, b, c, d)` of a glyph's transform followed by the scale from the
/// font size to Krilla's design units.
fn linear_part(transform: PaintTransform, scale: f32) -> [f32; 4] {
    [transform.a, transform.b, transform.c, transform.d].map(|value| value * scale)
}

fn same_linear(left: [f32; 4], right: [f32; 4]) -> bool {
    left.iter()
        .zip(right)
        .all(|(left, right)| (left - right).abs() <= LINEAR_EPSILON * (1.0 + right.abs()))
}

/// Draws glyphs of one face that share their linear transform as one text
/// run in the space of that transform, where they sit on a straight line.
fn text_batch(
    surface: &mut Surface<'_>,
    run: &scene::TextRun,
    glyphs: &[scene::Glyph],
    font: &Font,
    [a, b, c, d]: [f32; 4],
) {
    let linear = PaintTransform::new(a, b, c, d, 0.0, 0.0);
    // A transform that cannot be inverted collapses the glyphs to a line.
    let Some(inverse) = invert(linear) else {
        return;
    };
    let size = run.font_size;
    let origins: Vec<(f32, f32)> = glyphs
        .iter()
        .map(|glyph| inverse.apply((glyph.transform.e, glyph.transform.f)))
        .collect();
    let mut text = String::new();
    let mut previous: Option<Range<usize>> = None;
    let mut positioned = Vec::with_capacity(glyphs.len());
    for (index, glyph) in glyphs.iter().enumerate() {
        // A glyph that spells nothing belongs to the cluster before it.
        let range = if glyph.text.is_empty() {
            previous.clone().unwrap_or(text.len()..text.len())
        } else {
            let start = text.len();
            text.push_str(&glyph.text);
            start..text.len()
        };
        previous = Some(range.clone());
        let next = origins.get(index + 1).unwrap_or(&origins[index]);
        positioned.push(KrillaGlyph {
            glyph_id: GlyphId::new(glyph.id),
            text_range: range,
            x_advance: (next.0 - origins[index].0) / size,
            x_offset: 0.0,
            // Krilla subtracts the offset: it grows upwards.
            y_offset: -(origins[index].1 - origins[0].1) / size,
            y_advance: 0.0,
            location: None,
        });
    }
    // Paints and widths are in the space of the run, and Krilla draws in the
    // space of the glyphs.
    let fill = run.fill.as_ref().map(|fill| krilla_fill(fill, inverse));
    let width_scale = 1.0 / (a * d - b * c).abs().sqrt();
    let stroke = run
        .stroke
        .as_ref()
        .and_then(|stroke| krilla_stroke(stroke, inverse, width_scale));
    let moved = linear != PaintTransform::IDENTITY;
    if moved {
        surface.push_transform(&transform(linear));
    }
    let start = Point::from_xy(origins[0].0, origins[0].1);
    match (fill, stroke) {
        (Some(fill), Some(stroke)) if run.paint_order == scene::PaintOrder::StrokeThenFill => {
            // Only the fill is text, so the characters are not there twice.
            surface.set_stroke(Some(stroke));
            surface.draw_glyphs(start, &positioned, font.clone(), &text, size, true);
            surface.set_stroke(None);
            surface.set_fill(Some(fill));
            surface.draw_glyphs(start, &positioned, font.clone(), &text, size, false);
            surface.set_fill(None);
        }
        (fill, stroke) => {
            surface.set_fill(fill);
            surface.set_stroke(stroke);
            surface.draw_glyphs(start, &positioned, font.clone(), &text, size, false);
            surface.set_fill(None);
            surface.set_stroke(None);
        }
    }
    if moved {
        surface.pop();
    }
}

fn invert(transform: PaintTransform) -> Option<PaintTransform> {
    let PaintTransform {
        a, b, c, d, e, f, ..
    } = transform;
    let det = a * d - b * c;
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let (a, b, c, d) = (d / det, -b / det, -c / det, a / det);
    Some(PaintTransform::new(
        a,
        b,
        c,
        d,
        -(a * e + c * f),
        -(b * e + d * f),
    ))
}

// Paints

fn transform(value: PaintTransform) -> Transform {
    Transform::from_row(value.a, value.b, value.c, value.d, value.e, value.f)
}

fn unit(value: f32) -> NormalizedF32 {
    NormalizedF32::new(value.clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE)
}

fn byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn color(value: PaintColor) -> rgb::Color {
    rgb::Color::new(byte(value.red), byte(value.green), byte(value.blue))
}

fn fill_rule(rule: Option<scene::FillRule>) -> FillRule {
    match rule {
        Some(scene::FillRule::EvenOdd) => FillRule::EvenOdd,
        _ => FillRule::NonZero,
    }
}

fn stops(stops: &[scene::GradientStop]) -> Vec<Stop> {
    stops
        .iter()
        .map(|stop| Stop {
            offset: unit(stop.offset),
            color: color(stop.color).into(),
            opacity: unit(stop.color.alpha),
        })
        .collect()
}

fn spread(method: scene::SpreadMethod) -> SpreadMethod {
    match method {
        scene::SpreadMethod::Reflect => SpreadMethod::Reflect,
        scene::SpreadMethod::Repeat => SpreadMethod::Repeat,
        _ => SpreadMethod::Pad,
    }
}

/// The Krilla paint for `paint` and the opacity its own alpha adds. `to_space`
/// maps the space the paint is given in to the space it is used in.
fn krilla_paint(paint: &scene::Paint, to_space: PaintTransform) -> (Paint, f32) {
    match paint {
        scene::Paint::Color(value) => (color(*value).into(), value.alpha),
        scene::Paint::LinearGradient(gradient) => (
            LinearGradient {
                x1: gradient.start.0,
                y1: gradient.start.1,
                x2: gradient.end.0,
                y2: gradient.end.1,
                transform: transform(to_space * gradient.transform),
                spread_method: spread(gradient.spread),
                stops: stops(&gradient.stops),
                anti_alias: false,
            }
            .into(),
            1.0,
        ),
        scene::Paint::RadialGradient(gradient) => (
            RadialGradient {
                fx: gradient.focal.0,
                fy: gradient.focal.1,
                fr: gradient.focal_radius,
                cx: gradient.center.0,
                cy: gradient.center.1,
                cr: gradient.radius,
                transform: transform(to_space * gradient.transform),
                spread_method: spread(gradient.spread),
                stops: stops(&gradient.stops),
                anti_alias: false,
            }
            .into(),
            1.0,
        ),
        _ => (rgb::Color::black().into(), 0.0),
    }
}

fn krilla_fill(fill: &scene::Fill, to_space: PaintTransform) -> Fill {
    let (paint, alpha) = krilla_paint(&fill.paint, to_space);
    Fill {
        paint,
        opacity: unit(fill.opacity * alpha),
        rule: fill_rule(Some(fill.rule)),
    }
}

/// A Krilla stroke for `stroke`. Lengths of the stroke are multiplied by
/// `length_scale`, for a stroke drawn in a space that scales the path's own.
/// `None` when the stroke has no width to draw.
fn krilla_stroke(
    stroke: &scene::Stroke,
    to_space: PaintTransform,
    length_scale: f32,
) -> Option<Stroke> {
    let width = stroke.width * length_scale;
    if width.is_nan() || width <= 0.0 {
        return None;
    }
    let (paint, alpha) = krilla_paint(&stroke.paint, to_space);
    Some(Stroke {
        paint,
        width,
        miter_limit: stroke.miter_limit,
        line_cap: match stroke.cap {
            scene::LineCap::Round => LineCap::Round,
            scene::LineCap::Square => LineCap::Square,
            _ => LineCap::Butt,
        },
        line_join: match stroke.join {
            scene::LineJoin::Round => LineJoin::Round,
            scene::LineJoin::Bevel => LineJoin::Bevel,
            // PDF has no clipped miter.
            _ => LineJoin::Miter,
        },
        opacity: unit(stroke.opacity * alpha),
        dash: stroke.dash.as_ref().map(|dash| StrokeDash {
            array: dash.array.iter().map(|len| len * length_scale).collect(),
            offset: dash.offset * length_scale,
        }),
    })
}

// Caching

/// The scenes of one source, by the host opacity removed from the root (its
/// bits). `None` stands for a source that is drawn another way.
type Variants = Vec<(Option<u32>, Option<Arc<scene::VectorImage>>)>;

/// Scenes by prepared source, then by the host opacity removed from the root
/// (its bits). A scene is kept only when its pair is built again, so one-off
/// SVGs never pile up, and the kept scenes stay under a heap budget. A source
/// that is drawn another way is kept like a scene, so one repeated on every
/// page is not built again just to fail again.
#[derive(Default)]
pub(super) struct SceneMemo {
    scenes: HashMap<String, Variants>,
    seen: HashSet<u64>,
    hasher: RandomState,
    kept_bytes: usize,
}

impl SceneMemo {
    /// The scene of `source` for `key`, built by `build` when it is not kept.
    /// `build` gives `None` for a source that has to be drawn another way.
    pub(super) fn get_or_build(
        &mut self,
        source: &str,
        key: Option<u32>,
        build: impl FnOnce() -> Option<scene::VectorImage>,
    ) -> Option<Arc<scene::VectorImage>> {
        if let Some(scene) = self
            .scenes
            .get(source)
            .and_then(|scenes| scenes.iter().find(|(known, _)| *known == key))
        {
            return scene.1.clone();
        }
        let scene = build().map(Arc::new);
        let repeated = !self.seen.insert(self.hasher.hash_one((source, key)));
        let cost = source.len()
            + scene
                .as_ref()
                .map_or(0, |scene| scene.estimated_heap_bytes());
        if repeated && self.kept_bytes + cost <= MAX_CACHED_SCENE_BYTES {
            self.kept_bytes += cost;
            self.scenes
                .entry(source.to_owned())
                .or_default()
                .push((key, scene.clone()));
        }
        scene
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.scenes.values().map(Vec::len).sum()
    }
}

#[cfg(test)]
mod tests;
