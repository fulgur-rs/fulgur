//! ImagePageable — renders images in PDF via Krilla's Image API.

use std::sync::Arc;

/// Image format detected from data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
}

impl ImageFormat {
    /// Create a krilla Image from raw data in this format.
    #[allow(clippy::result_unit_err)]
    pub fn to_krilla_image(self, data: krilla::Data) -> Result<krilla::image::Image, ()> {
        match self {
            ImageFormat::Png => krilla::image::Image::from_png(data, true).map_err(|_| ()),
            ImageFormat::Jpeg => krilla::image::Image::from_jpeg(data, true).map_err(|_| ()),
            ImageFormat::Gif => krilla::image::Image::from_gif(data, true).map_err(|_| ()),
        }
    }
}

/// Classification of an asset's bytes for rendering path selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    /// Raster image (PNG/JPEG/GIF) → renders via `ImagePageable`.
    Raster(ImageFormat),
    /// SVG vector image → renders via `SvgPageable`.
    Svg,
    /// Unrecognized / unsupported.
    Unknown,
}

impl AssetKind {
    /// Classify raw asset bytes by sniffing the header.
    ///
    /// Accepts SVG via the `<svg` element start or `<?xml` prolog, tolerant
    /// of an optional leading UTF-8 BOM and ASCII whitespace. Falls through
    /// to `ImagePageable::detect_format` for raster magic bytes.
    pub fn detect(data: &[u8]) -> AssetKind {
        if let Some(format) = ImagePageable::detect_format(data) {
            return AssetKind::Raster(format);
        }
        let mut slice = data;
        // Strip optional UTF-8 BOM so `<svg` / `<?xml` detection works.
        if slice.starts_with(b"\xEF\xBB\xBF") {
            slice = &slice[3..];
        }
        // Skip ASCII whitespace that commonly precedes the SVG root.
        while let Some((first, rest)) = slice.split_first() {
            if first.is_ascii_whitespace() {
                slice = rest;
            } else {
                break;
            }
        }
        if slice.starts_with(b"<?xml") || slice.starts_with(b"<svg") {
            return AssetKind::Svg;
        }
        AssetKind::Unknown
    }
}

/// An image element that renders an image at its computed size.
#[derive(Clone)]
pub struct ImagePageable {
    pub image_data: Arc<Vec<u8>>,
    pub format: ImageFormat,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub visible: bool,
    /// fulgur-r6we (Phase 3.2.a): DOM NodeId for `slice_for_page`
    /// geometry lookup. See `BlockPageable::node_id`.
    pub node_id: Option<usize>,
}

impl ImagePageable {
    pub fn new(data: Arc<Vec<u8>>, format: ImageFormat, width: f32, height: f32) -> Self {
        Self {
            image_data: data,
            format,
            width,
            height,
            opacity: 1.0,
            visible: true,
            node_id: None,
        }
    }

    pub fn with_node_id(mut self, node_id: Option<usize>) -> Self {
        self.node_id = node_id;
        self
    }

    /// Decode image dimensions (width, height) from header bytes.
    /// Returns None if the data is too short or malformed.
    pub fn decode_dimensions(data: &[u8], format: ImageFormat) -> Option<(u32, u32)> {
        match format {
            ImageFormat::Png => {
                // PNG IHDR: bytes 16..20 = width (BE u32), 20..24 = height (BE u32)
                if data.len() < 24 {
                    return None;
                }
                let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
                let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
                Some((w, h))
            }
            ImageFormat::Gif => {
                // GIF header: bytes 6..8 = width (LE u16), 8..10 = height (LE u16)
                if data.len() < 10 {
                    return None;
                }
                let w = u16::from_le_bytes([data[6], data[7]]) as u32;
                let h = u16::from_le_bytes([data[8], data[9]]) as u32;
                Some((w, h))
            }
            ImageFormat::Jpeg => {
                // JPEG: scan for SOF0..SOF15 markers (0xC0..0xCF, excluding 0xC4 DHT and 0xCC DAC)
                // SOF structure: FF Cx, 2-byte length, 1 byte precision, 2 bytes height (BE), 2 bytes width (BE)
                if data.len() < 2 {
                    return None;
                }
                let mut i = 2; // skip SOI (FF D8)
                while i + 1 < data.len() {
                    if data[i] != 0xFF {
                        i += 1;
                        continue;
                    }
                    let marker = data[i + 1];
                    i += 2;
                    if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xCC {
                        if i + 7 > data.len() {
                            return None;
                        }
                        let h = u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32;
                        let w = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                        return Some((w, h));
                    }
                    if i + 1 >= data.len() {
                        return None;
                    }
                    let seg_len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
                    if seg_len < 2 {
                        return None;
                    }
                    i += seg_len;
                }
                None
            }
        }
    }

    /// Detect image format from data magic bytes.
    pub fn detect_format(data: &[u8]) -> Option<ImageFormat> {
        if data.starts_with(b"\x89PNG") {
            Some(ImageFormat::Png)
        } else if data.starts_with(b"\xFF\xD8\xFF") {
            Some(ImageFormat::Jpeg)
        } else if data.starts_with(b"GIF") {
            Some(ImageFormat::Gif)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1x1 red PNG
    const MINIMAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    // 1x1 white GIF89a
    const MINIMAL_GIF: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x3B,
    ];

    #[test]
    fn test_png_dimensions() {
        let dims = ImagePageable::decode_dimensions(MINIMAL_PNG, ImageFormat::Png);
        assert_eq!(dims, Some((1, 1)));
    }

    #[test]
    fn test_gif_dimensions() {
        let dims = ImagePageable::decode_dimensions(MINIMAL_GIF, ImageFormat::Gif);
        assert_eq!(dims, Some((1, 1)));
    }

    // Minimal 1x1 white JPEG (SOF0 baseline)
    const MINIMAL_JPEG: &[u8] = &[
        0xFF, 0xD8, // SOI
        0xFF, 0xE0, // APP0 (JFIF)
        0x00, 0x10, // length = 16
        0x4A, 0x46, 0x49, 0x46, 0x00, // "JFIF\0"
        0x01, 0x01, // version
        0x00, // units
        0x00, 0x01, // X density
        0x00, 0x01, // Y density
        0x00, 0x00, // thumbnail
        0xFF, 0xC0, // SOF0
        0x00, 0x0B, // length = 11
        0x08, // precision = 8
        0x00, 0x01, // height = 1
        0x00, 0x01, // width = 1
        0x01, // components = 1
        0x01, 0x11, 0x00, // component 1
        0xFF, 0xD9, // EOI
    ];

    #[test]
    fn test_jpeg_dimensions() {
        let dims = ImagePageable::decode_dimensions(MINIMAL_JPEG, ImageFormat::Jpeg);
        assert_eq!(dims, Some((1, 1)));
    }

    #[test]
    fn test_truncated_data_returns_none() {
        let dims = ImagePageable::decode_dimensions(&[0x89, 0x50], ImageFormat::Png);
        assert_eq!(dims, None);
    }

    #[test]
    fn test_detect_asset_kind_png() {
        assert!(matches!(
            AssetKind::detect(MINIMAL_PNG),
            AssetKind::Raster(ImageFormat::Png)
        ));
    }

    #[test]
    fn test_detect_asset_kind_jpeg() {
        assert!(matches!(
            AssetKind::detect(MINIMAL_JPEG),
            AssetKind::Raster(ImageFormat::Jpeg)
        ));
    }

    #[test]
    fn test_detect_asset_kind_gif() {
        assert!(matches!(
            AssetKind::detect(MINIMAL_GIF),
            AssetKind::Raster(ImageFormat::Gif)
        ));
    }

    #[test]
    fn test_detect_asset_kind_svg_tag() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
        assert!(matches!(AssetKind::detect(svg), AssetKind::Svg));
    }

    #[test]
    fn test_detect_asset_kind_svg_xml_prolog() {
        let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
        assert!(matches!(AssetKind::detect(svg), AssetKind::Svg));
    }

    #[test]
    fn test_detect_asset_kind_svg_with_utf8_bom() {
        let svg = b"\xEF\xBB\xBF<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        assert!(matches!(AssetKind::detect(svg), AssetKind::Svg));
    }

    #[test]
    fn test_detect_asset_kind_empty() {
        assert!(matches!(AssetKind::detect(&[]), AssetKind::Unknown));
    }

    #[test]
    fn test_detect_asset_kind_unknown() {
        assert!(matches!(
            AssetKind::detect(b"not an image"),
            AssetKind::Unknown
        ));
    }
}
