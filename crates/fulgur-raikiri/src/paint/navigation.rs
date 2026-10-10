use fulgur_core::units::PX_TO_PT;
use krilla::{
    action::{Action, LinkAction},
    annotation::{Annotation, LinkAnnotation, Target},
    destination::XyzDestination,
    geom::{Point, Quadrilateral, Rect},
};
use percent_encoding::percent_decode_str;
use raikiri_html::{AnchorIndex, Page};

/// Where a link on a page points.
pub(crate) enum Href {
    /// An anchor of the document itself, by its percent-decoded name.
    Internal(String),
    /// Any other URL.
    External(String),
}

/// One link of a page: its target and its areas in PDF pt.
pub(crate) struct PageLink {
    pub(crate) href: Href,
    pub(crate) rects: Vec<Rect>,
}

impl PageLink {
    /// The link annotation for an external link.
    pub(crate) fn external(&self) -> Option<Annotation> {
        match &self.href {
            Href::External(uri) => Some(annotation(
                &self.rects,
                Target::Action(LinkAction::new(uri.clone()).into()),
            )),
            Href::Internal(_) => None,
        }
    }

    /// The link annotation for an internal link, if `anchors` has its
    /// destination.
    pub(crate) fn internal(&self, anchors: &AnchorIndex) -> Option<Annotation> {
        let Href::Internal(name) = &self.href else {
            return None;
        };
        let anchor = anchors.get(name)?;
        if !anchor.point.0.is_finite() || !anchor.point.1.is_finite() {
            return None;
        }
        let destination = XyzDestination::new(
            anchor.page_index as usize,
            Point::from_xy(anchor.point.0 * PX_TO_PT, anchor.point.1 * PX_TO_PT),
        );
        Some(annotation(
            &self.rects,
            Target::Action(Action::Goto(destination.into())),
        ))
    }
}

fn href(base_url: Option<&url::Url>, document_url: &url::Url, href: &str) -> Option<Href> {
    let uri = base_url
        .map_or_else(|| url::Url::parse(href), |base| base.join(href))
        .ok()?;
    if let Some(fragment) = uri.fragment() {
        let mut original_url = document_url.clone();
        original_url.set_fragment(None);
        let mut target_url = uri.clone();
        target_url.set_fragment(None);
        if original_url == target_url {
            let name = percent_decode_str(fragment).decode_utf8().ok()?;
            return Some(Href::Internal(name.into_owned()));
        }
    }
    Some(Href::External(uri.into()))
}

/// The links of `page` that have a target and a visible area.
///
/// `base_url` resolves relative targets; a target that names `document_url`
/// with a fragment is an internal link.
pub(crate) fn links(
    page: &Page<'_>,
    base_url: Option<&url::Url>,
    document_url: &url::Url,
) -> Vec<PageLink> {
    let mut links = Vec::new();
    for link in page.links() {
        if link.target.is_empty() {
            continue;
        }
        let Some(href) = href(base_url, document_url, link.target) else {
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
        if !rects.is_empty() {
            links.push(PageLink { href, rects });
        }
    }
    links
}

/// The annotations of `page`, with internal links resolved by `anchors`.
pub(super) fn annotations(
    page: &Page<'_>,
    base_url: Option<&url::Url>,
    document_url: &url::Url,
    anchors: &AnchorIndex,
) -> Vec<Annotation> {
    links(page, base_url, document_url)
        .iter()
        .filter_map(|link| link.external().or_else(|| link.internal(anchors)))
        .collect()
}

fn annotation(rects: &[Rect], target: Target) -> Annotation {
    let link = match rects {
        [rect] => LinkAnnotation::new(*rect, target),
        _ => LinkAnnotation::new_with_quad_points(
            rects.iter().copied().map(Quadrilateral::from).collect(),
            target,
        ),
    };
    Annotation::new_link(link, None)
}
