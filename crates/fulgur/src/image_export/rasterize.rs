//! Rasterize an SVG document string into a tiny-skia Pixmap via resvg.

use crate::error::Error;
use tiny_skia::Pixmap;

/// Parse `svg` and render it into a `width × height` pixmap under
/// `transform`. The SVG's `viewBox` (authored in pt) is mapped onto the
/// device-pixel pixmap by usvg/resvg; `transform` applies the pt → device-px
/// scale so the caller controls scale through the pixmap dimensions and the
/// transform (the page SVG's intrinsic size equals its viewBox, so its own
/// internal transform is identity).
pub fn svg_to_pixmap(
    svg: &str,
    width: u32,
    height: u32,
    transform: tiny_skia::Transform,
) -> crate::error::Result<Pixmap> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg, &opt)
        .map_err(|e| Error::Other(format!("SVG parse failed: {e}")))?;
    let mut pixmap = Pixmap::new(width.max(1), height.max(1))
        .ok_or_else(|| Error::Other("failed to allocate pixmap".into()))?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(pixmap)
}

/// Render the page SVG (pt-space) into a `pixmap_w × pixmap_h` device-pixel
/// pixmap scaled by (sx, sy), then composite each SvgEntry sub-tree at its pt
/// rect.
pub fn render_page(
    svg: &str,
    pixmap_w: u32,
    pixmap_h: u32,
    sx: f32,
    sy: f32,
    composites: &[crate::image_export::svg_emit::SvgComposite],
) -> crate::error::Result<Pixmap> {
    let mut pixmap = svg_to_pixmap(
        svg,
        pixmap_w,
        pixmap_h,
        tiny_skia::Transform::from_scale(sx, sy),
    )?;
    for (tree, x, y, w, h) in composites {
        let size = tree.size();
        let (tw, th) = (size.width(), size.height());
        if tw <= 0.0 || th <= 0.0 {
            continue;
        }
        let ts = tiny_skia::Transform::from_scale(sx, sy)
            .pre_translate(*x, *y)
            .pre_scale(w / tw, h / th);
        resvg::render(tree, ts, &mut pixmap.as_mut());
    }
    Ok(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_red_square_to_pixmap() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" viewBox="0 0 10 10"><rect x="0" y="0" width="10" height="10" fill="rgb(255,0,0)"/></svg>"#;
        let pixmap =
            svg_to_pixmap(svg, 10, 10, tiny_skia::Transform::identity()).expect("rasterize");
        assert_eq!(pixmap.width(), 10);
        assert_eq!(pixmap.height(), 10);
        let px = pixmap.pixel(5, 5).unwrap();
        assert_eq!(px.red(), 255);
        assert_eq!(px.green(), 0);
        assert_eq!(px.blue(), 0);
        assert_eq!(px.alpha(), 255);
    }

    #[test]
    fn rejects_invalid_svg() {
        assert!(svg_to_pixmap("not svg", 10, 10, tiny_skia::Transform::identity()).is_err());
    }
}
