//! fulgur-specific User-Agent stylesheet (GCPM-only rules).
//!
//! This CSS is fed into the GCPM parser **before** author CSS so that
//! `h1`-`h6` default to sensible bookmark levels and labels. Author
//! rules with higher specificity or later declaration order can still
//! override per standard cascade behaviour.
//!
//! The UA sheet is GCPM-only — it never reaches Blitz. Blitz keeps its
//! own UA stylesheet for layout/presentation defaults.

/// Built-in GCPM UA stylesheet for fulgur.
///
/// Maps `h1`-`h6` to `bookmark-level: 1..6` with `bookmark-label:
/// content()` (the element's own text). This replaces the previous
/// hardcoded `h1`-`h6` → outline walker in the pageable tree with a
/// CSS-driven pass, and lets users override either the level or the
/// label for any heading via regular author CSS.
pub const FULGUR_UA_CSS: &str = r#"
h1 { bookmark-level: 1; bookmark-label: content(); }
h2 { bookmark-level: 2; bookmark-label: content(); }
h3 { bookmark-level: 3; bookmark-label: content(); }
h4 { bookmark-level: 4; bookmark-label: content(); }
h5 { bookmark-level: 5; bookmark-label: content(); }
h6 { bookmark-level: 6; bookmark-label: content(); }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcpm::bookmark::BookmarkLevel;
    use crate::gcpm::parser::parse_gcpm;
    use crate::gcpm::{ContentItem, ParsedSelector};

    #[test]
    fn fulgur_ua_css_produces_h1_to_h6_mappings() {
        let ctx = parse_gcpm(FULGUR_UA_CSS);
        assert_eq!(
            ctx.bookmark_mappings.len(),
            6,
            "expected one mapping per heading level h1..h6"
        );
        for (i, m) in ctx.bookmark_mappings.iter().enumerate() {
            let level = (i + 1) as u8;
            assert_eq!(
                m.selector,
                ParsedSelector::Tag(format!("h{level}")),
                "mapping {i} selector"
            );
            assert_eq!(
                m.level,
                Some(BookmarkLevel::Integer(level)),
                "mapping {i} level"
            );
            assert_eq!(
                m.label.as_deref(),
                Some(&[ContentItem::ContentText][..]),
                "mapping {i} label"
            );
        }
    }
}
