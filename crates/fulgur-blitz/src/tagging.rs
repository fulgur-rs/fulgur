//! Tagged PDF for the Blitz backend: the shared HTML classification from
//! [`fulgur_core::tagging`], plus its conversion to this crate's Krilla
//! version.

pub use fulgur_core::tagging::*;

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
        PdfTag::L { numbering } => krilla::tagging::Tag::L(list_numbering(*numbering)).into(),
        PdfTag::Lbl => krilla::tagging::Tag::<krilla::tagging::kind::Lbl>::Lbl.into(),
        PdfTag::LBody => krilla::tagging::Tag::<krilla::tagging::kind::LBody>::LBody.into(),
        PdfTag::Li => krilla::tagging::Tag::<krilla::tagging::kind::LI>::LI.into(),
        PdfTag::Table => krilla::tagging::Tag::<krilla::tagging::kind::Table>::Table.into(),
        PdfTag::THead => krilla::tagging::Tag::<krilla::tagging::kind::THead>::THead.into(),
        PdfTag::TBody => krilla::tagging::Tag::<krilla::tagging::kind::TBody>::TBody.into(),
        PdfTag::TFoot => krilla::tagging::Tag::<krilla::tagging::kind::TFoot>::TFoot.into(),
        PdfTag::Tr => krilla::tagging::Tag::<krilla::tagging::kind::TR>::TR.into(),
        PdfTag::Th { scope } => krilla::tagging::Tag::TH(header_scope(*scope)).into(),
        PdfTag::Td => krilla::tagging::Tag::<krilla::tagging::kind::TD>::TD.into(),
        PdfTag::Link => krilla::tagging::Tag::<krilla::tagging::kind::Link>::Link.into(),
    }
}

fn list_numbering(numbering: ListNumbering) -> krilla::tagging::ListNumbering {
    use krilla::tagging::ListNumbering as K;
    match numbering {
        ListNumbering::None => K::None,
        ListNumbering::Disc => K::Disc,
        ListNumbering::Circle => K::Circle,
        ListNumbering::Square => K::Square,
        ListNumbering::Decimal => K::Decimal,
        ListNumbering::LowerRoman => K::LowerRoman,
        ListNumbering::UpperRoman => K::UpperRoman,
        ListNumbering::LowerAlpha => K::LowerAlpha,
        ListNumbering::UpperAlpha => K::UpperAlpha,
    }
}

fn header_scope(scope: TableHeaderScope) -> krilla::tagging::TableHeaderScope {
    use krilla::tagging::TableHeaderScope as K;
    match scope {
        TableHeaderScope::Row => K::Row,
        TableHeaderScope::Column => K::Column,
        TableHeaderScope::Both => K::Both,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                numbering: ListNumbering::Decimal,
            },
            None,
            None,
        );
        assert!(matches!(k, krilla::tagging::TagKind::L(_)));
    }

    #[test]
    fn pdf_tag_to_krilla_tag_th_scope_variants() {
        use krilla::tagging::TagKind;
        for scope in [
            TableHeaderScope::Row,
            TableHeaderScope::Column,
            TableHeaderScope::Both,
        ] {
            let k = pdf_tag_to_krilla_tag(&PdfTag::Th { scope }, None, None);
            assert!(matches!(k, TagKind::TH(_)), "scope = {scope:?}");
            if let TagKind::TH(tag) = k {
                assert_eq!(tag.scope(), header_scope(scope), "scope = {scope:?}");
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
                    numbering: ListNumbering::Disc
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
                    scope: TableHeaderScope::Both
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
}
