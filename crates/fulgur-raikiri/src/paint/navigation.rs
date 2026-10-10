use fulgur_core::{Result, units::PX_TO_PT};
use krilla::{
    action::{Action, LinkAction},
    annotation::{LinkAnnotation, Target},
    destination::XyzDestination,
    geom::{Point, Quadrilateral, Rect},
};
use percent_encoding::percent_decode_str;
use raikiri_html::{DocumentLayout, NodeId, Page};

enum LinkTarget {
    Internal(XyzDestination),
    External(String),
}

impl LinkTarget {
    fn annotation_target(&self) -> Target {
        Target::Action(match self {
            Self::Internal(destination) => Action::Goto(destination.clone().into()),
            Self::External(uri) => LinkAction::new(uri.clone()).into(),
        })
    }
}

fn anchor_target(document: &DocumentLayout, fragment: &str) -> Option<LinkTarget> {
    let fragment = percent_decode_str(fragment).decode_utf8().ok()?;
    let anchor = document.anchors().get(&fragment)?;
    if !anchor.point.0.is_finite() || !anchor.point.1.is_finite() {
        return None;
    }
    Some(LinkTarget::Internal(XyzDestination::new(
        anchor.page_index as usize,
        Point::from_xy(anchor.point.0 * PX_TO_PT, anchor.point.1 * PX_TO_PT),
    )))
}

fn target(document: &DocumentLayout, document_url: &url::Url, href: &str) -> Option<LinkTarget> {
    let uri = document
        .base_url()
        .map_or_else(|| url::Url::parse(href), |base| base.join(href))
        .ok()?;
    if let Some(fragment) = uri.fragment() {
        let mut original_url = document_url.clone();
        original_url.set_fragment(None);
        let mut target_url = uri.clone();
        target_url.set_fragment(None);
        if original_url == target_url {
            return anchor_target(document, fragment);
        }
    }
    Some(LinkTarget::External(uri.into()))
}

/// The link annotations of `page`, each with its `<a>` element.
pub(super) fn links(
    document: &DocumentLayout,
    page: &Page<'_>,
    document_url: &url::Url,
) -> Result<Vec<(NodeId, LinkAnnotation)>> {
    let mut annotations = Vec::new();
    for link in page.links() {
        if link.target.is_empty() {
            continue;
        }
        let Some(target) = target(document, document_url, link.target) else {
            continue;
        };
        let mut rects = Vec::new();
        for quad in link.quads {
            if ![quad.x, quad.y, quad.width, quad.height]
                .iter()
                .all(|value| value.is_finite())
                || quad.width <= 0.0
                || quad.height <= 0.0
            {
                continue;
            }
            let Some(rect) = Rect::from_xywh(
                quad.x * PX_TO_PT,
                quad.y * PX_TO_PT,
                quad.width * PX_TO_PT,
                quad.height * PX_TO_PT,
            ) else {
                continue;
            };
            rects.push(rect);
        }
        let annotation = match rects.as_slice() {
            [] => continue,
            [rect] => LinkAnnotation::new(*rect, target.annotation_target()),
            _ => LinkAnnotation::new_with_quad_points(
                rects.into_iter().map(Quadrilateral::from).collect(),
                target.annotation_target(),
            ),
        };
        annotations.push((link.owner, annotation));
    }
    Ok(annotations)
}
