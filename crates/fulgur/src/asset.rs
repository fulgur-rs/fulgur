//! AssetBundle for managing CSS, fonts, and images.

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
        self.fonts.push(Arc::new(data));
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
}
