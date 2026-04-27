//! PDF link annotation emission.
//!
//! Bridges fulgur's in-memory `LinkOccurrence` records (captured per-page by
//! `LinkCollector` during `draw`) to krilla's `LinkAnnotation` API. One
//! annotation is emitted per occurrence; multiple rects on the same
//! occurrence (a link broken across lines) become a single annotation with
//! multiple `quad_points`.
//!
//! Internal anchors (`href="#foo"`) are resolved against a
//! `DestinationRegistry` built from the paginated page tree. Unresolved
//! anchors are logged to stderr and skipped — they are a content error, not
//! a rendering error.

use krilla::action::{Action, LinkAction};
use krilla::annotation::{Annotation, LinkAnnotation, Target};
use krilla::destination::{Destination, XyzDestination};
use krilla::geom::{Point, Quadrilateral};
use krilla::page::Page;

use crate::pageable::{DestinationRegistry, LinkOccurrence};
use crate::paragraph::LinkTarget;

/// Emit PDF link annotations for every occurrence on the given page.
///
/// `occurrences` must already be filtered to the page represented by `page`.
/// Internal anchors that cannot be resolved in `registry` are logged via
/// `eprintln!` and skipped; rendering continues.
pub(crate) fn emit_link_annotations(
    page: &mut Page,
    occurrences: &[LinkOccurrence],
    registry: &DestinationRegistry,
) {
    for occ in occurrences {
        let target = match &occ.target {
            LinkTarget::External(uri) => {
                Target::Action(Action::Link(LinkAction::new(uri.as_str().to_string())))
            }
            LinkTarget::Internal(id) => match registry.get(id.as_str()) {
                Some((page_idx, x_pt, y_pt)) => {
                    // x and y are in page-local (top-down) coordinates;
                    // krilla flips to PDF bottom-up during serialization.
                    let dest = XyzDestination::new(page_idx, Point::from_xy(x_pt, y_pt));
                    Target::Destination(Destination::Xyz(dest))
                }
                None => {
                    eprintln!("fulgur: unresolved internal anchor #{id}");
                    continue;
                }
            },
        };

        let quads: Vec<Quadrilateral> = occ.quads.iter().map(|q| q.to_krilla()).collect();
        if quads.is_empty() {
            continue;
        }

        let link_ann = LinkAnnotation::new_with_quad_points(quads, target);
        let annotation = Annotation::new_link(link_ann, occ.alt_text.clone());
        page.add_annotation(annotation);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::pageable::{DestinationRegistry, LinkOccurrence, Quad};
    use crate::paragraph::LinkTarget;

    use super::emit_link_annotations;

    fn page_settings() -> krilla::page::PageSettings {
        krilla::page::PageSettings::from_wh(595.0, 842.0).unwrap()
    }

    fn make_quad(x: f32, y: f32, w: f32, h: f32) -> Quad {
        // bottom-left → bottom-right → top-right → top-left (Y-down)
        Quad {
            points: [[x, y + h], [x + w, y + h], [x + w, y], [x, y]],
        }
    }

    fn ext_occ(url: &str, quads: Vec<Quad>) -> LinkOccurrence {
        LinkOccurrence {
            page_idx: 0,
            target: LinkTarget::External(Arc::new(url.to_string())),
            alt_text: None,
            quads,
        }
    }

    fn int_occ(id: &str, quads: Vec<Quad>) -> LinkOccurrence {
        LinkOccurrence {
            page_idx: 0,
            target: LinkTarget::Internal(Arc::new(id.to_string())),
            alt_text: None,
            quads,
        }
    }

    /// Serialize the finished document and count `/Annots` entries on page 0.
    ///
    /// lopdf is already a direct dependency of the fulgur crate, so this can be
    /// used in unit tests without adding any new dependencies.
    ///
    /// Panics on unexpected PDF structural shapes so tests fail loudly rather
    /// than silently returning 0 and hiding regressions.
    fn page0_annotation_count(doc: krilla::Document) -> usize {
        let bytes = doc.finish().unwrap();
        let pdf = lopdf::Document::load_mem(&bytes).unwrap();
        let page_id = pdf.page_iter().next().expect("PDF produced no pages");
        let page_obj = pdf.get_object(page_id).unwrap();
        let page_dict = match page_obj {
            lopdf::Object::Dictionary(d) => d,
            other => panic!("page 0 object is not a dictionary: {other:?}"),
        };
        // Absent /Annots key is the legitimate "no annotations on this page" case.
        let annots_obj = match page_dict.get(b"Annots") {
            Ok(obj) => obj,
            Err(_) => return 0,
        };
        // /Annots may be a direct array or an indirect reference.
        match annots_obj {
            lopdf::Object::Array(arr) => arr.len(),
            lopdf::Object::Reference(r) => match pdf.get_object(*r) {
                Ok(lopdf::Object::Array(arr)) => arr.len(),
                Ok(other) => panic!("Annots reference resolves to non-array: {other:?}"),
                Err(e) => panic!("failed to dereference Annots: {e}"),
            },
            other => panic!("Annots has unexpected type: {other:?}"),
        }
    }

    // ── empty occurrences ──────────────────────────────────────────────────

    #[test]
    fn empty_occurrences_produces_no_annotations() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let registry = DestinationRegistry::new();
            emit_link_annotations(&mut page, &[], &registry);
        }
        assert_eq!(page0_annotation_count(doc), 0);
    }

    // ── external links ─────────────────────────────────────────────────────

    #[test]
    fn external_link_single_quad_emits_one_annotation() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let registry = DestinationRegistry::new();
            let occ = ext_occ(
                "https://example.com",
                vec![make_quad(10.0, 20.0, 80.0, 14.0)],
            );
            emit_link_annotations(&mut page, &[occ], &registry);
        }
        assert_eq!(page0_annotation_count(doc), 1);
    }

    #[test]
    fn external_link_with_alt_text_emits_one_annotation() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let registry = DestinationRegistry::new();
            let occ = LinkOccurrence {
                page_idx: 0,
                target: LinkTarget::External(Arc::new("https://alt.example".to_string())),
                alt_text: Some("Visit example".to_string()),
                quads: vec![make_quad(0.0, 0.0, 100.0, 12.0)],
            };
            emit_link_annotations(&mut page, &[occ], &registry);
        }
        assert_eq!(page0_annotation_count(doc), 1);
    }

    #[test]
    fn external_link_multi_quad_emits_one_annotation() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let registry = DestinationRegistry::new();
            // Two quads for a link wrapping across lines — still one occurrence, one annotation.
            let occ = ext_occ(
                "https://long.example",
                vec![
                    make_quad(0.0, 0.0, 200.0, 14.0),
                    make_quad(0.0, 14.0, 150.0, 14.0),
                ],
            );
            emit_link_annotations(&mut page, &[occ], &registry);
        }
        assert_eq!(page0_annotation_count(doc), 1);
    }

    // ── internal links ─────────────────────────────────────────────────────

    #[test]
    fn internal_link_resolved_emits_one_annotation() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let mut registry = DestinationRegistry::new();
            // Use page 0 so the destination is valid within this single-page document.
            registry.set_current_page(0);
            registry.record("section1", 0.0, 120.0);
            let occ = int_occ("section1", vec![make_quad(10.0, 40.0, 80.0, 12.0)]);
            emit_link_annotations(&mut page, &[occ], &registry);
        }
        assert_eq!(page0_annotation_count(doc), 1);
    }

    #[test]
    fn internal_link_unresolved_emits_no_annotation() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let registry = DestinationRegistry::new(); // "missing" is not registered
            let occ = int_occ("missing", vec![make_quad(0.0, 0.0, 50.0, 12.0)]);
            // eprintln! log is emitted; the occurrence must be skipped entirely.
            emit_link_annotations(&mut page, &[occ], &registry);
        }
        assert_eq!(page0_annotation_count(doc), 0);
    }

    // ── empty-quads guard ──────────────────────────────────────────────────

    #[test]
    fn occurrence_with_empty_quads_emits_no_annotation() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let registry = DestinationRegistry::new();
            let occ = ext_occ("https://no-quads.example", vec![]);
            emit_link_annotations(&mut page, &[occ], &registry);
        }
        assert_eq!(page0_annotation_count(doc), 0);
    }

    #[test]
    fn empty_quads_does_not_suppress_later_valid_occurrences() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let registry = DestinationRegistry::new();
            let occs = vec![
                ext_occ("https://first.example", vec![]),
                ext_occ(
                    "https://second.example",
                    vec![make_quad(0.0, 0.0, 80.0, 12.0)],
                ),
            ];
            emit_link_annotations(&mut page, &occs, &registry);
        }
        // First occurrence has empty quads (skipped); second is valid → 1 annotation.
        assert_eq!(page0_annotation_count(doc), 1);
    }

    // ── mixed occurrences ─────────────────────────────────────────────────

    #[test]
    fn mixed_occurrences_skips_unresolved_and_empty_quads() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let mut registry = DestinationRegistry::new();
            registry.set_current_page(0);
            registry.record("anchor", 0.0, 300.0);
            let occs = vec![
                ext_occ("https://a.example", vec![make_quad(0.0, 0.0, 60.0, 12.0)]), // emitted
                int_occ("anchor", vec![make_quad(0.0, 20.0, 60.0, 12.0)]), // emitted (resolved)
                int_occ("gone", vec![make_quad(0.0, 40.0, 60.0, 12.0)]),   // skipped (unresolved)
                ext_occ("https://empty.example", vec![]),                  // skipped (empty quads)
            ];
            emit_link_annotations(&mut page, &occs, &registry);
        }
        assert_eq!(page0_annotation_count(doc), 2);
    }
}
