use super::inline_root;
use super::positioned::{is_absolutely_positioned, walk_absolute_children};
use super::replaced::{make_image_entry, resolve_image_dimensions};
use super::*;
use crate::units::{F32Units, Px};

/// Build an `ImageEntry` for a `::before`/`::after` pseudo-element node
/// whose computed `content` resolves to a single `url(...)` image.
///
/// Returns `None` under the same conditions as the v1 `build_pseudo_image`.
pub(super) fn build_pseudo_image_entry(
    pseudo_node: &Node,
    parent_content_width: Px,
    parent_content_height: Px,
    assets: Option<&AssetBundle>,
) -> Option<crate::drawables::ImageEntry> {
    let assets = assets?;

    let raw_url = crate::blitz_adapter::extract_content_image_url(pseudo_node)?;
    let asset_name = extract_asset_name(&raw_url);
    let data = Arc::clone(assets.get_image(asset_name)?);
    let format = ImageRender::detect_format(&data)?;

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
    // Probe with O(1) `drawables_total_len` instead of constructing a
    // 6-map `BTreeSet<usize>` snapshot before and after — the snapshot
    // was the dominant residual O(N²) factor inside the per-cell
    // convert walk (`register_pseudo_content` fires once per block-level
    // node, and the snapshot is O(K) where K = total drawables already
    // recorded). Convert never removes entries from these maps, so the
    // length sum is monotonic and a strict inequality is exactly
    // equivalent to "the BTreeSet diff would be non-empty".
    // (fulgur-vrkv)
    let before_total = super::drawables_total_len(out);
    walk_absolute_children(doc, node, ctx, depth, out);
    if super::drawables_total_len(out) > before_total {
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
///
/// Currently unused — kept because it mirrors `node_has_block_pseudo_image`
/// and the v1 container path used both probes. The v2 inline-root path
/// detects inline pseudos directly via `build_inline_pseudo_image` instead.
#[allow(dead_code)]
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
    parent_content_width: Px,
    parent_content_height: Px,
    assets: Option<&AssetBundle>,
) -> Option<InlineImage> {
    let assets = assets?;
    let raw_url = crate::blitz_adapter::extract_content_image_url(pseudo_node)?;
    let asset_name = extract_asset_name(&raw_url);
    let data = Arc::clone(assets.get_image(asset_name)?);
    let format = ImageRender::detect_format(&data)?;

    let styles = pseudo_node.primary_styles()?;
    let css_w = resolve_pseudo_size(&styles.clone_width(), parent_content_width);
    let css_h = resolve_pseudo_size(&styles.clone_height(), parent_content_height);
    let (w, h) = resolve_image_dimensions(&data, format, css_w, css_h);
    let (opacity, visible) = extract_opacity_visible(pseudo_node);
    let vertical_align = crate::blitz_adapter::extract_vertical_align(pseudo_node);
    Some(InlineImage {
        data,
        format,
        width: w.as_pt(),
        height: h.as_pt(),
        x_offset: crate::units::Pt::ZERO,
        vertical_align,
        opacity,
        visible,
        computed_y: crate::units::Pt::ZERO,
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
            img.x_offset = crate::units::Pt::ZERO;
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
                                .sum::<crate::units::Pt>()
                    }
                    LineItem::Image(i) => i.x_offset + i.width,
                    LineItem::InlineBox(ib) => ib.x_offset + ib.width,
                })
                .fold(crate::units::Pt::ZERO, crate::units::Pt::max);
            img.x_offset = last_end;
            last_line.items.push(LineItem::Image(img));
        }
    }
}

/// Resolve a stylo `Size` (`width` / `height`) to an absolute `f32` in pt,
/// or `None` for `auto` and intrinsic keywords.
///
/// `basis` is the containing-block extent (width for `width` / `min-width` /
/// `max-width`, height for the height-axis siblings) in **CSS px** — the
/// layout-space Stylo's `LengthPercentage::resolve` expects. See
/// `.claude/rules/coordinate-system.md` ("Stylo length-percentage
/// resolution"). The return value is in Pt (matches downstream
/// `make_image_entry` / `resolve_image_dimensions`).
fn resolve_pseudo_size(size: &::style::values::computed::Size, basis: Px) -> Option<f32> {
    use ::style::values::computed::Length;
    use ::style::values::generics::length::GenericSize;
    match size {
        GenericSize::LengthPercentage(lp) => Some(
            lp.0.resolve(Length::new(basis.to_f32()))
                .px()
                .as_px()
                .in_pt()
                .to_f32(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::image::ImageFormat;
    use crate::paragraph::{
        InlineBoxItem, InlineImage, LineItem, ShapedGlyph, ShapedGlyphRun, ShapedLine,
        TextDecoration, VerticalAlign,
    };
    use crate::units::F32Units;
    use std::sync::Arc;

    fn make_image(width: f32) -> InlineImage {
        InlineImage {
            data: Arc::new(vec![]),
            format: ImageFormat::Png,
            width: width.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: crate::units::Pt::ZERO,
            link: None,
        }
    }

    fn empty_line() -> ShapedLine {
        ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![],
        }
    }

    fn image_line(x_offset: f32, width: f32) -> ShapedLine {
        ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![LineItem::Image(InlineImage {
                data: Arc::new(vec![]),
                format: ImageFormat::Png,
                width: width.as_pt(),
                height: 10.0_f32.as_pt(),
                x_offset: x_offset.as_pt(),
                vertical_align: VerticalAlign::Baseline,
                opacity: 1.0,
                visible: true,
                computed_y: crate::units::Pt::ZERO,
                link: None,
            })],
        }
    }

    fn approx(a: crate::units::Pt, b: f32) -> bool {
        (a.to_f32() - b).abs() < 0.001
    }

    // ── empty-lines guards ───────────────────────────────────────────────────

    #[test]
    fn no_lines_both_none_is_noop() {
        let mut lines: Vec<ShapedLine> = vec![];
        super::inject_inline_pseudo_images(&mut lines, None, None);
        assert!(lines.is_empty());
    }

    #[test]
    fn no_lines_before_some_is_noop() {
        let mut lines: Vec<ShapedLine> = vec![];
        super::inject_inline_pseudo_images(&mut lines, Some(make_image(20.0)), None);
        assert!(lines.is_empty());
    }

    #[test]
    fn no_lines_after_some_is_noop() {
        let mut lines: Vec<ShapedLine> = vec![];
        super::inject_inline_pseudo_images(&mut lines, None, Some(make_image(20.0)));
        assert!(lines.is_empty());
    }

    // ── ::before insertion ───────────────────────────────────────────────────

    #[test]
    fn before_inserts_at_start_of_first_line() {
        let mut lines = vec![empty_line()];
        super::inject_inline_pseudo_images(&mut lines, Some(make_image(20.0)), None);
        assert_eq!(lines[0].items.len(), 1);
        match &lines[0].items[0] {
            LineItem::Image(img) => {
                assert!(approx(img.x_offset, 0.0), "x_offset={:?}", img.x_offset);
                assert!(approx(img.width, 20.0));
            }
            _ => panic!("expected Image at index 0"),
        }
    }

    #[test]
    fn before_shifts_existing_image_items() {
        // Original image at x=5, w=10.  Before image w=15.
        // After injection: before@x=0, original@x=20.
        let mut lines = vec![image_line(5.0, 10.0)];
        super::inject_inline_pseudo_images(&mut lines, Some(make_image(15.0)), None);
        assert_eq!(lines[0].items.len(), 2);
        match &lines[0].items[0] {
            LineItem::Image(img) => assert!(approx(img.x_offset, 0.0)),
            _ => panic!("expected before Image at index 0"),
        }
        match &lines[0].items[1] {
            LineItem::Image(img) => {
                assert!(approx(img.x_offset, 20.0), "shifted x={:?}", img.x_offset);
            }
            _ => panic!("expected shifted Image at index 1"),
        }
    }

    #[test]
    fn before_shifts_text_items() {
        // Text run at x_offset=3, font_size=10, glyph advance=5.
        // Before image w=20 → text run shifts to x=23.
        let run = ShapedGlyphRun {
            font_data: Arc::new(vec![]),
            font_index: 0,
            font_size: 10.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![ShapedGlyph {
                id: 0,
                x_advance: 5.0,
                x_offset: 0.0,
                y_offset: 0.0,
                text_range: 0..1,
            }],
            text: Arc::from("a"),
            x_offset: 3.0_f32.as_pt(),
            link: None,
        };
        let mut lines = vec![ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![LineItem::Text(run)],
        }];
        super::inject_inline_pseudo_images(&mut lines, Some(make_image(20.0)), None);
        assert_eq!(lines[0].items.len(), 2);
        match &lines[0].items[1] {
            LineItem::Text(r) => {
                assert!(approx(r.x_offset, 23.0), "x_offset={}", r.x_offset.to_f32());
            }
            _ => panic!("expected Text at index 1"),
        }
    }

    #[test]
    fn before_shifts_inline_box_items() {
        let ib = InlineBoxItem {
            node_id: None,
            width: 30.0_f32.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: 5.0_f32.as_pt(),
            computed_y: crate::units::Pt::ZERO,
            link: None,
            opacity: 1.0,
            visible: true,
        };
        let mut lines = vec![ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![LineItem::InlineBox(ib)],
        }];
        super::inject_inline_pseudo_images(&mut lines, Some(make_image(10.0)), None);
        assert_eq!(lines[0].items.len(), 2);
        match &lines[0].items[1] {
            LineItem::InlineBox(b) => {
                assert!(approx(b.x_offset, 15.0), "x_offset={}", b.x_offset.to_f32());
            }
            _ => panic!("expected InlineBox at index 1"),
        }
    }

    #[test]
    fn before_only_affects_first_line() {
        // Second line must not have its items shifted.
        let mut lines = vec![image_line(0.0, 10.0), image_line(7.0, 10.0)];
        super::inject_inline_pseudo_images(&mut lines, Some(make_image(20.0)), None);
        assert_eq!(lines[1].items.len(), 1);
        match &lines[1].items[0] {
            LineItem::Image(img) => {
                assert!(
                    approx(img.x_offset, 7.0),
                    "second-line x_offset={:?}",
                    img.x_offset
                );
            }
            _ => panic!("expected untouched Image in second line"),
        }
    }

    // ── ::after insertion ────────────────────────────────────────────────────

    #[test]
    fn after_appends_to_last_line_x_offset_from_image_item() {
        // Existing image: x=5, w=10 → end=15.  After gets x_offset=15.
        let mut lines = vec![image_line(5.0, 10.0)];
        super::inject_inline_pseudo_images(&mut lines, None, Some(make_image(20.0)));
        assert_eq!(lines[0].items.len(), 2);
        match &lines[0].items[1] {
            LineItem::Image(img) => {
                assert!(
                    approx(img.x_offset, 15.0),
                    "after x_offset={:?}",
                    img.x_offset
                );
            }
            _ => panic!("expected appended after Image"),
        }
    }

    #[test]
    fn after_x_offset_computed_from_text_run_glyphs() {
        // x_offset=2, font_size=4, glyphs=[advance=3, advance=5]
        // end_x = 2 + (3+5)*4 = 2 + 32 = 34
        let run = ShapedGlyphRun {
            font_data: Arc::new(vec![]),
            font_index: 0,
            font_size: 4.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![
                ShapedGlyph {
                    id: 0,
                    x_advance: 3.0,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    text_range: 0..1,
                },
                ShapedGlyph {
                    id: 1,
                    x_advance: 5.0,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    text_range: 1..2,
                },
            ],
            text: Arc::from("ab"),
            x_offset: 2.0_f32.as_pt(),
            link: None,
        };
        let mut lines = vec![ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![LineItem::Text(run)],
        }];
        super::inject_inline_pseudo_images(&mut lines, None, Some(make_image(20.0)));
        match &lines[0].items[1] {
            LineItem::Image(img) => {
                assert!(
                    approx(img.x_offset, 34.0),
                    "after x_offset={:?}",
                    img.x_offset
                );
            }
            _ => panic!("expected appended after Image"),
        }
    }

    #[test]
    fn after_x_offset_from_inline_box() {
        // x_offset=3, width=7 → end=10
        let ib = InlineBoxItem {
            node_id: None,
            width: 7.0_f32.as_pt(),
            height: 5.0_f32.as_pt(),
            x_offset: 3.0_f32.as_pt(),
            computed_y: crate::units::Pt::ZERO,
            link: None,
            opacity: 1.0,
            visible: true,
        };
        let mut lines = vec![ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![LineItem::InlineBox(ib)],
        }];
        super::inject_inline_pseudo_images(&mut lines, None, Some(make_image(5.0)));
        match &lines[0].items[1] {
            LineItem::Image(img) => {
                assert!(
                    approx(img.x_offset, 10.0),
                    "after x_offset={:?}",
                    img.x_offset
                );
            }
            _ => panic!("expected appended after Image"),
        }
    }

    #[test]
    fn after_empty_last_line_gets_zero_x_offset() {
        let mut lines = vec![empty_line()];
        super::inject_inline_pseudo_images(&mut lines, None, Some(make_image(20.0)));
        assert_eq!(lines[0].items.len(), 1);
        match &lines[0].items[0] {
            LineItem::Image(img) => {
                assert!(approx(img.x_offset, 0.0), "x_offset={:?}", img.x_offset);
            }
            _ => panic!("expected after Image in empty line"),
        }
    }

    #[test]
    fn after_only_affects_last_line() {
        let mut lines = vec![image_line(0.0, 10.0), image_line(0.0, 20.0)];
        super::inject_inline_pseudo_images(&mut lines, None, Some(make_image(5.0)));
        assert_eq!(lines[0].items.len(), 1, "first line must be untouched");
        assert_eq!(lines[1].items.len(), 2, "last line gets after image");
    }

    #[test]
    fn after_uses_max_across_multiple_item_types() {
        // item1: Image x=0 w=5 → end=5
        // item2: InlineBox x=3 w=10 → end=13  (rightmost)
        // item3: Image x=1 w=8 → end=9
        // fold(f32::max) must yield 13
        let mut lines = vec![ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![
                LineItem::Image(InlineImage {
                    data: Arc::new(vec![]),
                    format: ImageFormat::Png,
                    width: 5.0_f32.as_pt(),
                    height: 10.0_f32.as_pt(),
                    x_offset: crate::units::Pt::ZERO,
                    vertical_align: VerticalAlign::Baseline,
                    opacity: 1.0,
                    visible: true,
                    computed_y: crate::units::Pt::ZERO,
                    link: None,
                }),
                LineItem::InlineBox(InlineBoxItem {
                    node_id: None,
                    width: 10.0_f32.as_pt(),
                    height: 5.0_f32.as_pt(),
                    x_offset: 3.0_f32.as_pt(),
                    computed_y: crate::units::Pt::ZERO,
                    link: None,
                    opacity: 1.0,
                    visible: true,
                }),
                LineItem::Image(InlineImage {
                    data: Arc::new(vec![]),
                    format: ImageFormat::Png,
                    width: 8.0_f32.as_pt(),
                    height: 10.0_f32.as_pt(),
                    x_offset: 1.0_f32.as_pt(),
                    vertical_align: VerticalAlign::Baseline,
                    opacity: 1.0,
                    visible: true,
                    computed_y: crate::units::Pt::ZERO,
                    link: None,
                }),
            ],
        }];
        super::inject_inline_pseudo_images(&mut lines, None, Some(make_image(20.0)));
        match lines[0].items.last().unwrap() {
            LineItem::Image(img) => {
                assert!(
                    approx(img.x_offset, 13.0),
                    "after x_offset={:?}",
                    img.x_offset
                );
            }
            _ => panic!("expected after Image appended"),
        }
    }

    // ── before + after together ──────────────────────────────────────────────

    #[test]
    fn before_and_after_on_separate_lines() {
        let mut lines = vec![image_line(0.0, 10.0), image_line(0.0, 20.0)];
        super::inject_inline_pseudo_images(
            &mut lines,
            Some(make_image(5.0)),
            Some(make_image(5.0)),
        );
        // First line: [before_img, shifted_original]
        assert_eq!(lines[0].items.len(), 2);
        match &lines[0].items[0] {
            LineItem::Image(img) => assert!(approx(img.width, 5.0)),
            _ => panic!("expected before Image"),
        }
        // Last line: [original, after_img]
        assert_eq!(lines[1].items.len(), 2);
        match lines[1].items.last().unwrap() {
            LineItem::Image(img) => assert!(approx(img.width, 5.0)),
            _ => panic!("expected after Image"),
        }
    }

    #[test]
    fn single_line_before_and_after_both_affect_it() {
        // Line: image x=5 w=10.  Before w=3, after w=7.
        // After before insertion: [before@x=0 w=3, original@x=8 w=10]
        // After after insertion: x_offset = max(0+3, 8+10) = 18
        let mut lines = vec![image_line(5.0, 10.0)];
        super::inject_inline_pseudo_images(
            &mut lines,
            Some(make_image(3.0)),
            Some(make_image(7.0)),
        );
        assert_eq!(lines[0].items.len(), 3);
        match &lines[0].items[0] {
            LineItem::Image(img) => assert!(approx(img.x_offset, 0.0)),
            _ => panic!("expected before Image at 0"),
        }
        match &lines[0].items[1] {
            LineItem::Image(img) => {
                assert!(approx(img.x_offset, 8.0), "shifted x={:?}", img.x_offset);
            }
            _ => panic!("expected shifted Image at 1"),
        }
        match lines[0].items.last().unwrap() {
            LineItem::Image(img) => {
                assert!(approx(img.x_offset, 18.0), "after x={:?}", img.x_offset);
            }
            _ => panic!("expected after Image at end"),
        }
    }

    // ── smoke tests via Engine::render_html (Blitz-dependent paths) ─────────
    //
    // These cover branches in `build_pseudo_image_entry`,
    // `build_block_pseudo_image_entries`, and `resolve_pseudo_size` that
    // require a live Blitz document.  Pattern mirrors the smoke helpers in
    // convert/list_item.rs.

    const RED_1X1_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn render(html: &str) -> Vec<u8> {
        crate::engine::Engine::builder()
            .build()
            .render(html)
            .expect("render failed")
    }

    fn render_with_assets(html: &str, bundle: crate::asset::AssetBundle) -> Vec<u8> {
        crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(html)
            .expect("render failed")
    }

    // build_block_pseudo_image_entries: `if assets.is_none()` early-return.
    // No bundle is registered; the pseudo url is silently skipped.
    #[test]
    fn smoke_block_pseudo_no_asset_bundle_skips_image() {
        let pdf = render(
            r#"<!doctype html><html><head><style>
            div::before { content: url("dot.png"); display: block; width: 20px; height: 20px; }
            </style></head><body><div>Text</div></body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_pseudo_image_entry: `assets.get_image(name)?` returns None when the
    // image name in CSS is not registered in the bundle.
    #[test]
    fn smoke_block_pseudo_image_url_not_in_bundle() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("other.png", RED_1X1_PNG.to_vec());
        bundle.add_css(
            r#"div::before { content: url("missing.png"); display: block; width: 20px; height: 20px; }"#,
        );
        let pdf = render_with_assets(
            r#"<!doctype html><html><body><div>Text</div></body></html>"#,
            bundle,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_pseudo_image_entry happy path + resolve_pseudo_size LengthPercentage
    // arm: ::before with `display: block` and explicit pixel dimensions is
    // registered as a block-pseudo ImageEntry.
    #[test]
    fn smoke_block_pseudo_before_image_explicit_size() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(
            r#"div::before { content: url("dot.png"); display: block; width: 20px; height: 15px; }"#,
        );
        let pdf = render_with_assets(
            r#"<!doctype html><html><body><div>With block before</div></body></html>"#,
            bundle,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // resolve_pseudo_size `_ => None` arm: ::before with `display: block` but no
    // explicit width/height → auto/intrinsic dimensions → resolve_pseudo_size
    // returns None for both axes and image falls back to intrinsic 1×1 pixels.
    #[test]
    fn smoke_block_pseudo_before_image_auto_size() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"div::before { content: url("dot.png"); display: block; }"#);
        let pdf = render_with_assets(
            r#"<!doctype html><html><body><div>Auto-size pseudo</div></body></html>"#,
            bundle,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_block_pseudo_image_entries: the `after` slot.
    // ::after with `display: block` exercises the second `load(parent.after)` call.
    #[test]
    fn smoke_block_pseudo_after_image_explicit_size() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(
            r#"div::after { content: url("dot.png"); display: block; width: 20px; height: 15px; }"#,
        );
        let pdf = render_with_assets(
            r#"<!doctype html><html><body><div>With block after</div></body></html>"#,
            bundle,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_block_pseudo_image_entries: `if is_absolutely_positioned(pseudo)` branch.
    // An absolutely-positioned ::before with `display: block` hits `is_block_pseudo`
    // (returns true) and then `is_absolutely_positioned` (returns true) → the block-
    // pseudo-image slot returns None; the pseudo is instead handled by
    // walk_absolute_children.
    #[test]
    fn smoke_block_pseudo_absolute_position_excluded_from_image_slot() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(
            r#"div { position: relative; }
               div::before { content: url("dot.png"); display: block; position: absolute;
                             width: 20px; height: 20px; top: 0; left: 0; }"#,
        );
        let pdf = render_with_assets(
            r#"<!doctype html><html><body><div>Abs-pos pseudo</div></body></html>"#,
            bundle,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // node_has_block_pseudo_image / node_has_absolute_pseudo: exercised via the
    // inline-root fast-path in inline_root.rs (lines 44-46).
    // A <p> (inline root) with a block ::before image causes
    // `node_has_block_pseudo_image` to return true, routing processing through
    // register_pseudo_content rather than the pure-inline path.
    #[test]
    fn smoke_inline_root_with_block_pseudo_image() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(
            r#"p::before { content: url("dot.png"); display: block; width: 10px; height: 10px; }"#,
        );
        let pdf = render_with_assets(
            r#"<!doctype html><html><body><p>Paragraph with block pseudo</p></body></html>"#,
            bundle,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // node_has_absolute_pseudo: a <p> (inline root) with an absolutely-positioned
    // ::before pseudo causes `node_has_absolute_pseudo` to return true.
    #[test]
    fn smoke_inline_root_with_absolute_pseudo() {
        let pdf = render(
            r#"<!doctype html><html><head><style>
            p { position: relative; }
            p::before { content: "x"; position: absolute; top: 0; left: 0; }
            </style></head><body><p>Paragraph with abs pseudo</p></body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_pseudo_image_entry: `extract_content_image_url` returns None when
    // the block pseudo's content is a string literal (no url()).
    // Covers the line 19 `?` short-circuit in build_pseudo_image_entry.
    #[test]
    fn smoke_block_pseudo_text_content_skips_image_entry() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"div::before { content: "arrow"; display: block; }"#);
        let pdf = render_with_assets(
            r#"<!DOCTYPE html><html><body><div>Text</div></body></html>"#,
            bundle,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_pseudo_image_entry: `ImageRender::detect_format` returns None when
    // the registered image bytes are not a recognised format.
    // Covers the line 22 `?` short-circuit in build_pseudo_image_entry.
    #[test]
    fn smoke_block_pseudo_unrecognized_format_skips_entry() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("bad.bin", vec![0xDE, 0xAD, 0xBE, 0xEF]);
        bundle.add_css(
            r#"div::before { content: url("bad.bin"); display: block; width: 10px; height: 10px; }"#,
        );
        let pdf = render_with_assets(
            r#"<!DOCTYPE html><html><body><div>Text</div></body></html>"#,
            bundle,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_inline_pseudo_image: `get_image` returns None when the inline
    // pseudo's url() is not registered in the bundle (covers line 171 `?`).
    #[test]
    fn smoke_inline_pseudo_image_url_not_in_bundle() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("other.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"p::before { content: url("missing.png"); }"#);
        let pdf = render_with_assets(
            r#"<!DOCTYPE html><html><body><p>Paragraph</p></body></html>"#,
            bundle,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_inline_pseudo_image: `detect_format` returns None for unrecognised
    // bytes in the bundle (covers line 172 `?`).
    #[test]
    fn smoke_inline_pseudo_unrecognized_format_skips() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("bad.bin", vec![0xDE, 0xAD, 0xBE, 0xEF]);
        bundle.add_css(r#"p::before { content: url("bad.bin"); }"#);
        let pdf = render_with_assets(
            r#"<!DOCTYPE html><html><body><p>Paragraph</p></body></html>"#,
            bundle,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── node_has_inline_pseudo_image direct unit tests ───────────────────────
    // The function is #[allow(dead_code)] because v2 inline-root no longer
    // calls it, but it mirrors node_has_block_pseudo_image and its semantics
    // warrant direct coverage.

    fn parse_doc_for_inline_pseudo_tests(html: &str) -> blitz_html::HtmlDocument {
        crate::blitz_adapter::parse_and_layout(
            html,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        )
    }

    fn find_elem_by_tag_in_pseudo_tests<'a>(
        base: &'a crate::blitz_adapter::BaseDocument,
        start_id: usize,
        tag: &str,
    ) -> Option<&'a crate::blitz_adapter::Node> {
        let node = base.get_node(start_id)?;
        if node
            .element_data()
            .is_some_and(|e| e.name.local.as_ref() == tag)
        {
            return Some(node);
        }
        for &child in &node.children {
            if let Some(found) = find_elem_by_tag_in_pseudo_tests(base, child, tag) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn node_has_inline_pseudo_image_true_for_before_with_url() {
        use std::ops::Deref;
        let doc = parse_doc_for_inline_pseudo_tests(
            r#"<!DOCTYPE html><html><head><style>
            div::before { content: url("dot.png"); }
            </style></head><body><div>Text</div></body></html>"#,
        );
        let base = doc.deref();
        let div_node = find_elem_by_tag_in_pseudo_tests(base, doc.root_element().id, "div")
            .expect("div element must be present");
        assert!(
            super::node_has_inline_pseudo_image(base, div_node),
            "inline ::before with url() must return true"
        );
    }

    #[test]
    fn node_has_inline_pseudo_image_true_for_after_with_url() {
        use std::ops::Deref;
        let doc = parse_doc_for_inline_pseudo_tests(
            r#"<!DOCTYPE html><html><head><style>
            div::after { content: url("dot.png"); }
            </style></head><body><div>Text</div></body></html>"#,
        );
        let base = doc.deref();
        let div_node = find_elem_by_tag_in_pseudo_tests(base, doc.root_element().id, "div")
            .expect("div element must be present");
        assert!(
            super::node_has_inline_pseudo_image(base, div_node),
            "inline ::after with url() must return true"
        );
    }

    #[test]
    fn node_has_inline_pseudo_image_false_when_before_is_block() {
        use std::ops::Deref;
        let doc = parse_doc_for_inline_pseudo_tests(
            r#"<!DOCTYPE html><html><head><style>
            div::before { content: url("dot.png"); display: block; }
            </style></head><body><div>Text</div></body></html>"#,
        );
        let base = doc.deref();
        let div_node = find_elem_by_tag_in_pseudo_tests(base, doc.root_element().id, "div")
            .expect("div element must be present");
        assert!(
            !super::node_has_inline_pseudo_image(base, div_node),
            "block ::before must not be reported as inline pseudo image"
        );
    }

    #[test]
    fn node_has_inline_pseudo_image_false_when_no_pseudo() {
        use std::ops::Deref;
        let doc = parse_doc_for_inline_pseudo_tests(
            r#"<!DOCTYPE html><html><body><div>No pseudo here</div></body></html>"#,
        );
        let base = doc.deref();
        let div_node = find_elem_by_tag_in_pseudo_tests(base, doc.root_element().id, "div")
            .expect("div element must be present");
        assert!(
            !super::node_has_inline_pseudo_image(base, div_node),
            "element with no pseudo must return false"
        );
    }

    #[test]
    fn node_has_inline_pseudo_image_false_when_before_has_text_content() {
        use std::ops::Deref;
        let doc = parse_doc_for_inline_pseudo_tests(
            r#"<!DOCTYPE html><html><head><style>
            div::before { content: "arrow"; }
            </style></head><body><div>Text</div></body></html>"#,
        );
        let base = doc.deref();
        let div_node = find_elem_by_tag_in_pseudo_tests(base, doc.root_element().id, "div")
            .expect("div element must be present");
        assert!(
            !super::node_has_inline_pseudo_image(base, div_node),
            "::before with text content must return false (no url())"
        );
    }
}
