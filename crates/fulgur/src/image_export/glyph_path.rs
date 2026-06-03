//! Convert a shaped glyph to SVG path data using skrifa outlines.

use skrifa::GlyphId;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::prelude::*;

/// Accumulates an SVG path `d` string from font outline commands.
///
/// Font outline space is Y-up; SVG is Y-down. The pen maps each outline
/// point `(x, y)` (already scaled to `font_size` by skrifa) to
/// `(origin_x + x, baseline_y - y)`.
struct SvgPathPen {
    d: String,
    ox: f32,
    baseline_y: f32,
}

impl SvgPathPen {
    fn tx(&self, x: f32) -> f32 {
        self.ox + x
    }
    fn ty(&self, y: f32) -> f32 {
        self.baseline_y - y
    }
}

impl OutlinePen for SvgPathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.d
            .push_str(&format!("M{:.2} {:.2}", self.tx(x), self.ty(y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.d
            .push_str(&format!("L{:.2} {:.2}", self.tx(x), self.ty(y)));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.d.push_str(&format!(
            "Q{:.2} {:.2} {:.2} {:.2}",
            self.tx(cx),
            self.ty(cy),
            self.tx(x),
            self.ty(y)
        ));
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.d.push_str(&format!(
            "C{:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
            self.tx(c1x),
            self.ty(c1y),
            self.tx(c2x),
            self.ty(c2y),
            self.tx(x),
            self.ty(y)
        ));
    }
    fn close(&mut self) {
        self.d.push('Z');
    }
}

/// Build SVG path data (`d` attribute) for a single glyph.
///
/// `font_index` selects the face in a collection. `glyph_id` is the shaped
/// glyph id. `font_size` is the em size in pt. `origin_x` / `baseline_y` are
/// the glyph pen position in the target SVG (pt, Y-down). Returns an empty
/// string for glyphs with no contours (e.g. whitespace) or on any font error.
pub fn glyph_to_svg_path(
    font_data: &[u8],
    font_index: u32,
    glyph_id: u32,
    font_size: f32,
    origin_x: f32,
    baseline_y: f32,
) -> String {
    let Ok(font) = FontRef::from_index(font_data, font_index) else {
        return String::new();
    };
    let outlines = font.outline_glyphs();
    let Some(glyph) = outlines.get(GlyphId::new(glyph_id)) else {
        return String::new();
    };
    let mut pen = SvgPathPen {
        d: String::new(),
        ox: origin_x,
        baseline_y,
    };
    let settings = DrawSettings::unhinted(Size::new(font_size), LocationRef::default());
    if glyph.draw(settings, &mut pen).is_err() {
        return String::new();
    }
    pen.d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noto() -> Vec<u8> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/.fonts/NotoSans-Regular.ttf"
        );
        std::fs::read(path).expect("read NotoSans-Regular.ttf")
    }

    #[test]
    fn outline_for_uppercase_a_is_nonempty_path() {
        let font_bytes = noto();
        let font = FontRef::from_index(&font_bytes, 0).unwrap();
        let a_id = font.charmap().map('A').unwrap().to_u32();
        let d = glyph_to_svg_path(&font_bytes, 0, a_id, 100.0, 50.0, 200.0);
        assert!(d.starts_with('M'), "path should start with a moveto: {d}");
        assert!(d.contains('Z'), "path should close a contour: {d}");
        assert!(d.len() > 10);
    }

    #[test]
    fn outline_for_space_is_empty() {
        let font_bytes = noto();
        let font = FontRef::from_index(&font_bytes, 0).unwrap();
        let space_id = font.charmap().map(' ').unwrap().to_u32();
        let d = glyph_to_svg_path(&font_bytes, 0, space_id, 100.0, 0.0, 100.0);
        assert!(d.is_empty(), "space glyph should produce empty path: {d:?}");
    }
}
