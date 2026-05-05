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
    parent_content_width: f32,
    parent_content_height: f32,
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

#[cfg(test)]
mod tests {
    use crate::image::ImageFormat;
    use crate::paragraph::{
        InlineBoxItem, InlineImage, LineItem, ShapedGlyph, ShapedGlyphRun, ShapedLine,
        TextDecoration, VerticalAlign,
    };
    use std::sync::Arc;

    fn make_image(width: f32) -> InlineImage {
        InlineImage {
            data: Arc::new(vec![]),
            format: ImageFormat::Png,
            width,
            height: 10.0,
            x_offset: 0.0,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: 0.0,
            link: None,
        }
    }

    fn empty_line() -> ShapedLine {
        ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![],
        }
    }

    fn image_line(x_offset: f32, width: f32) -> ShapedLine {
        ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::Image(InlineImage {
                data: Arc::new(vec![]),
                format: ImageFormat::Png,
                width,
                height: 10.0,
                x_offset,
                vertical_align: VerticalAlign::Baseline,
                opacity: 1.0,
                visible: true,
                computed_y: 0.0,
                link: None,
            })],
        }
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.001
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
                assert!(approx(img.x_offset, 0.0), "x_offset={}", img.x_offset);
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
                assert!(approx(img.x_offset, 20.0), "shifted x={}", img.x_offset);
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
            font_size: 10.0,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![ShapedGlyph {
                id: 0,
                x_advance: 5.0,
                x_offset: 0.0,
                y_offset: 0.0,
                text_range: 0..1,
            }],
            text: "a".to_string(),
            x_offset: 3.0,
            link: None,
        };
        let mut lines = vec![ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::Text(run)],
        }];
        super::inject_inline_pseudo_images(&mut lines, Some(make_image(20.0)), None);
        assert_eq!(lines[0].items.len(), 2);
        match &lines[0].items[1] {
            LineItem::Text(r) => {
                assert!(approx(r.x_offset, 23.0), "x_offset={}", r.x_offset);
            }
            _ => panic!("expected Text at index 1"),
        }
    }

    #[test]
    fn before_shifts_inline_box_items() {
        let ib = InlineBoxItem {
            node_id: None,
            width: 30.0,
            height: 10.0,
            x_offset: 5.0,
            computed_y: 0.0,
            link: None,
            opacity: 1.0,
            visible: true,
        };
        let mut lines = vec![ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::InlineBox(ib)],
        }];
        super::inject_inline_pseudo_images(&mut lines, Some(make_image(10.0)), None);
        assert_eq!(lines[0].items.len(), 2);
        match &lines[0].items[1] {
            LineItem::InlineBox(b) => {
                assert!(approx(b.x_offset, 15.0), "x_offset={}", b.x_offset);
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
                    "second-line x_offset={}",
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
                    "after x_offset={}",
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
            font_size: 4.0,
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
            text: "ab".to_string(),
            x_offset: 2.0,
            link: None,
        };
        let mut lines = vec![ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::Text(run)],
        }];
        super::inject_inline_pseudo_images(&mut lines, None, Some(make_image(20.0)));
        match &lines[0].items[1] {
            LineItem::Image(img) => {
                assert!(
                    approx(img.x_offset, 34.0),
                    "after x_offset={}",
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
            width: 7.0,
            height: 5.0,
            x_offset: 3.0,
            computed_y: 0.0,
            link: None,
            opacity: 1.0,
            visible: true,
        };
        let mut lines = vec![ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::InlineBox(ib)],
        }];
        super::inject_inline_pseudo_images(&mut lines, None, Some(make_image(5.0)));
        match &lines[0].items[1] {
            LineItem::Image(img) => {
                assert!(
                    approx(img.x_offset, 10.0),
                    "after x_offset={}",
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
                assert!(approx(img.x_offset, 0.0), "x_offset={}", img.x_offset);
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
            height: 16.0,
            baseline: 12.0,
            items: vec![
                LineItem::Image(InlineImage {
                    data: Arc::new(vec![]),
                    format: ImageFormat::Png,
                    width: 5.0,
                    height: 10.0,
                    x_offset: 0.0,
                    vertical_align: VerticalAlign::Baseline,
                    opacity: 1.0,
                    visible: true,
                    computed_y: 0.0,
                    link: None,
                }),
                LineItem::InlineBox(InlineBoxItem {
                    node_id: None,
                    width: 10.0,
                    height: 5.0,
                    x_offset: 3.0,
                    computed_y: 0.0,
                    link: None,
                    opacity: 1.0,
                    visible: true,
                }),
                LineItem::Image(InlineImage {
                    data: Arc::new(vec![]),
                    format: ImageFormat::Png,
                    width: 8.0,
                    height: 10.0,
                    x_offset: 1.0,
                    vertical_align: VerticalAlign::Baseline,
                    opacity: 1.0,
                    visible: true,
                    computed_y: 0.0,
                    link: None,
                }),
            ],
        }];
        super::inject_inline_pseudo_images(&mut lines, None, Some(make_image(20.0)));
        match lines[0].items.last().unwrap() {
            LineItem::Image(img) => {
                assert!(
                    approx(img.x_offset, 13.0),
                    "after x_offset={}",
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
                assert!(approx(img.x_offset, 8.0), "shifted x={}", img.x_offset);
            }
            _ => panic!("expected shifted Image at 1"),
        }
        match lines[0].items.last().unwrap() {
            LineItem::Image(img) => {
                assert!(approx(img.x_offset, 18.0), "after x={}", img.x_offset);
            }
            _ => panic!("expected after Image at end"),
        }
    }
}
