//! Draw a Raikiri `DocumentLayout` into a PDF with Krilla.
//!
//! The painter reads Raikiri's pages directly: page geometry, the fragments
//! placed on each page, and the computed style of their nodes. Raikiri works
//! in CSS px with the origin at the top-left of the page box and y growing
//! downward, which is also Krilla's orientation, so each page surface is
//! scaled by [`PX_TO_PT`] once and everything below draws in px.

mod border;
mod clip;
mod decoration;
mod gradient;
mod navigation;
mod order;
mod raster;
mod shape;
mod svg;
mod text_clip;

use clip::{ClipMap, ClipStack};
use fulgur_core::units::PX_TO_PT;
use fulgur_core::{Error, Result};
use krilla::color::rgb;
use krilla::geom::{Path, Transform};
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::{Fill, FillRule};
use krilla::surface::Surface;
use raikiri_html::computed::{
    ComputedBackgroundImage, ComputedLengthPercentage, ComputedValues, ComputedVisibility,
    ComputedVisualBox, CssColor,
};
use raikiri_html::{
    ClipKind, DocumentLayout, FontId, FragmentKind, Page, PaintEvent, PaintRect,
    PositionedGlyphRun, RunSource,
};
use shape::{Edges, RoundedRect, Slice};
use std::collections::HashMap;

/// Draw every page of `document` and return the PDF bytes.
pub(crate) fn paint_document(
    document: &DocumentLayout,
    resources: &raikiri_html::RenderResources<'_>,
    config: &fulgur_core::Config,
    outline: Option<krilla::outline::Outline>,
    document_url: &url::Url,
    options: &crate::RenderOptions<'_>,
) -> Result<Vec<u8>> {
    let mut pdf = krilla::Document::new();
    pdf.set_metadata(crate::metadata::build(config)?);
    if let Some(outline) = outline {
        pdf.set_outline(outline);
    }
    let mut fonts = FontCache::default();
    let mut svg = svg::SvgCache::new(*options);
    let mut raster = raster::RasterCache::new(resources.image_pixel_source_ref());
    for page in document.pages() {
        paint_page(
            &mut pdf,
            document,
            &page,
            &mut fonts,
            &mut svg,
            &mut raster,
            document_url,
        )?;
    }
    pdf.finish()
        .map_err(|error| Error::PdfGeneration(format!("{error:?}")))
}

fn paint_page(
    pdf: &mut krilla::Document,
    document: &DocumentLayout,
    page: &Page<'_>,
    fonts: &mut FontCache,
    svg: &mut svg::SvgCache<'_>,
    raster: &mut raster::RasterCache<'_>,
    document_url: &url::Url,
) -> Result<()> {
    let page_box = page.geometry().page_box;
    let settings = PageSettings::from_wh(page_box.width * PX_TO_PT, page_box.height * PX_TO_PT)
        .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;
    let mut pdf_page = pdf.start_page_with(settings);
    let mut surface = pdf_page.surface();
    surface.push_transform(&Transform::from_scale(PX_TO_PT, PX_TO_PT));

    let runs = page.text_runs();
    let events = page.paint_order_for_text_runs(&runs);
    if order::supported(&events, &runs) {
        paint_ordered(&mut surface, page, &events, &runs, fonts, svg, raster)?;
    } else {
        paint_legacy(&mut surface, page, &runs, fonts, svg, raster)?;
    }

    surface.pop();
    surface.finish();
    for annotation in navigation::annotations(document, page, document_url)? {
        pdf_page.add_annotation(annotation);
    }
    pdf_page.finish();
    Ok(())
}

fn paint_legacy(
    surface: &mut Surface<'_>,
    page: &Page<'_>,
    runs: &[PositionedGlyphRun<'_>],
    fonts: &mut FontCache,
    svg: &mut svg::SvgCache<'_>,
    raster: &mut raster::RasterCache<'_>,
) -> Result<()> {
    let clips = ClipMap::new(page);
    let mut active = ClipStack::default();
    let dom = page.dom();

    // Keep the existing whole-page rendering when text events cannot map
    // every text run to exactly one draw step.
    let mut boxes: Vec<_> = page
        .fragments()
        .filter(|fragment| fragment.kind() == FragmentKind::Box)
        .collect();
    boxes.sort_by_key(|fragment| (fragment.node(), fragment.fragment_index()));
    for fragment in boxes {
        if let Some(style) = page.computed(fragment.node()) {
            // A box's own overflow clips its content, not its decorations.
            let chain = clips.chain(page, dom.parent(fragment.node()), fragment.paint_rect());
            active.apply(surface, &clips, &chain);
            let text = || text_clip::outlines(dom, fragment.node(), runs);
            paint_box(
                surface,
                fragment.paint_rect(),
                Slice::of(&fragment),
                style,
                text,
            );
        }
    }
    for fragment in page.fragments() {
        let chain = clips.chain(page, Some(fragment.node()), fragment.paint_rect());
        active.apply(surface, &clips, &chain);
        // This painter has no opacity groups, so the SVG keeps its root opacity.
        svg.paint(surface, page, &fragment, false)?;
        raster.paint(surface, page, &fragment, false)?;
    }
    // Text goes above every block background and border.
    let text: Vec<_> = runs.iter().collect();
    paint_text_batch(surface, page, &clips, &mut active, &text, fonts);
    active.clear(surface);
    Ok(())
}

fn paint_ordered(
    surface: &mut Surface<'_>,
    page: &Page<'_>,
    events: &[PaintEvent<'_>],
    runs: &[PositionedGlyphRun<'_>],
    fonts: &mut FontCache,
    svg: &mut svg::SvgCache<'_>,
    raster: &mut raster::RasterCache<'_>,
) -> Result<()> {
    let clips = ClipMap::new(page);
    let mut active = ClipStack::default();
    let dom = page.dom();
    let mut by_node: HashMap<_, Vec<_>> = HashMap::new();
    let mut by_line: HashMap<_, Vec<_>> = HashMap::new();
    for run in runs {
        by_line.entry(run.line).or_default().push(run);
        if let RunSource::Text(node) = run.source {
            by_node.entry(node).or_default().push(run);
        }
    }
    let mut batch = Vec::new();
    let mut event_clips = Vec::new();
    for event in events {
        if let PaintEvent::Text(fragment) = event {
            batch.extend(by_node.get(&fragment.node()).into_iter().flatten().copied());
            continue;
        }
        if let PaintEvent::TextLine(line) = event {
            batch.extend(by_line.get(line).into_iter().flatten().copied());
            continue;
        }
        // Clip, opacity, box and stacking steps delimit a text paint batch.
        paint_text_batch(surface, page, &clips, &mut active, &batch, fonts);
        batch.clear();
        match event {
            PaintEvent::Box(fragment) => {
                if let Some(style) = page.computed(fragment.node()) {
                    let chain =
                        clips.chain(page, dom.parent(fragment.node()), fragment.paint_rect());
                    active.apply(surface, &clips, &chain);
                    let text = || text_clip::outlines(dom, fragment.node(), runs);
                    paint_box(
                        surface,
                        fragment.paint_rect(),
                        Slice::of(fragment),
                        style,
                        text,
                    );
                }
            }
            PaintEvent::GeneratedBox(piece) => {
                let chain = clips.chain(page, Some(piece.clip_owner), piece.rect);
                active.apply(surface, &clips, &chain);
                let text = || text_clip::generated_outlines(piece, runs);
                paint_box(
                    surface,
                    piece.rect,
                    Slice::generated(piece),
                    piece.style,
                    text,
                );
            }
            PaintEvent::Replaced(fragment) => {
                let chain = clips.chain(page, Some(fragment.node()), fragment.paint_rect());
                active.apply(surface, &clips, &chain);
                svg.paint(surface, page, fragment, true)?;
                raster.paint(surface, page, fragment, true)?;
            }
            PaintEvent::MarkerImage(owner) => {
                if let Some(placement) = raster.marker(page, *owner) {
                    let chain = clips.chain(page, placement.clip_owner, placement.rect);
                    active.apply(surface, &clips, &chain);
                    raster.paint_placement(surface, page, &placement, true)?;
                }
            }
            PaintEvent::PushClip(shape, kind) => {
                // Table-part decorations are drawn once per cell intersection.
                // These clips supplement the lazily applied ancestor overflow.
                let path = (*kind == ClipKind::TableCell)
                    .then(|| clip::clip_path(*shape, page.geometry().page_box))
                    .flatten();
                if let Some(path) = &path {
                    active.clear(surface);
                    surface.push_clip_path(path, &FillRule::NonZero);
                }
                event_clips.push(path.is_some());
            }
            PaintEvent::PopClip => {
                if event_clips.pop() == Some(true) {
                    active.clear(surface);
                    surface.pop();
                }
            }
            PaintEvent::PushOpacity(alpha) => {
                // Clips and opacity groups share the surface stack. Close clips
                // at each group boundary and reapply them on the next draw.
                active.clear(surface);
                if let Some(alpha) = NormalizedF32::new(*alpha) {
                    surface.push_opacity(alpha);
                }
            }
            PaintEvent::PopOpacity => {
                active.clear(surface);
                surface.pop();
            }
            // Clips are applied lazily before each content draw.
            // Other events have already been rejected by `order::supported`.
            _ => {}
        }
    }
    paint_text_batch(surface, page, &clips, &mut active, &batch, fonts);
    active.clear(surface);
    Ok(())
}

/// Keep the line's decorations below or above all neighboring glyph ink.
fn paint_text_batch(
    surface: &mut Surface<'_>,
    page: &Page<'_>,
    clips: &ClipMap,
    active: &mut ClipStack,
    runs: &[&PositionedGlyphRun<'_>],
    fonts: &mut FontCache,
) {
    let dom = page.dom();
    let mut indices = HashMap::new();
    let mut lines: Vec<Vec<&PositionedGlyphRun<'_>>> = Vec::new();
    for &run in runs {
        let index = *indices.entry(run.line).or_insert_with(|| {
            lines.push(Vec::new());
            lines.len() - 1
        });
        lines[index].push(run);
    }
    // Reunite color and font slices of each line before painting its three
    // phases, without reordering independent lines that overlap on the page.
    for runs in lines {
        for phase in [
            Some(decoration::Phase::BeforeGlyphs),
            None,
            Some(decoration::Phase::AfterGlyphs),
        ] {
            for run in &runs {
                let element = text_clip::run_element(dom, run);
                // Standalone markers precede the item's own overflow clip.
                let clip_owner = if run.is_standalone_marker() {
                    element.and_then(|node| dom.parent(node))
                } else {
                    element
                };
                let area = PaintRect::new(
                    run.origin.0,
                    run.origin.1 - run.ascent,
                    run.advance,
                    run.ascent + run.descent,
                );
                let chain = clips.chain(page, clip_owner, area);
                active.apply(surface, clips, &chain);
                if let Some(phase) = phase {
                    decoration::paint(surface, &run.decorations, phase);
                } else {
                    paint_glyph_run(surface, run, fonts);
                }
            }
        }
    }
}

/// Background and borders of one box fragment (`rect` is its border box).
///
/// The fragment is drawn as its own box, apart from its broken edges: the
/// corner radii (percentages and the §5.5 scaling) and the background
/// positioning area refer to the fragment, because the fragment API does
/// not give the size of the unbroken box or the fragment's offset in it.
/// `text` gives the outlines of the element's text, for
/// `background-clip: text`.
fn paint_box(
    surface: &mut Surface<'_>,
    rect: PaintRect,
    slice: Slice,
    style: &ComputedValues,
    text: impl FnOnce() -> Option<Path>,
) {
    if !matches!(style.visibility, ComputedVisibility::Visible) {
        return;
    }
    let border_box = RoundedRect::border_box(rect, &style.border_radius).sliced(slice);
    paint_background(surface, &border_box, slice, style, text);
    border::paint_borders(surface, &border_box, style, slice);
}

/// CSS Backgrounds 3 §2: the background color, then the background image
/// over it, both clipped to the `background-clip` box with its corner
/// curves (§5.3). CSS Backgrounds 4 §2.6 adds two painting areas:
/// `border-area` (the area under the border) and `text` (the glyphs of the
/// element's text).
fn paint_background(
    surface: &mut Surface<'_>,
    border_box: &RoundedRect,
    slice: Slice,
    style: &ComputedValues,
    text: impl FnOnce() -> Option<Path>,
) {
    let borders = slice.edges(border::widths(style));
    let padding = slice.edges(padding(style));
    let visual_box = |visual: ComputedVisualBox| match visual {
        ComputedVisualBox::PaddingBox => border_box.inset(borders),
        ComputedVisualBox::ContentBox => border_box.inset(borders.add(padding)),
        // `border-box`, and the boxes this painter does not distinguish.
        _ => *border_box,
    };
    let painting_area = match style.background_clip {
        ComputedVisualBox::BorderArea => shape::ring(border_box, &border_box.inset(borders))
            .map(|path| (path, FillRule::EvenOdd)),
        ComputedVisualBox::Text => text().map(|path| (path, FillRule::NonZero)),
        visual => visual_box(visual)
            .path()
            .map(|path| (path, FillRule::NonZero)),
    };
    let Some((area, rule)) = painting_area else {
        return;
    };
    if style.background_color.a > 0 {
        surface.set_fill(Some(Fill {
            rule,
            ..fill(style.background_color)
        }));
        surface.draw_path(&area);
    }
    // One layer with the initial size, position and repeat: the gradient
    // covers the positioning area and its end colors extend to the rest of
    // the painting area. `url()` images are not drawn: the layout result
    // carries no decoded image data.
    if let ComputedBackgroundImage::Gradient(gradient) = &style.background_image {
        let positioning_area = visual_box(style.background_origin);
        if let Some(paint) = gradient::paint(gradient, &positioning_area, style.color) {
            surface.set_fill(Some(Fill {
                paint,
                opacity: NormalizedF32::ONE,
                rule,
            }));
            surface.draw_path(&area);
        }
    }
}

/// Used padding widths. A percentage refers to the containing block's
/// width, which the fragment does not carry, so it counts as zero.
fn padding(style: &ComputedValues) -> Edges {
    let px = |value: ComputedLengthPercentage| match value {
        ComputedLengthPercentage::Px(px) => px.max(0.0),
        ComputedLengthPercentage::Percent(_) => 0.0,
    };
    Edges {
        top: px(style.padding.top),
        right: px(style.padding.right),
        bottom: px(style.padding.bottom),
        left: px(style.padding.left),
    }
}

pub(crate) fn fill(color: CssColor) -> Fill {
    Fill {
        paint: rgb::Color::new(color.r, color.g, color.b).into(),
        opacity: NormalizedF32::new(f32::from(color.a) / 255.0).unwrap_or(NormalizedF32::ONE),
        rule: Default::default(),
    }
}

/// Krilla fonts by Raikiri face and variation coordinates. `Font::new`
/// parses the font, so each face is created once per document and every run
/// that uses it shares the same PDF font object.
#[derive(Default)]
struct FontCache {
    fonts: HashMap<FontKey, Option<krilla::text::Font>>,
}

/// A face plus its variation coordinates (axis tag, value bits).
type FontKey = (FontId, Vec<([u8; 4], u32)>);

impl FontCache {
    fn font(&mut self, run: &PositionedGlyphRun<'_>) -> Option<krilla::text::Font> {
        let variations: Vec<([u8; 4], u32)> = run
            .variations
            .iter()
            .map(|variation| (variation.tag.0, variation.value.to_bits()))
            .collect();
        self.fonts
            .entry((run.font.id, variations))
            .or_insert_with(|| {
                let data: krilla::Data = run.font.data.to_arc().into();
                let coords: Vec<(krilla::text::Tag, f32)> = run
                    .variations
                    .iter()
                    .map(|variation| (krilla::text::Tag::new(&variation.tag.0), variation.value))
                    .collect();
                // An instance with no coordinates is the font's default.
                krilla::text::Font::new_variable(data, run.font.index, &coords)
            })
            .clone()
    }
}

/// Draw one positioned glyph run.
///
/// Raikiri reports glyph advances and offsets in px with y growing downward;
/// Krilla takes them per unit of font size and subtracts `y_offset`, so both
/// are divided by the font size and `y_offset` changes sign.
fn paint_glyph_run(surface: &mut Surface<'_>, run: &PositionedGlyphRun<'_>, fonts: &mut FontCache) {
    if run.glyphs.is_empty() || run.font_size <= 0.0 || run.color.a == 0 {
        return;
    }
    // A face Krilla cannot parse is skipped rather than failing the page.
    let Some(font) = fonts.font(run) else { return };
    let size = run.font_size;
    let glyphs: Vec<krilla::text::KrillaGlyph> = run
        .glyphs
        .iter()
        .map(|glyph| krilla::text::KrillaGlyph {
            glyph_id: krilla::text::GlyphId::new(glyph.id),
            text_range: glyph.text_range.clone(),
            x_advance: glyph.advance / size,
            x_offset: glyph.x_offset / size,
            y_offset: -glyph.y_offset / size,
            y_advance: 0.0,
            location: None,
        })
        .collect();
    surface.set_fill(Some(fill(run.color)));
    surface.draw_glyphs(
        krilla::geom::Point::from_xy(run.origin.0, run.origin.1),
        &glyphs,
        font,
        run.text,
        size,
        false,
    );
}

#[cfg(test)]
mod tests;
