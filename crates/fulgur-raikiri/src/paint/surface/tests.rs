use super::*;
use krilla::geom::PathBuilder;
use krilla::mask::MaskType;
use krilla::page::PageSettings;
use krilla::paint::Fill;

fn clip() -> Path {
    let mut builder = PathBuilder::new();
    builder.push_rect(krilla::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap());
    builder.finish().unwrap()
}

/// A mask over an empty stream, which is enough for the push to be counted.
fn mask() -> Mask {
    let mut pdf = krilla::Document::new();
    let mut page = pdf.start_page_with(PageSettings::from_wh(20.0, 20.0).unwrap());
    let mut surface = page.surface();
    let mut builder = surface.stream_builder();
    builder.surface().finish();
    let stream = builder.finish();
    surface.finish();
    page.finish();
    Mask::new(stream, MaskType::Alpha)
}

#[test]
fn a_surface_with_every_push_closed_finishes() {
    let mut pdf = krilla::Document::new();
    let mut page = pdf.start_page_with(PageSettings::from_wh(20.0, 20.0).unwrap());
    let mut surface = Surface::new(page.surface());
    surface.push_transform(&Transform::identity());
    surface.push_clip_path(&clip(), &FillRule::NonZero);
    surface.pop();
    surface.pop();
    surface.finish();
    page.finish();
    assert!(pdf.finish().is_ok());
}

#[test]
fn unwind_closes_every_kind_of_push() {
    let mut pdf = krilla::Document::new();
    let mut page = pdf.start_page_with(PageSettings::from_wh(20.0, 20.0).unwrap());
    let mut surface = Surface::new(page.surface());
    surface.push_transform(&Transform::identity());
    surface.push_clip_path(&clip(), &FillRule::NonZero);
    surface.push_opacity(NormalizedF32::new(0.5).unwrap());
    surface.push_mask(mask());
    surface.set_fill(Some(Fill::default()));
    surface.draw_path(&clip());
    // Krilla panics when a surface with an open push is dropped.
    surface.unwind();
    surface.finish();
    page.finish();
    assert!(pdf.finish().is_ok());
}

#[test]
fn unwind_with_nothing_open_does_nothing() {
    let mut pdf = krilla::Document::new();
    let mut page = pdf.start_page_with(PageSettings::from_wh(20.0, 20.0).unwrap());
    let mut surface = Surface::new(page.surface());
    surface.unwind();
    surface.finish();
    page.finish();
    assert!(pdf.finish().is_ok());
}
