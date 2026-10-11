//! Tagged PDF semantic layer shared by Fulgur's layout backends.
//!
//! Carries a fulgur-internal classification of HTML elements that each
//! backend's render pass and StructTree builder translate into Krilla
//! `Tag` / `ContentTag` calls. It does not depend on Krilla, so the Blitz
//! and Raikiri backends map HTML semantics the same way even when they build
//! against different Krilla versions; each backend converts [`PdfTag`] to its
//! own Krilla `TagKind`.
//!
//! See `docs/plans/2026-05-03-tagged-pdf-drawables-redesign.md` for the
//! design and `docs/plans/2026-04-22-tagged-pdf-krilla-api-design.md`
//! for the underlying Krilla API analysis.

/// Key of a semantic record: a backend node id, or a synthetic id the
/// backend allocates for structure that has no node of its own (the `Lbl`
/// and `LBody` children of a list item).
pub type NodeId = usize;

/// Subset of Krilla `tagging::Tag` variants that fulgur maps HTML
/// semantics to. It carries no render-time data (alt text, heading
/// title); backends take those from the DOM when they build the tree.
/// `ListNumbering` is carried here because `ul`/`ol` distinction is
/// known at classify time from the element local name.
/// `TableHeaderScope` is carried here because it is determined by the
/// `scope` HTML attribute (defaulting to `Both` when absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfTag {
    P,
    H { level: u8 },
    Div,
    Span,
    Figure,
    L { numbering: ListNumbering },
    Lbl,
    LBody,
    Li,
    Table,
    THead,
    TBody,
    TFoot,
    Tr,
    Th { scope: TableHeaderScope },
    Td,
    Link,
}

/// List numbering of an `L` structure element (PDF 1.7 Table 347), mirroring
/// Krilla's `ListNumbering` so this crate stays independent of the Krilla
/// version each backend uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListNumbering {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    LowerRoman,
    UpperRoman,
    LowerAlpha,
    UpperAlpha,
}

/// Scope of a `TH` structure element (PDF 1.7 Table 349), mirroring Krilla's
/// `TableHeaderScope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableHeaderScope {
    Row,
    Column,
    Both,
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
            numbering: ListNumbering::Disc,
        }),
        "ol" => Some(PdfTag::L {
            numbering: ListNumbering::Decimal,
        }),
        "li" => Some(PdfTag::Li),
        "table" => Some(PdfTag::Table),
        "thead" => Some(PdfTag::THead),
        "tbody" => Some(PdfTag::TBody),
        "tfoot" => Some(PdfTag::TFoot),
        "tr" => Some(PdfTag::Tr),
        "th" => Some(PdfTag::Th {
            scope: TableHeaderScope::Both,
        }),
        "td" => Some(PdfTag::Td),
        _ => None,
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
                scope: TableHeaderScope::Both
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
                    numbering: ListNumbering::Disc
                }
            )
            .contains("Disc"),
            "PdfTag::L debug must include numbering variant name"
        );
        assert!(
            format!(
                "{:?}",
                PdfTag::Th {
                    scope: TableHeaderScope::Column
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
            numbering: ListNumbering::Decimal,
        };
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn pdf_tag_clone_th_variant() {
        let original = PdfTag::Th {
            scope: TableHeaderScope::Row,
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
                    numbering: ListNumbering::Disc
                }
            )
            .contains("L")
        );
        assert!(
            format!(
                "{:?}",
                PdfTag::Th {
                    scope: TableHeaderScope::Column
                }
            )
            .contains("Th")
        );
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
                numbering: ListNumbering::Disc
            },
            PdfTag::L {
                numbering: ListNumbering::Decimal
            }
        );
    }

    #[test]
    fn pdf_tag_partial_eq_th_scope_inequality() {
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
                numbering: ListNumbering::Disc
            },
            PdfTag::Li
        );
        assert_ne!(
            PdfTag::Th {
                scope: TableHeaderScope::Both
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
