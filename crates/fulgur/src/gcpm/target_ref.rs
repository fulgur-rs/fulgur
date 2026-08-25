//! Cross-reference resolution for CSS GCPM `target-counter()` /
//! `target-counters()` / `target-text()`.
//!
//! `AnchorMap` is built at the end of pass 1 (after pagination has
//! assigned each DOM element a page) and consumed by pass 2 via the
//! resolver helpers below.

use crate::gcpm::counter::{format_counter, format_counter_chain};
use crate::gcpm::{CounterStyle, TargetTextKind};
use crate::pagination_layout::PaginationGeometryTable;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct AnchorMap {
    entries: BTreeMap<String, AnchorEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct AnchorEntry {
    pub page_num: u32,
    /// Counter name -> outer-to-inner instance chain at the target
    /// element. Mirrors `CounterState::chain_snapshot`.
    pub counters: BTreeMap<String, Vec<i32>>,
    pub text: String,
    pub before_text: String,
    pub after_text: String,
}

impl AnchorMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, fragment_id: impl Into<String>, entry: AnchorEntry) {
        self.entries.insert(fragment_id.into(), entry);
    }

    pub fn get(&self, fragment_id: &str) -> Option<&AnchorEntry> {
        self.entries.get(fragment_id)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Convert an attribute value (e.g. `"#sec1"`) to a fragment identifier.
/// Returns `None` for non-fragment URLs (anything not starting with `#`,
/// or empty after the `#`). The leading `#` is stripped; URL-decoding
/// and case normalization are NOT applied — HTML id matching is
/// case-sensitive in HTML5.
pub fn fragment_id_from_href(href: &str) -> Option<&str> {
    href.strip_prefix('#').filter(|s| !s.is_empty())
}

/// Resolve `target-counter(attr(<url_attr>), <counter_name>)`.
/// Returns the formatted value, or empty string on any failure.
pub fn resolve_target_counter(
    href: &str,
    counter_name: &str,
    style: CounterStyle,
    map: &AnchorMap,
) -> String {
    let Some(frag) = fragment_id_from_href(href) else {
        return String::new();
    };
    let Some(entry) = map.get(frag) else {
        return String::new();
    };
    // CSS spec treats `page` as the implicit page counter. Resolve it
    // straight from `entry.page_num` regardless of whether the snapshot
    // happens to carry a user-defined `page` counter — `target-counter(..,
    // page)` is documented to mean "the page number where this anchor
    // lands", and giving precedence to a custom `counter-reset: page`
    // produces a result that disagrees with the rendered page sequence.
    if counter_name == "page" {
        return format_counter(entry.page_num as i32, style);
    }
    let Some(chain) = entry.counters.get(counter_name) else {
        return String::new();
    };
    chain
        .last()
        .copied()
        .map(|v| format_counter(v, style))
        .unwrap_or_default()
}

pub fn resolve_target_counters(
    href: &str,
    counter_name: &str,
    separator: &str,
    style: CounterStyle,
    map: &AnchorMap,
) -> String {
    let Some(frag) = fragment_id_from_href(href) else {
        return String::new();
    };
    let Some(entry) = map.get(frag) else {
        return String::new();
    };
    let Some(chain) = entry.counters.get(counter_name) else {
        return String::new();
    };
    format_counter_chain(chain, separator, style)
}

pub fn resolve_target_text(href: &str, kind: TargetTextKind, map: &AnchorMap) -> String {
    let Some(frag) = fragment_id_from_href(href) else {
        return String::new();
    };
    let Some(entry) = map.get(frag) else {
        return String::new();
    };
    match kind {
        TargetTextKind::Content => entry.text.clone(),
        TargetTextKind::Before => entry.before_text.clone(),
        TargetTextKind::After => entry.after_text.clone(),
        TargetTextKind::FirstLetter => compute_first_letter(&entry.text),
    }
}

/// First-letter, based on CSS Pseudo-Elements 4 §3.2: optional leading
/// typographic punctuation, the first typographic letter/digit
/// (grapheme cluster), then optional trailing typographic punctuation.
/// This is **extended beyond the literal §3.2 category set** (which is
/// punctuation-only: Pc Pd Ps Pe Pi Pf Po): leading and trailing runs
/// also include currency, math, and modifier/other symbols (e.g. `$`,
/// `¥`, `+`, `©`), so `$Hello` yields `$H`. The symbol inclusion is an
/// intentional design extension, not a consequence of the spec.
/// Whitespace appearing between the leading punctuation/symbol run and
/// the first letter (after fully-trimmed leading whitespace) terminates
/// the first-letter, yielding `""` — an intentional interpretation of
/// the spec's ambiguous contiguity wording. Returns `""` when there is
/// no letter.
fn compute_first_letter(text: &str) -> String {
    use unicode_properties::{GeneralCategory as GC, UnicodeGeneralCategory};
    use unicode_segmentation::UnicodeSegmentation;

    fn is_punct(g: &str) -> bool {
        g.chars().all(|c| {
            matches!(
                c.general_category(),
                GC::OpenPunctuation
                    | GC::ClosePunctuation
                    | GC::InitialPunctuation
                    | GC::FinalPunctuation
                    | GC::OtherPunctuation
                    | GC::ConnectorPunctuation
                    | GC::DashPunctuation
                    | GC::MathSymbol
                    | GC::OtherSymbol
                    | GC::CurrencySymbol
                    | GC::ModifierSymbol
            )
        })
    }
    fn is_letter(g: &str) -> bool {
        g.chars().any(|c| {
            matches!(
                c.general_category(),
                GC::UppercaseLetter
                    | GC::LowercaseLetter
                    | GC::TitlecaseLetter
                    | GC::ModifierLetter
                    | GC::OtherLetter
                    | GC::DecimalNumber
                    | GC::LetterNumber
                    | GC::OtherNumber
            )
        })
    }

    // Iterate graphemes lazily and stop right after the first letter plus its
    // trailing punctuation run, tracking only a byte offset. Materializing the
    // whole trimmed string (e.g. `graphemes(true).collect::<Vec<&str>>()`) would
    // allocate one fat pointer per grapheme for attacker-controlled target text,
    // an OOM/DoS vector even when the returned prefix is a single grapheme
    // (fulgur-lfgg).
    let trimmed = text.trim_start();
    let mut first_letter_end = None;
    for (idx, g) in trimmed.grapheme_indices(true) {
        match first_letter_end {
            // Already past the first letter: absorb the trailing punctuation
            // run, then stop at the first non-punctuation grapheme.
            Some(_) => {
                if is_punct(g) {
                    first_letter_end = Some(idx + g.len());
                    continue;
                }
                break;
            }
            // Still scanning the optional leading punctuation run.
            None => {
                if is_punct(g) {
                    continue;
                }
                if is_letter(g) {
                    first_letter_end = Some(idx + g.len());
                    continue;
                }
                // A non-punctuation, non-letter grapheme before any letter
                // (e.g. whitespace after leading punctuation) yields "".
                return String::new();
            }
        }
    }
    first_letter_end
        .map(|end| trimmed[..end].to_string())
        .unwrap_or_default()
}

/// Return the **1-based** page number for a DOM node, derived from
/// the first fragment in the node's pagination geometry. Returns
/// `None` if the node has no fragments (out-of-flow nodes the
/// fragmenter skipped, or non-laid-out subtrees).
pub fn page_for_node(geometry: &PaginationGeometryTable, node_id: usize) -> Option<u32> {
    geometry
        .get(&node_id)
        .and_then(|g| g.fragments.first())
        .map(|f| f.page_index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::F32Units;

    fn make_map() -> AnchorMap {
        let mut m = AnchorMap::new();
        let mut counters = BTreeMap::new();
        counters.insert("section".into(), vec![1, 2]);
        m.insert(
            "sec-1-2",
            AnchorEntry {
                page_num: 7,
                counters,
                text: "Introduction".into(),
                before_text: "Before".into(),
                after_text: "After".into(),
            },
        );
        m
    }

    #[test]
    fn fragment_id_strips_hash() {
        assert_eq!(fragment_id_from_href("#sec1"), Some("sec1"));
    }

    #[test]
    fn fragment_id_rejects_external() {
        assert_eq!(fragment_id_from_href("https://example.com/"), None);
        assert_eq!(fragment_id_from_href("foo.html#bar"), None);
        assert_eq!(fragment_id_from_href("#"), None);
        assert_eq!(fragment_id_from_href(""), None);
    }

    #[test]
    fn target_counter_page_uses_page_num() {
        let m = make_map();
        assert_eq!(
            resolve_target_counter("#sec-1-2", "page", CounterStyle::Decimal, &m),
            "7"
        );
    }

    #[test]
    fn target_counter_page_name_ignores_user_defined_counter() {
        // `target-counter(href, page)` always resolves to the actual
        // page number where the anchor lands. A user-defined
        // `counter-reset: page` on the target element must not shadow
        // the implicit page counter.
        let mut m = AnchorMap::new();
        let mut counters = BTreeMap::new();
        counters.insert("page".into(), vec![999]);
        m.insert(
            "x",
            AnchorEntry {
                page_num: 5,
                counters,
                text: String::new(),
                before_text: String::new(),
                after_text: String::new(),
            },
        );
        assert_eq!(
            resolve_target_counter("#x", "page", CounterStyle::Decimal, &m),
            "5"
        );
    }

    #[test]
    fn target_counter_named_uses_innermost() {
        let m = make_map();
        assert_eq!(
            resolve_target_counter("#sec-1-2", "section", CounterStyle::Decimal, &m),
            "2"
        );
    }

    #[test]
    fn target_counter_missing_fragment_returns_empty() {
        let m = make_map();
        assert_eq!(
            resolve_target_counter("#nope", "page", CounterStyle::Decimal, &m),
            ""
        );
    }

    #[test]
    fn target_counter_external_href_returns_empty() {
        let m = make_map();
        assert_eq!(
            resolve_target_counter("https://example.com/", "page", CounterStyle::Decimal, &m),
            ""
        );
    }

    #[test]
    fn target_counters_joins_chain() {
        let m = make_map();
        assert_eq!(
            resolve_target_counters("#sec-1-2", "section", ".", CounterStyle::Decimal, &m),
            "1.2"
        );
    }

    #[test]
    fn target_text_returns_text() {
        let m = make_map();
        assert_eq!(
            resolve_target_text("#sec-1-2", TargetTextKind::Content, &m),
            "Introduction"
        );
    }

    #[test]
    fn target_text_returns_before_after_and_first_letter() {
        let m = make_map();
        assert_eq!(
            resolve_target_text("#sec-1-2", TargetTextKind::Before, &m),
            "Before"
        );
        assert_eq!(
            resolve_target_text("#sec-1-2", TargetTextKind::After, &m),
            "After"
        );
        assert_eq!(
            resolve_target_text("#sec-1-2", TargetTextKind::FirstLetter, &m),
            "I"
        );
    }

    #[test]
    fn target_text_first_letter_is_grapheme_cluster() {
        let mut m = make_map();
        m.entries.get_mut("sec-1-2").expect("target fixture").text = "A\u{0301}BC".to_string();
        assert_eq!(
            resolve_target_text("#sec-1-2", TargetTextKind::FirstLetter, &m),
            "A\u{0301}"
        );
    }

    #[test]
    fn first_letter_ascii() {
        assert_eq!(compute_first_letter("Hello world"), "H");
    }
    #[test]
    fn first_letter_skips_leading_punct_and_keeps_trailing() {
        assert_eq!(compute_first_letter("「『Hello』"), "「『H");
    }
    #[test]
    fn first_letter_digit_counts_as_letter() {
        assert_eq!(compute_first_letter("123abc"), "1");
    }
    #[test]
    fn first_letter_empty_and_all_punct() {
        assert_eq!(compute_first_letter(""), "");
        assert_eq!(compute_first_letter("   "), "");
        assert_eq!(compute_first_letter("...!?"), "");
    }
    #[test]
    fn first_letter_space_before_letter_yields_nothing() {
        assert_eq!(compute_first_letter("『 H"), "");
    }
    #[test]
    fn first_letter_grapheme_cluster() {
        assert_eq!(compute_first_letter("e\u{0301}tude"), "e\u{0301}");
    }
    #[test]
    fn first_letter_includes_leading_currency_symbol() {
        // Intentional extension beyond literal §3.2 (P*-only): leading
        // currency/math/symbol clusters ride with the first letter.
        assert_eq!(compute_first_letter("$Hello"), "$H");
    }
    #[test]
    fn first_letter_trailing_punct_then_more_letters() {
        // Exercises the trailing-punct (`j`) advance: stop at the next letter.
        assert_eq!(compute_first_letter("H!Hello"), "H!");
    }
    #[test]
    fn first_letter_trailing_punct_then_nonletter() {
        assert_eq!(compute_first_letter("「Hello」 world"), "「H");
    }
    #[test]
    fn first_letter_large_tail_stops_early() {
        // Regression for fulgur-lfgg: the first letter is at the start,
        // followed by a huge non-punctuation tail. The result must be the
        // single leading letter, and the implementation must not materialize
        // the whole string (early-exit offset path).
        let mut s = String::from("H");
        s.push_str(&"x".repeat(1_000_000));
        assert_eq!(compute_first_letter(&s), "H");
    }

    #[test]
    fn target_text_missing_returns_empty() {
        let m = make_map();
        assert_eq!(
            resolve_target_text("#nope", TargetTextKind::Content, &m),
            ""
        );
    }

    #[test]
    fn page_for_node_returns_first_page_for_split_node() {
        use crate::pagination_layout::{Fragment, PaginationGeometry};
        let mut table = PaginationGeometryTable::new();
        table.insert(
            42,
            PaginationGeometry {
                fragments: vec![
                    Fragment {
                        page_index: 2,
                        x: 0.0_f32.as_px(),
                        y: 0.0_f32.as_px(),
                        width: 0.0_f32.as_px(),
                        height: 0.0_f32.as_px(),
                    },
                    Fragment {
                        page_index: 3,
                        x: 0.0_f32.as_px(),
                        y: 0.0_f32.as_px(),
                        width: 0.0_f32.as_px(),
                        height: 0.0_f32.as_px(),
                    },
                ],
                is_repeat: false,
                ..Default::default()
            },
        );
        assert_eq!(page_for_node(&table, 42), Some(3)); // page_index 2 -> 1-based page 3
    }

    #[test]
    fn page_for_node_returns_none_for_absent_node() {
        let table = PaginationGeometryTable::new();
        assert_eq!(page_for_node(&table, 999), None);
    }

    #[test]
    fn page_for_node_returns_none_for_node_with_no_fragments() {
        use crate::pagination_layout::PaginationGeometry;
        let mut table = PaginationGeometryTable::new();
        table.insert(
            7,
            PaginationGeometry {
                fragments: vec![],
                is_repeat: false,
                ..Default::default()
            },
        );
        assert_eq!(page_for_node(&table, 7), None);
    }

    #[test]
    fn anchor_map_is_empty_after_construction() {
        let m = AnchorMap::new();
        assert!(m.is_empty());
    }

    #[test]
    fn anchor_map_is_not_empty_after_insert() {
        let mut m = AnchorMap::new();
        m.insert("a", AnchorEntry::default());
        assert!(!m.is_empty());
    }

    #[test]
    fn target_counter_undefined_named_counter_returns_empty() {
        // Entry exists, counter_name is not "page", and counters map
        // does not contain the requested key. Hits the second
        // `let-else` empty-return path in resolve_target_counter.
        let m = make_map();
        assert_eq!(
            resolve_target_counter("#sec-1-2", "no-such-counter", CounterStyle::Decimal, &m),
            ""
        );
    }

    #[test]
    fn target_counters_external_href_returns_empty() {
        let m = make_map();
        assert_eq!(
            resolve_target_counters("foo.html#bar", "section", ".", CounterStyle::Decimal, &m),
            ""
        );
    }

    #[test]
    fn target_counters_missing_fragment_returns_empty() {
        let m = make_map();
        assert_eq!(
            resolve_target_counters("#nope", "section", ".", CounterStyle::Decimal, &m),
            ""
        );
    }

    #[test]
    fn target_counters_undefined_counter_returns_empty() {
        let m = make_map();
        assert_eq!(
            resolve_target_counters("#sec-1-2", "undefined", ".", CounterStyle::Decimal, &m),
            ""
        );
    }

    #[test]
    fn target_text_external_href_returns_empty() {
        let m = make_map();
        assert_eq!(
            resolve_target_text("https://example.com/", TargetTextKind::Content, &m),
            ""
        );
    }
}
