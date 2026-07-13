use super::*;
use crate::blitz_adapter::{Marker, marker_skrifa_text, marker_to_string};
use crate::units::F32Units;

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
    line_height: crate::units::Pt,
) -> Option<(crate::units::Pt, crate::units::Pt)> {
    let (iw, ih) = ImageRender::decode_dimensions(data, format)?;
    let intrinsic_w = (iw as f32).as_px().in_pt();
    let intrinsic_h = (ih as f32).as_px().in_pt();
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
    line_height: crate::units::Pt,
    assets: Option<&AssetBundle>,
) -> Option<ListItemMarker> {
    use crate::image::AssetKind;

    // Zero or negative line-height (e.g. list-style-position: inside where
    // extract_marker_lines returns 0.0) would clamp image size to 0x0.
    // Return None so the caller falls back to the text marker instead of
    // creating an invisible image marker that suppresses the fallback.
    if line_height <= crate::units::Pt::ZERO {
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
            let intrinsic_w = size.width().as_px().in_pt();
            let intrinsic_h = size.height().as_px().in_pt();
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
    first_line_height: crate::units::Pt,
    assets: Option<&AssetBundle>,
) -> Option<InlineImage> {
    use crate::image::AssetKind;

    let elem_data = node.element_data()?;
    let list_data = elem_data.list_item_data.as_ref()?;
    if !crate::blitz_adapter::is_list_position_inside(&list_data.position) {
        return None;
    }
    if first_line_height <= crate::units::Pt::ZERO {
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
                x_offset: crate::units::Pt::ZERO,
                vertical_align: VerticalAlign::Baseline,
                opacity: 1.0,
                visible: true,
                computed_y: crate::units::Pt::ZERO,
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
) -> (Vec<ShapedLine>, crate::units::Pt, crate::units::Pt) {
    let elem_data = match node.element_data() {
        Some(d) => d,
        None => return (Vec::new(), crate::units::Pt::ZERO, crate::units::Pt::ZERO),
    };
    let list_item_data = match &elem_data.list_item_data {
        Some(d) => d,
        None => return (Vec::new(), crate::units::Pt::ZERO, crate::units::Pt::ZERO),
    };
    let Some(parley_layout) =
        crate::blitz_adapter::list_position_outside_layout(&list_item_data.position)
    else {
        return (Vec::new(), crate::units::Pt::ZERO, crate::units::Pt::ZERO);
    };

    let marker_text: std::sync::Arc<str> =
        std::sync::Arc::from(marker_to_string(&list_item_data.marker));

    let mut shaped_lines = Vec::new();
    let mut max_width = crate::units::Pt::ZERO;
    let mut line_height_pt = crate::units::Pt::ZERO;

    for line in parley_layout.lines() {
        let metrics = line.metrics();
        if line_height_pt == crate::units::Pt::ZERO {
            // Marker-row height returned from this fn (a distinct value from the
            // per-line `ShapedLine.height` below, though both are `Pt`).
            line_height_pt = metrics.line_height.as_px().in_pt();
        }
        let mut items = Vec::new();
        let mut line_width = crate::units::Pt::ZERO;
        let mut prev_run_key = usize::MAX;
        let mut run_glyph_offset = 0usize;

        for item in line.items() {
            if let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item {
                let run = glyph_run.run();
                let font_ref = run.font();
                let font_index = font_ref.index;
                let font_arc = ctx.get_or_insert_font(font_ref);
                // Parley reports font size in CSS px; Drawables / Krilla
                // consume pt. See `extract_paragraph` for the matching
                // conversion. Glyph ratios stay unitless by dividing by
                // the original parley value.
                let font_size_parley = run.font_size();
                let font_size = font_size_parley.as_px().in_pt();

                let brush = &glyph_run.style().brush;
                let color = get_text_color(doc, brush.id);

                let run_key = run.cluster_range().start;
                if run_key != prev_run_key {
                    prev_run_key = run_key;
                    run_glyph_offset = 0;
                }
                let glyph_start = run_glyph_offset;

                let mut annotated = run
                    .visual_clusters()
                    .flat_map(|cluster| {
                        let r = cluster.text_range();
                        cluster.glyphs().map(move |g| (r.clone(), g))
                    })
                    .skip(glyph_start);

                let mut glyphs = Vec::new();
                for g in glyph_run.glyphs() {
                    let (text_range, _) = annotated.next().unwrap_or_else(|| {
                        panic!(
                            "annotated cluster iterator exhausted before glyph_run.glyphs(); \
                             run cluster_range={:?}, glyph_start={glyph_start}",
                            run.cluster_range()
                        )
                    });
                    run_glyph_offset += 1;
                    line_width += g.advance.as_px().in_pt();
                    glyphs.push(ShapedGlyph {
                        id: g.id,
                        x_advance: g.advance / font_size_parley,
                        x_offset: g.x / font_size_parley,
                        y_offset: g.y / font_size_parley,
                        text_range,
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
                        text: std::sync::Arc::clone(&marker_text),
                        x_offset: glyph_run.offset().as_px().in_pt(),
                        link: None,
                    }));
                }
            }
        }

        max_width = max_width.max(line_width);
        shaped_lines.push(ShapedLine {
            height: metrics.line_height.as_px().in_pt(),
            baseline: metrics.baseline.as_px().in_pt(),
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
    font_size: crate::units::Pt,
    color: [u8; 4],
) -> Option<ShapedGlyphRun> {
    let text: std::sync::Arc<str> = std::sync::Arc::from(marker_skrifa_text(marker));

    let font_ref = skrifa::FontRef::from_index(font_data, font_index).ok()?;
    let charmap = font_ref.charmap();
    let glyph_metrics = font_ref.glyph_metrics(
        // skrifa external boundary: font size is a raw f32 in font-metric space.
        skrifa::instance::Size::new(font_size.to_f32()),
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
            x_advance: advance / font_size.to_f32(),
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
        x_offset: crate::units::Pt::ZERO,
        link: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::AssetBundle;
    use crate::blitz_adapter::Marker;
    use crate::drawables::{Drawables, ParagraphEntry};
    use crate::image::ImageFormat;

    // Minimal 1×1 red PNG (same bytes as in convert/replaced.rs tests).
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

    /// Load NotoSans-Regular WOFF2 and return decoded TTF bytes — the same
    /// format that `AssetBundle::fonts` stores after `add_font_bytes`.
    fn load_noto_sans_ttf() -> Arc<Vec<u8>> {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fonts/NotoSans-Regular.woff2");
        let woff2 =
            std::fs::read(&fixture).expect("NotoSans-Regular.woff2 missing from test fixtures");
        let mut bundle = AssetBundle::new();
        bundle.add_font_bytes(woff2).expect("WOFF2 decode failed");
        Arc::clone(&bundle.fonts[0])
    }

    // ── size_raster_marker ────────────────────────────────────────────────────

    #[test]
    fn size_raster_marker_valid_png_within_line_height_passes_through() {
        // 1×1 px PNG → intrinsic 0.75×0.75 pt; line_height=12 → no downscale.
        let result = size_raster_marker(&sample_png_arc(), ImageFormat::Png, 12.0.as_pt());
        assert!(result.is_some());
        let (w, h) = result.unwrap();
        let (w, h) = (w.to_f32(), h.to_f32());
        assert!((w - 0.75).abs() < 1e-4, "expected w≈0.75, got {w}");
        assert!((h - 0.75).abs() < 1e-4, "expected h≈0.75, got {h}");
    }

    #[test]
    fn size_raster_marker_invalid_bytes_returns_none() {
        let bad = Arc::new(vec![0u8; 8]);
        let result = size_raster_marker(&bad, ImageFormat::Png, 12.0.as_pt());
        assert!(result.is_none());
    }

    #[test]
    fn size_raster_marker_small_line_height_scales_down() {
        // Intrinsic 0.75×0.75 pt, line_height=0.5 → scale=0.5/0.75≈0.667
        // → result height clamped to line_height, width scaled proportionally.
        let result = size_raster_marker(&sample_png_arc(), ImageFormat::Png, 0.5.as_pt());
        assert!(result.is_some());
        let (w, h) = result.unwrap();
        let (w, h) = (w.to_f32(), h.to_f32());
        assert!((h - 0.5).abs() < 1e-4, "expected h≈0.5, got {h}");
        assert!((w - 0.5).abs() < 1e-4, "expected w≈0.5, got {w}");
    }

    // ── find_marker_font ──────────────────────────────────────────────────────

    #[test]
    fn find_marker_font_no_assets_empty_drawables_returns_none() {
        let drawables = Drawables::new();
        let result = find_marker_font(&Marker::Char('•'), None, &drawables);
        assert!(result.is_none());
    }

    #[test]
    fn find_marker_font_empty_bundle_empty_drawables_returns_none() {
        let bundle = AssetBundle::new();
        let drawables = Drawables::new();
        let result = find_marker_font(&Marker::Char('•'), Some(&bundle), &drawables);
        assert!(result.is_none());
    }

    #[test]
    fn find_marker_font_bundle_covering_char_returns_font() {
        let font_data = load_noto_sans_ttf();
        let mut bundle = AssetBundle::new();
        bundle.fonts.push(Arc::clone(&font_data));
        let drawables = Drawables::new();

        let result = find_marker_font(&Marker::Char('•'), Some(&bundle), &drawables);
        assert!(result.is_some(), "NotoSans must cover U+2022");
        let (fd, idx) = result.unwrap();
        assert_eq!(idx, 0);
        assert_eq!(fd.len(), font_data.len());
    }

    #[test]
    fn find_marker_font_bundle_covering_string_marker() {
        let font_data = load_noto_sans_ttf();
        let mut bundle = AssetBundle::new();
        bundle.fonts.push(Arc::clone(&font_data));
        let drawables = Drawables::new();

        // "1. " — whitespace chars are filtered out before the charmap check,
        // so only '1' and '.' must be covered.
        let result = find_marker_font(
            &Marker::String("1. ".to_string()),
            Some(&bundle),
            &drawables,
        );
        assert!(result.is_some());
    }

    #[test]
    fn find_marker_font_fallback_from_drawables_paragraph() {
        let font_data = load_noto_sans_ttf();
        let empty_bundle = AssetBundle::new();

        let glyph_run = ShapedGlyphRun {
            font_data: Arc::clone(&font_data),
            font_index: 0,
            font_size: 12.0.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![ShapedGlyph {
                id: 1,
                x_advance: 0.5,
                x_offset: 0.0,
                y_offset: 0.0,
                text_range: 0..1,
            }],
            text: Arc::from("A"),
            x_offset: crate::units::Pt::ZERO,
            link: None,
        };
        let line = ShapedLine {
            height: 12.0.as_pt(),
            baseline: 9.0.as_pt(),
            items: vec![LineItem::Text(glyph_run)],
        };
        let mut drawables = Drawables::new();
        drawables.paragraphs.insert(
            1,
            ParagraphEntry {
                lines: vec![line],
                opacity: 1.0,
                visible: true,
                id: None,
            },
        );

        let result = find_marker_font(&Marker::Char('•'), Some(&empty_bundle), &drawables);
        assert!(
            result.is_some(),
            "should fall back to NotoSans from drawables"
        );
        let (_, idx) = result.unwrap();
        assert_eq!(idx, 0);
    }

    // find_marker_font: when the bundle contains a single-face font (NotoSans) that
    // does NOT cover the marker character, the inner TTC loop's `else { break }`
    // (lines 297-299) is triggered at the second `from_index` call:
    //   idx=0: from_index succeeds; charmap.map(U+E000) is None → continue.
    //   idx=1: from_index fails (single-face, no sub-font 1) → else { break }.
    // No Drawables fallback → returns None.
    //
    // U+E000 is the first Private Use Area codepoint; standard fonts never map it.
    #[test]
    fn find_marker_font_single_face_font_missing_char_hits_ttc_break() {
        let font_data = load_noto_sans_ttf();
        let mut bundle = AssetBundle::new();
        bundle.fonts.push(Arc::clone(&font_data));
        let drawables = Drawables::new();

        let result = find_marker_font(&Marker::Char('\u{E000}'), Some(&bundle), &drawables);
        assert!(
            result.is_none(),
            "U+E000 (Private Use Area) must not be found in NotoSans or empty drawables"
        );
    }

    // ── shape_marker_with_skrifa ──────────────────────────────────────────────

    #[test]
    fn shape_marker_with_skrifa_invalid_font_returns_none() {
        let bad_font = Arc::new(vec![0u8; 16]);
        let result = shape_marker_with_skrifa(
            &Marker::Char('•'),
            &bad_font,
            0,
            12.0.as_pt(),
            [0, 0, 0, 255],
        );
        assert!(result.is_none());
    }

    #[test]
    fn shape_marker_with_skrifa_char_produces_two_glyphs() {
        // Marker::Char('•') → skrifa text "• " (2 chars = 2 glyphs).
        let font_data = load_noto_sans_ttf();
        let result = shape_marker_with_skrifa(
            &Marker::Char('•'),
            &font_data,
            0,
            12.0.as_pt(),
            [255, 0, 0, 255],
        );
        assert!(result.is_some());
        let run = result.unwrap();
        assert_eq!(run.glyphs.len(), 2, "bullet + trailing space = 2 glyphs");
        assert_eq!(&*run.text, "• ");
        assert_eq!(run.font_size.to_f32(), 12.0);
        assert_eq!(run.color, [255, 0, 0, 255]);
        assert_eq!(run.font_index, 0);
        assert_eq!(run.x_offset.to_f32(), 0.0);
    }

    #[test]
    fn shape_marker_with_skrifa_string_marker_matches_char_count() {
        // Marker::String("1. ") → skrifa text "1. " (3 chars = 3 glyphs).
        let font_data = load_noto_sans_ttf();
        let result = shape_marker_with_skrifa(
            &Marker::String("1. ".to_string()),
            &font_data,
            0,
            10.0.as_pt(),
            [0, 0, 0, 255],
        );
        assert!(result.is_some());
        let run = result.unwrap();
        assert_eq!(run.glyphs.len(), 3, "\"1. \" = 3 chars = 3 glyphs");
        assert_eq!(&*run.text, "1. ");
    }

    #[test]
    fn shape_marker_with_skrifa_x_advance_is_normalised_by_font_size() {
        // x_advance values are stored as advance / font_size (unit-less ratio),
        // so they should be in [0, ~2] for typical Latin glyphs.
        let font_data = load_noto_sans_ttf();
        let result = shape_marker_with_skrifa(
            &Marker::String("A".to_string()),
            &font_data,
            0,
            12.0.as_pt(),
            [0, 0, 0, 255],
        );
        let run = result.unwrap();
        for g in &run.glyphs {
            assert!(g.x_advance >= 0.0, "x_advance must be non-negative");
            assert!(
                g.x_advance < 5.0,
                "x_advance should be a unit-less ratio, got {}",
                g.x_advance
            );
        }
    }

    #[test]
    fn shape_marker_with_skrifa_text_ranges_cover_full_string() {
        let font_data = load_noto_sans_ttf();
        let result = shape_marker_with_skrifa(
            &Marker::String("AB".to_string()),
            &font_data,
            0,
            12.0.as_pt(),
            [0, 0, 0, 255],
        );
        let run = result.unwrap();
        // Each glyph covers exactly one character's byte span; together they
        // tile the full text.  Check ranges are non-empty and within bounds.
        let text_len = run.text.len();
        for g in &run.glyphs {
            assert!(
                g.text_range.start < g.text_range.end,
                "range must be non-empty"
            );
            assert!(g.text_range.end <= text_len, "range must stay within text");
        }
        // The last glyph's range should reach the end of the string.
        let last = run.glyphs.last().unwrap();
        assert_eq!(
            last.text_range.end, text_len,
            "last glyph must reach text end"
        );
    }

    // ── smoke tests via Engine::render_html ──────────────────────────────────
    //
    // These exercises cover paths in `resolve_list_style_image_asset`,
    // `resolve_list_marker`, and `resolve_inside_image_marker` that require a
    // live Blitz document and cannot be reached by the pure-function tests above.
    //
    // Key dependency: `resolve_list_marker` guards on `line_height <= Pt::ZERO`
    // (the value returned by `extract_marker_lines`). In the outside-marker path,
    // `extract_marker_lines` queries Parley for the marker's glyph metrics; if no
    // font covers the bullet '•' the layout is empty and line_height stays zero,
    // causing an early return before `resolve_list_style_image_asset` is reached.
    // Bundling NotoSans-Regular ensures Parley can shape the bullet and returns a
    // non-zero line_height, allowing the image-resolution path to proceed.
    //
    // The inside-marker path (`resolve_inside_image_marker`) is different: it
    // computes `line_height` directly from CSS `font-size` / `line-height`
    // properties, so it reaches `resolve_list_style_image_asset` even without
    // bundled fonts.

    // Minimal valid SVG used as a list-style-image in the smoke tests below.
    const MINIMAL_SVG: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg' width='8' height='8'>\
          <circle cx='4' cy='4' r='3' fill='blue'/></svg>";

    fn noto_bundle_with_png() -> crate::asset::AssetBundle {
        let font_data = load_noto_sans_ttf();
        let mut bundle = crate::asset::AssetBundle::new();
        bundle.fonts.push(font_data);
        bundle.add_image("dot.png", TEST_PNG_1X1.to_vec());
        bundle
    }

    fn noto_bundle_with_svg() -> crate::asset::AssetBundle {
        let font_data = load_noto_sans_ttf();
        let mut bundle = crate::asset::AssetBundle::new();
        bundle.fonts.push(font_data);
        bundle.add_image("bullet.svg", MINIMAL_SVG.to_vec());
        bundle
    }

    // resolve_list_marker — Raster arm (lines 72-86 in list_marker.rs):
    // An outside-positioned `<li>` with `list-style-image: url("dot.png")` and
    // NotoSans bundled. NotoSans covers '•' (U+2022) so Parley produces a
    // non-zero line_height, allowing `resolve_list_marker` to proceed past the
    // `line_height <= Pt::ZERO` guard and call `resolve_list_style_image_asset`.
    // The PNG resolves to `AssetKind::Raster` → the `Raster` arm is taken and a
    // `ListItemMarker::Image { marker: ImageMarker::Raster(...) }` is built.
    //
    // Covers in `list_marker.rs`:
    //   - `resolve_list_style_image_asset`: lines 15 (styles), 17-19 (Url match),
    //     21-22 (Valid URL arm), 25-26 (asset lookup)
    //   - `resolve_list_marker`: lines 70 (call), 72 (Raster arm), 73-86 (entry)
    #[test]
    fn smoke_outside_marker_png_list_style_image_with_bundled_font() {
        let bundle = noto_bundle_with_png();
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .system_fonts(false)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul style="list-style-image: url('dot.png')">
                    <li>Item with PNG bullet</li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // resolve_list_marker — SVG arm (lines 89-108 in list_marker.rs):
    // Same setup as above but with an SVG image. The SVG resolves to
    // `AssetKind::Svg` → the `Svg` arm is taken: `usvg::Tree::from_data` parses
    // the SVG, dimensions are clamped, and a `ListItemMarker::Image { marker:
    // ImageMarker::Svg(...) }` is built.
    //
    // Covers in `list_marker.rs`:
    //   - `resolve_list_style_image_asset`: same as above for lines 15-26
    //   - `resolve_list_marker`: lines 88 (Svg arm), 89-107 (SVG tree + entry)
    #[test]
    fn smoke_outside_marker_svg_list_style_image_with_bundled_font() {
        let bundle = noto_bundle_with_svg();
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .system_fonts(false)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul style="list-style-image: url('bullet.svg')">
                    <li>Item with SVG bullet</li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // resolve_inside_image_marker — SVG/Unknown fallback (line 151):
    // An inside-positioned `<li>` with a block child (so Branch 3 in
    // `try_convert` is entered) and `list-style-image: url("bullet.svg")`.
    // The inside-marker path computes `line_height` from CSS font metrics (not
    // from `extract_marker_lines`), so no bundled font is required to reach
    // `resolve_list_style_image_asset`. The SVG resolves to `AssetKind::Svg`,
    // which hits the `AssetKind::Svg | AssetKind::Unknown => None` arm — SVG
    // is not supported as an inline image in `resolve_inside_image_marker`.
    // The fallback then calls `find_marker_font`; NotoSans in the bundle covers
    // '•' so a text marker is produced.
    //
    // Covers in `list_marker.rs`:
    //   - `resolve_list_style_image_asset`: lines 15-26 (via inside path)
    //   - `resolve_inside_image_marker`: line 151 (Svg/Unknown arm)
    #[test]
    fn smoke_inside_marker_svg_list_style_image_falls_back_to_text() {
        let bundle = noto_bundle_with_svg();
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .system_fonts(false)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul style="list-style-position: inside; list-style-image: url('bullet.svg')">
                    <li><p>Block child keeps li non-inline-root</p></li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // resolve_list_marker — zero line_height guard (line 68):
    // `resolve_list_marker` is called with `line_height = Pt::ZERO` whenever
    // `extract_marker_lines` returns no lines (empty Parley layout). The guard
    // at line 67 (`if line_height <= Pt::ZERO { return None; }`) ensures we do
    // not call `resolve_list_style_image_asset` with a zero line_height that
    // would produce a 0×0 invisible image marker.
    //
    // This path is exercised by the existing outside-marker smoke tests that run
    // without any bundled font (Parley produces an empty layout → zero
    // line_height). The test below makes the guard path explicit using the
    // inside-marker fallback where the SVG is present but the inside path calls
    // `resolve_list_marker` for the *outside* sub-call site with zero line_height.
    //
    // NOTE: The guard is already indirectly covered by all smoke tests that render
    // a default `<ul><li>` without bundled fonts — this test documents the
    // behaviour rather than adding new coverage.
    #[test]
    fn smoke_outside_marker_zero_line_height_returns_text_marker() {
        // No fonts bundled → Parley produces empty layout → line_height = 0 →
        // resolve_list_marker returns None → marker falls back to text (empty
        // lines, zero width).
        let pdf = crate::engine::Engine::builder()
            .system_fonts(false)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul style="list-style-image: url('dot.png')">
                    <li>Bullet with no bundled font</li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }
}
