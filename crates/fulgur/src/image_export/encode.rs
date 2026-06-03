//! Encode a tiny-skia pixmap to PNG or lossless WebP bytes.

use crate::error::Error;
use crate::image_export::options::ImageFormat;
use tiny_skia::Pixmap;

/// Encode `pixmap` to the requested format.
pub fn encode_pixmap(pixmap: &Pixmap, format: ImageFormat) -> crate::error::Result<Vec<u8>> {
    match format {
        ImageFormat::Png => pixmap
            .encode_png()
            .map_err(|e| Error::Other(format!("PNG encode failed: {e}"))),
        ImageFormat::WebpLossless => encode_webp_lossless(pixmap),
    }
}

/// tiny-skia stores premultiplied alpha; the `image` crate wants straight
/// (unpremultiplied) RGBA. Demultiply into a fresh buffer, then encode
/// lossless WebP.
fn encode_webp_lossless(pixmap: &Pixmap) -> crate::error::Result<Vec<u8>> {
    let (w, h) = (pixmap.width(), pixmap.height());
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for px in pixmap.pixels() {
        let d = px.demultiply();
        rgba.extend_from_slice(&[d.red(), d.green(), d.blue(), d.alpha()]);
    }
    let mut out = Vec::new();
    let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
    encoder
        .encode(&rgba, w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| Error::Other(format!("WebP encode failed: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_export::options::ImageFormat;

    fn red_pixmap() -> tiny_skia::Pixmap {
        let mut p = tiny_skia::Pixmap::new(4, 4).unwrap();
        p.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        p
    }

    #[test]
    fn png_has_magic_bytes() {
        let bytes = encode_pixmap(&red_pixmap(), ImageFormat::Png).unwrap();
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    #[test]
    fn webp_has_riff_header() {
        let bytes = encode_pixmap(&red_pixmap(), ImageFormat::WebpLossless).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }
}
