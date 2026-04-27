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
    use ::style::values::specified::box_::DisplayOutside;
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
fn resolve_pseudo_size(size: &::style::values::computed::Size, parent_width: f32) -> Option<f32> {
    use ::style::values::computed::Length;
    use ::style::values::generics::length::GenericSize;
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

#[cfg(test)]
mod tests {
    use super::super::tests::{collect_images, find_h1, walk_all_children};
    use super::super::{ConvertContext, dom_to_pageable, size_in_pt};
    use crate::asset::AssetBundle;
    use crate::paragraph::ParagraphPageable;
    use std::collections::HashMap;

    #[test]
    fn test_build_pseudo_image_reads_content_url() {
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .expect("read examples/image/icon.png");
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            h1::before {
                content: url("icon.png");
                display: block;
                width: 48px;
                height: 48px;
            }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let h1_id = find_h1(&doc);
        let before_id = doc
            .get_node(h1_id)
            .unwrap()
            .before
            .expect("::before pseudo");
        let pseudo = doc.get_node(before_id).unwrap();
        let (parent_w, parent_h) = size_in_pt(doc.get_node(h1_id).unwrap().final_layout.size);

        let img = super::build_pseudo_image(pseudo, parent_w, parent_h, Some(&bundle))
            .expect("build_pseudo_image should return Some for content: url()");
        // 48 CSS px × 0.75 = 36 pt
        assert_eq!(img.width, 36.0);
        assert_eq!(img.height, 36.0);
    }

    #[test]
    fn test_build_pseudo_image_width_only_uses_intrinsic_aspect() {
        // icon.png is 32x32 so aspect = 1.0. width:20px → 15 pt, height
        // back-propagates via intrinsic aspect → 15 pt.
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
            h1::before { content: url("icon.png"); display: block; width: 20px; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.unwrap();
        let pseudo = doc.get_node(before_id).unwrap();
        let (parent_w, parent_h) = size_in_pt(doc.get_node(h1_id).unwrap().final_layout.size);

        let img = super::build_pseudo_image(pseudo, parent_w, parent_h, Some(&bundle)).unwrap();
        assert_eq!(img.width, 15.0);
        assert_eq!(img.height, 15.0);
    }

    #[test]
    fn test_build_pseudo_image_missing_asset_returns_none() {
        let bundle = AssetBundle::new();
        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("missing.png"); display: block; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.unwrap();
        let pseudo = doc.get_node(before_id).unwrap();
        assert!(
            super::build_pseudo_image(pseudo, 800.0, 600.0, Some(&bundle)).is_none(),
            "missing asset should silently return None"
        );
    }

    #[test]
    fn test_build_pseudo_image_no_assets_returns_none() {
        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("icon.png"); }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.unwrap();
        let pseudo = doc.get_node(before_id).unwrap();
        assert!(super::build_pseudo_image(pseudo, 800.0, 600.0, None).is_none());
    }

    #[test]
    fn test_build_pseudo_image_height_percent_resolves_against_parent_height() {
        // Verifies the coderabbit fix: height: 50% on the pseudo should
        // resolve against parent_content_height, not parent_content_width.
        // icon.png is 32x32 intrinsic, so with height=50% of 200 = 100 and
        // no explicit width, the aspect ratio gives width = 100.
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
            h1::before { content: url("icon.png"); display: block; height: 50%; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.unwrap();
        let pseudo = doc.get_node(before_id).unwrap();

        // Explicitly call with distinguishable width (400) and height (200)
        // so we can verify which basis is used for `height: 50%`.
        let img = super::build_pseudo_image(pseudo, 400.0, 200.0, Some(&bundle)).unwrap();
        assert_eq!(
            img.height, 100.0,
            "height: 50% should resolve against parent_content_height (200.0)"
        );
        assert_eq!(
            img.width, 100.0,
            "intrinsic aspect (1:1) should give width = height"
        );
    }

    #[test]
    fn test_build_inline_pseudo_image_returns_some_for_inline_pseudo() {
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .expect("icon.png fixture must exist");
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("icon.png"); width: 24px; height: 24px; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.expect("::before");
        let pseudo = doc.get_node(before_id).unwrap();

        // Inline pseudos have display: inline by default (not block)
        assert!(
            !super::is_block_pseudo(pseudo),
            "pseudo should be inline by default"
        );

        let img = super::build_inline_pseudo_image(pseudo, 800.0, 600.0, Some(&bundle));
        assert!(img.is_some(), "should return Some for inline pseudo");
        let img = img.unwrap();
        // 24 CSS px × 0.75 = 18 pt
        assert_eq!(img.width, 18.0);
        assert_eq!(img.height, 18.0);
    }

    #[test]
    fn test_build_inline_pseudo_image_does_not_filter_display() {
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .expect("icon.png fixture must exist");
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("icon.png"); display: block; width: 24px; height: 24px; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.expect("::before");
        let pseudo = doc.get_node(before_id).unwrap();

        assert!(
            super::is_block_pseudo(pseudo),
            "pseudo with display:block should be block"
        );

        // The inline builder should NOT produce an image for block pseudos
        // (the caller filters with !is_block_pseudo, but we verify the function
        // itself still returns Some — the filtering is done at the call site)
        // Here we verify the function works, the call-site filter is tested
        // by the integration test above.
        let img = super::build_inline_pseudo_image(pseudo, 800.0, 600.0, Some(&bundle));
        // build_inline_pseudo_image doesn't check display, so this will be Some.
        // The call site filters with !is_block_pseudo.
        assert!(
            img.is_some(),
            "build_inline_pseudo_image itself doesn't filter display"
        );
    }

    #[test]
    fn test_dom_to_pageable_emits_block_pseudo_image() {
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
            .wrap::before {
                content: url("icon.png");
                display: block;
                width: 24px;
                height: 24px;
            }
        </style></head><body><div class="wrap">hello</div></body></html>"#;

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
            "expected an 18x18 pt ImagePageable (24 CSS px × 0.75) from ::before pseudo, got {:?}",
            images
        );
    }

    #[test]
    fn test_dom_to_pageable_emits_inline_pseudo_image_as_line_item() {
        // An inline `::before` with `content: url()` should be injected into
        // the host paragraph's first line as a `LineItem::Image` (not as a
        // standalone `ImagePageable`). This intentionally inspects line items
        // directly: `collect_images` only finds `ImagePageable` and would pass
        // vacuously here, hiding regressions where the inline image is dropped.
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
            p::before { content: url("icon.png"); width: 10px; height: 10px; }
        </style></head><body><p>hello</p></body></html>"#;

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

        // Sanity: no standalone ImagePageable was emitted (block-pseudo path
        // didn't fire because `display` defaults to inline for pseudos).
        let mut block_images = Vec::new();
        collect_images(&*tree, &mut block_images);
        assert!(
            block_images.is_empty(),
            "inline pseudo must not surface as standalone ImagePageable; got {:?}",
            block_images
        );

        // Walk every paragraph in the tree and collect inline image dimensions.
        let mut line_images: Vec<(f32, f32)> = Vec::new();
        walk_all_children(&*tree, &mut |p| {
            if let Some(para) = p.as_any().downcast_ref::<ParagraphPageable>() {
                for line in &para.lines {
                    for item in &line.items {
                        if let LineItem::Image(img) = item {
                            line_images.push((img.width, img.height));
                        }
                    }
                }
            }
        });
        // 10 CSS px × 0.75 = 7.5 pt for both dimensions.
        assert!(
            line_images
                .iter()
                .any(|(w, h)| (*w - 7.5).abs() < 1e-3 && (*h - 7.5).abs() < 1e-3),
            "expected a 7.5×7.5 pt LineItem::Image from p::before; got {:?}",
            line_images
        );
    }

    #[test]
    fn test_dom_to_pageable_emits_pseudo_on_childless_element() {
        // Regression for Devin Review comment on PR #70: the children.is_empty()
        // branch used to skip pseudo injection. `<div class="icon"></div>` with
        // a block pseudo should still render the image.
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
            .icon::before {
                content: url("icon.png");
                display: block;
                width: 16px;
                height: 16px;
            }
        </style></head><body><div class="icon"></div></body></html>"#;
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
            images.iter().any(|(w, h)| *w == 12.0 && *h == 12.0),
            "childless element ::before pseudo should emit a 12x12 pt image (16 CSS px × 0.75); got {:?}",
            images
        );
    }

    #[test]
    fn test_dom_to_pageable_emits_pseudo_on_zero_size_block_leaf() {
        // Regression for coderabbit follow-up on PR #70: a 0x0 block leaf
        // was being skipped by the collect_positioned_children zero-size
        // leaf filter BEFORE reaching the convert_node `children.is_empty()`
        // branch. The pseudo probe (`node_has_block_pseudo_image`) now lets
        // such leaves fall through.
        //
        // Scope note: this test specifically targets a BLOCK element with
        // explicit width:0;height:0 that still has a ::before image — e.g.
        // a decorative sentinel div a template sets to 0x0 with a pseudo
        // icon. Inline `<span>` with a block ::before is a different
        // edge case that requires Phase 2 inline-flow handling.
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
            .zero { display: block; width: 0; height: 0; }
            .zero::before {
                content: url("icon.png");
                display: block;
                width: 18px;
                height: 18px;
            }
        </style></head><body><section><div class="zero"></div></section></body></html>"#;
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
        walk_all_children(&*tree, &mut |p| collect_images(p, &mut images));
        assert!(
            images.iter().any(|(w, h)| *w == 13.5 && *h == 13.5),
            "zero-size block leaf with block pseudo should emit a 13.5x13.5 pt image (18 CSS px × 0.75); got {:?}",
            images
        );
    }

    #[test]
    fn test_dom_to_pageable_emits_pseudo_on_list_item_with_text() {
        // Regression for Devin Review comment on PR #70: the list item
        // inline-root body path used to skip pseudo injection. A <li> with
        // inline text content and a block ::before should still render.
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
            li::before {
                content: url("icon.png");
                display: block;
                width: 12px;
                height: 12px;
            }
        </style></head><body><ul><li>item text</li></ul></body></html>"#;
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
        walk_all_children(&*tree, &mut |p| collect_images(p, &mut images));
        assert!(
            images.iter().any(|(w, h)| *w == 9.0 && *h == 9.0),
            "list item with text + block pseudo should emit a 9x9 pt image (12 CSS px × 0.75); got {:?}",
            images
        );
    }

    // ---- inline pseudo image inject tests ----

    use super::super::tests::sample_png_arc;
    use crate::image::ImageFormat;
    use crate::paragraph::{
        InlineImage, LineItem, ShapedGlyph, ShapedGlyphRun, ShapedLine, TextDecoration,
        VerticalAlign,
    };

    fn make_test_inline_image(w: f32, h: f32) -> InlineImage {
        InlineImage {
            data: sample_png_arc(),
            format: ImageFormat::Png,
            width: w,
            height: h,
            x_offset: 0.0,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: 0.0,
            link: None,
        }
    }

    fn make_test_text_run(x_offset: f32, advance: f32) -> ShapedGlyphRun {
        ShapedGlyphRun {
            font_data: sample_png_arc(), // dummy — not rendered in unit tests
            font_index: 0,
            font_size: 12.0,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![ShapedGlyph {
                id: 0,
                x_advance: advance / 12.0, // normalized by font_size
                x_offset: 0.0,
                y_offset: 0.0,
                text_range: 0..1,
            }],
            text: "A".to_string(),
            x_offset,
            link: None,
        }
    }

    #[test]
    fn test_inject_before_shifts_existing_items() {
        let run = make_test_text_run(0.0, 60.0);
        let mut lines = vec![ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::Text(run)],
        }];
        let img = make_test_inline_image(20.0, 16.0);
        super::inject_inline_pseudo_images(&mut lines, Some(img), None);

        assert_eq!(lines[0].items.len(), 2);
        // First item should be the image at x_offset 0
        if let LineItem::Image(ref i) = lines[0].items[0] {
            assert!((i.x_offset).abs() < 0.01, "img x_offset={}", i.x_offset);
            assert!((i.width - 20.0).abs() < 0.01);
        } else {
            panic!("expected Image at index 0");
        }
        // Second item (text) should be shifted by 20.0
        if let LineItem::Text(ref r) = lines[0].items[1] {
            assert!(
                (r.x_offset - 20.0).abs() < 0.01,
                "text x_offset={}",
                r.x_offset,
            );
        } else {
            panic!("expected Text at index 1");
        }
    }

    #[test]
    fn test_inject_after_appends_at_end() {
        let run = make_test_text_run(0.0, 60.0);
        let mut lines = vec![ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::Text(run)],
        }];
        let img = make_test_inline_image(15.0, 16.0);
        super::inject_inline_pseudo_images(&mut lines, None, Some(img));

        assert_eq!(lines[0].items.len(), 2);
        // Last item should be the image
        if let LineItem::Image(ref i) = lines[0].items[1] {
            // Text run width = advance (normalized x_advance * font_size) = (60/12) * 12 = 60
            assert!(
                (i.x_offset - 60.0).abs() < 0.01,
                "after img x_offset={}",
                i.x_offset,
            );
        } else {
            panic!("expected Image at index 1");
        }
    }

    #[test]
    fn test_inject_both_before_and_after() {
        let run = make_test_text_run(0.0, 36.0);
        let mut lines = vec![ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::Text(run)],
        }];
        let before = make_test_inline_image(10.0, 16.0);
        let after = make_test_inline_image(10.0, 16.0);
        super::inject_inline_pseudo_images(&mut lines, Some(before), Some(after));

        assert_eq!(lines[0].items.len(), 3);
        // Before image at 0
        if let LineItem::Image(ref i) = lines[0].items[0] {
            assert!((i.x_offset).abs() < 0.01);
        }
        // Text shifted by 10
        if let LineItem::Text(ref r) = lines[0].items[1] {
            assert!((r.x_offset - 10.0).abs() < 0.01);
        }
        // After image at 10 (before width) + 36 (text width) = 46
        if let LineItem::Image(ref i) = lines[0].items[2] {
            assert!(
                (i.x_offset - 46.0).abs() < 0.01,
                "after x_offset={}",
                i.x_offset,
            );
        }
    }
}
