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
/// (`ListNumbering`, `TableHeaderScope`, alt text, heading title) —
/// those flow from the DOM at render time once the wire-up lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfTag {
    P,
    H { level: u8 },
    Div,
    Span,
    Figure,
    L,
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
        "ul" | "ol" => Some(PdfTag::L),
        "li" => Some(PdfTag::Li),
        "table" => Some(PdfTag::Table),
        "thead" | "tbody" | "tfoot" => Some(PdfTag::TRowGroup),
        "tr" => Some(PdfTag::Tr),
        "th" => Some(PdfTag::Th),
        "td" => Some(PdfTag::Td),
        _ => None,
    }
}

/// Convert a fulgur `PdfTag` to the corresponding Krilla `TagKind`.
///
/// Called by `render_v2` when building the flat TagTree after all pages
/// are drawn. Only P / H{n} / Span are supported in fulgur-izp.4; all
/// other variants fall back to `Tag::P` as a safe placeholder until later
/// issues wire lists, tables, and figures.
pub fn pdf_tag_to_krilla_tag(tag: &PdfTag) -> krilla::tagging::TagKind {
    use std::num::NonZeroU16;
    match tag {
        PdfTag::P => krilla::tagging::Tag::<krilla::tagging::kind::P>::P.into(),
        PdfTag::H { level } => {
            let level = NonZeroU16::new((*level).max(1) as u16)
                .unwrap_or_else(|| NonZeroU16::new(1).unwrap());
            krilla::tagging::Tag::Hn(level, None).into()
        }
        PdfTag::Span => krilla::tagging::Tag::<krilla::tagging::kind::Span>::Span.into(),
        _ => krilla::tagging::Tag::<krilla::tagging::kind::P>::P.into(),
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
        assert_eq!(classify_element("ul"), Some(PdfTag::L));
        assert_eq!(classify_element("ol"), Some(PdfTag::L));
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
        let k = pdf_tag_to_krilla_tag(&PdfTag::P);
        assert!(matches!(k, krilla::tagging::TagKind::P(_)));
    }

    #[test]
    fn pdf_tag_to_krilla_tag_headings() {
        for level in 1u8..=6 {
            let k = pdf_tag_to_krilla_tag(&PdfTag::H { level });
            assert!(
                matches!(k, krilla::tagging::TagKind::Hn(_)),
                "level={level}"
            );
        }
    }

    #[test]
    fn pdf_tag_to_krilla_tag_span() {
        let k = pdf_tag_to_krilla_tag(&PdfTag::Span);
        assert!(matches!(k, krilla::tagging::TagKind::Span(_)));
    }
}
