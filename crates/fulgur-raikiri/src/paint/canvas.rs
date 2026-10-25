//! The canvas background (CSS Backgrounds 3 §2.11.2).
//!
//! The background of the root element becomes the background of the canvas
//! and is painted under the page area of every page, not inside the root's
//! own box. When the root is an `<html>` element whose background is
//! transparent (`background-color: transparent` and no image), the first
//! `<body>` child propagates its background to the canvas instead. The
//! element that gave its background to the canvas does not paint it again.
//! An element with `display: none` generates no box, so it leaves the canvas
//! transparent.
//!
//! The canvas runs through every page as one surface, so its images are
//! positioned in the root element's box, which sits higher on each later
//! page, and the root's `transform` moves them with the page content
//! (CSS Transforms 1 §6) while the canvas still covers the page area.

use super::raster::{RasterCache, SvgChoice};
use super::shape::{Edges, RoundedRect, Slice};
use super::{paint_background, transform};
use crate::tagging::{Tags, Target};
use fulgur_core::Result;
use krilla::num::NormalizedF32;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use krilla::tagging::ArtifactType;
use raikiri_html::computed::{
    ComputedBackgroundAttachment, ComputedBackgroundLayer, ComputedDisplay, ComputedValues,
    ComputedVisualBox, CssColor,
};
use raikiri_html::{NodeId, NodeKind, Page, PaintInsets, PaintTransform, RootBox};
use std::sync::Arc;

/// What the root element's `transform` does to the page (CSS Transforms 1 §6).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RootSpace {
    /// The root has no transform: the page is drawn as laid out.
    Plain,
    /// The page content and the canvas images are drawn through this matrix.
    Moved(PaintTransform),
    /// The matrix is singular, so the root and its content are not rendered.
    Collapsed,
}

impl RootSpace {
    /// The space the page content of `page` is drawn in.
    pub(crate) fn of_page(page: &Page<'_>) -> Self {
        Self::of(page.root_box().as_ref())
    }

    /// The space the `root` box of a page draws its content in.
    pub(super) fn of(root: Option<&RootBox>) -> Self {
        match root.and_then(|root| root.transform) {
            None => Self::Plain,
            Some(matrix) if matrix == PaintTransform::IDENTITY => Self::Plain,
            Some(matrix) if transform::inverse(matrix).is_some() => Self::Moved(matrix),
            Some(_) => Self::Collapsed,
        }
    }

    /// Where a point of the layout lands on the page, `None` when the root
    /// is not rendered.
    pub(crate) fn apply(self, point: (f32, f32)) -> Option<(f32, f32)> {
        match self {
            Self::Plain => Some(point),
            Self::Moved(matrix) => Some(matrix.apply(point)),
            Self::Collapsed => None,
        }
    }
}

/// The element whose background is the canvas background of `page`, with
/// its computed style.
pub(super) fn owner<'p>(page: &'p Page<'_>) -> Option<(NodeId, &'p ComputedValues)> {
    let html = root(page)?;
    let html_style = page.computed(html)?;
    // A root without a box leaves the canvas transparent; its body does not
    // take over.
    if !generates_box(html_style) {
        return None;
    }
    if has_background(html_style) {
        return Some((html, html_style));
    }
    let dom = page.dom();
    let body = dom
        .children(html)
        .find(|&node| is_element(page, node, "body"))?;
    page.computed(body)
        .filter(|style| has_background(style) && generates_box(style))
        .map(|style| (body, style))
}

/// Whether the root element of `page` has `display: none`, so the document
/// generates no boxes at all (CSS Display 3 §2.5).
pub(super) fn root_generates_no_box(page: &Page<'_>) -> bool {
    root(page)
        .and_then(|html| page.computed(html))
        .is_some_and(|style| !generates_box(style))
}

/// The root `<html>` element of `page`.
pub(super) fn root(page: &Page<'_>) -> Option<NodeId> {
    let dom = page.dom();
    dom.children(dom.root())
        .find(|&node| is_element(page, node, "html"))
}

fn is_element(page: &Page<'_>, node: NodeId, name: &str) -> bool {
    let dom = page.dom();
    dom.kind(node) == Some(NodeKind::Element) && dom.local_name(node) == Some(name)
}

fn generates_box(style: &ComputedValues) -> bool {
    style.display != ComputedDisplay::None
}

fn has_background(style: &ComputedValues) -> bool {
    style.background_color.a > 0 || style.has_background_image()
}

/// Paint the canvas background of `page`, if any, as an artifact, and return
/// the element that gave its background to the canvas.
///
/// The images are positioned in `root`, the root element's box on this page,
/// and drawn through its transform `space`.
pub(super) fn paint_page(
    surface: &mut Surface<'_>,
    tags: &mut Tags,
    raster: &mut RasterCache<'_>,
    page: &Page<'_>,
    root: Option<&RootBox>,
    space: RootSpace,
) -> Result<Option<NodeId>> {
    let Some((owner, style)) = owner(page) else {
        return Ok(None);
    };
    tags.mark(
        surface,
        page,
        Target::Artifact(ArtifactType::Other),
        |surface| {
            let svg = SvgChoice::of(page, owner);
            paint(surface, raster, page, style, root, space, svg)
        },
    )
    .map(|()| Some(owner))
}

/// The opacity of the root element's group, when it fades.
///
/// The paint order starts at the body, so the root's group, which holds the
/// canvas as well as the page content, is left to the page painter
/// (Compositing and Blending 1 §3.3.1).
pub(super) fn root_opacity(page: &Page<'_>) -> Option<NormalizedF32> {
    root(page)
        .and_then(|html| page.computed(html))
        .and_then(|style| NormalizedF32::new(style.opacity))
        .filter(|alpha| *alpha < NormalizedF32::ONE)
}

/// Paint `style`'s background over the page area of `page`.
///
/// The color fills the page area whatever the root's transform does. The
/// images are positioned in the root element's box `root` (`fixed` ones in
/// the page area) and move with the root's transform `space`, clipped to the
/// page area. A singular transform leaves the color only.
fn paint(
    surface: &mut Surface<'_>,
    raster: &mut RasterCache<'_>,
    page: &Page<'_>,
    style: &ComputedValues,
    root: Option<&RootBox>,
    space: RootSpace,
    svg: SvgChoice,
) -> Result<()> {
    let geometry = page.geometry();
    let (page_box, margins) = (geometry.page_box, geometry.margins);
    let area = RoundedRect::rect(
        page_box.x + margins.left,
        page_box.y + margins.top,
        (page_box.width - margins.left - margins.right).max(0.0),
        (page_box.height - margins.top - margins.bottom).max(0.0),
    );
    // The page area has no borders or padding of its own: every edge is
    // treated as broken so the owner's are not subtracted.
    let open = Slice {
        top: true,
        bottom: true,
        left: true,
        right: true,
    };
    let mut images = style.clone();
    // `background-clip` has no effect on the canvas, which covers the whole
    // page area (CSS Backgrounds 3 §2.11.1). The page area has no border or
    // padding, so the other boxes already coincide with it; `text` and
    // `border-area` would otherwise paint glyphs or an empty ring.
    images.background_layers = images
        .background_layers()
        .iter()
        .map(|layer| {
            let mut layer = layer.clone();
            if matches!(
                layer.clip,
                ComputedVisualBox::Text | ComputedVisualBox::BorderArea
            ) {
                layer.clip = ComputedVisualBox::BorderBox;
            }
            layer
        })
        .collect();
    let mut color = images.clone();
    color.background_layers = Arc::from([ComputedBackgroundLayer::initial()]);
    paint_background(
        surface,
        raster,
        &area,
        Edges::default(),
        open,
        &color,
        None,
        svg,
        || None,
    )?;
    images.background_color = CssColor::TRANSPARENT;
    let positioning = |layer: &ComputedBackgroundLayer| positioning_area(root, &area, layer);
    let draw = |surface: &mut Surface<'_>, raster: &mut RasterCache<'_>, painting: &RoundedRect| {
        paint_background(
            surface,
            raster,
            painting,
            Edges::default(),
            open,
            &images,
            Some(&positioning),
            svg,
            || None,
        )
    };
    match space {
        RootSpace::Collapsed => Ok(()),
        RootSpace::Plain => draw(surface, raster, &area),
        RootSpace::Moved(matrix) => {
            let (Some(inverse), Some(clip)) = (transform::inverse(matrix), area.path()) else {
                return Ok(());
            };
            // The transform moves the layers; the part of the canvas that
            // lands on the page area is what the inverse maps it from.
            let from = transform::bounds(inverse, area.bounds());
            let painting = RoundedRect::rect(from.x, from.y, from.width, from.height);
            surface.push_clip_path(&clip, &FillRule::NonZero);
            let painted = transform::with(surface, Some(matrix), |surface| {
                draw(surface, raster, &painting)
            });
            surface.pop();
            painted
        }
    }
}

/// The positioning area of `layer` on the canvas: the page area for a
/// `fixed` layer, and otherwise the root element's border box inset to the
/// layer's `background-origin` (CSS Backgrounds 3 §2.11.2). Without a root
/// box, the page area.
fn positioning_area(
    root: Option<&RootBox>,
    page_area: &RoundedRect,
    layer: &ComputedBackgroundLayer,
) -> RoundedRect {
    let Some(root) = root.filter(|_| layer.attachment != ComputedBackgroundAttachment::Fixed)
    else {
        return *page_area;
    };
    let rect = root.border_box;
    let border_box = RoundedRect::rect(rect.x, rect.y, rect.width, rect.height);
    let edges = |insets: PaintInsets| Edges {
        top: insets.top,
        right: insets.right,
        bottom: insets.bottom,
        left: insets.left,
    };
    match layer.origin {
        ComputedVisualBox::PaddingBox => border_box.inset(edges(root.border)),
        ComputedVisualBox::ContentBox => {
            border_box.inset(edges(root.border).add(edges(root.padding)))
        }
        _ => border_box,
    }
}

/// `style` without its background, for the element that gave it to the
/// canvas.
pub(super) fn without_background(style: &ComputedValues) -> ComputedValues {
    let mut style = style.clone();
    style.background_color = CssColor::TRANSPARENT;
    style.background_layers = Arc::from([ComputedBackgroundLayer::initial()]);
    style
}
