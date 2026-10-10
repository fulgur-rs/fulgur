use fulgur_core::{Result, units::PX_TO_PT};
use krilla::{
    action::{Action, LinkAction},
    annotation::{Annotation, LinkAnnotation, Target},
    destination::XyzDestination,
    geom::{Point, Quadrilateral, Rect},
};
use percent_encoding::percent_decode_str;
use raikiri_html::{DocumentLayout, Page, PaintRect};

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

/// Where a page's links land on the PDF page: `page` itself, or a running
/// element layout drawn at `origin` and clipped to a margin box.
#[derive(Clone, Copy)]
pub(super) struct Placement {
    pub(super) origin: (f32, f32),
    pub(super) clip: Option<PaintRect>,
}

impl Placement {
    pub(super) const PAGE: Self = Self {
        origin: (0.0, 0.0),
        clip: None,
    };

    /// `quad` moved to the PDF page and cut to the clip, `None` when
    /// nothing of it is visible.
    fn place(self, quad: PaintRect) -> Option<PaintRect> {
        let mut x0 = quad.x + self.origin.0;
        let mut y0 = quad.y + self.origin.1;
        let mut x1 = x0 + quad.width;
        let mut y1 = y0 + quad.height;
        if let Some(clip) = self.clip {
            x0 = x0.max(clip.x);
            y0 = y0.max(clip.y);
            x1 = x1.min(clip.x + clip.width);
            y1 = y1.min(clip.y + clip.height);
        }
        (x1 > x0 && y1 > y0).then(|| PaintRect::new(x0, y0, x1 - x0, y1 - y0))
    }
}

pub(super) fn annotations(
    document: &DocumentLayout,
    page: &Page<'_>,
    placement: Placement,
    document_url: &url::Url,
) -> Result<Vec<Annotation>> {
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
            let Some(quad) = placement.place(*quad) else {
                continue;
            };
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
        annotations.push(Annotation::new_link(annotation, None));
    }
    Ok(annotations)
}

#[cfg(test)]
mod tests {
    use super::Placement;
    use raikiri_html::PaintRect;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> PaintRect {
        PaintRect::new(x, y, width, height)
    }

    #[test]
    fn placed_quads_move_by_the_origin_and_are_cut_to_the_clip() {
        let placement = Placement {
            origin: (10.0, 20.0),
            clip: Some(rect(0.0, 0.0, 40.0, 30.0)),
        };
        assert_eq!(
            placement.place(rect(5.0, 5.0, 50.0, 50.0)),
            Some(rect(15.0, 25.0, 25.0, 5.0))
        );
        assert_eq!(placement.place(rect(40.0, 0.0, 10.0, 10.0)), None);
        assert_eq!(
            Placement::PAGE.place(rect(1.0, 2.0, 3.0, 4.0)),
            Some(rect(1.0, 2.0, 3.0, 4.0))
        );
    }
}
