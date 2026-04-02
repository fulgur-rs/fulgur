//! ImagePageable — renders images in PDF via Krilla's Image API.

use std::sync::Arc;

use crate::pageable::{Canvas, Pageable, Pagination, Pt, Size};

/// Image format detected from data.
#[derive(Clone, Copy, Debug)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
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
        }
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
                        let h = u16::from_be_bytes([data[i + 1], data[i + 2]]) as u32;
                        let w = u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32;
                        return Some((w, h));
                    }
                    if i + 1 >= data.len() {
                        return None;
                    }
                    let seg_len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
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

impl Pageable for ImagePageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        // Images cannot be split
        None
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, _avail_width: Pt, _avail_height: Pt) {
        use crate::pageable::draw_with_opacity;

        if !self.visible {
            return;
        }
        draw_with_opacity(canvas, self.opacity, |canvas| {
            let data: krilla::Data = Arc::clone(&self.image_data).into();
            let image_result = match self.format {
                ImageFormat::Png => krilla::image::Image::from_png(data, true),
                ImageFormat::Jpeg => krilla::image::Image::from_jpeg(data, true),
                ImageFormat::Gif => krilla::image::Image::from_gif(data, true),
            };

            let Ok(image) = image_result else {
                return;
            };

            let Some(size) = krilla::geom::Size::from_wh(self.width, self.height) else {
                return;
            };

            let transform = krilla::geom::Transform::from_translate(x, y);
            canvas.surface.push_transform(&transform);
            canvas.surface.draw_image(image, size);
            canvas.surface.pop();
        });
    }

    fn pagination(&self) -> Pagination {
        Pagination::default()
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.height
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1x1 red PNG
    const MINIMAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
        0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
        0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78,
        0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92,
        0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
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

    #[test]
    fn test_truncated_data_returns_none() {
        let dims = ImagePageable::decode_dimensions(&[0x89, 0x50], ImageFormat::Png);
        assert_eq!(dims, None);
    }
}
