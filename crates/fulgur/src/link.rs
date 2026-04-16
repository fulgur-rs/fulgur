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
