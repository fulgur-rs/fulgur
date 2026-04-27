use super::inline_root;
use super::positioned::{
    AbsCb, build_absolute_pseudo_children, is_absolutely_positioned,
    try_build_absolute_pseudo_image,
};
use super::replaced::{make_image_pageable, resolve_image_dimensions};
use super::*;

/// Build an `ImagePageable` for a `::before`/`::after` pseudo-element node
/// whose computed `content` resolves to a single `url(...)` image.
///
/// Returns `None` if:
///
/// - `assets` is `None`
/// - the pseudo's computed content is not a single image URL
/// - the URL cannot be resolved in the `AssetBundle` (silent skip — matches
///   background-image handling in `extract_block_style`)
/// - the image format is unsupported by `ImagePageable::detect_format`
///
/// `parent_content_width` / `parent_content_height` are the content-box
/// dimensions of the pseudo's containing block, used to resolve percentage
/// `width` / `height` on the pseudo itself. Passing the values separately
/// (instead of a single `parent_size`) ensures `height: 50%` resolves
/// against the parent height, not the parent width — which was the bug
/// flagged by coderabbit in PR #70.
pub(super) fn build_pseudo_image(
    pseudo_node: &Node,
    parent_content_width: f32,
    parent_content_height: f32,
    assets: Option<&AssetBundle>,
) -> Option<ImagePageable> {
    let assets = assets?;

    let raw_url = crate::blitz_adapter::extract_content_image_url(pseudo_node)?;
    let asset_name = extract_asset_name(&raw_url);
    let data = Arc::clone(assets.get_image(asset_name)?);
    let format = ImagePageable::detect_format(&data)?;

    // Read computed CSS width / height on the pseudo-element itself. Blitz
    // does not propagate these to `final_layout` for pseudos that lack a text
    // child, so we must go directly to stylo.
    let styles = pseudo_node.primary_styles()?;
    let css_w = resolve_pseudo_size(&styles.clone_width(), parent_content_width);
    let css_h = resolve_pseudo_size(&styles.clone_height(), parent_content_height);

    let (opacity, visible) = extract_opacity_visible(pseudo_node);
    Some(make_image_pageable(
        data, format, css_w, css_h, opacity, visible,
    ))
}

/// True iff the pseudo-element has `display: block` outside.
///
/// Phase 1 only emits pseudo images for block-outside pseudos. Inline pseudos
/// fall through to Phase 2 work (tracked separately) where the image has to
/// be injected into `ParagraphPageable`'s line layout.
pub(super) fn is_block_pseudo(pseudo: &Node) -> bool {
    use style::values::specified::box_::DisplayOutside;
    pseudo
        .primary_styles()
        .is_some_and(|s| s.clone_display().outside() == DisplayOutside::Block)
}

/// Effective `(width, height)` of `pseudo` in CSS px, for inset resolution.
///
/// Taffy's `final_layout.size` is `(0, 0)` for textless `content:url(...)`
/// pseudos (Blitz limitation documented in `build_pseudo_image`). Naively
/// using it for `right` / `bottom` resolution makes the pseudo land at
/// `cb_w - 0 - r = cb_w - r` instead of `cb_w - img_w - r`, shifting the
/// pseudo by its own width.
///
/// We mirror the same shortcut `build_absolute_pseudo_child` takes for the
/// child Pageable so the inset basis matches the rendered size. For pseudos
/// where the shortcut does not apply (text content, visual style + content
/// url, etc.), Taffy's `final_layout.size` is reliable and we use it
/// directly.
pub(super) fn effective_pseudo_size_px(
    pseudo: &Node,
    parent: &Node,
    cb: Option<AbsCb>,
    assets: Option<&AssetBundle>,
) -> (f32, f32) {
    let layout = pseudo.final_layout.size;
    if layout.width > 0.0 || layout.height > 0.0 {
        return (layout.width, layout.height);
    }
    if let Some(img) = try_build_absolute_pseudo_image(pseudo, parent, cb, assets) {
        return (pt_to_px(img.width), pt_to_px(img.height));
    }
    (layout.width, layout.height)
}

/// Orchestrator that combines block-pseudo-image wrapping with absolute
/// pseudo positioning. Returns `(positioned_children, has_pseudo)` where
/// `has_pseudo` is true if EITHER a block-pseudo image OR an
/// absolutely-positioned pseudo contributed to the child vec.
///
/// Call sites previously did `build_block_pseudo_images` +
/// `wrap_with_block_pseudo_images` back to back and computed `has_pseudo`
/// from the pair of `Option<ImagePageable>`; that two-step is folded here
/// so the absolute-pseudo path is picked up uniformly without duplicating
/// boilerplate at every construction site.
pub(super) fn wrap_with_pseudo_content(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    parent_cb: ContentBox,
    children: Vec<PositionedChild>,
) -> (Vec<PositionedChild>, bool) {
    let (before_img, after_img) = build_block_pseudo_images(doc, node, parent_cb, ctx.assets);
    let has_img_pseudo = before_img.is_some() || after_img.is_some();
    let mut out = wrap_with_block_pseudo_images(before_img, after_img, parent_cb, children);
    let abs = build_absolute_pseudo_children(doc, node, ctx, depth);
    let has_any_pseudo = has_img_pseudo || !abs.is_empty();
    out.extend(abs);
    (out, has_any_pseudo)
}

/// Cheap probe: does `node` have at least one `::before` / `::after` pseudo
/// slot whose computed `content` resolves to a block-display image URL?
///
/// Used by `collect_positioned_children` to opt zero-sized leaves (e.g.
/// `<span class="icon"></span>`) out of its zero-size skip, so the leaf can
/// reach `convert_node_inner`'s `children.is_empty()` branch and emit its
/// pseudo image. Does not resolve the AssetBundle or decode the image — if
/// the asset is missing, `build_block_pseudo_images` later silently skips,
/// which is harmless but slightly wasteful; that trade-off is fine because
/// zero-size elements with `content: url()` are rare.
pub(super) fn node_has_block_pseudo_image(doc: &blitz_dom::BaseDocument, node: &Node) -> bool {
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

/// Returns `true` if `node` has a `::before` or `::after` pseudo-element that
/// is an inline (non-block) pseudo with a `content: url(...)` image.
///
/// Used by the zero-size leaf filter to let elements like
/// `<span class="icon"></span>` with `::before { content: url(...) }` through
/// to `convert_node_inner` where the inline pseudo path can synthesize a
/// `ParagraphPageable`.
pub(super) fn node_has_inline_pseudo_image(doc: &blitz_dom::BaseDocument, node: &Node) -> bool {
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
/// whose computed `position` is `absolute` or `fixed`. Such a pseudo is
/// emitted by `build_absolute_pseudo_children` when the node reaches
/// `convert_node_inner`; we need `collect_positioned_children`'s zero-size
/// leaf / container filter to NOT drop the node on the way there.
///
/// Without this probe, a pattern like
///
/// ```html
/// <style>
///   .marker { position: relative; width: 0; height: 0; }
///   .marker::before {
///     content: ""; position: absolute;
///     width: 8px; height: 8px; background: red;
///   }
/// </style>
/// <div class="marker"></div>
/// ```
///
/// would be skipped by the zero-size-leaf branch of
/// `collect_positioned_children` and the pseudo would never paint.
pub(super) fn node_has_absolute_pseudo(doc: &blitz_dom::BaseDocument, node: &Node) -> bool {
    for pseudo_id in [node.before, node.after].into_iter().flatten() {
        if let Some(pseudo) = doc.get_node(pseudo_id)
            && is_absolutely_positioned(pseudo)
        {
            return true;
        }
    }
    false
}

/// Build `ImagePageable` instances for `::before` and `::after` pseudos on
/// `parent` when their `content` resolves to a single `url(...)` image and
/// their `display` is block-outside. Returns `(before, after)`, either of
/// which may be `None`.
///
/// This is the single walk of the pseudo slots — callers use it both to
/// decide whether to take the `BlockPageable` wrapping path in the
/// inline-root branch and to materialize the children to inject.
///
/// Pseudo sizes resolve against the parent's content-box (`parent_cb.width`
/// for `width`, `parent_cb.height` for `height`), so `width: 50%` and
/// `height: 100%` behave per spec.
///
/// **Known limitation (fulgur-ai3 Phase 1):** Because Blitz assigns a
/// zero-sized layout to text-less pseudo elements, the pseudo image does not
/// push subsequent real children down. Authors can work around this by
/// adding `margin-top` / `margin-bottom` on the first / last real child to
/// reserve space. Properly pushing content will be handled in a follow-up
/// issue that round-trips the synthetic pseudo size through Taffy.
pub(super) fn build_block_pseudo_images(
    doc: &blitz_dom::BaseDocument,
    parent: &Node,
    parent_cb: ContentBox,
    assets: Option<&AssetBundle>,
) -> (Option<ImagePageable>, Option<ImagePageable>) {
    if assets.is_none() {
        return (None, None);
    }
    let load = |pseudo_id: Option<usize>| -> Option<ImagePageable> {
        let pseudo = doc.get_node(pseudo_id?)?;
        if !is_block_pseudo(pseudo) {
            return None;
        }
        // Absolutely-positioned pseudos are handled by
        // `build_absolute_pseudo_children`. CSS §9.7 blockifies them, so
        // `is_block_pseudo` is true even with `position: absolute`, and
        // without this guard a pseudo with both `content: url(...)` and
        // `position: absolute` would be emitted twice (once as an
        // `ImagePageable` here and once via the absolute path).
        if is_absolutely_positioned(pseudo) {
            return None;
        }
        build_pseudo_image(pseudo, parent_cb.width, parent_cb.height, assets)
    };
    (load(parent.before), load(parent.after))
}

/// Prepend / append block pseudo images around `children`. `::before` lands
/// at the content-box top-left `(origin_x, origin_y)` and `::after` at the
/// content-box bottom-left `(origin_x, origin_y + height)`.
///
/// This returns a new vec instead of mutating in place so `::before` does
/// not trigger an O(n) shift on large child lists.
pub(super) fn wrap_with_block_pseudo_images(
    before: Option<ImagePageable>,
    after: Option<ImagePageable>,
    parent_cb: ContentBox,
    children: Vec<PositionedChild>,
) -> Vec<PositionedChild> {
    let mut out = Vec::with_capacity(children.len() + 2);
    if let Some(img) = before {
        out.push(PositionedChild {
            child: Box::new(img),
            x: parent_cb.origin_x,
            y: parent_cb.origin_y,
        });
    }
    out.extend(children);
    if let Some(img) = after {
        out.push(PositionedChild {
            child: Box::new(img),
            x: parent_cb.origin_x,
            y: parent_cb.origin_y + parent_cb.height,
        });
    }
    out
}

/// Build an `InlineImage` for a `::before`/`::after` pseudo-element whose
/// computed `content` resolves to a single `url(...)` image and whose
/// `display` is NOT block-outside (i.e. it is inline, the CSS default for
/// pseudo-elements).
///
/// Returns `None` under the same conditions as `build_pseudo_image`.
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
/// whose real originating node is `origin_node_id` (typically the pseudo's
/// parent — the element that owns `::before` / `::after`). If that node is
/// enclosed by an `<a href>` ancestor, attach a fresh `LinkSpan`.
///
/// We build a fresh `LinkSpan` here rather than sharing through the
/// `extract_paragraph` cache because pseudo images are injected into the
/// paragraph's line vector by callers, not emitted from within the glyph-run
/// loop — they live on a separate control-flow path. Rect-dedup in a later
/// task will be keyed on the LinkTarget+alt_text payload for pseudo images,
/// not on Arc identity, and this is fine because most anchors contain at
/// most one pseudo image.
pub(super) fn attach_link_to_inline_image(
    img: &mut InlineImage,
    doc: &blitz_dom::BaseDocument,
    origin_node_id: usize,
) {
    if let Some((_, span)) = inline_root::resolve_enclosing_anchor(doc, origin_node_id) {
        img.link = Some(Arc::new(span));
    }
}

/// Inject an inline pseudo image at the start (::before) and/or end (::after)
/// of the shaped lines. The ::before image is prepended to the first line and
/// all existing items are shifted right by its width. The ::after image is
/// appended to the last line at the end of existing content.
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

/// Resolve a stylo `Size` (i.e. `width` / `height`) to an absolute `f32` in
/// pt, or `None` if the size is `auto` or one of the intrinsic keywords.
///
/// Percentages resolve against `parent_width` — the containing block width.
/// (Percentage heights on replaced elements technically reference the parent
/// height, but Phase 1 only cares about block-display pseudo icons whose
/// height is typically an explicit px value; using parent_width as the basis
/// for both dimensions is a conscious simplification.)
fn resolve_pseudo_size(size: &style::values::computed::Size, parent_width: f32) -> Option<f32> {
    use style::values::computed::Length;
    use style::values::generics::length::GenericSize;
    match size {
        GenericSize::LengthPercentage(lp) => {
            // Stylo resolves length-percentages in CSS px space: absolute
            // lengths (`48px`) come back as raw px, while percentages scale
            // against whatever basis we hand in. Feeding it a CSS px basis
            // and converting the result to pt keeps both branches consistent
            // with the docstring's "f32 in pt" contract. The caller's basis
            // is already pt (from Pageable tree geometry), so round-trip
            // via pt → px → resolve → pt.
            let basis_px = pt_to_px(parent_width);
            Some(px_to_pt(lp.0.resolve(Length::new(basis_px)).px()))
        }
        // auto / min-content / max-content / fit-content / stretch etc. are
        // all treated as "not specified" here. The `make_image_pageable`
        // helper will fall back to intrinsic dimensions / aspect-ratio.
        _ => None,
    }
}
