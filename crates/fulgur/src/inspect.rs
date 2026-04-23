use serde::Serialize;
use std::path::Path;

const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

#[derive(Debug, Serialize, PartialEq)]
pub struct InspectResult {
    pub pages: u32,
    pub metadata: Metadata,
    pub text_items: Vec<TextItem>,
    pub images: Vec<ImageItem>,
}

#[derive(Debug, Serialize, PartialEq, Default)]
pub struct Metadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct TextItem {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub text: String,
    pub font: String,
    pub font_size: f32,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ImageItem {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub format: String,
    pub width_px: u32,
    pub height_px: u32,
}

pub fn inspect(path: &Path) -> crate::Result<InspectResult> {
    let doc = lopdf::Document::load(path)
        .map_err(|e| crate::Error::Other(format!("Failed to load PDF: {e}")))?;

    let pages = doc.get_pages().len() as u32;
    let metadata = extract_metadata(&doc);
    let text_items = extract_text_items(&doc)?;
    let images = extract_image_items(&doc)?;

    Ok(InspectResult {
        pages,
        metadata,
        text_items,
        images,
    })
}

fn obj_as_name_str(obj: &lopdf::Object) -> Option<&str> {
    obj.as_name().ok().and_then(|b| std::str::from_utf8(b).ok())
}

fn extract_metadata(doc: &lopdf::Document) -> Metadata {
    let mut meta = Metadata::default();
    let info_id = match doc.trailer.get(b"Info") {
        Ok(obj) => match obj.as_reference() {
            Ok(id) => id,
            Err(_) => return meta,
        },
        Err(_) => return meta,
    };
    let info = match doc.get_object(info_id) {
        Ok(lopdf::Object::Dictionary(d)) => d,
        _ => return meta,
    };

    let get_str = |dict: &lopdf::Dictionary, key: &[u8]| -> Option<String> {
        dict.get(key)
            .ok()
            .and_then(|o| o.as_str().ok())
            .map(decode_pdf_string)
    };

    meta.title = get_str(info, b"Title");
    meta.author = get_str(info, b"Author");
    meta.creator = get_str(info, b"Creator");
    meta.created_at = get_str(info, b"CreationDate");
    meta.modified_at = get_str(info, b"ModDate");
    meta
}

fn extract_text_items(doc: &lopdf::Document) -> crate::Result<Vec<TextItem>> {
    use lopdf::content::Operation;
    let mut items = Vec::new();

    for (&page_num, &page_id) in &doc.get_pages() {
        let content_bytes = match doc.get_page_content(page_id) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let content = match lopdf::content::Content::decode(&content_bytes) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let identity = IDENTITY;
        // Graphics state stack: (CTM, font_name, font_size).
        // Tf is part of the graphics state (PDF §8.4.5 Table 52), so q/Q save/restore it.
        let mut gs_stack: Vec<([f32; 6], String, f32)> =
            vec![(identity, "unknown".to_string(), 12.0)];
        // Text matrix linear components (scale/rotation); updated by Tm, reset by BT.
        let mut tm_a: f32 = 1.0;
        let mut tm_b: f32 = 0.0;
        let mut tm_c: f32 = 0.0;
        let mut tm_d: f32 = 1.0;
        // Text line matrix translation in user space; updated by Tm/Td/TD/T*, reset by BT.
        let mut tlm_e: f32 = 0.0;
        let mut tlm_f: f32 = 0.0;
        // Current text origin in page space.
        let mut tx: f32 = 0.0;
        let mut ty: f32 = 0.0;
        let mut font_name = String::from("unknown");
        let mut font_size: f32 = 12.0;
        let mut text_leading: f32 = 0.0;

        for Operation { operator, operands } in &content.operations {
            match operator.as_str() {
                "q" => {
                    let top = gs_stack.last().expect("gs_stack non-empty").clone();
                    gs_stack.push(top);
                }
                "Q" if gs_stack.len() > 1 => {
                    gs_stack.pop();
                    let (_, ref saved_font, saved_size) =
                        *gs_stack.last().expect("gs_stack non-empty after Q");
                    font_name = saved_font.clone();
                    font_size = saved_size;
                }
                "cm" if operands.len() == 6 => {
                    let new_m = [
                        obj_to_f32(&operands[0]),
                        obj_to_f32(&operands[1]),
                        obj_to_f32(&operands[2]),
                        obj_to_f32(&operands[3]),
                        obj_to_f32(&operands[4]),
                        obj_to_f32(&operands[5]),
                    ];
                    if let Some(gs) = gs_stack.last_mut() {
                        gs.0 = concat_matrix(&gs.0, &new_m);
                    }
                }
                "Tf" => {
                    if let (Some(name_obj), Some(size)) = (operands.first(), operands.get(1)) {
                        font_name = obj_as_name_str(name_obj).unwrap_or("unknown").to_string();
                        font_size = obj_to_f32(size);
                        if let Some(gs) = gs_stack.last_mut() {
                            gs.1 = font_name.clone();
                            gs.2 = font_size;
                        }
                    }
                }
                "TL" if !operands.is_empty() => {
                    text_leading = obj_to_f32(&operands[0]);
                }
                // BT resets the text matrix and text line matrix to identity (PDF §9.4.1).
                // tx/ty are derived from tlm + CTM: text origin is the CTM translation.
                "BT" => {
                    tm_a = 1.0;
                    tm_b = 0.0;
                    tm_c = 0.0;
                    tm_d = 1.0;
                    tlm_e = 0.0;
                    tlm_f = 0.0;
                    let ctm = gs_stack.last().map(|gs| &gs.0).unwrap_or(&identity);
                    tx = ctm[4];
                    ty = ctm[5];
                }
                "Tm" if operands.len() >= 6 => {
                    tm_a = obj_to_f32(&operands[0]);
                    tm_b = obj_to_f32(&operands[1]);
                    tm_c = obj_to_f32(&operands[2]);
                    tm_d = obj_to_f32(&operands[3]);
                    tlm_e = obj_to_f32(&operands[4]);
                    tlm_f = obj_to_f32(&operands[5]);
                    let ctm = gs_stack.last().map(|gs| &gs.0).unwrap_or(&identity);
                    tx = ctm[0] * tlm_e + ctm[2] * tlm_f + ctm[4];
                    ty = ctm[1] * tlm_e + ctm[3] * tlm_f + ctm[5];
                }
                // Td/TD advances the text line matrix in text space (PDF §9.4.2).
                // The offset (dx, dy) is in text coordinates; multiply through the
                // linear part of the text matrix to get user-space displacement.
                // TD also sets the text leading to -dy (PDF §9.4.2).
                "Td" | "TD" if operands.len() >= 2 => {
                    let dx = obj_to_f32(&operands[0]);
                    let dy = obj_to_f32(&operands[1]);
                    if operator == "TD" {
                        text_leading = -dy;
                    }
                    tlm_e += dx * tm_a + dy * tm_c;
                    tlm_f += dx * tm_b + dy * tm_d;
                    let ctm = gs_stack.last().map(|gs| &gs.0).unwrap_or(&identity);
                    tx = ctm[0] * tlm_e + ctm[2] * tlm_f + ctm[4];
                    ty = ctm[1] * tlm_e + ctm[3] * tlm_f + ctm[5];
                }
                // T* ≡ Td 0 -text_leading (PDF §9.4.2).
                "T*" => {
                    tlm_e += (-text_leading) * tm_c;
                    tlm_f += (-text_leading) * tm_d;
                    let ctm = gs_stack.last().map(|gs| &gs.0).unwrap_or(&identity);
                    tx = ctm[0] * tlm_e + ctm[2] * tlm_f + ctm[4];
                    ty = ctm[1] * tlm_e + ctm[3] * tlm_f + ctm[5];
                }
                "Tj" => {
                    if let Some(text_obj) = operands.first() {
                        if let Ok(bytes) = text_obj.as_str() {
                            let text = decode_pdf_string(bytes);
                            if !text.trim().is_empty() {
                                let w = estimate_width(&text, font_size);
                                items.push(TextItem {
                                    page: page_num,
                                    x: tx,
                                    y: ty,
                                    width: w,
                                    height: font_size,
                                    text,
                                    font: font_name.clone(),
                                    font_size,
                                });
                                tx += w;
                            }
                        }
                    }
                }
                "TJ" => {
                    if let Some(array_obj) = operands.first() {
                        if let Ok(array) = array_obj.as_array() {
                            let mut combined = String::new();
                            for elem in array {
                                if let Ok(bytes) = elem.as_str() {
                                    combined.push_str(&decode_pdf_string(bytes));
                                }
                            }
                            if !combined.trim().is_empty() {
                                let w = estimate_width(&combined, font_size);
                                items.push(TextItem {
                                    page: page_num,
                                    x: tx,
                                    y: ty,
                                    width: w,
                                    height: font_size,
                                    text: combined,
                                    font: font_name.clone(),
                                    font_size,
                                });
                                tx += w;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(items)
}

fn resolve_page_resources(
    doc: &lopdf::Document,
    page_id: lopdf::ObjectId,
) -> Option<lopdf::Dictionary> {
    // Walk the page's Parent chain to find an inherited Resources dictionary.
    let mut current_id = page_id;
    loop {
        let dict = match doc.get_object(current_id) {
            Ok(lopdf::Object::Dictionary(d)) => d.clone(),
            _ => return None,
        };
        if let Ok(res) = dict.get(b"Resources") {
            if let Ok((_, lopdf::Object::Dictionary(resources))) = doc.dereference(res) {
                return Some(resources.clone());
            }
        }
        match dict.get(b"Parent").and_then(|p| p.as_reference()) {
            Ok(parent_id) if parent_id != current_id => current_id = parent_id,
            _ => return None,
        }
    }
}

fn transform_point(m: &[f32; 6], x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn extract_image_items(doc: &lopdf::Document) -> crate::Result<Vec<ImageItem>> {
    let mut items = Vec::new();

    for (&page_num, &page_id) in &doc.get_pages() {
        // Step 1: XObject から画像情報を一時 map に収集
        // Resources は親 /Pages ノードから継承される場合があるため、継承チェーンを辿る。
        // key = XObject name, value = (format, width_px, height_px)
        let mut image_xobjects: std::collections::BTreeMap<String, (String, u32, u32)> =
            std::collections::BTreeMap::new();

        if let Some(resources) = resolve_page_resources(doc, page_id) {
            if let Ok(xo) = resources.get(b"XObject") {
                if let Ok((_, lopdf::Object::Dictionary(xobjects))) = doc.dereference(xo) {
                    for (name, obj_ref) in xobjects.iter() {
                        if let Ok((_, lopdf::Object::Stream(xobj))) = doc.dereference(obj_ref) {
                            let subtype = xobj
                                .dict
                                .get(b"Subtype")
                                .ok()
                                .and_then(|o| obj_as_name_str(o))
                                .unwrap_or_default();
                            if subtype == "Image" {
                                let fmt = detect_image_format(&xobj.dict);
                                let w_px = xobj
                                    .dict
                                    .get(b"Width")
                                    .ok()
                                    .and_then(|o| o.as_i64().ok())
                                    .unwrap_or(0) as u32;
                                let h_px = xobj
                                    .dict
                                    .get(b"Height")
                                    .ok()
                                    .and_then(|o| o.as_i64().ok())
                                    .unwrap_or(0) as u32;
                                let name_str = String::from_utf8_lossy(name).into_owned();
                                image_xobjects.insert(name_str, (fmt, w_px, h_px));
                            }
                        }
                    }
                }
            }
        }

        if image_xobjects.is_empty() {
            continue;
        }

        // Step 2: content stream から Do オペレータで位置を取得し、突き合わせて push
        let content_bytes = match doc.get_page_content(page_id) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let content = match lopdf::content::Content::decode(&content_bytes) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let identity = IDENTITY;
        let mut ctm_stack: Vec<[f32; 6]> = vec![identity];
        for op in &content.operations {
            match op.operator.as_str() {
                "q" => {
                    let top = *ctm_stack.last().unwrap_or(&identity);
                    ctm_stack.push(top);
                }
                "Q" if ctm_stack.len() > 1 => {
                    ctm_stack.pop();
                }
                "cm" if op.operands.len() == 6 => {
                    let new_m = [
                        obj_to_f32(&op.operands[0]),
                        obj_to_f32(&op.operands[1]),
                        obj_to_f32(&op.operands[2]),
                        obj_to_f32(&op.operands[3]),
                        obj_to_f32(&op.operands[4]),
                        obj_to_f32(&op.operands[5]),
                    ];
                    let current = *ctm_stack.last().unwrap_or(&identity);
                    *ctm_stack.last_mut().unwrap() = concat_matrix(&current, &new_m);
                }
                "Do" => {
                    if let Some(name_obj) = op.operands.first() {
                        if let Some(name) = obj_as_name_str(name_obj) {
                            if let Some((fmt, w_px, h_px)) = image_xobjects.get(name) {
                                let ctm = ctm_stack.last().unwrap_or(&identity);
                                // PDF images occupy the unit square [0,1]x[0,1].
                                // Transform all 4 corners through the CTM and take
                                // the axis-aligned bounding box so rotated/sheared
                                // images produce correct width/height.
                                let corners = [
                                    transform_point(ctm, 0.0, 0.0),
                                    transform_point(ctm, 1.0, 0.0),
                                    transform_point(ctm, 0.0, 1.0),
                                    transform_point(ctm, 1.0, 1.0),
                                ];
                                let min_x = corners
                                    .iter()
                                    .map(|(x, _)| *x)
                                    .fold(f32::INFINITY, f32::min);
                                let max_x = corners
                                    .iter()
                                    .map(|(x, _)| *x)
                                    .fold(f32::NEG_INFINITY, f32::max);
                                let min_y = corners
                                    .iter()
                                    .map(|(_, y)| *y)
                                    .fold(f32::INFINITY, f32::min);
                                let max_y = corners
                                    .iter()
                                    .map(|(_, y)| *y)
                                    .fold(f32::NEG_INFINITY, f32::max);
                                items.push(ImageItem {
                                    page: page_num,
                                    x: min_x,
                                    y: min_y,
                                    width: max_x - min_x,
                                    height: max_y - min_y,
                                    format: fmt.clone(),
                                    width_px: *w_px,
                                    height_px: *h_px,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(items)
}

fn obj_to_f32(obj: &lopdf::Object) -> f32 {
    match obj {
        lopdf::Object::Integer(i) => *i as f32,
        lopdf::Object::Real(f) => *f,
        _ => 0.0,
    }
}

/// Concatenate two PDF transformation matrices.
///
/// PDF transformation matrices use the row-vector convention:
/// ```text
/// a c e
/// b d f
/// 0 0 1
/// ```
/// This function computes `M_result = M_new × M_current`.
fn concat_matrix(current: &[f32; 6], new: &[f32; 6]) -> [f32; 6] {
    let (a, b, c, d, e, f) = (new[0], new[1], new[2], new[3], new[4], new[5]);
    let (a2, b2, c2, d2, e2, f2) = (
        current[0], current[1], current[2], current[3], current[4], current[5],
    );
    [
        a * a2 + b * c2,
        a * b2 + b * d2,
        c * a2 + d * c2,
        c * b2 + d * d2,
        e * a2 + f * c2 + e2,
        e * b2 + f * d2 + f2,
    ]
}

/// Decode a PDF string to a Rust String.
///
/// Handles UTF-16 BE (BOM `\xFE\xFF`) strings. For all other strings,
/// falls back to treating each byte as a Latin-1 code point.
///
/// Note: fulgur-generated PDFs use CID fonts where text in the content
/// stream consists of glyph IDs, not Unicode code points. The decoded
/// text for such PDFs will appear as raw byte sequences, not readable text.
/// Full Unicode reconstruction requires ToUnicode CMap parsing, which is
/// not yet implemented.
fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let chars: Vec<u16> = bytes[2..]
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&chars);
    }
    bytes.iter().map(|&b| b as char).collect()
}

fn estimate_width(text: &str, font_size: f32) -> f32 {
    text.chars().count() as f32 * font_size * 0.5
}

fn detect_image_format(dict: &lopdf::Dictionary) -> String {
    if let Ok(filter) = dict.get(b"Filter") {
        let name = match filter {
            lopdf::Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
            lopdf::Object::Array(arr) => arr
                .last()
                .and_then(|o| obj_as_name_str(o))
                .unwrap_or("")
                .to_string(),
            _ => String::new(),
        };
        match name.as_str() {
            "DCTDecode" => return "jpeg".to_string(),
            "JPXDecode" => return "jp2".to_string(),
            "CCITTFaxDecode" => return "tiff".to_string(),
            "FlateDecode" => return "flate".to_string(),
            _ => {}
        }
    }
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_test_pdf(html: &str) -> Vec<u8> {
        crate::engine::Engine::builder()
            .build()
            .render_html(html)
            .unwrap()
    }

    fn inspect_bytes(bytes: &[u8]) -> InspectResult {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), bytes).unwrap();
        inspect(tmp.path()).unwrap()
    }

    #[test]
    fn inspect_page_count() {
        let pdf = render_test_pdf("<html><body><p>Hello</p></body></html>");
        let result = inspect_bytes(&pdf);
        assert_eq!(result.pages, 1);
    }

    #[test]
    fn inspect_metadata_title() {
        let pdf = crate::engine::Engine::builder()
            .title("Test Title".to_string())
            .build()
            .render_html("<html><body><p>Hi</p></body></html>")
            .unwrap();
        let result = inspect_bytes(&pdf);
        assert_eq!(result.metadata.title.as_deref(), Some("Test Title"));
    }

    #[test]
    fn inspect_text_items_non_empty() {
        let pdf = render_test_pdf("<html><body><p>Hello World</p></body></html>");
        let result = inspect_bytes(&pdf);
        assert!(!result.text_items.is_empty(), "expected text items");
    }

    #[test]
    fn inspect_text_item_fields() {
        let pdf = render_test_pdf("<html><body><p>Hello</p></body></html>");
        let result = inspect_bytes(&pdf);
        let item = result
            .text_items
            .first()
            .expect("text items should not be empty");
        assert!(item.page >= 1);
        assert!(item.font_size > 0.0);
        assert!(!item.text.is_empty());
    }

    #[test]
    fn inspect_result_serializes_to_json() {
        let pdf = render_test_pdf("<html><body><p>Test</p></body></html>");
        let result = inspect_bytes(&pdf);
        let json = serde_json::to_string_pretty(&result).unwrap();
        assert!(json.contains("\"pages\""));
        assert!(json.contains("\"metadata\""));
        assert!(json.contains("\"text_items\""));
        assert!(json.contains("\"images\""));
    }

    #[test]
    fn inspect_error_on_nonexistent_file() {
        let result = inspect(std::path::Path::new("/nonexistent/path/to.pdf"));
        assert!(result.is_err(), "expected error for nonexistent file");
    }

    #[test]
    fn inspect_multi_page_pdf() {
        // Force two pages by making content taller than a single A4 page
        let html = "<html><body>\
            <p style='margin-bottom:2000pt'>Page one</p>\
            <p>Page two</p>\
            </body></html>";
        let pdf = render_test_pdf(html);
        let result = inspect_bytes(&pdf);
        assert!(result.pages >= 2, "expected at least 2 pages");
    }

    #[test]
    fn inspect_metadata_all_fields() {
        let pdf = crate::engine::Engine::builder()
            .title("My Title".to_string())
            .authors(vec!["Alice".to_string()])
            .creator("TestApp".to_string())
            .build()
            .render_html("<html><body><p>x</p></body></html>")
            .unwrap();
        let result = inspect_bytes(&pdf);
        assert_eq!(result.metadata.title.as_deref(), Some("My Title"));
        assert_eq!(result.metadata.author.as_deref(), Some("Alice"));
        assert_eq!(result.metadata.creator.as_deref(), Some("TestApp"));
    }

    #[test]
    fn inspect_image_embedded() {
        // Generate a valid 4x4 red PNG via the image crate (already a dev-dep)
        let img = image::RgbImage::from_fn(4, 4, |_, _| image::Rgb([255u8, 0, 0]));
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        let mut bundle = crate::asset::AssetBundle::new();
        bundle.add_image("test.png", png_bytes);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render_html(r#"<html><body><img src="test.png" width="50" height="50"></body></html>"#)
            .unwrap();
        let result = inspect_bytes(&pdf);
        assert!(!result.images.is_empty(), "expected at least one image");
        let img = &result.images[0];
        assert_eq!(img.page, 1);
        assert!(img.width > 0.0, "image width should be positive");
        assert!(img.height > 0.0, "image height should be positive");
    }

    // --- pure function unit tests ---

    #[test]
    fn decode_pdf_string_latin1() {
        let bytes = b"Hello";
        assert_eq!(decode_pdf_string(bytes), "Hello");
    }

    #[test]
    fn decode_pdf_string_utf16be() {
        // UTF-16 BE BOM + "Hi" (U+0048, U+0069)
        let bytes = &[0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69];
        assert_eq!(decode_pdf_string(bytes), "Hi");
    }

    #[test]
    fn decode_pdf_string_utf16be_odd_trailing_byte_ignored() {
        // BOM + one complete pair + one orphan byte
        let bytes = &[0xFE, 0xFF, 0x00, 0x41, 0xFF];
        let s = decode_pdf_string(bytes);
        assert_eq!(s, "A"); // orphan byte filtered by chunks(2) + len==2
    }

    #[test]
    fn detect_image_format_jpeg() {
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Filter", lopdf::Object::Name(b"DCTDecode".to_vec()));
        assert_eq!(detect_image_format(&dict), "jpeg");
    }

    #[test]
    fn detect_image_format_flate() {
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Filter", lopdf::Object::Name(b"FlateDecode".to_vec()));
        assert_eq!(detect_image_format(&dict), "flate");
    }

    #[test]
    fn detect_image_format_jp2() {
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Filter", lopdf::Object::Name(b"JPXDecode".to_vec()));
        assert_eq!(detect_image_format(&dict), "jp2");
    }

    #[test]
    fn detect_image_format_tiff() {
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Filter", lopdf::Object::Name(b"CCITTFaxDecode".to_vec()));
        assert_eq!(detect_image_format(&dict), "tiff");
    }

    #[test]
    fn detect_image_format_unknown() {
        let dict = lopdf::Dictionary::new(); // no Filter key
        assert_eq!(detect_image_format(&dict), "unknown");
    }

    #[test]
    fn detect_image_format_array_filter() {
        // Array filter — last entry wins
        let mut dict = lopdf::Dictionary::new();
        dict.set(
            b"Filter",
            lopdf::Object::Array(vec![
                lopdf::Object::Name(b"ASCII85Decode".to_vec()),
                lopdf::Object::Name(b"DCTDecode".to_vec()),
            ]),
        );
        assert_eq!(detect_image_format(&dict), "jpeg");
    }

    #[test]
    fn concat_matrix_identity() {
        let id = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
        let m = [1.0f32, 0.0, 0.0, 1.0, 10.0, 20.0];
        let result = concat_matrix(&id, &m);
        // id × m = m
        assert!((result[4] - 10.0).abs() < 1e-4);
        assert!((result[5] - 20.0).abs() < 1e-4);
    }

    #[test]
    fn concat_matrix_translation() {
        let a = [1.0f32, 0.0, 0.0, 1.0, 5.0, 10.0];
        let b = [1.0f32, 0.0, 0.0, 1.0, 3.0, 4.0];
        let result = concat_matrix(&a, &b);
        // Translations add: e = 3+5=8, f = 4+10=14
        assert!((result[4] - 8.0).abs() < 1e-4);
        assert!((result[5] - 14.0).abs() < 1e-4);
    }
}
