//! Glyph outlines of text runs, for `background-clip: text` (CSS
//! Backgrounds 4 §2.6): the background is painted only inside the glyphs
//! of the element's text.

use krilla::geom::{Path, PathBuilder};
use raikiri_html::{DomView, GeneratedBox, NodeId, PositionedGlyphRun, RunSource};
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// The element a run's text belongs to: the parent of its text node, or
/// the originating element of generated content.
pub(super) fn run_element(dom: DomView<'_>, run: &PositionedGlyphRun<'_>) -> Option<NodeId> {
    match run.source {
        RunSource::Text(node) => dom.parent(node),
        RunSource::Generated(node, _) => Some(node),
        _ => None,
    }
}

/// The outlines of every glyph in `runs` whose element is `element` or one
/// of its descendants, or `None` when there are none.
pub(super) fn outlines(
    dom: DomView<'_>,
    element: NodeId,
    runs: &[PositionedGlyphRun<'_>],
) -> Option<Path> {
    let mut pen = Pen {
        builder: PathBuilder::new(),
        x: 0.0,
        y: 0.0,
        skew: 0.0,
    };
    for run in runs {
        let mut node = run_element(dom, run);
        let inside = loop {
            match node {
                Some(current) if current == element => break true,
                Some(current) => node = dom.parent(current),
                None => break false,
            }
        };
        if inside {
            append_run(&mut pen, run);
        }
    }
    pen.builder.finish()
}

/// Outlines of this pseudo-element on this piece's line, excluding its owner text.
pub(super) fn generated_outlines(
    piece: &GeneratedBox<'_>,
    runs: &[PositionedGlyphRun<'_>],
) -> Option<Path> {
    let mut pen = Pen {
        builder: PathBuilder::new(),
        x: 0.0,
        y: 0.0,
        skew: 0.0,
    };
    for run in runs {
        if run.line == piece.line && run.source == RunSource::Generated(piece.owner, piece.kind) {
            append_run(&mut pen, run);
        }
    }
    pen.builder.finish()
}

/// Append the outlines of one run, placed as `paint_text_run` draws it.
/// Synthetic oblique is applied; synthetic bold is not.
fn append_run(pen: &mut Pen, run: &PositionedGlyphRun<'_>) {
    let Ok(font) = FontRef::from_index(run.font.data.as_bytes(), run.font.index) else {
        return;
    };
    let glyphs = font.outline_glyphs();
    let coords: Vec<NormalizedCoord> = run
        .normalized_coords
        .iter()
        .map(|bits| NormalizedCoord::from_bits(*bits))
        .collect();
    let location = LocationRef::new(&coords);
    pen.skew = run
        .synthesis
        .skew
        .map_or(0.0, |degrees| degrees.to_radians().tan());
    let mut x = run.origin.0;
    for glyph in &run.glyphs {
        if let Some(outline) = glyphs.get(GlyphId::new(glyph.id)) {
            pen.x = x + glyph.x_offset;
            pen.y = run.origin.1 + glyph.y_offset;
            // An outline that fails to draw leaves at most a partial glyph.
            let _ = outline.draw(
                DrawSettings::unhinted(Size::new(run.font_size), location),
                pen,
            );
        }
        x += glyph.advance;
    }
}

/// Collects outlines in px: the font's y-up coordinates are flipped and
/// offset to the glyph origin `(x, y)`, then slanted by `skew`.
struct Pen {
    builder: PathBuilder,
    x: f32,
    y: f32,
    skew: f32,
}

impl Pen {
    fn point(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x + x + y * self.skew, self.y - y)
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.point(x, y);
        self.builder.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.point(x, y);
        self.builder.line_to(x, y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (cx0, cy0) = self.point(cx0, cy0);
        let (x, y) = self.point(x, y);
        self.builder.quad_to(cx0, cy0, x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (cx0, cy0) = self.point(cx0, cy0);
        let (cx1, cy1) = self.point(cx1, cy1);
        let (x, y) = self.point(x, y);
        self.builder.cubic_to(cx0, cy0, cx1, cy1, x, y);
    }

    fn close(&mut self) {
        self.builder.close();
    }
}
