//! HTML → image output (PNG / lossless WebP).
//!
//! Gated behind the `image-export` cargo feature. Reuses the existing
//! layout → `Drawables` pipeline and branches at the draw stage: page 0's
//! drawables are serialized to SVG (text as glyph-outline paths), then
//! rasterized with resvg/tiny-skia and encoded.

mod b64;
mod encode;
mod glyph_path;
mod options;
mod rasterize;
mod svg_emit;

pub use options::{Background, ImageFormat, ImageOptions};

use crate::drawables::Drawables;
use crate::pagination_layout::PaginationGeometryTable;

/// Render page-0 drawables to encoded image bytes per `opts`.
pub fn render_drawables_to_image(
    drawables: &Drawables,
    geometry: &PaginationGeometryTable,
    opts: &ImageOptions,
) -> crate::error::Result<Vec<u8>> {
    opts.validate()?;
    let (svg, composites) = svg_emit::page_to_svg(drawables, geometry, opts);
    let (pw, ph) = opts.pixmap_dims();
    let width_pt = opts.width_px as f32 * 0.75;
    let height_pt = opts.height_px as f32 * 0.75;
    let sx = pw as f32 / width_pt;
    let sy = ph as f32 / height_pt;
    let pixmap = rasterize::render_page(&svg, pw, ph, sx, sy, &composites)?;
    encode::encode_pixmap(&pixmap, opts.format)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw_primitives::BlockStyle;
    use crate::drawables::{BlockEntry, Drawables};
    use crate::image_export::options::{Background, ImageFormat, ImageOptions};
    use crate::pagination_layout::{Fragment, PaginationGeometry};
    use std::collections::BTreeMap;

    #[test]
    fn renders_single_red_block_to_png() {
        let style = BlockStyle {
            background_color: Some([255, 0, 0, 255]),
            ..Default::default()
        };
        let mut drawables = Drawables::default();
        drawables.block_styles.insert(
            1,
            BlockEntry {
                style,
                opacity: 1.0,
                visible: true,
                id: None,
                layout_size: None,
                clip_descendants: vec![],
                opacity_descendants: vec![],
            },
        );
        let mut geometry: BTreeMap<usize, PaginationGeometry> = BTreeMap::new();
        geometry.insert(
            1,
            PaginationGeometry {
                fragments: vec![Fragment {
                    page_index: 0,
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                }],
                is_repeat: false,
            },
        );
        let mut opts = ImageOptions::new(100, 100, ImageFormat::Png);
        opts.background = Background::Solid([255, 255, 255, 255]);
        let bytes = render_drawables_to_image(&drawables, &geometry, &opts).unwrap();
        assert_eq!(&bytes[..4], &[0x89, b'P', b'N', b'G']);
    }
}
