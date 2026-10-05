//! Draw a Raikiri `DocumentLayout` into a PDF with Krilla.
//!
//! The painter reads Raikiri's pages directly: page geometry, the fragments
//! placed on each page, and the computed style of their nodes. Raikiri works
//! in CSS px with the origin at the top-left of the page box and y growing
//! downward, which is also Krilla's orientation, so each page surface is
//! scaled by [`PX_TO_PT`] once and everything below draws in px.

use fulgur_core::units::PX_TO_PT;
use fulgur_core::{Error, Result};
use krilla::color::rgb;
use krilla::geom::{PathBuilder, Rect, Transform};
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::Fill;
use krilla::surface::Surface;
use raikiri_html::computed::{
    BorderColor, BorderStyle, ComputedBorder, ComputedValues, ComputedVisibility, CssColor,
};
use raikiri_html::{DocumentLayout, FontId, FragmentKind, Page, PaintRect, PositionedGlyphRun};
use std::collections::HashMap;

/// Draw every page of `document` and return the PDF bytes.
pub(crate) fn paint_document(document: &DocumentLayout) -> Result<Vec<u8>> {
    let mut pdf = krilla::Document::new();
    let mut fonts = FontCache::default();
    for page in document.pages() {
        paint_page(&mut pdf, &page, &mut fonts)?;
    }
    pdf.finish()
        .map_err(|error| Error::PdfGeneration(format!("{error:?}")))
}

fn paint_page(pdf: &mut krilla::Document, page: &Page<'_>, fonts: &mut FontCache) -> Result<()> {
    let page_box = page.geometry().page_box;
    let settings = PageSettings::from_wh(page_box.width * PX_TO_PT, page_box.height * PX_TO_PT)
        .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;
    let mut pdf_page = pdf.start_page_with(settings);
    let mut surface = pdf_page.surface();
    surface.push_transform(&Transform::from_scale(PX_TO_PT, PX_TO_PT));

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
    for fragment in boxes {
        if let Some(style) = page.computed(fragment.node()) {
            paint_box(&mut surface, fragment.paint_rect(), style);
        }
    }
    // Text goes above every block background and border.
    for run in page.text_runs() {
        paint_text_run(&mut surface, &run, fonts);
    }

    surface.pop();
    surface.finish();
    pdf_page.finish();
    Ok(())
}

/// Background color and borders of one box fragment (`rect` is its border
/// box).
fn paint_box(surface: &mut Surface<'_>, rect: PaintRect, style: &ComputedValues) {
    if !matches!(style.visibility, ComputedVisibility::Visible) {
        return;
    }
    fill_rect(
        surface,
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        style.background_color,
    );

    let side = |border: &ComputedBorder| {
        let visible = !matches!(border.style(), BorderStyle::None | BorderStyle::Hidden);
        let width = if visible { border.width().px() } else { 0.0 };
        (width, resolve_border_color(border.color, style.color))
    };
    let (top, top_color) = side(&style.border.top);
    let (right, right_color) = side(&style.border.right);
    let (bottom, bottom_color) = side(&style.border.bottom);
    let (left, left_color) = side(&style.border.left);
    // Every line style is drawn solid for now.
    fill_rect(surface, rect.x, rect.y, rect.width, top, top_color);
    fill_rect(
        surface,
        rect.x + rect.width - right,
        rect.y,
        right,
        rect.height,
        right_color,
    );
    fill_rect(
        surface,
        rect.x,
        rect.y + rect.height - bottom,
        rect.width,
        bottom,
        bottom_color,
    );
    fill_rect(surface, rect.x, rect.y, left, rect.height, left_color);
}

fn resolve_border_color(color: BorderColor, current: CssColor) -> CssColor {
    match color {
        BorderColor::Resolved(color) => color,
        // `currentcolor`, and any color form this painter does not know yet.
        _ => current,
    }
}

fn fill_rect(surface: &mut Surface<'_>, x: f32, y: f32, width: f32, height: f32, color: CssColor) {
    if color.a == 0 || width <= 0.0 || height <= 0.0 {
        return;
    }
    // A positive, finite rectangle always yields a path.
    let path = Rect::from_xywh(x, y, width, height).and_then(|rect| {
        let mut builder = PathBuilder::new();
        builder.push_rect(rect);
        builder.finish()
    });
    if let Some(path) = path {
        surface.set_fill(Some(fill(color)));
        surface.draw_path(&path);
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
