//! Tagged PDF semantic layer (fulgur-izp.3).
//!
//! Carries a fulgur-internal classification of HTML elements that the
//! render pass (`fulgur-izp.4`) and the StructTree builder
//! (`fulgur-izp.5`) translate into Krilla `Tag` / `ContentTag` calls.
//!
//! See `docs/plans/2026-05-03-tagged-pdf-drawables-redesign.md` for the
//! design and `docs/plans/2026-04-22-tagged-pdf-krilla-api-design.md`
//! for the underlying Krilla API analysis.

use crate::drawables::NodeId;

/// Subset of Krilla `tagging::Tag` variants that fulgur intends to map
/// HTML semantics to. Render-side translation to the Krilla type
/// happens in `fulgur-izp.5`; until then this enum is convert-side
/// only, so it intentionally avoids carrying Krilla-specific types
/// (alt text, heading title) — those flow from the DOM at render time
/// once the wire-up lands.
/// `ListNumbering` is carried here because `ul`/`ol` distinction is
/// known at classify time from the element local name.
/// `TableHeaderScope` is carried here because it is determined by the
/// `scope` HTML attribute (defaulting to `Both` when absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfTag {
    P,
    H {
        level: u8,
    },
    Div,
    Span,
    Figure,
    L {
        numbering: krilla::tagging::ListNumbering,
    },
    Lbl,
    LBody,
    Li,
    Table,
    THead,
    TBody,
    TFoot,
    Tr,
    Th {
        scope: krilla::tagging::TableHeaderScope,
    },
    Td,
    Link,
}

/// Per-NodeId semantic record stored in `Drawables.semantics`.
///
/// `parent` points to the nearest ancestor NodeId whose own
/// `SemanticEntry` is recorded, letting a render-time pass rebuild the
/// StructTree without re-walking the DOM. `None` marks an entry whose
/// ancestors carry no recognised tag.
#[derive(Debug, Clone)]
pub struct SemanticEntry {
    pub tag: PdfTag,
    pub parent: Option<NodeId>,
    /// Alt text for `Figure` nodes (`<img alt="...">`).
    /// `Some("")` = decorative image; `None` = alt attribute absent.
    pub alt_text: Option<String>,
}

/// Map an HTML element local name to a `PdfTag` when the element has a
/// known semantic mapping. Returns `None` for elements that should not
/// participate in the StructTree (text-only wrappers, custom elements,
/// `<script>`, `<style>`, etc.).
///
/// Heading levels are encoded as `PdfTag::H { level }` with `level` in
/// `1..=6`. `<th>` defaults to `TableHeaderScope::Both`; callers that
/// read the `scope` HTML attribute should override this field after the
/// initial classification (fulgur-izp.8).
pub fn classify_element(local_name: &str) -> Option<PdfTag> {
    match local_name {
        "p" => Some(PdfTag::P),
        "h1" => Some(PdfTag::H { level: 1 }),
        "h2" => Some(PdfTag::H { level: 2 }),
        "h3" => Some(PdfTag::H { level: 3 }),
        "h4" => Some(PdfTag::H { level: 4 }),
        "h5" => Some(PdfTag::H { level: 5 }),
        "h6" => Some(PdfTag::H { level: 6 }),
        "div" | "section" | "article" | "main" | "aside" | "nav" | "header" | "footer" => {
            Some(PdfTag::Div)
        }
        "span" => Some(PdfTag::Span),
        "img" => Some(PdfTag::Figure),
        "ul" => Some(PdfTag::L {
            numbering: krilla::tagging::ListNumbering::Disc,
        }),
        "ol" => Some(PdfTag::L {
            numbering: krilla::tagging::ListNumbering::Decimal,
        }),
        "li" => Some(PdfTag::Li),
        "table" => Some(PdfTag::Table),
        "thead" => Some(PdfTag::THead),
        "tbody" => Some(PdfTag::TBody),
        "tfoot" => Some(PdfTag::TFoot),
        "tr" => Some(PdfTag::Tr),
        "th" => Some(PdfTag::Th {
            scope: krilla::tagging::TableHeaderScope::Both,
        }),
        "td" => Some(PdfTag::Td),
        _ => None,
    }
}

/// Map a fulgur-internal [`PdfTag`] to the Krilla [`TagKind`] used when
/// building the PDF StructTree.
///
/// `heading_title` is forwarded to [`krilla::tagging::Tag::Hn`] as the
/// `/T` (Title) attribute required by PDF/UA-1. Pass `None` for non-heading
/// tags or when the text is unavailable.
///
/// `alt_text` is forwarded to [`krilla::tagging::Tag::Figure`] as the
/// `/Alt` attribute. `Some("")` marks a decorative image; `None` omits `/Alt`.
pub fn pdf_tag_to_krilla_tag(
    tag: &PdfTag,
    heading_title: Option<String>,
    alt_text: Option<String>,
) -> krilla::tagging::TagKind {
    use std::num::NonZeroU16;
    match tag {
        PdfTag::P => krilla::tagging::Tag::<krilla::tagging::kind::P>::P.into(),
        PdfTag::H { level } => {
            let level = NonZeroU16::new((*level).clamp(1, 6) as u16).unwrap();
            krilla::tagging::Tag::Hn(level, heading_title).into()
        }
        PdfTag::Span => krilla::tagging::Tag::<krilla::tagging::kind::Span>::Span.into(),
        PdfTag::Div => krilla::tagging::Tag::<krilla::tagging::kind::Div>::Div.into(),
        PdfTag::Figure => {
            krilla::tagging::Tag::<krilla::tagging::kind::Figure>::Figure(alt_text).into()
        }
        PdfTag::L { numbering } => krilla::tagging::Tag::L(*numbering).into(),
        PdfTag::Lbl => krilla::tagging::Tag::<krilla::tagging::kind::Lbl>::Lbl.into(),
        PdfTag::LBody => krilla::tagging::Tag::<krilla::tagging::kind::LBody>::LBody.into(),
        PdfTag::Li => krilla::tagging::Tag::<krilla::tagging::kind::LI>::LI.into(),
        PdfTag::Table => krilla::tagging::Tag::<krilla::tagging::kind::Table>::Table.into(),
        PdfTag::THead => krilla::tagging::Tag::<krilla::tagging::kind::THead>::THead.into(),
        PdfTag::TBody => krilla::tagging::Tag::<krilla::tagging::kind::TBody>::TBody.into(),
        PdfTag::TFoot => krilla::tagging::Tag::<krilla::tagging::kind::TFoot>::TFoot.into(),
        PdfTag::Tr => krilla::tagging::Tag::<krilla::tagging::kind::TR>::TR.into(),
        PdfTag::Th { scope } => krilla::tagging::Tag::TH(*scope).into(),
        PdfTag::Td => krilla::tagging::Tag::<krilla::tagging::kind::TD>::TD.into(),
        PdfTag::Link => krilla::tagging::Tag::<krilla::tagging::kind::Link>::Link.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_element_recognises_block_text() {
        assert_eq!(classify_element("p"), Some(PdfTag::P));
        assert_eq!(classify_element("h1"), Some(PdfTag::H { level: 1 }));
        assert_eq!(classify_element("h6"), Some(PdfTag::H { level: 6 }));
    }

    #[test]
    fn classify_element_h2_through_h5_have_correct_levels() {
        assert_eq!(classify_element("h2"), Some(PdfTag::H { level: 2 }));
        assert_eq!(classify_element("h3"), Some(PdfTag::H { level: 3 }));
        assert_eq!(classify_element("h4"), Some(PdfTag::H { level: 4 }));
        assert_eq!(classify_element("h5"), Some(PdfTag::H { level: 5 }));
    }

    #[test]
    fn classify_element_empty_string_returns_none() {
        assert_eq!(classify_element(""), None);
    }

    #[test]
    fn classify_element_recognises_generic_containers_as_div() {
        for tag in [
            "div", "section", "article", "main", "aside", "nav", "header", "footer",
        ] {
            assert_eq!(classify_element(tag), Some(PdfTag::Div), "tag = {tag}");
        }
    }

    #[test]
    fn classify_element_recognises_span_and_img() {
        assert_eq!(classify_element("span"), Some(PdfTag::Span));
        assert_eq!(classify_element("img"), Some(PdfTag::Figure));
    }

    #[test]
    fn classify_element_recognises_lists_and_tables() {
        use krilla::tagging::ListNumbering;
        assert_eq!(
            classify_element("ul"),
            Some(PdfTag::L {
                numbering: ListNumbering::Disc
            })
        );
        assert_eq!(
            classify_element("ol"),
            Some(PdfTag::L {
                numbering: ListNumbering::Decimal
            })
        );
        assert_eq!(classify_element("li"), Some(PdfTag::Li));
        assert_eq!(classify_element("table"), Some(PdfTag::Table));
        assert_eq!(classify_element("thead"), Some(PdfTag::THead));
        assert_eq!(classify_element("tbody"), Some(PdfTag::TBody));
        assert_eq!(classify_element("tfoot"), Some(PdfTag::TFoot));
        assert_eq!(classify_element("tr"), Some(PdfTag::Tr));
        assert_eq!(
            classify_element("th"),
            Some(PdfTag::Th {
                scope: krilla::tagging::TableHeaderScope::Both
            })
        );
        assert_eq!(classify_element("td"), Some(PdfTag::Td));
    }

    #[test]
    fn classify_element_returns_none_for_unrecognised() {
        assert_eq!(classify_element("script"), None);
        assert_eq!(classify_element("style"), None);
        assert_eq!(classify_element("custom-tag"), None);
        assert_eq!(classify_element("a"), None);
        assert_eq!(classify_element("body"), None);
        assert_eq!(classify_element("html"), None);
    }

    #[test]
    fn pdf_tag_to_krilla_tag_p() {
        let k = pdf_tag_to_krilla_tag(&PdfTag::P, None, None);
        assert!(matches!(k, krilla::tagging::TagKind::P(_)));
    }

    #[test]
    fn pdf_tag_to_krilla_tag_headings() {
        for level in 1u8..=6 {
            let k = pdf_tag_to_krilla_tag(&PdfTag::H { level }, None, None);
            assert!(
                matches!(k, krilla::tagging::TagKind::Hn(_)),
                "level={level}"
            );
        }
    }

    #[test]
    fn pdf_tag_to_krilla_tag_span() {
        let k = pdf_tag_to_krilla_tag(&PdfTag::Span, None, None);
        assert!(matches!(k, krilla::tagging::TagKind::Span(_)));
    }

    #[test]
    fn pdf_tag_to_krilla_tag_heading_with_title() {
        // Heading title flows through to the Hn variant.
        let k = pdf_tag_to_krilla_tag(&PdfTag::H { level: 2 }, Some("Chapter 1".to_owned()), None);
        assert!(matches!(k, krilla::tagging::TagKind::Hn(_)));
    }

    #[test]
    fn pdf_tag_to_krilla_tag_figure_none_alt_text() {
        // None = alt attribute absent (not decorative).
        let k = pdf_tag_to_krilla_tag(&PdfTag::Figure, None, None);
        assert!(matches!(k, krilla::tagging::TagKind::Figure(_)));
    }

    #[test]
    fn pdf_tag_to_krilla_tag_figure_empty_alt_text() {
        // Some("") = decorative image.
        let k = pdf_tag_to_krilla_tag(&PdfTag::Figure, None, Some(String::new()));
        assert!(matches!(k, krilla::tagging::TagKind::Figure(_)));
    }

    #[test]
    fn pdf_tag_to_krilla_tag_l_decimal() {
        let k = pdf_tag_to_krilla_tag(
            &PdfTag::L {
                numbering: krilla::tagging::ListNumbering::Decimal,
            },
            None,
            None,
        );
        assert!(matches!(k, krilla::tagging::TagKind::L(_)));
    }

    #[test]
    fn pdf_tag_to_krilla_tag_th_scope_variants() {
        use krilla::tagging::{TableHeaderScope, TagKind};
        for scope in [
            TableHeaderScope::Row,
            TableHeaderScope::Column,
            TableHeaderScope::Both,
        ] {
            let k = pdf_tag_to_krilla_tag(&PdfTag::Th { scope }, None, None);
            assert!(matches!(k, TagKind::TH(_)), "scope = {scope:?}");
            if let TagKind::TH(tag) = k {
                assert_eq!(tag.scope(), scope, "scope = {scope:?}");
            }
        }
    }

    #[test]
    fn pdf_tag_to_krilla_tag_covers_all_variants() {
        use krilla::tagging::TagKind;
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Div, None, None),
            TagKind::Div(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Figure, None, Some("logo".to_owned())),
            TagKind::Figure(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(
                &PdfTag::L {
                    numbering: krilla::tagging::ListNumbering::Disc
                },
                None,
                None
            ),
            TagKind::L(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Lbl, None, None),
            TagKind::Lbl(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::LBody, None, None),
            TagKind::LBody(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Li, None, None),
            TagKind::LI(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Table, None, None),
            TagKind::Table(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::THead, None, None),
            TagKind::THead(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::TBody, None, None),
            TagKind::TBody(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::TFoot, None, None),
            TagKind::TFoot(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Tr, None, None),
            TagKind::TR(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(
                &PdfTag::Th {
                    scope: krilla::tagging::TableHeaderScope::Both
                },
                None,
                None
            ),
            TagKind::TH(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Td, None, None),
            TagKind::TD(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Link, None, None),
            TagKind::Link(_)
        ));
    }

    // ── heading level clamping ────────────────────────────────────────────────

    #[test]
    fn pdf_tag_to_krilla_tag_heading_level_zero_clamped_to_one() {
        // level=0 is below the valid range; clamp(1,6) must bring it up to 1
        // so NonZeroU16::new(1) succeeds and we still get an Hn tag.
        let k = pdf_tag_to_krilla_tag(&PdfTag::H { level: 0 }, None, None);
        assert!(
            matches!(k, krilla::tagging::TagKind::Hn(_)),
            "level=0 should produce Hn after clamping to 1"
        );
    }

    #[test]
    fn pdf_tag_to_krilla_tag_heading_level_above_six_clamped() {
        // level=7 and level=255 are above the valid range; clamp(1,6) caps at 6.
        for level in [7u8, 10, 255] {
            let k = pdf_tag_to_krilla_tag(&PdfTag::H { level }, None, None);
            assert!(
                matches!(k, krilla::tagging::TagKind::Hn(_)),
                "level={level} should produce Hn after clamping to 6"
            );
        }
    }

    #[test]
    fn pdf_tag_to_krilla_tag_heading_level_clamping_with_title() {
        // Clamping path with a heading_title to confirm both clamp and title flow.
        let title = Some("Appendix".to_owned());
        let k_low = pdf_tag_to_krilla_tag(&PdfTag::H { level: 0 }, title.clone(), None);
        let k_high = pdf_tag_to_krilla_tag(&PdfTag::H { level: 9 }, title, None);
        assert!(matches!(k_low, krilla::tagging::TagKind::Hn(_)));
        assert!(matches!(k_high, krilla::tagging::TagKind::Hn(_)));
    }

    // ── SemanticEntry ─────────────────────────────────────────────────────────

    #[test]
    fn semantic_entry_construction_and_clone() {
        // No parent, no alt text.
        let e1 = SemanticEntry {
            tag: PdfTag::P,
            parent: None,
            alt_text: None,
        };
        assert!(e1.parent.is_none());
        assert!(e1.alt_text.is_none());

        // With parent NodeId and alt text.
        let e2 = SemanticEntry {
            tag: PdfTag::Figure,
            parent: Some(42_usize),
            alt_text: Some("company logo".to_owned()),
        };
        assert_eq!(e2.parent, Some(42));
        assert_eq!(e2.alt_text.as_deref(), Some("company logo"));

        // Decorative image: alt_text = Some("").
        let e3 = SemanticEntry {
            tag: PdfTag::Figure,
            parent: None,
            alt_text: Some(String::new()),
        };
        assert_eq!(e3.alt_text.as_deref(), Some(""));

        // Clone reproduces all fields.
        let e2c = e2.clone();
        assert_eq!(e2c.parent, e2.parent);
        assert_eq!(e2c.alt_text, e2.alt_text);
    }

    // ── PdfTag derives ────────────────────────────────────────────────────────

    #[test]
    fn pdf_tag_partial_eq_and_clone() {
        let a = PdfTag::H { level: 3 };
        let b = a.clone();
        assert_eq!(a, b);

        assert_ne!(PdfTag::P, PdfTag::Div);
        assert_ne!(
            PdfTag::H { level: 1 },
            PdfTag::H { level: 2 },
            "different heading levels must not compare equal"
        );
        assert_ne!(
            PdfTag::L {
                numbering: krilla::tagging::ListNumbering::Disc,
            },
            PdfTag::L {
                numbering: krilla::tagging::ListNumbering::Decimal,
            },
            "list numbering variants must not compare equal"
        );
        assert_ne!(
            PdfTag::Th {
                scope: krilla::tagging::TableHeaderScope::Row,
            },
            PdfTag::Th {
                scope: krilla::tagging::TableHeaderScope::Column,
            },
            "different TH scopes must not compare equal"
        );
    }

    #[test]
    fn pdf_tag_debug_is_non_empty() {
        // Smoke-test that Debug is implemented and produces something legible.
        assert!(!format!("{:?}", PdfTag::P).is_empty());
        assert!(!format!("{:?}", PdfTag::H { level: 2 }).is_empty());
        assert!(
            !format!(
                "{:?}",
                PdfTag::L {
                    numbering: krilla::tagging::ListNumbering::Disc
                }
            )
            .is_empty()
        );
    }

    // ── classify_element edge cases ───────────────────────────────────────────

    #[test]
    fn classify_element_is_case_sensitive() {
        // HTML local names are always lowercase in the DOM; uppercase must not match.
        for tag in ["P", "H1", "DIV", "SPAN", "TABLE", "UL", "LI"] {
            assert_eq!(
                classify_element(tag),
                None,
                "uppercase '{tag}' should return None (case-sensitive match)"
            );
        }
    }

    #[test]
    fn classify_element_none_for_interactive_and_metadata_elements() {
        // Interactive, form, media, and metadata elements carry no PDF semantic tag.
        for tag in [
            "form", "input", "button", "select", "textarea", "label", "fieldset", "legend", "head",
            "body", "html", "meta", "link", "title", "noscript", "iframe", "video", "audio",
            "source", "canvas", "map", "area",
        ] {
            assert_eq!(
                classify_element(tag),
                None,
                "element '{tag}' should return None"
            );
        }
    }
}
