//! Output configuration for `Engine::render_html_to_image`.

use crate::error::Error;

/// Encoded output image format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG (lossless, alpha). Encoded via tiny-skia.
    Png,
    /// Lossless WebP (alpha). Encoded via the `image` crate. Lossy WebP is
    /// intentionally unsupported (it requires a C library).
    WebpLossless,
}

/// Canvas background fill.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Background {
    /// Fully transparent canvas (alpha 0).
    Transparent,
    /// Solid RGBA fill.
    Solid([u8; 4]),
}

/// Configuration for rendering an HTML document to a single image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageOptions {
    /// Logical canvas width in CSS pixels.
    pub width_px: u32,
    /// Logical canvas height in CSS pixels.
    pub height_px: u32,
    /// Encoded output format.
    pub format: ImageFormat,
    /// Canvas background. Defaults to `Transparent`.
    pub background: Background,
    /// Device-pixel multiplier (e.g. 2.0 for @2x). Defaults to 1.0.
    pub scale: f32,
}

impl ImageOptions {
    /// New options with `Transparent` background and `scale = 1.0`.
    pub fn new(width_px: u32, height_px: u32, format: ImageFormat) -> Self {
        Self {
            width_px,
            height_px,
            format,
            background: Background::Transparent,
            scale: 1.0,
        }
    }

    /// Output pixmap dimensions in device pixels: logical size × scale.
    pub fn pixmap_dims(&self) -> (u32, u32) {
        let w = (self.width_px as f32 * self.scale).round() as u32;
        let h = (self.height_px as f32 * self.scale).round() as u32;
        (w.max(1), h.max(1))
    }

    /// Reject zero dimensions and non-finite / non-positive scale.
    pub fn validate(&self) -> Result<(), Error> {
        if self.width_px == 0 || self.height_px == 0 {
            return Err(Error::Other(
                "image width and height must be non-zero".into(),
            ));
        }
        if !self.scale.is_finite() || self.scale <= 0.0 {
            return Err(Error::Other(
                "image scale must be a positive, finite number".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_zero_dims() {
        let o = ImageOptions::new(0, 100, ImageFormat::Png);
        assert!(o.validate().is_err());
        let o = ImageOptions::new(100, 0, ImageFormat::Png);
        assert!(o.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_scale() {
        let mut o = ImageOptions::new(100, 100, ImageFormat::Png);
        o.scale = 0.0;
        assert!(o.validate().is_err());
        o.scale = -1.0;
        assert!(o.validate().is_err());
    }

    #[test]
    fn pixmap_dims_apply_scale() {
        let mut o = ImageOptions::new(1200, 630, ImageFormat::Png);
        o.scale = 2.0;
        assert_eq!(o.pixmap_dims(), (2400, 1260));
    }

    #[test]
    fn default_background_is_transparent() {
        let o = ImageOptions::new(10, 10, ImageFormat::Png);
        assert!(matches!(o.background, Background::Transparent));
    }
}
