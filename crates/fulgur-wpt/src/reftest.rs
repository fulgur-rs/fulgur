use anyhow::{Result, bail};
use std::ops::RangeInclusive;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyTolerance {
    pub url: Option<PathBuf>,
    pub max_diff: RangeInclusive<u8>,
    pub total_pixels: RangeInclusive<u32>,
}

impl FuzzyTolerance {
    /// Permissive tolerance (max_diff 0-255, total_pixels 0-u32::MAX).
    /// Used when a test declares no fuzzy meta.
    pub fn any() -> Self {
        Self {
            url: None,
            max_diff: 0..=255,
            total_pixels: 0..=u32::MAX,
        }
    }
}

/// Parse a WPT `<meta name=fuzzy content=...>` value into a canonical
/// `FuzzyTolerance`. Accepts every variant from the WPT reftest spec:
///
/// - numeric: `10;300`, `5-10;200-300`
/// - named:   `maxDifference=10;totalPixels=300`, or named + range
/// - url prefix: `ref.html:10-15;200-300`
/// - open range: `5-`, `-300`
pub fn parse_fuzzy(src: &str) -> Result<FuzzyTolerance> {
    let src = src.trim();

    // URL prefix: split at first ':' if the prefix is non-empty and
    // contains neither '=' nor ';' (those belong to value syntax, not URL).
    let (url, body) = match src.find(':') {
        Some(idx)
            if !src[..idx].contains('=') && !src[..idx].contains(';') && !src[..idx].is_empty() =>
        {
            let (u, rest) = src.split_at(idx);
            (Some(PathBuf::from(u.trim())), &rest[1..])
        }
        _ => (None, src),
    };

    let mut parts = body.split(';');
    let first = parts.next().ok_or_else(|| anyhow::anyhow!("empty fuzzy"))?;
    let second = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing ';' in fuzzy: {src}"))?;
    if parts.next().is_some() {
        bail!("too many ';' in fuzzy: {src}");
    }

    let (k1, v1) = split_named(first.trim());
    let (k2, v2) = split_named(second.trim());

    let (max_diff_src, total_src) = match (k1, k2) {
        (Some("maxDifference"), Some("totalPixels")) | (None, None) => (v1, v2),
        (Some("totalPixels"), Some("maxDifference")) => (v2, v1),
        (Some(k), _) => bail!("unknown fuzzy key: {k}"),
        (_, Some(k)) => bail!("unknown fuzzy key: {k}"),
    };

    let max_diff = parse_u8_range(max_diff_src)?;
    let total_pixels = parse_u32_range(total_src)?;
    Ok(FuzzyTolerance {
        url,
        max_diff,
        total_pixels,
    })
}

fn split_named(s: &str) -> (Option<&str>, &str) {
    match s.find('=') {
        Some(idx) => (Some(&s[..idx]), &s[idx + 1..]),
        None => (None, s),
    }
}

fn parse_u8_range(src: &str) -> Result<RangeInclusive<u8>> {
    let src = src.trim();
    let (lo, hi) = parse_range(src, 0u32, 255u32)?;
    if lo > hi {
        bail!("reversed range: {src}");
    }
    if hi > 255 {
        bail!("max_diff out of u8 range: {src}");
    }
    Ok((lo as u8)..=(hi as u8))
}

fn parse_u32_range(src: &str) -> Result<RangeInclusive<u32>> {
    let src = src.trim();
    let (lo, hi) = parse_range(src, 0u32, u32::MAX)?;
    if lo > hi {
        bail!("reversed range: {src}");
    }
    Ok(lo..=hi)
}

fn parse_range(src: &str, default_lo: u32, default_hi: u32) -> Result<(u32, u32)> {
    if src.is_empty() {
        bail!("empty range");
    }
    match src.find('-') {
        None => {
            let n: u32 = src.parse()?;
            Ok((n, n))
        }
        Some(0) => {
            let n: u32 = src[1..].trim().parse()?;
            Ok((default_lo, n))
        }
        Some(idx) if idx == src.len() - 1 => {
            let n: u32 = src[..idx].trim().parse()?;
            Ok((n, default_hi))
        }
        Some(idx) => {
            let lo: u32 = src[..idx].trim().parse()?;
            let hi: u32 = src[idx + 1..].trim().parse()?;
            Ok((lo, hi))
        }
    }
}

#[cfg(test)]
mod fuzzy_tests {
    use super::*;

    #[test]
    fn plain_numeric() {
        let t = parse_fuzzy("10;300").unwrap();
        assert_eq!(t.url, None);
        assert_eq!(t.max_diff, 10..=10);
        assert_eq!(t.total_pixels, 300..=300);
    }

    #[test]
    fn numeric_range_both() {
        let t = parse_fuzzy("5-10;200-300").unwrap();
        assert_eq!(t.max_diff, 5..=10);
        assert_eq!(t.total_pixels, 200..=300);
    }

    #[test]
    fn named_single() {
        let t = parse_fuzzy("maxDifference=10;totalPixels=300").unwrap();
        assert_eq!(t.max_diff, 10..=10);
        assert_eq!(t.total_pixels, 300..=300);
    }

    #[test]
    fn named_range() {
        let t = parse_fuzzy("maxDifference=5-10;totalPixels=200-300").unwrap();
        assert_eq!(t.max_diff, 5..=10);
        assert_eq!(t.total_pixels, 200..=300);
    }

    #[test]
    fn url_prefix() {
        let t = parse_fuzzy("ref.html:10-15;200-300").unwrap();
        assert_eq!(
            t.url.as_deref().map(|p| p.to_str().unwrap()),
            Some("ref.html")
        );
        assert_eq!(t.max_diff, 10..=15);
        assert_eq!(t.total_pixels, 200..=300);
    }

    #[test]
    fn open_range_lower_only() {
        let t = parse_fuzzy("5-;200-").unwrap();
        assert_eq!(t.max_diff, 5..=255);
        assert_eq!(t.total_pixels, 200..=u32::MAX);
    }

    #[test]
    fn open_range_upper_only() {
        let t = parse_fuzzy("-10;-300").unwrap();
        assert_eq!(t.max_diff, 0..=10);
        assert_eq!(t.total_pixels, 0..=300);
    }

    #[test]
    fn whitespace_tolerated() {
        let t = parse_fuzzy("  10 ; 300  ").unwrap();
        assert_eq!(t.max_diff, 10..=10);
        assert_eq!(t.total_pixels, 300..=300);
    }

    #[test]
    fn rejects_missing_semicolon() {
        assert!(parse_fuzzy("10").is_err());
    }

    #[test]
    fn rejects_reversed_range() {
        assert!(parse_fuzzy("10-5;300").is_err());
    }

    #[test]
    fn rejects_max_diff_over_255() {
        assert!(parse_fuzzy("256;300").is_err());
    }

    // ---- Additional edge-case coverage --------------------------------

    /// Named pairs may appear in either order per the WPT spec.
    #[test]
    fn named_reversed_order() {
        let t = parse_fuzzy("totalPixels=200-300;maxDifference=5-10").unwrap();
        assert_eq!(t.max_diff, 5..=10);
        assert_eq!(t.total_pixels, 200..=300);
    }

    /// URL + named syntax should coexist.
    #[test]
    fn url_prefix_with_named() {
        let t = parse_fuzzy("ref.html:maxDifference=10;totalPixels=300").unwrap();
        assert_eq!(
            t.url.as_deref().map(|p| p.to_str().unwrap()),
            Some("ref.html")
        );
        assert_eq!(t.max_diff, 10..=10);
        assert_eq!(t.total_pixels, 300..=300);
    }

    /// Mixing named + positional is malformed.
    #[test]
    fn rejects_mixed_named_and_positional() {
        assert!(parse_fuzzy("maxDifference=10;300").is_err());
        assert!(parse_fuzzy("10;totalPixels=300").is_err());
    }

    /// Unknown named keys must be rejected, not silently treated as pass-any.
    #[test]
    fn rejects_unknown_named_key() {
        assert!(parse_fuzzy("maxDiff=10;totalPixels=300").is_err());
        assert!(parse_fuzzy("maxDifference=10;pixels=300").is_err());
    }

    /// Empty input should not panic and must surface as an error.
    #[test]
    fn rejects_empty_input() {
        assert!(parse_fuzzy("").is_err());
    }

    /// Non-numeric garbage must produce a parse error, not a panic.
    #[test]
    fn rejects_non_numeric() {
        assert!(parse_fuzzy("abc;def").is_err());
        assert!(parse_fuzzy("10;xyz").is_err());
    }

    /// Three semicolon-separated parts are malformed.
    #[test]
    fn rejects_too_many_parts() {
        assert!(parse_fuzzy("10;20;30").is_err());
    }

    /// `any()` constructor must span full u8 / u32 range so it never rejects a diff.
    #[test]
    fn any_is_fully_permissive() {
        let a = FuzzyTolerance::any();
        assert_eq!(a.url, None);
        assert_eq!(a.max_diff, 0..=255);
        assert_eq!(a.total_pixels, 0..=u32::MAX);
    }
}
