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
use krilla::tagging::Identifier;

use crate::draw_primitives::{DestinationRegistry, LinkOccurrence, Rect};
use crate::paragraph::LinkTarget;

/// Clip each occurrence's quads to `content_area` (in Y-down PDF pt),
/// dropping any quad that falls entirely outside and dropping any
/// occurrence whose quads all disappear.
///
/// This runs on body-collected occurrences before emission. `render_v2`
/// paints `@page` margin boxes AFTER body content, so a body link
/// annotation that extends into a margin-box strip would remain
/// clickable under the margin-box content that visually replaces it —
/// click target and visible text disagree. Clipping to the content
/// area keeps the click target inside the region body content actually
/// owns. Margin-box occurrences are collected via a separate
/// `LinkCollector` and emitted unclipped.
pub(crate) fn clip_body_occurrences_to_content_area(
    occurrences: Vec<LinkOccurrence>,
    content_area: &Rect,
) -> Vec<LinkOccurrence> {
    let mut out = Vec::with_capacity(occurrences.len());
    for mut occ in occurrences {
        occ.quads = occ
            .quads
            .into_iter()
            .filter_map(|q| q.clip_to_rect(content_area))
            .collect();
        if !occ.quads.is_empty() {
            out.push(occ);
        }
    }
    out
}

/// Emit PDF link annotations for every occurrence on the given page.
///
/// `occurrences` must already be filtered to the page represented by `page`.
/// Internal anchors that cannot be resolved in `registry` are logged via
/// `eprintln!` and skipped; rendering continues.
///
/// When `wired_span_ptrs` is `Some`, `add_tagged_annotation` is used for
/// occurrences whose `span_ptr` is in the set (meaning a corresponding
/// `LinkContent` run entry exists in the struct tree).  Occurrences that are
/// not wired — e.g. links whose content rendered as `LineItem::InlineBox`
/// rather than `LineItem::Text`/`LineItem::Image` — fall back to plain
/// `add_annotation` so the click target is preserved without violating
/// Krilla's invariant (every tagged annotation must appear in the tag tree).
/// When `wired_span_ptrs` is `None`, all annotations are added untagged and
/// an empty `Vec` is returned.
pub(crate) fn emit_link_annotations(
    page: &mut Page,
    occurrences: &[LinkOccurrence],
    registry: &DestinationRegistry,
    wired_span_ptrs: Option<&std::collections::BTreeSet<usize>>,
) -> Vec<(usize, Identifier)> {
    let mut annot_ids = Vec::new();

    for occ in occurrences {
        let target = match &occ.target {
            LinkTarget::External(uri) => {
                Target::Action(Action::Link(LinkAction::new(uri.as_str().to_string())))
            }
            LinkTarget::Internal(id) => match registry.get(id.as_str()) {
                Some((page_idx, x_pt, y_pt)) => {
                    // x and y are in page-local (top-down) coordinates;
                    // krilla flips to PDF bottom-up during serialization.
                    let dest =
                        XyzDestination::new(page_idx, Point::from_xy(x_pt.to_f32(), y_pt.to_f32()));
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
        // Use add_tagged_annotation only when the span_ptr is wired into the
        // struct tree via a ParagraphRunItem::LinkContent entry. Unwired
        // occurrences (e.g. image-only links rendered as LineItem::InlineBox)
        // fall back to add_annotation so the click target is preserved without
        // violating Krilla's invariant that every tagged annotation must appear
        // in the tag tree. Link struct element emission for InlineBox links is
        // a follow-up task.
        let is_wired = wired_span_ptrs.is_some_and(|set| set.contains(&occ.span_ptr));
        if is_wired {
            let annot_id = page.add_tagged_annotation(annotation);
            annot_ids.push((occ.span_ptr, annot_id));
        } else {
            page.add_annotation(annotation);
        }
    }

    annot_ids
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::draw_primitives::{DestinationRegistry, LinkOccurrence, Quad};
    use crate::paragraph::LinkTarget;
    use crate::units::F32Units;

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
            span_ptr: 0,
        }
    }

    fn int_occ(id: &str, quads: Vec<Quad>) -> LinkOccurrence {
        LinkOccurrence {
            page_idx: 0,
            target: LinkTarget::Internal(Arc::new(id.to_string())),
            alt_text: None,
            quads,
            span_ptr: 0,
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
            emit_link_annotations(&mut page, &[], &registry, None);
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
            emit_link_annotations(&mut page, &[occ], &registry, None);
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
                span_ptr: 0,
            };
            emit_link_annotations(&mut page, &[occ], &registry, None);
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
            emit_link_annotations(&mut page, &[occ], &registry, None);
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
            registry.record("section1", 0.0.as_pt(), 120.0.as_pt());
            let occ = int_occ("section1", vec![make_quad(10.0, 40.0, 80.0, 12.0)]);
            emit_link_annotations(&mut page, &[occ], &registry, None);
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
            emit_link_annotations(&mut page, &[occ], &registry, None);
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
            emit_link_annotations(&mut page, &[occ], &registry, None);
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
            emit_link_annotations(&mut page, &occs, &registry, None);
        }
        // First occurrence has empty quads (skipped); second is valid → 1 annotation.
        assert_eq!(page0_annotation_count(doc), 1);
    }

    // ── tagging mode ──────────────────────────────────────────────────────

    #[test]
    fn non_tagged_emit_returns_empty_vec() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let registry = DestinationRegistry::new();
            let occ = ext_occ("https://x.example", vec![make_quad(0.0, 0.0, 50.0, 12.0)]);
            let ids = emit_link_annotations(&mut page, &[occ], &registry, None);
            assert!(ids.is_empty());
        }
        assert_eq!(page0_annotation_count(doc), 1);
    }

    // ── content-area clipping (body annotations) ──────────────────────────
    //
    // `render_v2` paints `@page` margin boxes AFTER body content so page
    // headers/footers are not hidden by body backgrounds. Because PDF link
    // annotations are independent interactive objects, a body link that
    // happens to fall inside a margin-box strip stays clickable even
    // though the margin box now paints over it visually — the click
    // target and the visible text disagree. Body occurrences must
    // therefore be clipped to the content area before emission;
    // margin-box occurrences are collected separately and emitted
    // unclipped.

    fn a4_content_area() -> crate::draw_primitives::Rect {
        // A4 595x842pt with 60pt top/bottom, 40pt left/right margins.
        crate::draw_primitives::Rect {
            x: 40.0.as_pt(),
            y: 60.0.as_pt(),
            width: 515.0.as_pt(),
            height: 722.0.as_pt(),
        }
    }

    #[test]
    fn clip_body_drops_occurrence_entirely_in_bottom_margin() {
        // Body anchor positioned exactly where the `@bottom-*` margin
        // box will paint — after clipping there is no residual quad, so
        // the occurrence is dropped and no annotation reaches the PDF.
        let area = a4_content_area();
        let occs = vec![ext_occ(
            "https://example.com/target",
            // y range [800, 820] — below the 782 content-area bottom.
            vec![Quad {
                points: [
                    [100.0, 820.0],
                    [250.0, 820.0],
                    [250.0, 800.0],
                    [100.0, 800.0],
                ],
            }],
        )];
        let clipped = crate::link::clip_body_occurrences_to_content_area(occs, &area);
        assert!(
            clipped.is_empty(),
            "body link fully inside footer strip must be dropped, got {clipped:?}"
        );
    }

    #[test]
    fn clip_body_clamps_straddling_quad_to_content_area() {
        // Body link crossing the bottom boundary (y in [770, 800],
        // area bottom = 782) has its clickable strip clamped to
        // y in [770, 782] so the click target no longer overlaps the
        // margin-box strip.
        let area = a4_content_area();
        let occs = vec![ext_occ(
            "https://body.example",
            vec![Quad {
                points: [
                    [100.0, 800.0],
                    [250.0, 800.0],
                    [250.0, 770.0],
                    [100.0, 770.0],
                ],
            }],
        )];
        let clipped = crate::link::clip_body_occurrences_to_content_area(occs, &area);
        assert_eq!(clipped.len(), 1);
        assert_eq!(clipped[0].quads.len(), 1);
        let ys: Vec<f32> = clipped[0].quads[0].points.iter().map(|p| p[1]).collect();
        let max_y = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!((max_y - 782.0).abs() < 1e-4);
    }

    #[test]
    fn clip_body_preserves_multi_quad_link_with_partial_overlap() {
        // A multi-quad link (e.g. a wrapping <a>) with one quad in the
        // content area and one in the margin strip: keep the first, drop
        // the second, so the visible-and-clickable portion survives.
        let area = a4_content_area();
        let occs = vec![ext_occ(
            "https://wrap.example",
            vec![
                // Inside content area.
                Quad {
                    points: [
                        [100.0, 214.0],
                        [250.0, 214.0],
                        [250.0, 200.0],
                        [100.0, 200.0],
                    ],
                },
                // Bottom margin strip.
                Quad {
                    points: [
                        [100.0, 820.0],
                        [250.0, 820.0],
                        [250.0, 800.0],
                        [100.0, 800.0],
                    ],
                },
            ],
        )];
        let clipped = crate::link::clip_body_occurrences_to_content_area(occs, &area);
        assert_eq!(clipped.len(), 1);
        assert_eq!(
            clipped[0].quads.len(),
            1,
            "second (in-margin) quad must be dropped"
        );
    }

    #[test]
    fn clip_body_preserves_occurrence_entirely_inside() {
        let area = a4_content_area();
        let occs = vec![ext_occ(
            "https://legit.example",
            vec![Quad {
                points: [
                    [100.0, 214.0],
                    [250.0, 214.0],
                    [250.0, 200.0],
                    [100.0, 200.0],
                ],
            }],
        )];
        let before = occs.clone();
        let clipped = crate::link::clip_body_occurrences_to_content_area(occs, &area);
        assert_eq!(clipped.len(), 1);
        assert_eq!(clipped[0].quads, before[0].quads);
    }

    #[test]
    fn body_link_in_margin_area_emits_no_annotation_via_emit_after_clip() {
        // End-to-end at the emit layer: a body occurrence positioned in
        // the bottom margin, run through the clip → emit pipeline, must
        // produce zero /Annots entries on the resulting page.
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let registry = DestinationRegistry::new();
            let area = a4_content_area();
            let body_occs = vec![ext_occ(
                "https://example.com/target",
                vec![Quad {
                    points: [
                        [100.0, 820.0],
                        [250.0, 820.0],
                        [250.0, 800.0],
                        [100.0, 800.0],
                    ],
                }],
            )];
            let clipped = crate::link::clip_body_occurrences_to_content_area(body_occs, &area);
            emit_link_annotations(&mut page, &clipped, &registry, None);
        }
        assert_eq!(page0_annotation_count(doc), 0);
    }

    // ── mixed occurrences ─────────────────────────────────────────────────

    #[test]
    fn mixed_occurrences_skips_unresolved_and_empty_quads() {
        let mut doc = krilla::Document::new();
        {
            let mut page = doc.start_page_with(page_settings());
            let mut registry = DestinationRegistry::new();
            registry.set_current_page(0);
            registry.record("anchor", 0.0.as_pt(), 300.0.as_pt());
            let occs = vec![
                ext_occ("https://a.example", vec![make_quad(0.0, 0.0, 60.0, 12.0)]), // emitted
                int_occ("anchor", vec![make_quad(0.0, 20.0, 60.0, 12.0)]), // emitted (resolved)
                int_occ("gone", vec![make_quad(0.0, 40.0, 60.0, 12.0)]),   // skipped (unresolved)
                ext_occ("https://empty.example", vec![]),                  // skipped (empty quads)
            ];
            emit_link_annotations(&mut page, &occs, &registry, None);
        }
        assert_eq!(page0_annotation_count(doc), 2);
    }
}
