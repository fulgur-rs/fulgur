//! ImagePageable — renders images in PDF via Krilla's Image API.

use std::sync::Arc;

use crate::pageable::{Canvas, Pageable, Pagination, Pt, Size};

/// Image format detected from data.
#[derive(Clone, Debug)]
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
}

impl ImagePageable {
    pub fn new(data: Arc<Vec<u8>>, format: ImageFormat, width: f32, height: f32) -> Self {
        Self {
            image_data: data,
            format,
            width,
            height,
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
}
