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
            // Fall through to generic handling below to preserve Taffy-computed dimensions
        }
        if tag == "svg" {
            if let Some(svg) = convert_svg(ctx, node, ctx.assets) {
                return Some(svg);
            }
            // Fall through — e.g., ImageData::None (parse failure upstream)
        }
    }
    // CSS `content: url(...)` on a normal element replaces its children with
    // the image (CSS Content L3 §2). Blitz 0.2.4 does not materialise this
    // in layout, so we read the computed value and build an ImagePageable.
    // Early return skips pseudo-element processing (spec-correct: replaced
    // elements do not generate ::before/::after).
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

#[cfg(test)]
mod tests {
    use super::super::tests::{collect_images, sample_png_arc};
    use super::super::{ConvertContext, dom_to_pageable};
    use crate::asset::AssetBundle;
    use crate::image::ImageFormat;
    use std::collections::HashMap;

    #[test]
    fn test_make_image_pageable_both_dimensions() {
        let img = super::make_image_pageable(
            sample_png_arc(),
            ImageFormat::Png,
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
    fn test_make_image_pageable_width_only_uses_intrinsic_aspect() {
        // Intrinsic 1x1 → aspect 1.0 → width=40 produces height=40.
        let img = super::make_image_pageable(
            sample_png_arc(),
            ImageFormat::Png,
            Some(40.0),
            None,
            1.0,
            true,
        );
        assert_eq!(img.width, 40.0);
        assert_eq!(img.height, 40.0);
    }

    #[test]
    fn test_make_image_pageable_height_only_uses_intrinsic_aspect() {
        let img = super::make_image_pageable(
            sample_png_arc(),
            ImageFormat::Png,
            None,
            Some(25.0),
            1.0,
            true,
        );
        assert_eq!(img.width, 25.0);
        assert_eq!(img.height, 25.0);
    }

    #[test]
    fn test_make_image_pageable_intrinsic_fallback() {
        let img =
            super::make_image_pageable(sample_png_arc(), ImageFormat::Png, None, None, 0.5, false);
        assert_eq!(img.width, 1.0);
        assert_eq!(img.height, 1.0);
        assert_eq!(img.opacity, 0.5);
        assert!(!img.visible);
    }

    #[test]
    fn test_convert_content_url_normal_element() {
        // A normal element with `content: url(...)` + explicit width/height
        // should produce an ImagePageable, replacing its text children.
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .unwrap();
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            .replaced { content: url("icon.png"); width: 24px; height: 24px; }
        </style></head><body><div class="replaced">This text should be replaced</div></body></html>"#;

        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: Some(&bundle),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = dom_to_pageable(&doc, &mut ctx);

        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.iter().any(|(w, h)| *w == 18.0 && *h == 18.0),
            "expected an 18x18 pt ImagePageable (24 CSS px × 0.75) from content: url(), got {:?}",
            images
        );
    }

    #[test]
    fn test_convert_content_url_no_content_falls_through() {
        // A normal div without content: url() should NOT produce an ImagePageable.
        let html = r#"<!doctype html><html><head><style>
            div { width: 100px; height: 50px; background: red; }
        </style></head><body><div>Normal text</div></body></html>"#;

        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = dom_to_pageable(&doc, &mut ctx);

        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.is_empty(),
            "normal div without content: url() should not produce images, got {:?}",
            images
        );
    }

    #[test]
    fn test_convert_content_url_missing_asset_falls_through() {
        // content: url("missing.png") where the asset is not in the bundle
        // should silently fall through to the normal conversion path.
        let bundle = AssetBundle::new(); // empty bundle

        let html = r#"<!doctype html><html><head><style>
            .replaced { content: url("missing.png"); width: 24px; height: 24px; }
        </style></head><body><div class="replaced">fallback text</div></body></html>"#;

        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: Some(&bundle),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = dom_to_pageable(&doc, &mut ctx);

        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.is_empty(),
            "missing asset should not produce images, got {:?}",
            images
        );
    }
}
