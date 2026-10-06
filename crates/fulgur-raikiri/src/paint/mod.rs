//! Draw a Raikiri `DocumentLayout` into a PDF with Krilla.
//!
//! The painter reads Raikiri's pages directly: page geometry, the fragments
//! placed on each page, and the computed style of their nodes. Raikiri works
//! in CSS px with the origin at the top-left of the page box and y growing
//! downward, which is also Krilla's orientation, so each page surface is
//! scaled by [`PX_TO_PT`] once and everything below draws in px.

mod border;
mod clip;
mod gradient;
mod navigation;
mod shape;
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
use raikiri_html::{DocumentLayout, FontId, FragmentKind, Page, PaintRect, PositionedGlyphRun};
use shape::{Edges, RoundedRect, Slice};
use std::collections::HashMap;

/// Draw every page of `document` and return the PDF bytes.
pub(crate) fn paint_document(
    document: &DocumentLayout,
    config: &fulgur_core::Config,
    outline: Option<krilla::outline::Outline>,
) -> Result<Vec<u8>> {
    let mut pdf = krilla::Document::new();
    pdf.set_metadata(crate::metadata::build(config)?);
    if let Some(outline) = outline {
        pdf.set_outline(outline);
    }
    let mut fonts = FontCache::default();
    for page in document.pages() {
        paint_page(&mut pdf, document, &page, &mut fonts)?;
    }
    pdf.finish()
        .map_err(|error| Error::PdfGeneration(format!("{error:?}")))
}

fn paint_page(
    pdf: &mut krilla::Document,
    document: &DocumentLayout,
    page: &Page<'_>,
    fonts: &mut FontCache,
) -> Result<()> {
    let page_box = page.geometry().page_box;
    let settings = PageSettings::from_wh(page_box.width * PX_TO_PT, page_box.height * PX_TO_PT)
        .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;
    let mut pdf_page = pdf.start_page_with(settings);
    let mut surface = pdf_page.surface();
    surface.push_transform(&Transform::from_scale(PX_TO_PT, PX_TO_PT));

    let clips = ClipMap::new(page);
    let mut active = ClipStack::default();
    let dom = page.dom();

    // CSS 2.1 Appendix E for normal flow: the backgrounds and borders of all
    // block boxes in tree order, then the inline content (text) of all of
    // them, so overflowing text stays above later blocks' backgrounds.
    // Positioned boxes and other stacking contexts paint in a different order
    // that the fragments alone do not describe; they are drawn in this
    // normal-flow order until Raikiri exposes the paint order.
    let mut boxes: Vec<_> = page
        .fragments()
        .filter(|fragment| fragment.kind() == FragmentKind::Box)
        .collect();
    boxes.sort_by_key(|fragment| (fragment.node(), fragment.fragment_index()));
    let runs = page.text_runs();
    for fragment in boxes {
        if let Some(style) = page.computed(fragment.node()) {
            // A box's own overflow clips its content, not its decorations.
            let chain = clips.chain(page, dom.parent(fragment.node()), fragment.paint_rect());
            active.apply(&mut surface, &clips, &chain);
            let text = || text_clip::outlines(dom, fragment.node(), &runs);
            paint_box(
                &mut surface,
                fragment.paint_rect(),
                Slice::of(&fragment),
                style,
                text,
            );
        }
    }
    // Text goes above every block background and border.
    for run in &runs {
        let element = text_clip::run_element(dom, run);
        let area = PaintRect::new(
            run.origin.0,
            run.origin.1 - run.ascent,
            run.advance,
            run.ascent + run.descent,
        );
        let chain = clips.chain(page, element, area);
        active.apply(&mut surface, &clips, &chain);
        paint_text_run(&mut surface, run, fonts);
    }
    active.clear(&mut surface);

    surface.pop();
    surface.finish();
    for annotation in navigation::annotations(document, page)? {
        pdf_page.add_annotation(annotation);
    }
    pdf_page.finish();
    Ok(())
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
fn paint_text_run(surface: &mut Surface<'_>, run: &PositionedGlyphRun<'_>, fonts: &mut FontCache) {
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
