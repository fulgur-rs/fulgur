//! AssetBundle for managing CSS, fonts, and images.

use crate::error::Error;
use crate::error::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Collection of external assets (CSS, fonts, images) for PDF generation.
pub struct AssetBundle {
    pub css: Vec<String>,
    pub fonts: Vec<Arc<Vec<u8>>>,
    pub images: HashMap<String, Arc<Vec<u8>>>,
}

impl AssetBundle {
    pub fn new() -> Self {
        Self {
            css: Vec::new(),
            fonts: Vec::new(),
            images: HashMap::new(),
        }
    }

    pub fn add_css(&mut self, css: impl Into<String>) {
        self.css.push(css.into());
    }

    pub fn add_css_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let css = std::fs::read_to_string(path)?;
        self.css.push(css);
        Ok(())
    }

    pub fn add_font_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let data = std::fs::read(path)?;
        self.add_font_bytes(data)
    }

    /// バイト列からフォントを登録する。
    ///
    /// マジックバイトで形式を自動判定する:
    /// - TTF / OTF / TTC: そのまま登録
    /// - WOFF2: `woff2_patched` で TTF にデコードしてから登録
    /// - WOFF1: `Error::UnsupportedFontFormat` を返す（未対応）
    /// - その他: 警告ログを出してそのまま登録（caller が正しい形式を渡している可能性）
    pub fn add_font_bytes(&mut self, data: Vec<u8>) -> Result<()> {
        let decoded = match detect_font_format(&data) {
            FontFormat::Woff2 => decode_woff2(&data)?,
            FontFormat::Woff1 => {
                return Err(Error::UnsupportedFontFormat(
                    "WOFF1 is not supported; convert to WOFF2 or TTF/OTF".into(),
                ));
            }
            FontFormat::Unknown => {
                log::warn!("add_font_bytes: unknown font magic bytes; passing through as-is");
                data
            }
            FontFormat::Ttf | FontFormat::Otf | FontFormat::Ttc => data,
        };
        self.fonts.push(Arc::new(decoded));
        Ok(())
    }

    /// Normalize an image key by stripping a leading `./` prefix.
    fn normalize_key(key: &mut String) {
        if key.starts_with("./") {
            key.drain(..2);
        }
    }

    pub fn add_image(&mut self, name: impl Into<String>, data: Vec<u8>) {
        let mut key = name.into();
        Self::normalize_key(&mut key);
        self.images.insert(key, Arc::new(data));
    }

    pub fn add_image_file(
        &mut self,
        name: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> Result<()> {
        let data = std::fs::read(path)?;
        let mut key = name.into();
        Self::normalize_key(&mut key);
        self.images.insert(key, Arc::new(data));
        Ok(())
    }

    pub fn get_image(&self, name: &str) -> Option<&Arc<Vec<u8>>> {
        let key = name.strip_prefix("./").unwrap_or(name);
        self.images.get(key)
    }

    /// Build combined CSS from all added stylesheets.
    pub fn combined_css(&self) -> String {
        self.css.join("\n")
    }
}

impl Default for AssetBundle {
    fn default() -> Self {
        Self::new()
    }
}

/// Font container format detected from magic bytes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FontFormat {
    Ttf,
    Otf,
    Ttc,
    Woff1,
    Woff2,
    Unknown,
}

/// Detect a font container format from the first four bytes.
///
/// Recognizes TrueType (`0x00010000`, `true`, `typ1`), OpenType (`OTTO`),
/// TrueType Collection (`ttcf`), WOFF (`wOFF`), and WOFF2 (`wOF2`) magic
/// sequences. Returns `FontFormat::Unknown` for anything else, including
/// inputs shorter than four bytes.
pub(crate) fn detect_font_format(bytes: &[u8]) -> FontFormat {
    match bytes.get(0..4) {
        Some(b"wOF2") => FontFormat::Woff2,
        Some(b"wOFF") => FontFormat::Woff1,
        Some(b"OTTO") => FontFormat::Otf,
        Some(b"ttcf") => FontFormat::Ttc,
        Some([0x00, 0x01, 0x00, 0x00]) => FontFormat::Ttf,
        Some(b"true") | Some(b"typ1") => FontFormat::Ttf,
        _ => FontFormat::Unknown,
    }
}

/// Decode a WOFF2 byte stream into an uncompressed TTF/OTF font.
fn decode_woff2(data: &[u8]) -> Result<Vec<u8>> {
    let mut buf: &[u8] = data;
    woff2_patched::decode::convert_woff2_to_ttf(&mut buf)
        .map_err(|e| Error::WoffDecode(format!("WOFF2 decode failed: {e:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_image_normalizes_dot_slash() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("logo.png", vec![1, 2, 3]);
        assert!(bundle.get_image("./logo.png").is_some());
        assert!(bundle.get_image("logo.png").is_some());
    }

    #[test]
    fn test_add_image_normalizes_dot_slash() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("./logo.png", vec![1, 2, 3]);
        assert!(bundle.get_image("logo.png").is_some());
    }

    #[test]
    fn test_nested_dot_slash_preserved() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("images/./logo.png", vec![1, 2, 3]);
        assert!(bundle.get_image("images/./logo.png").is_some());
        assert!(bundle.get_image("logo.png").is_none());
    }

    #[test]
    fn test_detect_font_format_ttf() {
        assert_eq!(
            detect_font_format(&[0x00, 0x01, 0x00, 0x00, 0xFF]),
            FontFormat::Ttf
        );
    }

    #[test]
    fn test_detect_font_format_otf() {
        assert_eq!(detect_font_format(b"OTTO\x00\x00"), FontFormat::Otf);
    }

    #[test]
    fn test_detect_font_format_ttc() {
        assert_eq!(detect_font_format(b"ttcf\x00\x00"), FontFormat::Ttc);
    }

    #[test]
    fn test_detect_font_format_woff2() {
        assert_eq!(detect_font_format(b"wOF2\x00\x00"), FontFormat::Woff2);
    }

    #[test]
    fn test_detect_font_format_woff1() {
        assert_eq!(detect_font_format(b"wOFF\x00\x00"), FontFormat::Woff1);
    }

    #[test]
    fn test_detect_font_format_unknown() {
        assert_eq!(detect_font_format(b"XXXX"), FontFormat::Unknown);
        assert_eq!(detect_font_format(&[0x00]), FontFormat::Unknown);
        assert_eq!(detect_font_format(&[]), FontFormat::Unknown);
    }

    #[test]
    fn test_detect_font_format_old_mac_ttf() {
        assert_eq!(detect_font_format(b"true\x00\x00"), FontFormat::Ttf);
        assert_eq!(detect_font_format(b"typ1\x00\x00"), FontFormat::Ttf);
    }

    #[test]
    fn test_add_font_bytes_ttf_passthrough() {
        let mut bundle = AssetBundle::new();
        let mut data = vec![0x00, 0x01, 0x00, 0x00];
        data.extend_from_slice(&[0xAA; 100]);
        bundle
            .add_font_bytes(data.clone())
            .expect("should accept TTF");
        assert_eq!(bundle.fonts.len(), 1);
        assert_eq!(&bundle.fonts[0][..], &data[..]);
    }

    #[test]
    fn test_add_font_bytes_unknown_passthrough() {
        let mut bundle = AssetBundle::new();
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00];
        bundle
            .add_font_bytes(data.clone())
            .expect("unknown format should pass through");
        assert_eq!(bundle.fonts.len(), 1);
        assert_eq!(&bundle.fonts[0][..], &data[..]);
    }

    #[test]
    fn test_add_font_bytes_woff1_rejected() {
        use crate::error::Error;
        let mut bundle = AssetBundle::new();
        let data = b"wOFF\x00\x01\x00\x00".to_vec();
        let err = bundle
            .add_font_bytes(data)
            .expect_err("WOFF1 must be rejected");
        match err {
            Error::UnsupportedFontFormat(s) => assert!(s.contains("WOFF1"), "msg: {s}"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn test_add_font_bytes_woff2_decodes_to_ttf_or_otf() {
        let data = std::fs::read("tests/fixtures/fonts/NotoSans-Regular.woff2")
            .expect("fixture must exist");
        assert_eq!(detect_font_format(&data), FontFormat::Woff2);

        let mut bundle = AssetBundle::new();
        bundle.add_font_bytes(data).expect("WOFF2 should decode");
        assert_eq!(bundle.fonts.len(), 1);

        let decoded = &bundle.fonts[0];
        let magic = &decoded[0..4];
        assert!(
            magic == [0x00, 0x01, 0x00, 0x00] || magic == b"OTTO",
            "decoded magic should be TTF or OTF, got {magic:?}"
        );
    }

    #[test]
    fn test_add_font_bytes_woff2_invalid_returns_error() {
        use crate::error::Error;
        let mut bundle = AssetBundle::new();
        let fake = b"wOF2\x00\x00\x00\x00garbagegarbagegarbage".to_vec();
        let err = bundle
            .add_font_bytes(fake)
            .expect_err("bad WOFF2 must error");
        match err {
            Error::WoffDecode(_) => {}
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }
}
