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
/// (`TableHeaderScope`, alt text, heading title) —
/// those flow from the DOM at render time once the wire-up lands.
/// `ListNumbering` is carried here because `ul`/`ol` distinction is
/// known at classify time from the element local name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfTag {
    P,
    H { level: u8 },
    Div,
    Span,
    Figure,
    L { numbering: krilla::tagging::ListNumbering },
    Lbl,
    LBody,
    Li,
    Table,
    TRowGroup,
    Tr,
    Th,
    Td,
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
/// `1..=6`. `<thead>` / `<tbody>` / `<tfoot>` collapse into
/// `TRowGroup`; render-side may emit them as `Tag::TBody` etc. when the
/// distinction matters for PDF/UA.
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
        "ul" => Some(PdfTag::L { numbering: krilla::tagging::ListNumbering::Disc }),
        "ol" => Some(PdfTag::L { numbering: krilla::tagging::ListNumbering::Decimal }),
        "li" => Some(PdfTag::Li),
        "table" => Some(PdfTag::Table),
        "thead" | "tbody" | "tfoot" => Some(PdfTag::TRowGroup),
        "tr" => Some(PdfTag::Tr),
        "th" => Some(PdfTag::Th),
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
        PdfTag::L { numbering } => {
            krilla::tagging::Tag::L(*numbering).into()
        }
        PdfTag::Lbl => krilla::tagging::Tag::<krilla::tagging::kind::Lbl>::Lbl.into(),
        PdfTag::LBody => krilla::tagging::Tag::<krilla::tagging::kind::LBody>::LBody.into(),
        PdfTag::Li => krilla::tagging::Tag::<krilla::tagging::kind::LI>::LI.into(),
        PdfTag::Table => krilla::tagging::Tag::<krilla::tagging::kind::Table>::Table.into(),
        PdfTag::TRowGroup => krilla::tagging::Tag::<krilla::tagging::kind::TBody>::TBody.into(),
        PdfTag::Tr => krilla::tagging::Tag::<krilla::tagging::kind::TR>::TR.into(),
        PdfTag::Th => {
            krilla::tagging::Tag::TH(krilla::tagging::TableHeaderScope::Both).into() // scope attr: fulgur-izp.8
        }
        PdfTag::Td => krilla::tagging::Tag::<krilla::tagging::kind::TD>::TD.into(),
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
            Some(PdfTag::L { numbering: ListNumbering::Disc })
        );
        assert_eq!(
            classify_element("ol"),
            Some(PdfTag::L { numbering: ListNumbering::Decimal })
        );
        assert_eq!(classify_element("li"), Some(PdfTag::Li));
        assert_eq!(classify_element("table"), Some(PdfTag::Table));
        assert_eq!(classify_element("thead"), Some(PdfTag::TRowGroup));
        assert_eq!(classify_element("tbody"), Some(PdfTag::TRowGroup));
        assert_eq!(classify_element("tfoot"), Some(PdfTag::TRowGroup));
        assert_eq!(classify_element("tr"), Some(PdfTag::Tr));
        assert_eq!(classify_element("th"), Some(PdfTag::Th));
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
                &PdfTag::L { numbering: krilla::tagging::ListNumbering::Disc },
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
            pdf_tag_to_krilla_tag(&PdfTag::TRowGroup, None, None),
            TagKind::TBody(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Tr, None, None),
            TagKind::TR(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Th, None, None),
            TagKind::TH(_)
        ));
        assert!(matches!(
            pdf_tag_to_krilla_tag(&PdfTag::Td, None, None),
            TagKind::TD(_)
        ));
    }
}
