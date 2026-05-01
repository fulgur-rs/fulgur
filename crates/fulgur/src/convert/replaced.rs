use super::*;

/// Dispatcher entry for replaced elements (img / svg / `content: url(...)`).
///
/// Returns `true` when the node matches a replaced-element branch and an
/// `ImageEntry` / `SvgEntry` (and a `BlockEntry` when the node has visual
/// styling) was inserted into `out`. Returns `false` so the caller falls
/// through to the next dispatch stage.
pub(super) fn try_convert(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut super::ConvertContext<'_>,
    out: &mut crate::drawables::Drawables,
) -> bool {
    let Some(node) = doc.get_node(node_id) else {
        return false;
    };
    if let Some(elem_data) = node.element_data() {
        let tag = elem_data.name.local.as_ref();
        // Fall through to the generic / content-url paths below when the
        // asset can't be resolved (missing in bundle, unsupported format,
        // or `ImageData::None` from upstream SVG parse failure).
        if tag == "img" && convert_image(ctx, node, ctx.assets, out) {
            return true;
        }
        if tag == "svg" && convert_svg(ctx, node, ctx.assets, out) {
            return true;
        }
    }
    // CSS `content: url(...)` on a normal element replaces its children with
    // the image (CSS Content L3 §2). Blitz 0.2.4 does not materialise this
    // in layout, so we read the computed value and build an ImageEntry.
    convert_content_url(ctx, node, ctx.assets, out)
}

/// Insert a `BlockEntry` for the replaced element when its computed style
/// has visual styling (background, border, etc.). Returns the inset
/// `(content_width, content_height, opacity, visible)` callers use to size
/// the inner image / svg entry.
///
/// When the node has no visual style, returns the full border-box
/// dimensions and the node's own opacity / visibility — no `BlockEntry`
/// is inserted because the dispatcher skips block-paint for nodes
/// without one.
fn maybe_insert_block_for_replaced(
    node: &Node,
    assets: Option<&AssetBundle>,
    out: &mut crate::drawables::Drawables,
) -> (f32, f32, f32, bool) {
    let (width, height) = size_in_pt(node.final_layout.size);
    let style = extract_block_style(node, assets);
    let (opacity, visible) = extract_opacity_visible(node);

    if style.has_visual_style() {
        let (left_inset, top_inset) = style.content_inset();
        let right_inset = style.border_widths[1] + style.padding[1];
        let bottom_inset = style.border_widths[2] + style.padding[2];
        let content_width = (width - left_inset - right_inset).max(0.0);
        let content_height = (height - top_inset - bottom_inset).max(0.0);
        out.block_styles.insert(
            node.id,
            crate::drawables::BlockEntry {
                style,
                opacity,
                visible,
                id: extract_block_id(node),
                layout_size: Some(Size { width, height }),
                clip_descendants: Vec::new(),
                opacity_descendants: Vec::new(),
            },
        );
        // Inner image carries visibility but full opacity — the wrapping
        // BlockEntry handles opacity once for the whole border-box,
        // otherwise the border would also be faded.
        (content_width, content_height, 1.0, visible)
    } else {
        (width, height, opacity, visible)
    }
}

/// Resolve CSS width/height against intrinsic image dimensions + aspect ratio.
pub(super) fn resolve_image_dimensions(
    data: &[u8],
    format: crate::image::ImageFormat,
    css_w: Option<f32>,
    css_h: Option<f32>,
) -> (f32, f32) {
    let (iw, ih) = ImagePageable::decode_dimensions(data, format).unwrap_or((1, 1));
    let iw = iw as f32;
    let ih = ih as f32;
    let aspect = if ih > 0.0 { iw / ih } else { 1.0 };
    match (css_w, css_h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, if aspect > 0.0 { w / aspect } else { w }),
        (None, Some(h)) => (h * aspect, h),
        (None, None) => (iw, ih),
    }
}

/// Build an `ImageEntry` from raw image bytes plus optional CSS dimensions.
/// Used by the `<img>` element path, the `content: url()` pseudo path, and
/// list-style-image marker resolution.
pub(super) fn make_image_entry(
    data: Arc<Vec<u8>>,
    format: crate::image::ImageFormat,
    css_w: Option<f32>,
    css_h: Option<f32>,
    opacity: f32,
    visible: bool,
) -> crate::drawables::ImageEntry {
    let (w, h) = resolve_image_dimensions(&data, format, css_w, css_h);
    crate::drawables::ImageEntry {
        image_data: data,
        format,
        width: w,
        height: h,
        opacity,
        visible,
    }
}

/// Convert a normal element whose computed `content` resolves to a single
/// `url(...)` image into an `ImageEntry`. Per CSS spec, `content` on a
/// normal element replaces the element's children — so we return early and
/// skip pseudo-element processing.
///
/// Returns `false` when the element has no `content: url()`, the asset is
/// missing, or the format is unsupported — callers fall through to the
/// standard conversion path.
fn convert_content_url(
    _ctx: &ConvertContext<'_>,
    node: &Node,
    assets: Option<&AssetBundle>,
    out: &mut crate::drawables::Drawables,
) -> bool {
    let Some(raw_url) = crate::blitz_adapter::extract_content_image_url(node) else {
        return false;
    };
    let asset_name = extract_asset_name(&raw_url);
    let Some(bundle) = assets else { return false };
    let Some(data) = bundle.get_image(asset_name).cloned() else {
        return false;
    };
    let Some(format) = ImagePageable::detect_format(&data) else {
        return false;
    };

    let (content_w, content_h, opacity, visible) =
        maybe_insert_block_for_replaced(node, assets, out);
    let entry = make_image_entry(
        data,
        format,
        Some(content_w),
        Some(content_h),
        opacity,
        visible,
    );
    out.images.insert(node.id, entry);
    true
}

/// Convert an `<img>` element into an `ImageEntry`, plus a wrapping
/// `BlockEntry` when the element has visual styling.
fn convert_image(
    _ctx: &ConvertContext<'_>,
    node: &Node,
    assets: Option<&AssetBundle>,
    out: &mut crate::drawables::Drawables,
) -> bool {
    let Some(elem) = node.element_data() else {
        return false;
    };
    let Some(src) = get_attr(elem, "src") else {
        return false;
    };
    let Some(bundle) = assets else { return false };
    let Some(data) = bundle.get_image(src).cloned() else {
        return false;
    };
    let Some(format) = ImagePageable::detect_format(&data) else {
        return false;
    };

    let (content_w, content_h, opacity, visible) =
        maybe_insert_block_for_replaced(node, assets, out);
    let entry = make_image_entry(
        data,
        format,
        Some(content_w),
        Some(content_h),
        opacity,
        visible,
    );
    out.images.insert(node.id, entry);
    true
}

/// Convert an inline `<svg>` element into an `SvgEntry`, plus a wrapping
/// `BlockEntry` when the element has visual styling.
fn convert_svg(
    _ctx: &ConvertContext<'_>,
    node: &Node,
    assets: Option<&AssetBundle>,
    out: &mut crate::drawables::Drawables,
) -> bool {
    let Some(elem) = node.element_data() else {
        return false;
    };
    let Some(tree) = extract_inline_svg_tree(elem) else {
        return false;
    };

    let (content_w, content_h, opacity, visible) =
        maybe_insert_block_for_replaced(node, assets, out);
    out.svgs.insert(
        node.id,
        crate::drawables::SvgEntry {
            tree,
            width: content_w,
            height: content_h,
            opacity,
            visible,
        },
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal 1x1 red PNG.
    const TEST_PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn sample_png_arc() -> Arc<Vec<u8>> {
        Arc::new(TEST_PNG_1X1.to_vec())
    }

    #[test]
    fn test_make_image_entry_both_dimensions() {
        let img = make_image_entry(
            sample_png_arc(),
            crate::image::ImageFormat::Png,
            Some(100.0),
            Some(50.0),
            1.0,
            true,
        );
        assert_eq!(img.width, 100.0);
        assert_eq!(img.height, 50.0);
        assert_eq!(img.opacity, 1.0);
        assert!(img.visible);
    }

    #[test]
    fn test_make_image_entry_width_only_uses_intrinsic_aspect() {
        // Intrinsic 1x1 → aspect 1.0 → width=40 produces height=40.
        let img = make_image_entry(
            sample_png_arc(),
            crate::image::ImageFormat::Png,
            Some(40.0),
            None,
            1.0,
            true,
        );
        assert_eq!(img.width, 40.0);
        assert_eq!(img.height, 40.0);
    }

    #[test]
    fn test_make_image_entry_height_only_uses_intrinsic_aspect() {
        let img = make_image_entry(
            sample_png_arc(),
            crate::image::ImageFormat::Png,
            None,
            Some(25.0),
            1.0,
            true,
        );
        assert_eq!(img.width, 25.0);
        assert_eq!(img.height, 25.0);
    }

    #[test]
    fn test_make_image_entry_intrinsic_fallback() {
        let img = make_image_entry(
            sample_png_arc(),
            crate::image::ImageFormat::Png,
            None,
            None,
            0.5,
            false,
        );
        assert_eq!(img.width, 1.0);
        assert_eq!(img.height, 1.0);
        assert_eq!(img.opacity, 0.5);
        assert!(!img.visible);
    }
}
