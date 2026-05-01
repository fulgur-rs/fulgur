use super::inline_root;
use super::positioned::{is_absolutely_positioned, walk_absolute_children};
use super::replaced::{make_image_entry, resolve_image_dimensions};
use super::*;

/// Build an `ImageEntry` for a `::before`/`::after` pseudo-element node
/// whose computed `content` resolves to a single `url(...)` image.
///
/// Returns `None` under the same conditions as the v1 `build_pseudo_image`.
pub(super) fn build_pseudo_image_entry(
    pseudo_node: &Node,
    parent_content_width: f32,
    parent_content_height: f32,
    assets: Option<&AssetBundle>,
) -> Option<crate::drawables::ImageEntry> {
    let assets = assets?;

    let raw_url = crate::blitz_adapter::extract_content_image_url(pseudo_node)?;
    let asset_name = extract_asset_name(&raw_url);
    let data = Arc::clone(assets.get_image(asset_name)?);
    let format = ImagePageable::detect_format(&data)?;

    let styles = pseudo_node.primary_styles()?;
    let css_w = resolve_pseudo_size(&styles.clone_width(), parent_content_width);
    let css_h = resolve_pseudo_size(&styles.clone_height(), parent_content_height);

    let (opacity, visible) = extract_opacity_visible(pseudo_node);
    Some(make_image_entry(
        data, format, css_w, css_h, opacity, visible,
    ))
}

/// True iff the pseudo-element has `display: block` outside.
pub(super) fn is_block_pseudo(pseudo: &Node) -> bool {
    use ::style::values::specified::box_::DisplayOutside;
    pseudo
        .primary_styles()
        .is_some_and(|s| s.clone_display().outside() == DisplayOutside::Block)
}

/// Register pseudo content (block-pseudo images + abs-positioned pseudos +
/// non-pseudo abs children) into `out`. Returns `true` when at least one
/// pseudo / abs entry was added so callers (e.g. `block::convert`) know to
/// keep the parent's wrapping `BlockEntry`.
pub(super) fn register_pseudo_content(
    doc: &BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    parent_cb: ContentBox,
    out: &mut crate::drawables::Drawables,
) -> bool {
    let mut produced = false;
    let (before_img, after_img) =
        build_block_pseudo_image_entries(doc, node, parent_cb, ctx.assets);
    if let Some((pseudo_id, entry)) = before_img {
        out.images.insert(pseudo_id, entry);
        produced = true;
    }
    if let Some((pseudo_id, entry)) = after_img {
        out.images.insert(pseudo_id, entry);
        produced = true;
    }
    let before_keys = collect_drawables_node_ids(out);
    walk_absolute_children(doc, node, ctx, depth, out);
    let after_keys = collect_drawables_node_ids(out);
    if after_keys.len() > before_keys.len() {
        produced = true;
    }
    produced
}

/// Cheap probe: does `node` have at least one `::before` / `::after` pseudo
/// slot whose computed `content` resolves to a block-display image URL?
pub(super) fn node_has_block_pseudo_image(doc: &BaseDocument, node: &Node) -> bool {
    for pseudo_id in [node.before, node.after].into_iter().flatten() {
        if let Some(pseudo) = doc.get_node(pseudo_id)
            && is_block_pseudo(pseudo)
            && crate::blitz_adapter::extract_content_image_url(pseudo).is_some()
        {
            return true;
        }
    }
    false
}

/// Cheap probe: does `node` have at least one `::before` / `::after` pseudo
/// slot whose computed `content` resolves to an inline image URL?
pub(super) fn node_has_inline_pseudo_image(doc: &BaseDocument, node: &Node) -> bool {
    for pseudo_id in [node.before, node.after].into_iter().flatten() {
        if let Some(pseudo) = doc.get_node(pseudo_id)
            && !is_block_pseudo(pseudo)
            && crate::blitz_adapter::extract_content_image_url(pseudo).is_some()
        {
            return true;
        }
    }
    false
}

/// Returns `true` if `node` has a `::before` or `::after` pseudo-element
/// whose computed `position` is `absolute` or `fixed`.
pub(super) fn node_has_absolute_pseudo(doc: &BaseDocument, node: &Node) -> bool {
    for pseudo_id in [node.before, node.after].into_iter().flatten() {
        if let Some(pseudo) = doc.get_node(pseudo_id)
            && is_absolutely_positioned(pseudo)
        {
            return true;
        }
    }
    false
}

/// `(pseudo_id, ImageEntry)` slot returned by
/// [`build_block_pseudo_image_entries`].
type BlockPseudoImageSlot = Option<(usize, crate::drawables::ImageEntry)>;

/// Build `(pseudo_id, ImageEntry)` for `::before` / `::after` block pseudos
/// whose `content: url(...)` resolves and which are not absolutely
/// positioned (those are handled by `walk_absolute_children`).
fn build_block_pseudo_image_entries(
    doc: &BaseDocument,
    parent: &Node,
    parent_cb: ContentBox,
    assets: Option<&AssetBundle>,
) -> (BlockPseudoImageSlot, BlockPseudoImageSlot) {
    if assets.is_none() {
        return (None, None);
    }
    let load = |pseudo_id: Option<usize>| -> BlockPseudoImageSlot {
        let id = pseudo_id?;
        let pseudo = doc.get_node(id)?;
        if !is_block_pseudo(pseudo) {
            return None;
        }
        if is_absolutely_positioned(pseudo) {
            return None;
        }
        let entry = build_pseudo_image_entry(pseudo, parent_cb.width, parent_cb.height, assets)?;
        Some((id, entry))
    };
    (load(parent.before), load(parent.after))
}

/// Build an `InlineImage` for a `::before`/`::after` pseudo whose
/// computed `content` resolves to a single `url(...)` image and whose
/// `display` is NOT block-outside (i.e. it is inline).
pub(super) fn build_inline_pseudo_image(
    pseudo_node: &Node,
    parent_content_width: f32,
    parent_content_height: f32,
    assets: Option<&AssetBundle>,
) -> Option<InlineImage> {
    let assets = assets?;
    let raw_url = crate::blitz_adapter::extract_content_image_url(pseudo_node)?;
    let asset_name = extract_asset_name(&raw_url);
    let data = Arc::clone(assets.get_image(asset_name)?);
    let format = ImagePageable::detect_format(&data)?;

    let styles = pseudo_node.primary_styles()?;
    let css_w = resolve_pseudo_size(&styles.clone_width(), parent_content_width);
    let css_h = resolve_pseudo_size(&styles.clone_height(), parent_content_height);
    let (w, h) = resolve_image_dimensions(&data, format, css_w, css_h);
    let (opacity, visible) = extract_opacity_visible(pseudo_node);
    let vertical_align = crate::blitz_adapter::extract_vertical_align(pseudo_node);
    Some(InlineImage {
        data,
        format,
        width: w,
        height: h,
        x_offset: 0.0,
        vertical_align,
        opacity,
        visible,
        computed_y: 0.0,
        link: None,
    })
}

/// Populate the `link` field on an `InlineImage` built for a pseudo-element
/// whose real originating node is `origin_node_id`.
pub(super) fn attach_link_to_inline_image(
    img: &mut InlineImage,
    doc: &BaseDocument,
    origin_node_id: usize,
) {
    if let Some((_, span)) = inline_root::resolve_enclosing_anchor(doc, origin_node_id) {
        img.link = Some(Arc::new(span));
    }
}

/// Inject an inline pseudo image at the start (::before) and/or end
/// (::after) of the shaped lines. Mirrors v1.
pub(super) fn inject_inline_pseudo_images(
    lines: &mut [ShapedLine],
    before: Option<InlineImage>,
    after: Option<InlineImage>,
) {
    if let Some(mut img) = before {
        if let Some(first_line) = lines.first_mut() {
            let shift = img.width;
            for item in &mut first_line.items {
                match item {
                    LineItem::Text(run) => run.x_offset += shift,
                    LineItem::Image(i) => i.x_offset += shift,
                    LineItem::InlineBox(ib) => ib.x_offset += shift,
                }
            }
            img.x_offset = 0.0;
            first_line.items.insert(0, LineItem::Image(img));
        }
    }
    if let Some(mut img) = after {
        if let Some(last_line) = lines.last_mut() {
            let last_end = last_line
                .items
                .iter()
                .map(|item| match item {
                    LineItem::Text(run) => {
                        run.x_offset
                            + run
                                .glyphs
                                .iter()
                                .map(|g| g.x_advance * run.font_size)
                                .sum::<f32>()
                    }
                    LineItem::Image(i) => i.x_offset + i.width,
                    LineItem::InlineBox(ib) => ib.x_offset + ib.width,
                })
                .fold(0.0_f32, f32::max);
            img.x_offset = last_end;
            last_line.items.push(LineItem::Image(img));
        }
    }
}

/// Resolve a stylo `Size` (`width` / `height`) to an absolute `f32` in pt,
/// or `None` for `auto` and intrinsic keywords.
fn resolve_pseudo_size(size: &::style::values::computed::Size, parent_width: f32) -> Option<f32> {
    use ::style::values::computed::Length;
    use ::style::values::generics::length::GenericSize;
    match size {
        GenericSize::LengthPercentage(lp) => {
            let basis_px = pt_to_px(parent_width);
            Some(px_to_pt(lp.0.resolve(Length::new(basis_px)).px()))
        }
        _ => None,
    }
}
