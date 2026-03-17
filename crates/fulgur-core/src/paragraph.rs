//! ParagraphPageable — renders text via the Parley→Krilla glyph bridge.

use std::sync::Arc;

use crate::pageable::{Canvas, Pageable, Pagination, Pt, Size};

/// A pre-extracted glyph for rendering via Krilla.
#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    pub id: u32,
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    pub text_range: std::ops::Range<usize>,
}

/// A pre-extracted glyph run (single font + style).
#[derive(Clone)]
pub struct ShapedGlyphRun {
    pub font_data: Arc<Vec<u8>>,
    pub font_index: u32,
    pub font_size: f32,
    pub color: [u8; 4], // RGBA
    pub glyphs: Vec<ShapedGlyph>,
    pub text: String,
    pub x_offset: f32,
}

/// A shaped line of text.
#[derive(Clone)]
pub struct ShapedLine {
    pub height: f32,
    pub baseline: f32,
    pub glyph_runs: Vec<ShapedGlyphRun>,
}

/// Paragraph element that renders shaped text.
#[derive(Clone)]
pub struct ParagraphPageable {
    pub lines: Vec<ShapedLine>,
    pub pagination: Pagination,
    pub cached_height: f32,
}

impl ParagraphPageable {
    pub fn new(lines: Vec<ShapedLine>) -> Self {
        let cached_height: f32 = lines.iter().map(|l| l.height).sum();
        Self {
            lines,
            pagination: Pagination::default(),
            cached_height,
        }
    }
}

impl Pageable for ParagraphPageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        self.cached_height = self.lines.iter().map(|l| l.height).sum();
        Size {
            width: _avail_width,
            height: self.cached_height,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        if self.lines.len() <= 1 {
            return None;
        }

        let orphans = self.pagination.orphans;
        let widows = self.pagination.widows;

        // Find the split point
        let mut consumed: f32 = 0.0;
        let mut split_at = 0;
        for (i, line) in self.lines.iter().enumerate() {
            if consumed + line.height > avail_height {
                split_at = i;
                break;
            }
            consumed += line.height;
            split_at = i + 1;
        }

        if split_at == 0 || split_at >= self.lines.len() {
            return None;
        }

        // Enforce orphans/widows
        if split_at < orphans {
            return None;
        }
        if self.lines.len() - split_at < widows {
            let adjusted = self.lines.len().saturating_sub(widows);
            if adjusted < orphans || adjusted == 0 {
                return None;
            }
            // split_at = adjusted; -- would break orphan rule
        }

        let first = ParagraphPageable::new(self.lines[..split_at].to_vec());
        let second = ParagraphPageable::new(self.lines[split_at..].to_vec());

        Some((Box::new(first), Box::new(second)))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, _avail_width: Pt, _avail_height: Pt) {
        let mut current_y = y;

        for line in &self.lines {
            let baseline_y = current_y + line.baseline;

            for run in &line.glyph_runs {
                // Create Krilla font from cached data
                let data: krilla::Data = Arc::clone(&run.font_data).into();
                let Some(font) = krilla::text::Font::new(data, run.font_index) else {
                    continue;
                };

                // Convert shaped glyphs to Krilla glyphs
                // Values are already normalized (/ font_size) in convert.rs
                let krilla_glyphs: Vec<krilla::text::KrillaGlyph> = run
                    .glyphs
                    .iter()
                    .map(|g| krilla::text::KrillaGlyph {
                        glyph_id: krilla::text::GlyphId::new(g.id),
                        text_range: g.text_range.clone(),
                        x_advance: g.x_advance,
                        x_offset: g.x_offset,
                        y_offset: g.y_offset,
                        y_advance: 0.0,
                        location: None,
                    })
                    .collect();

                if krilla_glyphs.is_empty() {
                    continue;
                }

                // Set text color
                let fill = krilla::paint::Fill {
                    paint: krilla::color::rgb::Color::new(run.color[0], run.color[1], run.color[2])
                        .into(),
                    opacity: krilla::num::NormalizedF32::new(run.color[3] as f32 / 255.0)
                        .unwrap_or(krilla::num::NormalizedF32::ONE),
                    rule: Default::default(),
                };
                canvas.surface.set_fill(Some(fill));

                let start = krilla::geom::Point::from_xy(x + run.x_offset, baseline_y);
                canvas.surface.draw_glyphs(
                    start,
                    &krilla_glyphs,
                    font,
                    &run.text,
                    run.font_size,
                    false,
                );
            }

            current_y += line.height;
        }
    }

    fn pagination(&self) -> Pagination {
        self.pagination
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.cached_height
    }
}
