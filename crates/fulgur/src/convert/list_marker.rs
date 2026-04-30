use super::inline_root;
use super::*;
use crate::blitz_adapter::{Marker, marker_skrifa_text, marker_to_string};

/// Resolve a node's computed `list-style-image` to bundled asset bytes and
/// detected asset kind. Returns `None` when there is no `list-style-image`,
/// the computed value is not a plain `url(...)`, no asset bundle is set, or
/// the asset is not registered in the bundle.
fn resolve_list_style_image_asset<'a>(
    node: &Node,
    assets: Option<&'a AssetBundle>,
) -> Option<(&'a Arc<Vec<u8>>, crate::image::AssetKind)> {
    use ::style::values::computed::image::Image;
    let assets = assets?;
    let styles = node.primary_styles()?;
    let image = styles.clone_list_style_image();
    let url = match image {
        Image::Url(u) => u,
        _ => return None,
    };
    let raw_src = match &url {
        ::style::servo::url::ComputedUrl::Valid(u) => u.as_str(),
        ::style::servo::url::ComputedUrl::Invalid(s) => s.as_str(),
    };
    let src = extract_asset_name(raw_src);
    let data = assets.get_image(src)?;
    let kind = crate::image::AssetKind::detect(data);
    Some((data, kind))
}

/// Clamp a raster image's intrinsic dimensions (in CSS px) to a marker size
/// bounded by `line_height`. Returns `(width_pt, height_pt)`.
fn size_raster_marker(
    data: &Arc<Vec<u8>>,
    format: crate::image::ImageFormat,
    line_height: f32,
) -> Option<(f32, f32)> {
    let (iw, ih) = ImagePageable::decode_dimensions(data, format)?;
    let intrinsic_w = px_to_pt(iw as f32);
    let intrinsic_h = px_to_pt(ih as f32);
    Some(crate::pageable::clamp_marker_size(
        intrinsic_w,
        intrinsic_h,
        line_height,
    ))
}

/// Resolve a list-style-image marker from the node's computed styles.
///
/// Returns `Some(ListItemMarker::Image { ... })` when the node's
/// `list-style-image` is a URL that resolves to a supported image
/// (PNG/JPEG/GIF or SVG) inside `ctx.assets`. Returns `None` for any
/// failure (no bundle, URL not found, unknown format, parse error),
/// and the caller must then fall back to the text marker produced by
/// `extract_marker_lines` — matching CSS spec fallback semantics.
pub(super) fn resolve_list_marker(
    node: &Node,
    line_height: f32,
    assets: Option<&AssetBundle>,
) -> Option<ListItemMarker> {
    use crate::image::AssetKind;

    // Zero or negative line-height (e.g. list-style-position: inside where
    // extract_marker_lines returns 0.0) would clamp image size to 0x0.
    // Return None so the caller falls back to the text marker instead of
    // creating an invisible image marker that suppresses the fallback.
    if line_height <= 0.0 {
        return None;
    }
    let (data, kind) = resolve_list_style_image_asset(node, assets)?;
    match kind {
        AssetKind::Raster(format) => {
            let (width, height) = size_raster_marker(data, format, line_height)?;
            let img = ImagePageable::new(Arc::clone(data), format, width, height);
            Some(ListItemMarker::Image {
                marker: ImageMarker::Raster(img),
                width,
                height,
            })
        }
        AssetKind::Svg => {
            let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
            let size = tree.size();
            let intrinsic_w = px_to_pt(size.width());
            let intrinsic_h = px_to_pt(size.height());
            let (width, height) =
                crate::pageable::clamp_marker_size(intrinsic_w, intrinsic_h, line_height);
            let svg = SvgPageable::new(Arc::new(tree), width, height);
            Some(ListItemMarker::Image {
                marker: ImageMarker::Svg(svg),
                width,
                height,
            })
        }
        AssetKind::Unknown => None,
    }
}

/// For `list-style-position: inside` with `list-style-image`, resolve
/// the image and return it as an `InlineImage` sized to match the
/// paragraph's first line height. Only supports raster images (PNG/JPEG/GIF).
/// Returns `None` when the node is not an inside list item, the image URL
/// cannot be resolved, or the image is SVG.
pub(super) fn resolve_inside_image_marker(
    node: &Node,
    first_line_height: f32,
    assets: Option<&AssetBundle>,
) -> Option<InlineImage> {
    use crate::image::AssetKind;

    let elem_data = node.element_data()?;
    let list_data = elem_data.list_item_data.as_ref()?;
    if !crate::blitz_adapter::is_list_position_inside(&list_data.position) {
        return None;
    }
    if first_line_height <= 0.0 {
        return None;
    }

    let (data, kind) = resolve_list_style_image_asset(node, assets)?;
    match kind {
        AssetKind::Raster(format) => {
            let (width, height) = size_raster_marker(data, format, first_line_height)?;
            Some(InlineImage {
                data: Arc::clone(data),
                format,
                width,
                height,
                x_offset: 0.0,
                vertical_align: VerticalAlign::Baseline,
                opacity: 1.0,
                visible: true,
                computed_y: 0.0,
                link: None,
            })
        }
        // SVG inline images are not yet supported in LineItem::Image
        AssetKind::Svg | AssetKind::Unknown => None,
    }
}

/// Extract shaped lines from a list marker's Parley layout.
pub(super) fn extract_marker_lines(
    doc: &BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
) -> (Vec<ShapedLine>, f32, f32) {
    let elem_data = match node.element_data() {
        Some(d) => d,
        None => return (Vec::new(), 0.0, 0.0),
    };
    let list_item_data = match &elem_data.list_item_data {
        Some(d) => d,
        None => return (Vec::new(), 0.0, 0.0),
    };
    let Some(parley_layout) =
        crate::blitz_adapter::list_position_outside_layout(&list_item_data.position)
    else {
        return (Vec::new(), 0.0, 0.0);
    };

    let marker_text = marker_to_string(&list_item_data.marker);

    let mut shaped_lines = Vec::new();
    let mut max_width: f32 = 0.0;
    let mut line_height_pt: f32 = 0.0;

    for line in parley_layout.lines() {
        let metrics = line.metrics();
        if line_height_pt == 0.0 {
            line_height_pt = px_to_pt(metrics.line_height);
        }
        let mut items = Vec::new();
        let mut line_width: f32 = 0.0;

        for item in line.items() {
            if let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item {
                let run = glyph_run.run();
                let font_ref = run.font();
                let font_index = font_ref.index;
                let font_arc = ctx.get_or_insert_font(font_ref);
                // Parley reports font size in CSS px; the Pageable tree is
                // in pt. See `extract_paragraph` for the matching
                // conversion. Glyph ratios stay unitless by dividing by
                // the original parley value.
                let font_size_parley = run.font_size();
                let font_size = px_to_pt(font_size_parley);

                let brush = &glyph_run.style().brush;
                let color = get_text_color(doc, brush.id);

                let text_len = marker_text.len();
                let mut glyphs = Vec::new();
                for g in glyph_run.glyphs() {
                    line_width += px_to_pt(g.advance);
                    glyphs.push(ShapedGlyph {
                        id: g.id,
                        x_advance: g.advance / font_size_parley,
                        x_offset: g.x / font_size_parley,
                        y_offset: g.y / font_size_parley,
                        text_range: 0..text_len,
                    });
                }

                if !glyphs.is_empty() {
                    items.push(LineItem::Text(ShapedGlyphRun {
                        font_data: font_arc,
                        font_index,
                        font_size,
                        color,
                        decoration: Default::default(),
                        glyphs,
                        text: marker_text.clone(),
                        x_offset: px_to_pt(glyph_run.offset()),
                        link: None,
                    }));
                }
            }
        }

        max_width = max_width.max(line_width);
        shaped_lines.push(ShapedLine {
            height: px_to_pt(metrics.line_height),
            baseline: px_to_pt(metrics.baseline),
            items,
        });
    }

    (shaped_lines, max_width, line_height_pt)
}

/// Search for a font that covers the marker's non-whitespace characters.
///
/// First checks `AssetBundle.fonts` for a font whose skrifa charmap covers all
/// non-whitespace characters in the marker text. If no asset fonts match (or no
/// bundle is provided), falls back to borrowing `font_data` + `font_index` from
/// the first `ShapedGlyphRun` found in the already-converted `children`.
///
/// Returns `None` only when no font source is available at all (empty `<li>`
/// without asset fonts).
pub(super) fn find_marker_font(
    marker: &Marker,
    assets: Option<&AssetBundle>,
    children: &[PositionedChild],
) -> Option<(Arc<Vec<u8>>, u32)> {
    let marker_text = marker_to_string(marker);
    let check_chars: Vec<char> = marker_text.chars().filter(|c| !c.is_whitespace()).collect();

    // Try AssetBundle fonts first — check charmap coverage.
    if let Some(bundle) = assets {
        for font_arc in &bundle.fonts {
            // Try sub-fonts in a TTC collection; break on first Err (no more faces).
            for idx in 0u32.. {
                if let Ok(font_ref) = skrifa::FontRef::from_index(font_arc, idx) {
                    let charmap = font_ref.charmap();
                    if check_chars.iter().all(|&c| charmap.map(c).is_some()) {
                        return Some((Arc::clone(font_arc), idx));
                    }
                } else {
                    break; // No more sub-fonts
                }
            }
        }
    }

    // Fallback: find the first ShapedGlyphRun in children's ParagraphPageables
    // whose font covers all marker characters.
    fn find_run_font_in_children(
        children: &[PositionedChild],
        check_chars: &[char],
    ) -> Option<(Arc<Vec<u8>>, u32)> {
        for pc in children {
            if let Some(para) = pc.child.as_any().downcast_ref::<ParagraphPageable>() {
                for line in &para.lines {
                    for item in &line.items {
                        if let LineItem::Text(run) = item {
                            if let Ok(font_ref) =
                                skrifa::FontRef::from_index(&run.font_data, run.font_index)
                            {
                                let charmap = font_ref.charmap();
                                if check_chars.iter().all(|c| charmap.map(*c).is_some()) {
                                    return Some((Arc::clone(&run.font_data), run.font_index));
                                }
                            }
                        }
                    }
                }
            }
            if let Some(block) = pc.child.as_any().downcast_ref::<BlockPageable>() {
                if let Some(result) = find_run_font_in_children(&block.children, check_chars) {
                    return Some(result);
                }
            }
        }
        None
    }

    find_run_font_in_children(children, &check_chars)
}

/// Shape a list marker string into a `ShapedGlyphRun` using skrifa.
///
/// Performs simplified character-by-character glyph mapping (no complex
/// OpenType shaping, kerning, or ligatures). This is sufficient for
/// bullet characters (U+2022) and ordered markers ("1. ") which don't
/// require advanced text layout.
///
/// For `Marker::Char`, appends a trailing space (matching Blitz's
/// `build_inline_layout` which does `format!("{char} ")`).
/// For `Marker::String`, uses the string as-is (Blitz already includes
/// trailing content like `"1. "`).
///
/// `x_advance` values are normalized by `font_size` following fulgur convention
/// (see `extract_marker_lines`).
pub(super) fn shape_marker_with_skrifa(
    marker: &Marker,
    font_data: &Arc<Vec<u8>>,
    font_index: u32,
    font_size: f32,
    color: [u8; 4],
) -> Option<ShapedGlyphRun> {
    let text = marker_skrifa_text(marker);

    let font_ref = skrifa::FontRef::from_index(font_data, font_index).ok()?;
    let charmap = font_ref.charmap();
    let glyph_metrics = font_ref.glyph_metrics(
        skrifa::instance::Size::new(font_size),
        skrifa::instance::LocationRef::default(),
    );

    let mut glyphs = Vec::new();
    let mut byte_offset = 0usize;
    for ch in text.chars() {
        let ch_len = ch.len_utf8();
        let gid = charmap.map(ch).unwrap_or(skrifa::GlyphId::new(0));
        let advance = glyph_metrics.advance_width(gid).unwrap_or(0.0);
        glyphs.push(ShapedGlyph {
            id: gid.to_u32(),
            x_advance: advance / font_size,
            x_offset: 0.0,
            y_offset: 0.0,
            text_range: byte_offset..byte_offset + ch_len,
        });
        byte_offset += ch_len;
    }

    Some(ShapedGlyphRun {
        font_data: Arc::clone(font_data),
        font_index,
        font_size,
        color,
        decoration: TextDecoration::default(),
        glyphs,
        text,
        x_offset: 0.0,
        link: None,
    })
}

/// Inject a marker `LineItem` (text or image) into the first `ParagraphPageable`
/// found in the `positioned_children` tree. Handles both direct children and
/// paragraphs nested inside `BlockPageable` wrappers. Returns `true` if
/// injection succeeded.
pub(super) fn inject_inside_marker_item_into_children(
    children: &mut [PositionedChild],
    marker_item: LineItem,
) -> bool {
    let target_idx = children
        .iter()
        .position(|pc| has_paragraph_descendant(pc.child.as_ref()));

    let Some(idx) = target_idx else {
        return false;
    };

    let pc = &mut children[idx];
    let marker_width: f32 = match &marker_item {
        LineItem::Text(run) => run.glyphs.iter().map(|g| g.x_advance * run.font_size).sum(),
        LineItem::Image(img) => img.width,
        LineItem::InlineBox(ib) => ib.width,
    };

    // Direct ParagraphPageable child
    if let Some(para) = pc.child.as_any().downcast_ref::<ParagraphPageable>() {
        let mut para_clone = para.clone();
        if para_clone.lines.is_empty() {
            // Empty paragraph — create a line with just the marker.
            let line_height = match &marker_item {
                LineItem::Text(run) => run.font_size * DEFAULT_LINE_HEIGHT_RATIO,
                LineItem::Image(img) => img.height,
                LineItem::InlineBox(ib) => ib.height,
            };
            para_clone.lines.push(ShapedLine {
                height: line_height,
                baseline: line_height / DEFAULT_LINE_HEIGHT_RATIO,
                items: vec![marker_item],
            });
        } else {
            for item in &mut para_clone.lines[0].items {
                match item {
                    LineItem::Text(run) => run.x_offset += marker_width,
                    LineItem::Image(i) => i.x_offset += marker_width,
                    LineItem::InlineBox(ib) => ib.x_offset += marker_width,
                }
            }
            para_clone.lines[0].items.insert(0, marker_item);
            inline_root::recalculate_paragraph_line_boxes(&mut para_clone.lines);
        }
        para_clone.cached_height = para_clone.lines.iter().map(|l| l.height).sum();
        pc.child = Box::new(para_clone);
        return true;
    }

    // ParagraphPageable nested inside a BlockPageable (e.g. <p> with styles)
    if let Some(block) = pc.child.as_any().downcast_ref::<BlockPageable>() {
        let mut block_clone = block.clone();
        if inject_inside_marker_item_into_children(&mut block_clone.children, marker_item) {
            let wrap_w = block_clone.layout_size.map(|s| s.width).unwrap_or(10000.0);
            block_clone.wrap(wrap_w, 10000.0);
            pc.child = Box::new(block_clone);
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blitz_adapter::Marker;
    use crate::image::ImageFormat;
    use crate::pageable::{BlockPageable, PositionedChild, SpacerPageable};
    use crate::paragraph::{
        InlineBoxItem, InlineImage, ParagraphPageable, ShapedGlyph, ShapedGlyphRun, ShapedLine,
        TextDecoration, VerticalAlign,
    };
    use std::sync::Arc;

    // ─── Helpers ──────────────────────────────────────────────────────────────

    fn dummy_arc() -> Arc<Vec<u8>> {
        Arc::new(vec![])
    }

    fn text_run(font_size: f32, x_offset: f32, text: &str) -> ShapedGlyphRun {
        ShapedGlyphRun {
            font_data: dummy_arc(),
            font_index: 0,
            font_size,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![ShapedGlyph {
                id: 1,
                x_advance: 0.5,
                x_offset: 0.0,
                y_offset: 0.0,
                text_range: 0..text.len(),
            }],
            text: text.to_string(),
            x_offset,
            link: None,
        }
    }

    fn text_run_with_two_glyphs(font_size: f32) -> ShapedGlyphRun {
        ShapedGlyphRun {
            font_data: dummy_arc(),
            font_index: 0,
            font_size,
            color: [255, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![
                ShapedGlyph {
                    id: 1,
                    x_advance: 0.5,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    text_range: 0..1,
                },
                ShapedGlyph {
                    id: 2,
                    x_advance: 0.5,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    text_range: 1..2,
                },
            ],
            text: "\u{2022} ".to_string(),
            x_offset: 0.0,
            link: None,
        }
    }

    fn paragraph_with_items(items: Vec<LineItem>) -> ParagraphPageable {
        ParagraphPageable::new(vec![ShapedLine {
            height: 12.0,
            baseline: 10.0,
            items,
        }])
    }

    fn in_flow(child: Box<dyn crate::pageable::Pageable>) -> PositionedChild {
        PositionedChild::in_flow(child, 0.0, 0.0)
    }

    fn noto_sans_bytes() -> Arc<Vec<u8>> {
        let bytes = std::fs::read("../../examples/.fonts/NotoSans-Regular.ttf")
            .expect("NotoSans-Regular.ttf not found; run from crates/fulgur/");
        Arc::new(bytes)
    }

    // ─── inject_inside_marker_item_into_children ──────────────────────────────

    #[test]
    fn inject_empty_children_returns_false() {
        let marker = LineItem::Text(text_run(12.0, 0.0, "•"));
        assert!(!inject_inside_marker_item_into_children(&mut [], marker));
    }

    #[test]
    fn inject_no_paragraph_descendant_returns_false() {
        let mut children = vec![in_flow(Box::new(SpacerPageable::new(10.0)))];
        let marker = LineItem::Text(text_run(12.0, 0.0, "•"));
        assert!(!inject_inside_marker_item_into_children(
            &mut children,
            marker
        ));
    }

    #[test]
    fn inject_text_marker_prepended_and_existing_items_shifted() {
        let existing = LineItem::Text(text_run(12.0, 5.0, "Hello"));
        let para = paragraph_with_items(vec![existing]);
        let mut children = vec![in_flow(Box::new(para))];

        // Two glyphs of x_advance 0.5 at font_size 12 → marker_width = 2 × 0.5 × 12 = 12 pt
        let marker = LineItem::Text(text_run_with_two_glyphs(12.0));
        let result = inject_inside_marker_item_into_children(&mut children, marker);
        assert!(result);

        let para = children[0]
            .child
            .as_any()
            .downcast_ref::<ParagraphPageable>()
            .unwrap();
        let line = &para.lines[0];
        assert_eq!(line.items.len(), 2, "marker + original");
        // marker text is the first item
        if let LineItem::Text(r) = &line.items[0] {
            assert_eq!(r.text, "\u{2022} ");
        } else {
            panic!("expected Text at index 0");
        }
        // original item's x_offset shifted by marker_width (12)
        if let LineItem::Text(r) = &line.items[1] {
            assert!(
                (r.x_offset - 17.0).abs() < 0.01,
                "x_offset should be 5+12=17, got {}",
                r.x_offset
            );
        } else {
            panic!("expected Text at index 1");
        }
    }

    #[test]
    fn inject_image_marker_shifts_existing_items_by_image_width() {
        let existing = LineItem::Text(text_run(12.0, 3.0, "A"));
        let para = paragraph_with_items(vec![existing]);
        let mut children = vec![in_flow(Box::new(para))];

        let img = InlineImage {
            data: dummy_arc(),
            format: ImageFormat::Png,
            width: 20.0,
            height: 10.0,
            x_offset: 0.0,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: 0.0,
            link: None,
        };
        let result = inject_inside_marker_item_into_children(&mut children, LineItem::Image(img));
        assert!(result);

        let para = children[0]
            .child
            .as_any()
            .downcast_ref::<ParagraphPageable>()
            .unwrap();
        // original Text item shifted by img.width = 20
        if let LineItem::Text(r) = &para.lines[0].items[1] {
            assert!(
                (r.x_offset - 23.0).abs() < 0.01,
                "x_offset should be 3+20=23, got {}",
                r.x_offset
            );
        } else {
            panic!("expected Text at index 1");
        }
    }

    #[test]
    fn inject_inline_box_marker_shifts_by_box_width() {
        let existing = LineItem::Text(text_run(12.0, 0.0, "Z"));
        let para = paragraph_with_items(vec![existing]);
        let mut children = vec![in_flow(Box::new(para))];

        let ib = InlineBoxItem {
            content: Box::new(SpacerPageable::new(0.0)),
            width: 15.0,
            height: 12.0,
            x_offset: 0.0,
            computed_y: 0.0,
            link: None,
            opacity: 1.0,
            visible: true,
        };
        let result =
            inject_inside_marker_item_into_children(&mut children, LineItem::InlineBox(ib));
        assert!(result);

        let para = children[0]
            .child
            .as_any()
            .downcast_ref::<ParagraphPageable>()
            .unwrap();
        if let LineItem::Text(r) = &para.lines[0].items[1] {
            assert!(
                (r.x_offset - 15.0).abs() < 0.01,
                "x_offset should be 0+15=15, got {}",
                r.x_offset
            );
        } else {
            panic!("expected Text at index 1");
        }
    }

    #[test]
    fn inject_text_marker_into_empty_paragraph_creates_line() {
        let para = ParagraphPageable::new(vec![]);
        let mut children = vec![in_flow(Box::new(para))];

        let run = text_run(10.0, 0.0, "•");
        let result = inject_inside_marker_item_into_children(&mut children, LineItem::Text(run));
        assert!(result);

        let para = children[0]
            .child
            .as_any()
            .downcast_ref::<ParagraphPageable>()
            .unwrap();
        assert_eq!(
            para.lines.len(),
            1,
            "a line should be created for the marker"
        );
        assert_eq!(para.lines[0].items.len(), 1);
        // line_height = font_size × DEFAULT_LINE_HEIGHT_RATIO = 10 × 1.2 = 12
        assert!((para.lines[0].height - 12.0).abs() < 0.01);
    }

    #[test]
    fn inject_image_marker_into_empty_paragraph_uses_image_height() {
        let para = ParagraphPageable::new(vec![]);
        let mut children = vec![in_flow(Box::new(para))];

        let img = InlineImage {
            data: dummy_arc(),
            format: ImageFormat::Png,
            width: 8.0,
            height: 18.0,
            x_offset: 0.0,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: 0.0,
            link: None,
        };
        let result = inject_inside_marker_item_into_children(&mut children, LineItem::Image(img));
        assert!(result);

        let para = children[0]
            .child
            .as_any()
            .downcast_ref::<ParagraphPageable>()
            .unwrap();
        assert_eq!(para.lines.len(), 1);
        assert!(
            (para.lines[0].height - 18.0).abs() < 0.01,
            "line height should match image height"
        );
    }

    #[test]
    fn inject_paragraph_nested_in_block_succeeds() {
        let para = paragraph_with_items(vec![LineItem::Text(text_run(12.0, 0.0, "Hi"))]);
        let block = BlockPageable::new(vec![Box::new(para)]);
        let mut children = vec![in_flow(Box::new(block))];

        let marker = LineItem::Text(text_run(12.0, 0.0, "•"));
        assert!(inject_inside_marker_item_into_children(
            &mut children,
            marker
        ));

        // Verify the block child was replaced and contains the updated paragraph
        let block = children[0]
            .child
            .as_any()
            .downcast_ref::<BlockPageable>()
            .unwrap();
        let para = block.children[0]
            .child
            .as_any()
            .downcast_ref::<ParagraphPageable>()
            .unwrap();
        assert_eq!(para.lines[0].items.len(), 2, "marker + original item");
    }

    // ─── shape_marker_with_skrifa ──────────────────────────────────────────────

    #[test]
    fn shape_marker_invalid_font_bytes_returns_none() {
        let bad_font = Arc::new(b"not a font".to_vec());
        assert!(
            shape_marker_with_skrifa(
                &Marker::Char('\u{2022}'),
                &bad_font,
                0,
                12.0,
                [0, 0, 0, 255]
            )
            .is_none()
        );
    }

    #[test]
    fn shape_marker_char_appends_trailing_space() {
        let font_data = noto_sans_bytes();
        let run = shape_marker_with_skrifa(&Marker::Char('A'), &font_data, 0, 12.0, [0, 0, 0, 255])
            .expect("valid font should succeed");
        // Marker::Char appends a space → "A "
        assert_eq!(run.text, "A ");
        assert_eq!(run.glyphs.len(), 2, "two chars → two glyphs");
        // glyph advances normalised by font_size
        for g in &run.glyphs {
            assert!(g.x_advance >= 0.0, "x_advance must be non-negative");
        }
        assert_eq!(run.font_size, 12.0);
        assert_eq!(run.color, [0, 0, 0, 255]);
        assert_eq!(run.x_offset, 0.0);
    }

    #[test]
    fn shape_marker_string_uses_text_verbatim() {
        let font_data = noto_sans_bytes();
        let run = shape_marker_with_skrifa(
            &Marker::String("1. ".to_string()),
            &font_data,
            0,
            10.0,
            [255, 0, 0, 255],
        )
        .expect("valid font should succeed");
        // Marker::String → text unchanged, no extra space
        assert_eq!(run.text, "1. ");
        assert_eq!(run.glyphs.len(), 3, "'1', '.', ' ' → three glyphs");
        assert_eq!(run.color, [255, 0, 0, 255]);
        assert_eq!(run.font_size, 10.0);
    }

    #[test]
    fn shape_marker_glyph_byte_offsets_cover_text() {
        let font_data = noto_sans_bytes();
        let run = shape_marker_with_skrifa(&Marker::Char('A'), &font_data, 0, 12.0, [0, 0, 0, 255])
            .unwrap();
        // text_range of last glyph should end at text.len()
        let last = run.glyphs.last().unwrap();
        assert_eq!(last.text_range.end, run.text.len());
    }

    // ─── find_marker_font ──────────────────────────────────────────────────────

    #[test]
    fn find_marker_font_no_assets_no_children_returns_none() {
        assert!(find_marker_font(&Marker::Char('\u{2022}'), None, &[]).is_none());
    }

    #[test]
    fn find_marker_font_from_paragraph_child() {
        let font_data = noto_sans_bytes();
        let run = ShapedGlyphRun {
            font_data: Arc::clone(&font_data),
            font_index: 0,
            font_size: 12.0,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![],
            text: "A".to_string(),
            x_offset: 0.0,
            link: None,
        };
        let para = paragraph_with_items(vec![LineItem::Text(run)]);
        let children = vec![in_flow(Box::new(para))];

        let result = find_marker_font(&Marker::Char('A'), None, &children);
        assert!(result.is_some(), "should find font that covers 'A'");
        let (found, idx) = result.unwrap();
        assert_eq!(idx, 0);
        assert_eq!(found.len(), font_data.len());
    }

    #[test]
    fn find_marker_font_from_block_wrapping_paragraph() {
        let font_data = noto_sans_bytes();
        let run = ShapedGlyphRun {
            font_data: Arc::clone(&font_data),
            font_index: 0,
            font_size: 12.0,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![],
            text: "B".to_string(),
            x_offset: 0.0,
            link: None,
        };
        let para = paragraph_with_items(vec![LineItem::Text(run)]);
        let block = BlockPageable::new(vec![Box::new(para)]);
        let children = vec![in_flow(Box::new(block))];

        let result = find_marker_font(&Marker::Char('B'), None, &children);
        assert!(
            result.is_some(),
            "should find font recursively through BlockPageable"
        );
    }

    #[test]
    fn find_marker_font_invalid_font_bytes_skipped() {
        // A ShapedGlyphRun with bytes that skrifa cannot parse → skipped, returns None.
        let run = ShapedGlyphRun {
            font_data: Arc::new(b"garbage".to_vec()),
            font_index: 0,
            font_size: 12.0,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![],
            text: "X".to_string(),
            x_offset: 0.0,
            link: None,
        };
        let para = paragraph_with_items(vec![LineItem::Text(run)]);
        let children = vec![in_flow(Box::new(para))];
        assert!(find_marker_font(&Marker::Char('X'), None, &children).is_none());
    }
}
