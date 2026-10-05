//! Overflow clipping (CSS Overflow 3 §3).
//!
//! A box whose `overflow` is anything but `visible` on an axis clips its
//! descendants' painting on that axis to its padding box. Raikiri's
//! fragments are a flat list, so the clips that apply to a fragment are
//! found by walking the DOM ancestors of its node.
//!
//! An absolutely or fixed positioned descendant whose containing block is
//! outside the clipping box should escape its clip. `ComputedValues::position`
//! has a type that `raikiri_html::computed` does not name, so the painter
//! cannot tell those descendants apart and clips them like in-flow ones.

use super::border;
use super::shape::{RoundedRect, Slice};
use krilla::geom::Path;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use raikiri_html::computed::{ComputedDisplay, ComputedValues, OverflowValue};
use raikiri_html::{FragmentKind, NodeId, Page, PaintRect};
use std::collections::HashMap;

/// One clipping box fragment: the node and the fragment's position among
/// that node's fragments on the page.
pub(super) type ClipKey = (NodeId, usize);

/// The overflow clip paths of the box fragments on one page, by node. A
/// node broken into several fragments on the page (columns) has one clip
/// per fragment, each at that fragment's padding box.
pub(super) struct ClipMap {
    clips: HashMap<NodeId, Vec<(PaintRect, Path)>>,
}

impl ClipMap {
    pub(super) fn new(page: &Page<'_>) -> Self {
        let bounds = page.geometry().page_box;
        let mut clips: HashMap<NodeId, Vec<(PaintRect, Path)>> = HashMap::new();
        let mut fragments: Vec<_> = page
            .fragments()
            .filter(|fragment| fragment.kind() == FragmentKind::Box)
            .collect();
        fragments.sort_by_key(|fragment| (fragment.node(), fragment.fragment_index()));
        for fragment in fragments {
            let node = fragment.node();
            let Some(style) = page.computed(node) else {
                continue;
            };
            if propagates_to_viewport(page, node) {
                continue;
            }
            let rect = fragment.paint_rect();
            if let Some(path) = clip_path(rect, Slice::of(&fragment), style, bounds) {
                clips.entry(node).or_default().push((rect, path));
            }
        }
        Self { clips }
    }

    /// The clipping ancestor fragments of a painted item, outermost first.
    /// `start` is the innermost node whose clip applies: the parent of a
    /// box, or the element a text run belongs to. `target` is the item's
    /// area; of an ancestor broken into several fragments, the one nearest
    /// to it (normally the one containing it) clips it.
    pub(super) fn chain(
        &self,
        page: &Page<'_>,
        start: Option<NodeId>,
        target: PaintRect,
    ) -> Vec<ClipKey> {
        let mut chain = Vec::new();
        if self.clips.is_empty() {
            return chain;
        }
        let dom = page.dom();
        let mut node = start;
        while let Some(current) = node {
            if let Some(fragments) = self.clips.get(&current) {
                let rects: Vec<PaintRect> = fragments.iter().map(|(rect, _)| *rect).collect();
                chain.push((current, nearest(&rects, target)));
            }
            node = dom.parent(current);
        }
        chain.reverse();
        chain
    }

    fn path(&self, key: ClipKey) -> Option<&Path> {
        self.clips.get(&key.0)?.get(key.1).map(|(_, path)| path)
    }
}

/// The index of the rectangle in `rects` nearest to the center of
/// `target`: zero distance for any rectangle containing it, so the
/// fragment an item lies in wins.
fn nearest(rects: &[PaintRect], target: PaintRect) -> usize {
    let (cx, cy) = (
        target.x + target.width / 2.0,
        target.y + target.height / 2.0,
    );
    let distance = |rect: &PaintRect| {
        let dx = (rect.x - cx).max(cx - (rect.x + rect.width)).max(0.0);
        let dy = (rect.y - cy).max(cy - (rect.y + rect.height)).max(0.0);
        dx * dx + dy * dy
    };
    rects
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))
        .map_or(0, |(index, _)| index)
}

/// CSS Overflow 3 §3.3: the root element's `overflow` applies to the
/// viewport (the page here), and so does the `body` element's when the
/// root's is `visible` on both axes. Such an `overflow` does not clip the
/// element's own box.
fn propagates_to_viewport(page: &Page<'_>, node: NodeId) -> bool {
    let dom = page.dom();
    let Some(parent) = dom.parent(node) else {
        return false;
    };
    let is_root_element = |element: NodeId| {
        dom.local_name(element) == Some("html") && dom.parent(element) == Some(dom.root())
    };
    if is_root_element(node) {
        return true;
    }
    if !(is_root_element(parent) && dom.local_name(node) == Some("body")) {
        return false;
    }
    page.computed(parent).is_some_and(|root| {
        matches!(root.overflow.x, OverflowValue::Visible)
            && matches!(root.overflow.y, OverflowValue::Visible)
    })
}

/// The clip of one box fragment, or `None` when it does not clip. §3.1: the
/// `hidden`, `clip`, `scroll` and `auto` values all clip; the cascade has
/// already applied the rule that couples the two axes.
fn clip_path(
    rect: PaintRect,
    slice: Slice,
    style: &ComputedValues,
    bounds: PaintRect,
) -> Option<Path> {
    let clip_x = !matches!(style.overflow.x, OverflowValue::Visible);
    let clip_y = !matches!(style.overflow.y, OverflowValue::Visible);
    // Overflow applies to block containers, flex and grid containers; an
    // inline box never clips.
    if !(clip_x || clip_y) || matches!(style.display, ComputedDisplay::Inline) {
        return None;
    }
    let padding_box = RoundedRect::border_box(rect, &style.border_radius)
        .sliced(slice)
        .inset(slice.edges(border::widths(style)));
    if clip_x && clip_y {
        // §3.1 and CSS Backgrounds 3 §5.3: the clip follows the curve of
        // the padding edge.
        return padding_box.path().or_else(empty_path);
    }
    // One axis clips: the other extends over the whole page. The corner
    // curves do not apply to a clip open on one side.
    let (x, width) = if clip_x {
        (padding_box.x, padding_box.width)
    } else {
        (
            bounds.x.min(padding_box.x),
            (bounds.x + bounds.width).max(padding_box.x + padding_box.width)
                - bounds.x.min(padding_box.x),
        )
    };
    let (y, height) = if clip_y {
        (padding_box.y, padding_box.height)
    } else {
        (
            bounds.y.min(padding_box.y),
            (bounds.y + bounds.height).max(padding_box.y + padding_box.height)
                - bounds.y.min(padding_box.y),
        )
    };
    RoundedRect::rect(x, y, width, height)
        .path()
        .or_else(empty_path)
}

/// A path that encloses nothing: the clip of a box with an empty padding
/// box, which hides everything inside it.
fn empty_path() -> Option<Path> {
    let mut builder = krilla::geom::PathBuilder::new();
    builder.move_to(0.0, 0.0);
    builder.line_to(0.0, 0.0);
    builder.close();
    builder.finish()
}

/// The clips currently pushed on a surface, outermost first. Moving from
/// one chain to the next pops and pushes only where they differ.
#[derive(Default)]
pub(super) struct ClipStack {
    active: Vec<ClipKey>,
}

impl ClipStack {
    pub(super) fn apply(&mut self, surface: &mut Surface<'_>, map: &ClipMap, chain: &[ClipKey]) {
        let common = self
            .active
            .iter()
            .zip(chain)
            .take_while(|(active, wanted)| active == wanted)
            .count();
        while self.active.len() > common {
            surface.pop();
            self.active.pop();
        }
        for key in &chain[common..] {
            if let Some(path) = map.path(*key) {
                surface.push_clip_path(path, &FillRule::NonZero);
                self.active.push(*key);
            }
        }
    }

    pub(super) fn clear(&mut self, surface: &mut Surface<'_>) {
        while self.active.pop().is_some() {
            surface.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_is_clipped_by_the_fragment_it_lies_in() {
        // Two columns of one box on a page.
        let columns = [
            PaintRect::new(20.0, 20.0, 120.0, 160.0),
            PaintRect::new(160.0, 20.0, 120.0, 160.0),
        ];
        assert_eq!(nearest(&columns, PaintRect::new(30.0, 40.0, 50.0, 20.0)), 0);
        assert_eq!(
            nearest(&columns, PaintRect::new(170.0, 40.0, 50.0, 20.0)),
            1
        );
        // Overflowing below the second column, it still belongs to it.
        assert_eq!(
            nearest(&columns, PaintRect::new(170.0, 190.0, 50.0, 20.0)),
            1
        );
    }
}
