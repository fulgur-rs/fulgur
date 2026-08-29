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

    // --- SemanticEntry construction and derived-trait coverage ---

    #[test]
    fn semantic_entry_fields_accessible_with_none_parent_and_alt() {
        let entry = SemanticEntry {
            tag: PdfTag::P,
            parent: None,
            alt_text: None,
        };
        assert!(matches!(entry.tag, PdfTag::P));
        assert!(entry.parent.is_none());
        assert!(entry.alt_text.is_none());
    }

    #[test]
    fn semantic_entry_fields_accessible_with_some_parent_and_alt() {
        let entry = SemanticEntry {
            tag: PdfTag::H { level: 2 },
            parent: Some(42),
            alt_text: Some("chapter heading".to_owned()),
        };
        assert_eq!(entry.parent, Some(42));
        assert_eq!(entry.alt_text.as_deref(), Some("chapter heading"));
    }

    #[test]
    fn semantic_entry_clone_preserves_all_fields() {
        let entry = SemanticEntry {
            tag: PdfTag::Figure,
            parent: Some(7),
            alt_text: Some("company logo".to_owned()),
        };
        let cloned = entry.clone();
        assert_eq!(cloned.tag, entry.tag);
        assert_eq!(cloned.parent, entry.parent);
        assert_eq!(cloned.alt_text, entry.alt_text);
    }

    #[test]
    fn semantic_entry_clone_with_none_fields() {
        let entry = SemanticEntry {
            tag: PdfTag::Div,
            parent: None,
            alt_text: None,
        };
        let cloned = entry.clone();
        assert_eq!(cloned.tag, entry.tag);
        assert!(cloned.parent.is_none());
        assert!(cloned.alt_text.is_none());
    }

    #[test]
    fn semantic_entry_debug_contains_struct_name_and_tag() {
        let entry = SemanticEntry {
            tag: PdfTag::Table,
            parent: Some(3),
            alt_text: None,
        };
        let s = format!("{entry:?}");
        assert!(s.contains("SemanticEntry"));
        assert!(s.contains("Table"));
        // Verify payload values appear in the debug output.
        assert!(
            s.contains("Some(3)"),
            "parent should appear as Some(3), got: {s}"
        );
        assert!(s.contains("None"), "alt_text: None should appear, got: {s}");
        // PdfTag::L and PdfTag::Th field values must also be visible in their debug output.
        assert!(
            format!(
                "{:?}",
                PdfTag::L {
                    numbering: krilla::tagging::ListNumbering::Disc
                }
            )
            .contains("Disc"),
            "PdfTag::L debug must include numbering variant name"
        );
        assert!(
            format!(
                "{:?}",
                PdfTag::Th {
                    scope: krilla::tagging::TableHeaderScope::Column
                }
            )
            .contains("Column"),
            "PdfTag::Th debug must include scope variant name"
        );
    }

    // --- PdfTag Clone coverage for field-carrying variants ---

    #[test]
    fn pdf_tag_clone_heading_variant() {
        let original = PdfTag::H { level: 3 };
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn pdf_tag_clone_list_variant() {
        let original = PdfTag::L {
            numbering: krilla::tagging::ListNumbering::Decimal,
        };
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn pdf_tag_clone_th_variant() {
        let original = PdfTag::Th {
            scope: krilla::tagging::TableHeaderScope::Row,
        };
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn pdf_tag_clone_unit_variants() {
        for tag in [
            PdfTag::P,
            PdfTag::Div,
            PdfTag::Span,
            PdfTag::Figure,
            PdfTag::Lbl,
            PdfTag::LBody,
            PdfTag::Li,
            PdfTag::Table,
            PdfTag::THead,
            PdfTag::TBody,
            PdfTag::TFoot,
            PdfTag::Tr,
            PdfTag::Td,
            PdfTag::Link,
        ] {
            assert_eq!(tag.clone(), tag);
        }
    }

    // --- PdfTag Debug formatting ---

    #[test]
    fn pdf_tag_debug_unit_variants() {
        assert!(format!("{:?}", PdfTag::P).contains("P"));
        assert!(format!("{:?}", PdfTag::Div).contains("Div"));
        assert!(format!("{:?}", PdfTag::Span).contains("Span"));
        assert!(format!("{:?}", PdfTag::Figure).contains("Figure"));
        assert!(format!("{:?}", PdfTag::Lbl).contains("Lbl"));
        assert!(format!("{:?}", PdfTag::LBody).contains("LBody"));
        assert!(format!("{:?}", PdfTag::Li).contains("Li"));
        assert!(format!("{:?}", PdfTag::Table).contains("Table"));
        assert!(format!("{:?}", PdfTag::THead).contains("THead"));
        assert!(format!("{:?}", PdfTag::TBody).contains("TBody"));
        assert!(format!("{:?}", PdfTag::TFoot).contains("TFoot"));
        assert!(format!("{:?}", PdfTag::Tr).contains("Tr"));
        assert!(format!("{:?}", PdfTag::Td).contains("Td"));
        assert!(format!("{:?}", PdfTag::Link).contains("Link"));
    }

    #[test]
    fn pdf_tag_debug_field_variants() {
        assert!(format!("{:?}", PdfTag::H { level: 4 }).contains("4"));
        assert!(
            format!(
                "{:?}",
                PdfTag::L {
                    numbering: krilla::tagging::ListNumbering::Disc
                }
            )
            .contains("L")
        );
        assert!(
            format!(
                "{:?}",
                PdfTag::Th {
                    scope: krilla::tagging::TableHeaderScope::Column
                }
            )
            .contains("Th")
        );
    }

    // --- pdf_tag_to_krilla_tag heading-level clamping ---

    #[test]
    fn pdf_tag_to_krilla_tag_heading_level_zero_clamped_to_one() {
        // level=0 is invalid; clamp(1,6) → 1.  Verify the stored level, not just the variant.
        let k = pdf_tag_to_krilla_tag(&PdfTag::H { level: 0 }, None, None);
        let krilla::tagging::TagKind::Hn(tag) = k else {
            panic!("expected TagKind::Hn for level=0");
        };
        assert_eq!(tag.level().get(), 1, "level=0 should clamp to H1");
    }

    #[test]
    fn pdf_tag_to_krilla_tag_heading_level_above_max_clamped_to_six() {
        // level=7 and level=255 are above the PDF maximum H6; both clamp to 6.
        // Verify the stored level, not just the variant, to catch a regressed clamp.
        for input in [7u8, 255u8] {
            let k = pdf_tag_to_krilla_tag(&PdfTag::H { level: input }, None, None);
            let krilla::tagging::TagKind::Hn(tag) = k else {
                panic!("expected TagKind::Hn for level={input}");
            };
            assert_eq!(tag.level().get(), 6, "level={input} should clamp to H6");
        }
    }

    // --- classify_element edge cases ---

    #[test]
    fn classify_element_figure_html_element_returns_none() {
        // HTML <figure> has no mapping; only <img> maps to PdfTag::Figure.
        assert_eq!(classify_element("figure"), None);
    }

    #[test]
    fn classify_element_anchor_and_link_return_none() {
        // <a> and <link> have no mapping in classify_element;
        // PdfTag::Link is set by the convert pass directly.
        assert_eq!(classify_element("a"), None);
        assert_eq!(classify_element("link"), None);
    }

    // --- PdfTag::PartialEq inequality tests ---
    //
    // The existing tests only compare same-variant values (e.g. `assert_eq!(tag.clone(), tag)`).
    // Cross-variant comparisons exercise the `_ => false` catch-all arm that the
    // `#[derive(PartialEq)]` macro generates, and field-inequality cases exercise the
    // false-branch of each per-field sub-comparison.

    #[test]
    fn pdf_tag_partial_eq_different_unit_variants_are_unequal() {
        assert_ne!(PdfTag::P, PdfTag::Div);
        assert_ne!(PdfTag::P, PdfTag::Span);
        assert_ne!(PdfTag::Div, PdfTag::Span);
        assert_ne!(PdfTag::Li, PdfTag::Lbl);
        assert_ne!(PdfTag::Lbl, PdfTag::LBody);
        assert_ne!(PdfTag::Td, PdfTag::Tr);
        assert_ne!(PdfTag::THead, PdfTag::TBody);
        assert_ne!(PdfTag::TBody, PdfTag::TFoot);
        assert_ne!(PdfTag::Table, PdfTag::Tr);
        assert_ne!(PdfTag::Link, PdfTag::Span);
        assert_ne!(PdfTag::Figure, PdfTag::Div);
    }

    #[test]
    fn pdf_tag_partial_eq_heading_level_inequality() {
        // Same variant, different field value → must be unequal.
        assert_ne!(PdfTag::H { level: 1 }, PdfTag::H { level: 2 });
        assert_ne!(PdfTag::H { level: 3 }, PdfTag::H { level: 4 });
        assert_ne!(PdfTag::H { level: 1 }, PdfTag::H { level: 6 });
    }

    #[test]
    fn pdf_tag_partial_eq_heading_vs_unit_variant() {
        assert_ne!(PdfTag::H { level: 1 }, PdfTag::P);
        assert_ne!(PdfTag::H { level: 2 }, PdfTag::Div);
    }

    #[test]
    fn pdf_tag_partial_eq_list_numbering_inequality() {
        assert_ne!(
            PdfTag::L {
                numbering: krilla::tagging::ListNumbering::Disc
            },
            PdfTag::L {
                numbering: krilla::tagging::ListNumbering::Decimal
            }
        );
    }

    #[test]
    fn pdf_tag_partial_eq_th_scope_inequality() {
        use krilla::tagging::TableHeaderScope;
        assert_ne!(
            PdfTag::Th {
                scope: TableHeaderScope::Row
            },
            PdfTag::Th {
                scope: TableHeaderScope::Column
            }
        );
        assert_ne!(
            PdfTag::Th {
                scope: TableHeaderScope::Both
            },
            PdfTag::Th {
                scope: TableHeaderScope::Row
            }
        );
    }

    #[test]
    fn pdf_tag_partial_eq_field_variants_vs_unit_variants() {
        assert_ne!(
            PdfTag::L {
                numbering: krilla::tagging::ListNumbering::Disc
            },
            PdfTag::Li
        );
        assert_ne!(
            PdfTag::Th {
                scope: krilla::tagging::TableHeaderScope::Both
            },
            PdfTag::Td
        );
    }

    /// Table-driven exhaustive cross-variant inequality check for all 14 unit variants.
    ///
    /// This exercises every discriminant pair in the derived `PartialEq` match —
    /// specifically the `_ => false` catch-all arm for differing variants — for
    /// every combination of unit `PdfTag` variants.
    #[test]
    fn pdf_tag_partial_eq_all_unit_pairs_are_unequal_across_types() {
        let all_units = [
            PdfTag::P,
            PdfTag::Div,
            PdfTag::Span,
            PdfTag::Figure,
            PdfTag::Lbl,
            PdfTag::LBody,
            PdfTag::Li,
            PdfTag::Table,
            PdfTag::THead,
            PdfTag::TBody,
            PdfTag::TFoot,
            PdfTag::Tr,
            PdfTag::Td,
            PdfTag::Link,
        ];
        for (i, a) in all_units.iter().enumerate() {
            for (j, b) in all_units.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b, "variant at index {i} must equal itself");
                } else {
                    assert_ne!(a, b, "variants at indices {i} and {j} must not be equal");
                }
            }
        }
    }

    // --- classify_element out-of-range heading levels ---

    #[test]
    fn classify_element_heading_levels_out_of_range_return_none() {
        // Only h1–h6 map to PdfTag::H. h0, h7, h8, … must return None
        // so unknown heading-like idents don't silently produce a H tag.
        assert_eq!(classify_element("h0"), None);
        assert_eq!(classify_element("h7"), None);
        assert_eq!(classify_element("h9"), None);
    }

    // --- classify_element does not match case-insensitively ---

    #[test]
    fn classify_element_requires_lowercase_input() {
        // HTML local names are always lowercase after parsing, but the function
        // accepts `&str` and must not match uppercase forms.
        assert_eq!(classify_element("P"), None);
        assert_eq!(classify_element("DIV"), None);
        assert_eq!(classify_element("Span"), None);
        assert_eq!(classify_element("H1"), None);
    }
}
