use super::*;

/// Dispatcher entry for replaced elements (img / svg / `content: url(...)`).
/// Returns `Some(Pageable)` if the node matches one of these branches and a
/// pageable is produced; returns `None` otherwise so the caller falls
/// through to the next dispatch stage.
pub(super) fn try_convert(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut super::ConvertContext<'_>,
) -> Option<Box<dyn crate::pageable::Pageable>> {
    let node = doc.get_node(node_id)?;
    if let Some(elem_data) = node.element_data() {
        let tag = elem_data.name.local.as_ref();
        if tag == "img" {
            if let Some(img) = convert_image(ctx, node, ctx.assets) {
                return Some(img);
            }
        }
        if tag == "svg" {
            if let Some(svg) = convert_svg(ctx, node, ctx.assets) {
                return Some(svg);
            }
        }
    }
    convert_content_url(ctx, node, ctx.assets)
}

/// Wrap an atomic replaced element (image, svg) in a styled `BlockPageable`
/// when the node has visual styling, or return the inner Pageable directly.
///
/// `build_inner` is invoked once with the dimensions and the opacity/visibility
/// values that should be applied to the inner element. In the styled branch
/// the inner receives `opacity = 1.0` (the wrapping block handles opacity)
/// and the dimensions are the content-box, not the border-box. In the unstyled
/// branch the inner receives the node's own opacity/visibility and full size.
fn wrap_replaced_in_block_style<F>(
    ctx: &ConvertContext<'_>,
    node: &Node,
    assets: Option<&AssetBundle>,
    build_inner: F,
) -> Box<dyn Pageable>
where
    F: FnOnce(f32, f32, f32, bool) -> Box<dyn Pageable>,
{
    let (width, height) = size_in_pt(node.final_layout.size);

    let style = extract_block_style(node, assets);
    let (opacity, visible) = extract_opacity_visible(node);
    let pagination = extract_pagination_from_column_css(ctx, node);

    if style.has_visual_style() {
        let (cx, cy) = style.content_inset();
        // content_inset returns (left, top); compute right/bottom insets for content-box
        let right_inset = style.border_widths[1] + style.padding[1];
        let bottom_inset = style.border_widths[2] + style.padding[2];
        let content_width = (width - cx - right_inset).max(0.0);
        let content_height = (height - cy - bottom_inset).max(0.0);
        // Inner element receives visibility (it IS the node's own content) but
        // NOT opacity — the wrapping block handles opacity once for the whole
        // border-box, otherwise the border would also be faded.
        let inner = build_inner(content_width, content_height, 1.0, visible);
        let child = PositionedChild {
            child: inner,
            x: cx,
            y: cy,
        };
        let mut block = BlockPageable::with_positioned_children(vec![child])
            .with_pagination(pagination)
            .with_style(style)
            .with_opacity(opacity)
            .with_visible(visible)
            .with_id(extract_block_id(node));
        block.wrap(width, height);
        block.layout_size = Some(Size { width, height });
        Box::new(block)
    } else if pagination != crate::pageable::Pagination::default() {
        // Replaced element with no visual style but a non-default Pagination
        // (e.g. `<img style="break-before: page">`): wrap in a thin
        // BlockPageable so paginate() honours the break.
        // Match the styled branch: the wrapper owns opacity, the inner keeps visibility.
        let inner = build_inner(width, height, 1.0, visible);
        let child = PositionedChild {
            child: inner,
            x: 0.0,
            y: 0.0,
        };
        let mut block = BlockPageable::with_positioned_children(vec![child])
            .with_pagination(pagination)
            .with_opacity(opacity)
            .with_visible(visible)
            .with_id(extract_block_id(node));
        block.wrap(width, height);
        block.layout_size = Some(Size { width, height });
        Box::new(block)
    } else {
        build_inner(width, height, opacity, visible)
    }
}

/// Shared sizing / construction for `ImagePageable`, used by both the `<img>`
/// element path and the `::before`/`::after` `content: url()` pseudo path.
///
/// Sizing rules match the CSS replaced-element spec:
///
/// - both css dims given → use them verbatim
/// - one given → scale the other by the image's intrinsic aspect ratio
/// - neither given → use intrinsic pixel dimensions (treated as 1px = 1pt
///   since `ImagePageable` draws in PDF points; this matches the existing
///   `<img>` behavior when Taffy has nothing to resolve the size from)
///
/// The intrinsic dimensions come from `ImagePageable::decode_dimensions`.
/// A zero-height decode result silently degrades to a 1:1 aspect so width-only
/// sizing does not produce NaN.
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

pub(super) fn make_image_pageable(
    data: Arc<Vec<u8>>,
    format: crate::image::ImageFormat,
    css_w: Option<f32>,
    css_h: Option<f32>,
    opacity: f32,
    visible: bool,
) -> ImagePageable {
    let (w, h) = resolve_image_dimensions(&data, format, css_w, css_h);
    let mut img = ImagePageable::new(data, format, w, h);
    img.opacity = opacity;
    img.visible = visible;
    img
}

/// Convert a normal element whose computed `content` resolves to a single
/// `url(...)` image into an `ImagePageable`. Per CSS spec, `content` on a
/// normal element replaces the element's children — so we return early and
/// skip pseudo-element processing.
///
/// Returns `None` when the element has no `content: url()`, the asset is
/// missing, or the format is unsupported — callers fall through to the
/// standard conversion path.
fn convert_content_url(
    ctx: &ConvertContext<'_>,
    node: &Node,
    assets: Option<&AssetBundle>,
) -> Option<Box<dyn Pageable>> {
    let raw_url = crate::blitz_adapter::extract_content_image_url(node)?;
    let asset_name = extract_asset_name(&raw_url);
    let bundle = assets?;
    let data = Arc::clone(bundle.get_image(asset_name)?);
    let format = ImagePageable::detect_format(&data)?;

    Some(wrap_replaced_in_block_style(
        ctx,
        node,
        assets,
        move |w, h, opacity, visible| {
            let img = make_image_pageable(data.clone(), format, Some(w), Some(h), opacity, visible);
            Box::new(img)
        },
    ))
}

/// Convert an `<img>` element into an `ImagePageable`, wrapped in `BlockPageable` if styled.
fn convert_image(
    ctx: &ConvertContext<'_>,
    node: &Node,
    assets: Option<&AssetBundle>,
) -> Option<Box<dyn Pageable>> {
    let elem = node.element_data()?;
    let src = get_attr(elem, "src")?;
    let bundle = assets?;
    let data = Arc::clone(bundle.get_image(src)?);
    let format = ImagePageable::detect_format(&data)?;

    Some(wrap_replaced_in_block_style(
        ctx,
        node,
        assets,
        move |w, h, opacity, visible| {
            // `wrap_replaced_in_block_style` has already resolved (w, h) from
            // Taffy's final layout, so we pass them as explicit css_w/css_h.
            // The shared helper then applies the same `ImagePageable::new`
            // construction path as the pseudo-content url() case, keeping
            // sizing behavior byte-identical to the previous <img> path.
            let img = make_image_pageable(data.clone(), format, Some(w), Some(h), opacity, visible);
            Box::new(img)
        },
    ))
}

/// Convert an inline `<svg>` element into an `SvgPageable`, wrapped in `BlockPageable` if styled.
///
/// Blitz parses the inline SVG into a `usvg::Tree` during DOM construction;
/// `blitz_adapter::extract_inline_svg_tree` retrieves it without exposing
/// blitz-internal types here.
fn convert_svg(
    ctx: &ConvertContext<'_>,
    node: &Node,
    assets: Option<&AssetBundle>,
) -> Option<Box<dyn Pageable>> {
    let elem = node.element_data()?;
    let tree = extract_inline_svg_tree(elem)?;

    Some(wrap_replaced_in_block_style(
        ctx,
        node,
        assets,
        move |w, h, opacity, visible| {
            let mut svg = SvgPageable::new(tree, w, h);
            svg.opacity = opacity;
            svg.visible = visible;
            Box::new(svg)
        },
    ))
}
