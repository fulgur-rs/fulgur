use anyhow::{Result, bail};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyTolerance {
    pub url: Option<PathBuf>,
    pub max_diff: RangeInclusive<u8>,
    pub total_pixels: RangeInclusive<u32>,
}

impl FuzzyTolerance {
    /// Strict tolerance (exact pixel match required).
    ///
    /// Used when a test declares no `<meta name=fuzzy>`. Per WPT reftest
    /// spec, absence of fuzzy metadata means the rendering must match
    /// the reference byte-for-byte at the rasterized resolution.
    pub fn strict() -> Self {
        Self {
            url: None,
            max_diff: 0..=0,
            total_pixels: 0..=0,
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

#[derive(Debug, Clone, PartialEq)]
pub struct Reftest {
    pub test: PathBuf,
    pub classification: ReftestKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReftestKind {
    /// Single rel=match + optional fuzzy tolerance.
    Match {
        ref_path: PathBuf,
        fuzzy: FuzzyTolerance,
    },
    /// Single rel=mismatch + optional fuzzy tolerance. The test PASSes
    /// when test and ref renders *differ* beyond the fuzzy threshold.
    Mismatch {
        ref_path: PathBuf,
        fuzzy: FuzzyTolerance,
    },
    /// Skipped: out-of-scope reftest variant.
    Skip { reason: SkipReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Legacy: retained so older expectations comments (`# Mismatch`)
    /// remain parseable. `classify()` no longer emits this for single
    /// mismatch — that path now returns `ReftestKind::Mismatch` instead.
    Mismatch,
    MultipleMatches,
    MultipleMismatches,
    MixedMatchAndMismatch,
    NoMatch,
    /// Reserved for Phase 2: reftest chain (ref HTML points at another ref).
    /// Not yet emitted by `classify()`.
    ChainedReference,
}

/// Inspect the test HTML at `test_path` and classify it (Phase 1 runner:
/// rel=match / rel=mismatch).
/// File I/O only — does not render.
pub fn classify(test_path: &Path) -> Result<Reftest> {
    let html = std::fs::read_to_string(test_path)?;
    let doc = scraper::Html::parse_document(&html);

    let link_sel = scraper::Selector::parse("link[rel]").unwrap();
    let mut matches: Vec<PathBuf> = Vec::new();
    let mut mismatches: Vec<PathBuf> = Vec::new();

    for el in doc.select(&link_sel) {
        for token in el
            .value()
            .attr("rel")
            .unwrap_or("")
            .split_ascii_whitespace()
        {
            match token.to_ascii_lowercase().as_str() {
                "match" => {
                    let href = el.value().attr("href").ok_or_else(|| {
                        anyhow::anyhow!("rel=match link without href in {}", test_path.display())
                    })?;
                    if href.starts_with('/') {
                        // Root-relative WPT hrefs need wpt_root context; skip.
                        continue;
                    }
                    matches.push(PathBuf::from(href));
                }
                "mismatch" => {
                    let href = el.value().attr("href").ok_or_else(|| {
                        anyhow::anyhow!("rel=mismatch link without href in {}", test_path.display())
                    })?;
                    if href.starts_with('/') {
                        // Root-relative WPT hrefs need wpt_root context; skip.
                        continue;
                    }
                    mismatches.push(PathBuf::from(href));
                }
                _ => {}
            }
        }
    }

    // Decision table:
    //   matches=N, mismatches=M
    //   N≥2              → MultipleMatches
    //   M≥2              → MultipleMismatches
    //   N=1, M≥1         → MixedMatchAndMismatch
    //   N=1, M=0         → Match
    //   N=0, M=1         → Mismatch
    //   N=0, M=0         → NoMatch
    let (is_mismatch, ref_path) = match (matches.len(), mismatches.len()) {
        (n, _) if n >= 2 => {
            return Ok(Reftest {
                test: test_path.to_path_buf(),
                classification: ReftestKind::Skip {
                    reason: SkipReason::MultipleMatches,
                },
            });
        }
        (_, m) if m >= 2 => {
            return Ok(Reftest {
                test: test_path.to_path_buf(),
                classification: ReftestKind::Skip {
                    reason: SkipReason::MultipleMismatches,
                },
            });
        }
        (1, 1) => {
            return Ok(Reftest {
                test: test_path.to_path_buf(),
                classification: ReftestKind::Skip {
                    reason: SkipReason::MixedMatchAndMismatch,
                },
            });
        }
        (1, 0) => (false, matches.into_iter().next().unwrap()),
        (0, 1) => (true, mismatches.into_iter().next().unwrap()),
        _ => {
            return Ok(Reftest {
                test: test_path.to_path_buf(),
                classification: ReftestKind::Skip {
                    reason: SkipReason::NoMatch,
                },
            });
        }
    };

    // Collect fuzzy metas. Selection policy:
    // - If any meta has `url == ref_path`, that wins (authoritative, break).
    // - Otherwise the last unscoped (no url) meta wins.
    // - Prefix-scoped metas whose url differs from our ref are ignored.
    let meta_sel = scraper::Selector::parse(r#"meta[name="fuzzy"]"#).unwrap();
    let mut chosen = FuzzyTolerance::strict();
    for el in doc.select(&meta_sel) {
        let Some(content) = el.value().attr("content") else {
            continue;
        };
        let parsed = parse_fuzzy(content).map_err(|e| {
            anyhow::anyhow!(
                "invalid <meta name=\"fuzzy\"> in {}: {e}",
                test_path.display()
            )
        })?;
        match &parsed.url {
            Some(u) if u == &ref_path => {
                chosen = parsed;
                break; // URL-scoped match is authoritative
            }
            None => {
                // Unscoped: accept, but keep iterating in case a scoped
                // match for our ref appears later (last-wins for unscoped).
                chosen = parsed;
            }
            _ => {} // different url prefix: ignore
        }
    }

    let classification = if is_mismatch {
        ReftestKind::Mismatch {
            ref_path,
            fuzzy: chosen,
        }
    } else {
        ReftestKind::Match {
            ref_path,
            fuzzy: chosen,
        }
    };
    Ok(Reftest {
        test: test_path.to_path_buf(),
        classification,
    })
}

/// Recursively collect candidate reftest HTML files under `root`.
///
/// Applies WPT conventions:
/// - Only `.html` files are returned.
/// - Files whose stem (before any `.tentative`/`.whatever` segment) ends with
///   `-ref` or `-notref` are excluded (they are reference files, not tests).
/// - Subdirectories named `reference`, `resources`, or `support` are skipped
///   entirely — they contain only support assets by WPT convention.
///
/// Output is sorted lexicographically for deterministic iteration.
pub fn collect_reftest_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    const SKIP_DIRS: &[&str] = &["reference", "resources", "support"];

    fn is_reftest_name(name: &str) -> bool {
        if !name.ends_with(".html") {
            return false;
        }
        let stem = name.strip_suffix(".html").unwrap_or(name);
        let base = stem.split('.').next().unwrap_or(stem);
        !base.ends_with("-ref") && !base.ends_with("-notref")
    }

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if SKIP_DIRS.contains(&name) {
                    continue;
                }
                walk(&path, out)?;
                continue;
            }
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if is_reftest_name(name) {
                out.push(path);
            }
        }
        Ok(())
    }

    let mut out = Vec::new();
    walk(root, &mut out)?;
    out.sort();
    Ok(out)
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

    /// `strict()` constructor must be zero/zero so it enforces exact match.
    #[test]
    fn strict_is_zero_zero() {
        let t = FuzzyTolerance::strict();
        assert_eq!(t.url, None);
        assert_eq!(t.max_diff, 0..=0);
        assert_eq!(t.total_pixels, 0..=0);
    }
}

#[cfg(test)]
mod reftest_tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(name: &str, body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn single_match_no_fuzzy() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html><link rel="match" href="t-ref.html"><body></body>"#,
        );
        let r = classify(&p).unwrap();
        match r.classification {
            ReftestKind::Match { ref_path, fuzzy } => {
                assert_eq!(ref_path.file_name().unwrap(), "t-ref.html");
                assert_eq!(fuzzy, FuzzyTolerance::strict());
            }
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn single_match_with_fuzzy() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="match" href="t-ref.html">
<meta name="fuzzy" content="5-10;200-300">
<body></body>"#,
        );
        let r = classify(&p).unwrap();
        let fuzzy = match r.classification {
            ReftestKind::Match { fuzzy, .. } => fuzzy,
            _ => unreachable!(),
        };
        assert_eq!(fuzzy.max_diff, 5..=10);
        assert_eq!(fuzzy.total_pixels, 200..=300);
    }

    #[test]
    fn multiple_matches_skip() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="match" href="a.html">
<link rel="match" href="b.html">
<body></body>"#,
        );
        assert!(matches!(
            classify(&p).unwrap().classification,
            ReftestKind::Skip {
                reason: SkipReason::MultipleMatches
            }
        ));
    }

    #[test]
    fn no_match_skip() {
        let (_d, p) = write_tmp("t.html", r#"<!DOCTYPE html><body></body>"#);
        assert!(matches!(
            classify(&p).unwrap().classification,
            ReftestKind::Skip {
                reason: SkipReason::NoMatch
            }
        ));
    }

    #[test]
    fn fuzzy_url_prefix_matching_ref_is_used() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="match" href="t-ref.html">
<meta name="fuzzy" content="t-ref.html:5-10;200-300">
<body></body>"#,
        );
        let r = classify(&p).unwrap();
        match r.classification {
            ReftestKind::Match { fuzzy, .. } => {
                assert_eq!(fuzzy.max_diff, 5..=10);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn fuzzy_url_prefix_mismatched_is_ignored() {
        // Prefix points at a different ref → the classifier falls back to strict
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="match" href="t-ref.html">
<meta name="fuzzy" content="other.html:5-10;200-300">
<body></body>"#,
        );
        let r = classify(&p).unwrap();
        match r.classification {
            ReftestKind::Match { fuzzy, .. } => {
                assert_eq!(fuzzy, FuzzyTolerance::strict());
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn rel_match_without_href_is_error() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html><link rel="match"><body></body>"#,
        );
        let err = classify(&p).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("rel=match"), "unexpected error: {msg}");
    }

    #[test]
    fn rel_tokenization_finds_match_with_extra_tokens() {
        // HTML spec: rel is a whitespace-separated token list. "match alternate"
        // must still be classified as a Match reftest.
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html><link rel="match alternate" href="t-ref.html"><body></body>"#,
        );
        let r = classify(&p).unwrap();
        assert!(matches!(r.classification, ReftestKind::Match { .. }));
    }

    #[test]
    fn malformed_fuzzy_meta_is_error() {
        // Broken fuzzy metadata must surface as an error, not silently
        // fall back to strict tolerance (which would hide bad test authoring).
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="match" href="t-ref.html">
<meta name="fuzzy" content="not-a-valid-fuzzy-value">
<body></body>"#,
        );
        let err = classify(&p).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("fuzzy"), "unexpected error: {msg}");
    }

    #[test]
    fn collect_reftest_files_recurses_and_filters() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Top-level test + matching ref
        std::fs::write(root.join("test-001-print.html"), "").unwrap();
        std::fs::write(root.join("test-001-print-ref.html"), "").unwrap();
        // `-ref.tentative.html` leaks through a naive `ends_with("-ref.html")` filter
        std::fs::write(root.join("test-002-ref.tentative.html"), "").unwrap();
        // `-notref` also excluded
        std::fs::write(root.join("test-003-notref.html"), "").unwrap();

        // Nested test (must be picked up by recursion)
        std::fs::create_dir_all(root.join("tentative")).unwrap();
        std::fs::write(root.join("tentative/nested-test.html"), "").unwrap();

        // Skipped directories: contents never returned
        for skip in ["reference", "resources", "support"] {
            let sd = root.join(skip);
            std::fs::create_dir_all(&sd).unwrap();
            std::fs::write(sd.join("should-not-appear.html"), "").unwrap();
        }

        let files = collect_reftest_files(root).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();

        assert_eq!(
            names,
            vec![
                "tentative/nested-test.html".to_string(),
                "test-001-print.html".to_string(),
            ],
            "got {names:?}"
        );
    }

    #[test]
    fn single_mismatch_classified_as_mismatch() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html><link rel="mismatch" href="t-notref.html"><body></body>"#,
        );
        let r = classify(&p).unwrap();
        match r.classification {
            ReftestKind::Mismatch { ref_path, fuzzy } => {
                assert_eq!(ref_path.file_name().unwrap(), "t-notref.html");
                assert_eq!(fuzzy, FuzzyTolerance::strict());
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn mismatch_with_fuzzy_meta() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="mismatch" href="t-notref.html">
<meta name="fuzzy" content="5-10;200-300">
<body></body>"#,
        );
        match classify(&p).unwrap().classification {
            ReftestKind::Mismatch { fuzzy, .. } => {
                assert_eq!(fuzzy.max_diff, 5..=10);
                assert_eq!(fuzzy.total_pixels, 200..=300);
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn multiple_mismatches_skip() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="mismatch" href="a.html">
<link rel="mismatch" href="b.html">
<body></body>"#,
        );
        assert!(matches!(
            classify(&p).unwrap().classification,
            ReftestKind::Skip {
                reason: SkipReason::MultipleMismatches
            }
        ));
    }

    #[test]
    fn mixed_match_and_mismatch_skip() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="match" href="a.html">
<link rel="mismatch" href="b.html">
<body></body>"#,
        );
        assert!(matches!(
            classify(&p).unwrap().classification,
            ReftestKind::Skip {
                reason: SkipReason::MixedMatchAndMismatch
            }
        ));
    }

    #[test]
    fn root_relative_match_href_is_skipped_to_no_match() {
        // Root-relative hrefs (e.g. /css/reference/foo.html) cannot be resolved
        // without wpt_root context; classify() skips them → NoMatch.
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html><link rel="match" href="/css/reference/foo.html"><body></body>"#,
        );
        assert!(matches!(
            classify(&p).unwrap().classification,
            ReftestKind::Skip {
                reason: SkipReason::NoMatch
            }
        ));
    }

    #[test]
    fn root_relative_mismatch_href_is_skipped_to_no_match() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html><link rel="mismatch" href="/css/reference/foo.html"><body></body>"#,
        );
        assert!(matches!(
            classify(&p).unwrap().classification,
            ReftestKind::Skip {
                reason: SkipReason::NoMatch
            }
        ));
    }
}
