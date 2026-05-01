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
    let (iw, ih) = ImageRender::decode_dimensions(data, format)?;
    let intrinsic_w = px_to_pt(iw as f32);
    let intrinsic_h = px_to_pt(ih as f32);
    Some(crate::draw_primitives::clamp_marker_size(
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
            let entry = crate::drawables::ImageEntry {
                image_data: Arc::clone(data),
                format,
                width,
                height,
                opacity: 1.0,
                visible: true,
            };
            Some(ListItemMarker::Image {
                marker: ImageMarker::Raster(entry),
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
                crate::draw_primitives::clamp_marker_size(intrinsic_w, intrinsic_h, line_height);
            let entry = crate::drawables::SvgEntry {
                tree: Arc::new(tree),
                width,
                height,
                opacity: 1.0,
                visible: true,
            };
            Some(ListItemMarker::Image {
                marker: ImageMarker::Svg(entry),
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
/// bundle is provided), falls back to scanning `paragraphs` already registered
/// in `Drawables` for a `ShapedGlyphRun` whose font covers the marker.
///
/// Returns `None` only when no font source is available at all (empty `<li>`
/// without asset fonts and without already-registered paragraphs).
pub(super) fn find_marker_font(
    marker: &Marker,
    assets: Option<&AssetBundle>,
    drawables: &crate::drawables::Drawables,
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

    // Fallback: scan already-registered paragraphs in Drawables for a font
    // whose charmap covers the marker characters. BTreeMap iteration is
    // deterministic so the chosen font is stable across runs.
    for entry in drawables.paragraphs.values() {
        for line in &entry.lines {
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
    None
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
