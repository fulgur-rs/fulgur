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
        let path = path.as_ref();
        // Gate on file size *before* reading the entire file into memory so
        // a malicious multi-gigabyte font cannot drive the process into OOM
        // via `std::fs::read`. `MAX_DECODED_FONT_BYTES` is the generous
        // upper bound used across the font pipeline; WOFF2-specific input
        // capping happens later in `decode_woff2`.
        let len = std::fs::metadata(path)?.len();
        if len > MAX_DECODED_FONT_BYTES as u64 {
            return Err(Error::Asset(format!(
                "font file {} exceeds {} byte limit (got {} bytes)",
                path.display(),
                MAX_DECODED_FONT_BYTES,
                len
            )));
        }
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

/// Upper bound on accepted WOFF2 input size (32 MiB). Rejects obviously
/// oversized or adversarial payloads before invoking the brotli decoder,
/// limiting decompression-bomb exposure.
const MAX_WOFF2_INPUT_BYTES: usize = 32 * 1024 * 1024;

/// Upper bound on accepted decompressed TTF/OTF output size (64 MiB).
/// A single real-world font family tops out well below this; anything larger
/// is likely a decompression bomb.
const MAX_DECODED_FONT_BYTES: usize = 64 * 1024 * 1024;

/// Decode a WOFF2 byte stream into an uncompressed TTF/OTF font.
///
/// Three layered defenses against decompression-bomb inputs, since
/// `woff2_patched` itself caps neither input nor output:
///
/// 1. `MAX_WOFF2_INPUT_BYTES` rejects oversized compressed inputs up front.
/// 2. The WOFF2 header's `totalSfntSize` field (bytes 16-19, big-endian
///    u32) is inspected *before* invoking brotli so an adversarial header
///    declaring a huge output cannot drive the decoder into OOM.
/// 3. `MAX_DECODED_FONT_BYTES` is re-checked after decode as a belt-and-
///    suspenders guard against a liar header.
fn decode_woff2(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() > MAX_WOFF2_INPUT_BYTES {
        return Err(Error::WoffDecode(format!(
            "WOFF2 input exceeds {MAX_WOFF2_INPUT_BYTES} byte limit (got {} bytes)",
            data.len()
        )));
    }
    // WOFF2 header: bytes 16..20 are totalSfntSize (big-endian u32).
    // See https://www.w3.org/TR/WOFF2/#woff20Header.
    if data.len() < 20 {
        return Err(Error::WoffDecode(format!(
            "WOFF2 header truncated: expected >= 20 bytes, got {}",
            data.len()
        )));
    }
    let total_sfnt_size = u32::from_be_bytes([data[16], data[17], data[18], data[19]]) as usize;
    if total_sfnt_size > MAX_DECODED_FONT_BYTES {
        return Err(Error::WoffDecode(format!(
            "WOFF2 header declares uncompressed size {total_sfnt_size} which exceeds {MAX_DECODED_FONT_BYTES} byte limit"
        )));
    }
    let mut buf: &[u8] = data;
    let decoded = woff2_patched::decode::convert_woff2_to_ttf(&mut buf)
        .map_err(|e| Error::WoffDecode(format!("WOFF2 decode failed: {e:?}")))?;
    if decoded.len() > MAX_DECODED_FONT_BYTES {
        return Err(Error::WoffDecode(format!(
            "WOFF2 decoded output exceeds {MAX_DECODED_FONT_BYTES} byte limit (got {} bytes)",
            decoded.len()
        )));
    }
    Ok(decoded)
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

    #[test]
    fn test_add_font_bytes_woff2_input_size_cap() {
        let mut bundle = AssetBundle::new();
        let mut oversized = b"wOF2".to_vec();
        oversized.resize(MAX_WOFF2_INPUT_BYTES + 1, 0);
        let err = bundle
            .add_font_bytes(oversized)
            .expect_err("oversized WOFF2 must error before decoding");
        match err {
            Error::WoffDecode(msg) => assert!(msg.contains("limit"), "msg: {msg}"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn test_add_font_bytes_woff2_header_declares_oversized_output() {
        // Craft a minimal 20-byte WOFF2 header where totalSfntSize
        // (bytes 16..20, big-endian u32) declares an uncompressed size
        // that exceeds MAX_DECODED_FONT_BYTES. Must be rejected before
        // the decoder runs.
        let mut header = vec![0u8; 20];
        header[0..4].copy_from_slice(b"wOF2");
        let declared = (MAX_DECODED_FONT_BYTES as u64 + 1) as u32;
        header[16..20].copy_from_slice(&declared.to_be_bytes());
        let mut bundle = AssetBundle::new();
        let err = bundle
            .add_font_bytes(header)
            .expect_err("declared-oversized WOFF2 must be rejected");
        match err {
            Error::WoffDecode(msg) => {
                assert!(msg.contains("declares uncompressed size"), "msg: {msg}")
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn test_add_font_file_rejects_oversized_before_reading() {
        use std::io::Seek;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        // Create a sparse file larger than the cap. `set_len` extends the
        // file without actually allocating blocks, so the test is cheap.
        let oversized = (MAX_DECODED_FONT_BYTES as u64) + 1;
        tmp.as_file_mut()
            .set_len(oversized)
            .expect("extend tempfile");
        tmp.as_file_mut().rewind().expect("rewind");
        let mut bundle = AssetBundle::new();
        let err = bundle
            .add_font_file(tmp.path())
            .expect_err("oversized font file must be rejected");
        match err {
            Error::Asset(msg) => assert!(msg.contains("limit"), "msg: {msg}"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn test_add_font_bytes_woff2_truncated_header() {
        let mut bundle = AssetBundle::new();
        // Only 8 bytes: long enough to pass the 4-byte magic detection
        // (FontFormat::Woff2) but too short to read totalSfntSize.
        let truncated = b"wOF2\x00\x00\x00\x00".to_vec();
        let err = bundle
            .add_font_bytes(truncated)
            .expect_err("truncated WOFF2 header must be rejected");
        match err {
            Error::WoffDecode(msg) => {
                assert!(msg.contains("header truncated"), "msg: {msg}")
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }
}
