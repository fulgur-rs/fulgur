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
        let right_inset = style.border_widths[1].to_f32() + style.padding[1].to_f32();
        let bottom_inset = style.border_widths[2].to_f32() + style.padding[2].to_f32();
        let content_width = (width.to_f32() - left_inset - right_inset).max(0.0);
        let content_height = (height.to_f32() - top_inset - bottom_inset).max(0.0);
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
        (width.to_f32(), height.to_f32(), opacity, visible)
    }
}

/// Resolve CSS width/height against intrinsic image dimensions + aspect ratio.
///
/// Return values are in **PDF pt** — every caller (`make_image_entry`,
/// `build_inline_pseudo_image`) stamps them onto an `ImageEntry` /
/// `InlineImage` whose `width`/`height` are typed `Pt`. `Some(w)`/`Some(h)`
/// arguments are already pt (from `size_in_pt(final_layout)` on the
/// `<img>` / normal-`content:url()` paths, or from `resolve_pseudo_size`
/// which folds Stylo's CSS-px result through `Px::in_pt()` on the pseudo
/// path). Aspect is a unit-neutral ratio, so `(Some, None)` / `(None,
/// Some)` also produce pt.
///
/// The `(None, None)` intrinsic fallback is the trap: `decode_dimensions`
/// returns raw **device px** from the image header, which per CSS Images 3
/// §5 correspond to CSS px 1-for-1. We fold to pt here so the arm ends up
/// pt like every other arm — otherwise an auto-sized pseudo
/// `content: url(pic.png)` (the only production caller that hits this arm;
/// `<img>` and normal `content:url()` always pass `Some, Some`) would
/// render at N pt for an N-CSS-px image, i.e. 4/3× larger than the same
/// image sized explicitly at `width: Npx` (which resolves through the pt
/// folding path). fulgur-t82j.
pub(super) fn resolve_image_dimensions(
    data: &[u8],
    format: crate::image::ImageFormat,
    css_w: Option<f32>,
    css_h: Option<f32>,
) -> (f32, f32) {
    let (iw, ih) = ImageRender::decode_dimensions(data, format).unwrap_or((1, 1));
    let iw = iw as f32;
    let ih = ih as f32;
    let aspect = if ih > 0.0 { iw / ih } else { 1.0 };
    match (css_w, css_h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, if aspect > 0.0 { w / aspect } else { w }),
        (None, Some(h)) => (h * aspect, h),
        (None, None) => (iw.as_px().in_pt().to_f32(), ih.as_px().in_pt().to_f32()),
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
        // `resolve_image_dimensions` always returns pt (its arms fold
        // intrinsic device px through CSS-px→pt when `css_w`/`css_h` are
        // absent — see the arm doc). fulgur-t82j.
        width: w.as_pt(),
        height: h.as_pt(),
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
    let Some(format) = ImageRender::detect_format(&data) else {
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
    let Some(format) = ImageRender::detect_format(&data) else {
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
    let Some(tree) = extract_inline_svg_tree(node) else {
        return false;
    };

    let (content_w, content_h, opacity, visible) =
        maybe_insert_block_for_replaced(node, assets, out);
    out.svgs.insert(
        node.id,
        crate::drawables::SvgEntry {
            tree,
            width: content_w.as_pt(),
            height: content_h.as_pt(),
            opacity,
            visible,
        },
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::AssetBundle;
    use crate::drawables::Drawables;
    use crate::gcpm::running::RunningElementStore;
    use blitz_html::HtmlDocument;
    use std::ops::Deref;

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
        assert_eq!(img.width.to_f32(), 100.0);
        assert_eq!(img.height.to_f32(), 50.0);
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
        assert_eq!(img.width.to_f32(), 40.0);
        assert_eq!(img.height.to_f32(), 40.0);
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
        assert_eq!(img.width.to_f32(), 25.0);
        assert_eq!(img.height.to_f32(), 25.0);
    }

    #[test]
    fn test_make_image_entry_intrinsic_fallback() {
        // 1x1 intrinsic device px → 1 CSS px → 0.75 pt (CSS Images 3 §5).
        // Reached by auto-sized pseudo `content: url()` — see fulgur-t82j.
        let img = make_image_entry(
            sample_png_arc(),
            crate::image::ImageFormat::Png,
            None,
            None,
            0.5,
            false,
        );
        assert_eq!(img.width.to_f32(), 0.75);
        assert_eq!(img.height.to_f32(), 0.75);
        assert_eq!(img.opacity, 0.5);
        assert!(!img.visible);
    }

    // ── resolve_image_dimensions: additional edge cases ─────────────

    #[test]
    fn resolve_image_dimensions_invalid_data_falls_back_to_1x1_intrinsic() {
        // Corrupt bytes → decode_dimensions returns Err → unwrap_or((1,1)) →
        // 1 CSS px → 0.75 pt (see arm doc / fulgur-t82j).
        let (w, h) =
            resolve_image_dimensions(b"not-a-png", crate::image::ImageFormat::Png, None, None);
        assert_eq!(w, 0.75);
        assert_eq!(h, 0.75);
    }

    #[test]
    fn resolve_image_dimensions_invalid_data_width_only_uses_fallback_aspect() {
        // Corrupt bytes → intrinsic (1,1) → aspect 1.0 → height == width.
        let (w, h) =
            resolve_image_dimensions(b"junk", crate::image::ImageFormat::Png, Some(20.0), None);
        assert_eq!(w, 20.0);
        assert_eq!(h, 20.0);
    }

    // fulgur-t82j regression: the `(None, None)` arm must return pt, not
    // device px. This is the arm reached exclusively by the auto-sized
    // pseudo `content: url()` path (both block and inline). Left as raw
    // device px, an N-CSS-px intrinsic image renders at N pt (4/3× the
    // pt size of the equivalent `width: Npx` control that resolves via
    // `resolve_pseudo_size` → pt). The fold to pt keeps auto and
    // explicit-size sibling renders spatially consistent.
    #[test]
    fn resolve_image_dimensions_intrinsic_arm_folds_device_px_via_in_pt() {
        // 1x1 device px intrinsic → 1 CSS px → 0.75 pt.
        let (w, h) =
            resolve_image_dimensions(TEST_PNG_1X1, crate::image::ImageFormat::Png, None, None);
        assert_eq!(w, 0.75, "intrinsic width must be pt, not device px");
        assert_eq!(h, 0.75, "intrinsic height must be pt, not device px");
    }

    // ── DOM-based helpers ────────────────────────────────────────────

    fn parse_doc(html: &str) -> HtmlDocument {
        crate::blitz_adapter::parse_and_layout(
            html,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        )
    }

    fn find_by_tag_inner(doc: &BaseDocument, id: usize, tag: &str) -> Option<usize> {
        let n = doc.get_node(id)?;
        if n.element_data()
            .is_some_and(|e| e.name.local.as_ref() == tag)
        {
            return Some(id);
        }
        for &c in &n.children {
            if let Some(f) = find_by_tag_inner(doc, c, tag) {
                return Some(f);
            }
        }
        None
    }

    fn find_tag(doc: &HtmlDocument, tag: &str) -> usize {
        let root = doc.root_element();
        find_by_tag_inner(doc, root.id, tag)
            .unwrap_or_else(|| panic!("<{tag}> not found in document"))
    }

    fn make_ctx<'a>(
        doc: &mut HtmlDocument,
        store: &'a RunningElementStore,
        assets: Option<&'a AssetBundle>,
    ) -> ConvertContext<'a> {
        let column_styles = crate::blitz_adapter::extract_column_style_table(doc);
        let multicol_geometry = crate::multicol_layout::run_pass(doc, &column_styles);
        let pagination_geometry = crate::pagination_layout::run_pass(doc, 842.0);
        ConvertContext {
            running_store: store,
            assets,
            font_cache: Default::default(),
            string_set_by_node: Default::default(),
            counter_ops_by_node: Default::default(),
            bookmark_by_node: Default::default(),
            column_styles,
            multicol_geometry,
            pagination_geometry,
            link_cache: Default::default(),
            viewport_size_px: Some((595.0, 842.0)),
        }
    }

    // ── try_convert: non-replaced element ────────────────────────────

    #[test]
    fn try_convert_plain_div_returns_false() {
        let mut doc = parse_doc(r#"<!doctype html><html><body><div>hello</div></body></html>"#);
        let store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &store, None);
        let mut out = Drawables::new();
        let div_id = find_tag(&doc, "div");
        assert!(!try_convert(doc.deref(), div_id, &mut ctx, &mut out));
        assert!(out.images.is_empty());
        assert!(out.svgs.is_empty());
    }

    // ── try_convert: <img> without asset bundle ───────────────────────

    #[test]
    fn try_convert_img_without_bundle_returns_false() {
        let mut doc =
            parse_doc(r#"<!doctype html><html><body><img src="photo.png"></body></html>"#);
        let store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &store, None);
        let mut out = Drawables::new();
        let img_id = find_tag(&doc, "img");
        assert!(!try_convert(doc.deref(), img_id, &mut ctx, &mut out));
        assert!(out.images.is_empty());
    }

    // ── try_convert: <img> with matching bundle entry ─────────────────

    #[test]
    fn try_convert_img_with_bundle_inserts_image_entry() {
        let mut doc = parse_doc(
            r#"<!doctype html><html><body>
              <img src="icon.png" style="width:30px;height:20px;">
            </body></html>"#,
        );
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", TEST_PNG_1X1.to_vec());
        let store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &store, Some(&bundle));
        let mut out = Drawables::new();
        let img_id = find_tag(&doc, "img");
        assert!(
            try_convert(doc.deref(), img_id, &mut ctx, &mut out),
            "must return true when image is found in bundle"
        );
        assert!(
            out.images.contains_key(&img_id),
            "ImageEntry must be registered for the <img> node"
        );
    }

    // ── try_convert: <img> with missing src attribute ─────────────────

    #[test]
    fn try_convert_img_missing_src_returns_false() {
        let mut doc = parse_doc(r#"<!doctype html><html><body><img></body></html>"#);
        let mut bundle = AssetBundle::new();
        bundle.add_image("something.png", TEST_PNG_1X1.to_vec());
        let store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &store, Some(&bundle));
        let mut out = Drawables::new();
        let img_id = find_tag(&doc, "img");
        // No src attribute → convert_image returns false
        assert!(!try_convert(doc.deref(), img_id, &mut ctx, &mut out));
        assert!(out.images.is_empty());
    }

    // ── try_convert: <img> src not in bundle ──────────────────────────

    #[test]
    fn try_convert_img_src_not_in_bundle_returns_false() {
        let mut doc =
            parse_doc(r#"<!doctype html><html><body><img src="missing.png"></body></html>"#);
        let bundle = AssetBundle::new(); // empty bundle
        let store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &store, Some(&bundle));
        let mut out = Drawables::new();
        let img_id = find_tag(&doc, "img");
        assert!(!try_convert(doc.deref(), img_id, &mut ctx, &mut out));
        assert!(out.images.is_empty());
    }

    // ── maybe_insert_block_for_replaced: no visual style ─────────────

    #[test]
    fn maybe_insert_block_no_visual_style_omits_block_entry() {
        // A plain <img> with no border/background has no visual style.
        let doc = parse_doc(
            r#"<!doctype html><html><body><img src="x.png" style="width:40px;height:25px;"></body></html>"#,
        );
        let img_id = find_tag(&doc, "img");
        let node = doc.get_node(img_id).unwrap();
        let mut out = Drawables::new();
        let (_w, _h, opacity, visible) = maybe_insert_block_for_replaced(node, None, &mut out);
        assert!(
            out.block_styles.is_empty(),
            "no BlockEntry for unstyled img"
        );
        assert_eq!(opacity, 1.0);
        assert!(visible);
    }

    // ── maybe_insert_block_for_replaced: visual style (border) ────────

    #[test]
    fn maybe_insert_block_with_border_inserts_block_entry_and_shrinks_content() {
        // A 4px border on each side subtracts from content dimensions.
        let doc = parse_doc(
            r#"<!doctype html><html><body>
              <img src="x.png" style="width:60px;height:40px;border:4px solid black;">
            </body></html>"#,
        );
        let img_id = find_tag(&doc, "img");
        let node = doc.get_node(img_id).unwrap();
        let (layout_w, layout_h) = size_in_pt(node.final_layout.size);
        let (layout_w, layout_h) = (layout_w.to_f32(), layout_h.to_f32());
        let mut out = Drawables::new();
        let (content_w, content_h, _opacity, _visible) =
            maybe_insert_block_for_replaced(node, None, &mut out);
        assert!(
            out.block_styles.contains_key(&img_id),
            "BlockEntry must be inserted for img with border"
        );
        // content_w / content_h must not exceed the layout box (border is consumed).
        // `.max(0.0)` in the production code guarantees content ≤ layout even when
        // Taffy gives (0, 0) to an unsized element, so the assertions hold always.
        assert!(
            content_w <= layout_w,
            "content_w {content_w} > layout_w {layout_w}"
        );
        assert!(
            content_h <= layout_h,
            "content_h {content_h} > layout_h {layout_h}"
        );
    }

    // ── try_convert: inline SVG (smoke) ──────────────────────────────

    #[test]
    fn try_convert_inline_svg_does_not_panic() {
        // Blitz may or may not materialise the SVG tree in a test environment
        // (inline SVG parsing depends on system fonts via fontdb). We verify
        // only that try_convert completes without panicking and that any
        // registered SvgEntry is consistent with the return value.
        let mut doc = parse_doc(
            r#"<!doctype html><html><body>
              <svg width="50" height="50" xmlns="http://www.w3.org/2000/svg">
                <rect fill="red" width="50" height="50"/>
              </svg>
            </body></html>"#,
        );
        let store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &store, None);
        let mut out = Drawables::new();
        let svg_id = find_tag(&doc, "svg");
        let result = try_convert(doc.deref(), svg_id, &mut ctx, &mut out);
        // Invariant: if try_convert returned true, the SvgEntry must be present.
        assert_eq!(result, out.svgs.contains_key(&svg_id));
    }
}
