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
use super::shape::RoundedRect;
use krilla::geom::Path;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use raikiri_html::computed::{ComputedDisplay, ComputedValues, OverflowValue};
use raikiri_html::{FragmentKind, NodeId, Page};
use std::collections::HashMap;

/// The overflow clip paths of the boxes on one page, by node.
pub(super) struct ClipMap {
    clips: HashMap<NodeId, Path>,
}

impl ClipMap {
    pub(super) fn new(page: &Page<'_>) -> Self {
        let bounds = page.geometry().page_box;
        let dom = page.dom();
        let mut clips = HashMap::new();
        for fragment in page.fragments() {
            if fragment.kind() != FragmentKind::Box || clips.contains_key(&fragment.node()) {
                continue;
            }
            let node = fragment.node();
            let Some(style) = page.computed(node) else {
                continue;
            };
            // §3.3: the root's and the body's `overflow` apply to the
            // viewport, which is the page here; they do not clip the
            // document to the body's box.
            if matches!(dom.local_name(node), Some("html" | "body")) {
                continue;
            }
            // A node split into several fragments on one page clips to its
            // first one.
            if let Some(path) = clip_path(fragment.paint_rect(), style, bounds) {
                clips.insert(node, path);
            }
        }
        Self { clips }
    }

    /// The clipping ancestors of a painted item, outermost first. `start`
    /// is the innermost node whose clip applies: the parent of a box, or the
    /// element a text run belongs to.
    pub(super) fn chain(&self, page: &Page<'_>, start: Option<NodeId>) -> Vec<NodeId> {
        let mut chain = Vec::new();
        if self.clips.is_empty() {
            return chain;
        }
        let dom = page.dom();
        let mut node = start;
        while let Some(current) = node {
            if self.clips.contains_key(&current) {
                chain.push(current);
            }
            node = dom.parent(current);
        }
        chain.reverse();
        chain
    }
}

/// The clip of one box, or `None` when it does not clip. §3.1: the
/// `hidden`, `clip`, `scroll` and `auto` values all clip; the cascade has
/// already applied the rule that couples the two axes.
fn clip_path(
    rect: raikiri_html::PaintRect,
    style: &ComputedValues,
    bounds: raikiri_html::PaintRect,
) -> Option<Path> {
    let clip_x = !matches!(style.overflow.x, OverflowValue::Visible);
    let clip_y = !matches!(style.overflow.y, OverflowValue::Visible);
    // Overflow applies to block containers, flex and grid containers; an
    // inline box never clips.
    if !(clip_x || clip_y) || matches!(style.display, ComputedDisplay::Inline) {
        return None;
    }
    let padding_box =
        RoundedRect::border_box(rect, &style.border_radius).inset(border::widths(style));
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
    active: Vec<NodeId>,
}

impl ClipStack {
    pub(super) fn apply(&mut self, surface: &mut Surface<'_>, map: &ClipMap, chain: &[NodeId]) {
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
        for node in &chain[common..] {
            if let Some(path) = map.clips.get(node) {
                surface.push_clip_path(path, &FillRule::NonZero);
                self.active.push(*node);
            }
        }
    }

    pub(super) fn clear(&mut self, surface: &mut Surface<'_>) {
        while self.active.pop().is_some() {
            surface.pop();
        }
    }
}
