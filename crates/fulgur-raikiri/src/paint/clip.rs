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

use super::shape::RoundedRect;
use krilla::geom::Path;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use raikiri_html::{NodeId, Page, PaintClip, PaintRect};
use std::collections::HashMap;

/// One clipping box fragment: the node and the fragment's position among
/// that node's fragments on the page.
pub(super) type ClipKey = (NodeId, usize);

/// Resolved overflow clip paths on one page, by source node. Includes
/// ancestors whose own boxes do not reach the page but whose descendants do.
/// Raikiri retains each whole padding-edge shape across page cuts.
pub(super) struct ClipMap {
    clips: HashMap<NodeId, Vec<(PaintRect, Path)>>,
}

impl ClipMap {
    pub(super) fn new(page: &Page<'_>) -> Self {
        let bounds = page.geometry().page_box;
        let mut clips: HashMap<NodeId, Vec<(PaintRect, Path)>> = HashMap::new();
        for clip in page.overflow_clips() {
            if let Some(path) = clip_path(clip.clip, bounds) {
                clips
                    .entry(clip.node)
                    .or_default()
                    .push((clip.border_box, path));
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

/// Draw Raikiri's resolved padding-edge clip without resolving percentages
/// on the page slice or normalizing a cropped inner ellipse again.
pub(super) fn clip_path(clip: PaintClip, bounds: PaintRect) -> Option<Path> {
    let rect = clip.rect;
    if clip.clip_x && clip.clip_y {
        return RoundedRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            radii: clip.corner_radii.unwrap_or([[0.0; 2]; 4]),
        }
        .path()
        .or_else(empty_path);
    }
    // An open axis extends over the page; it has no corner curves.
    let (x, width) = if clip.clip_x {
        (rect.x, rect.width)
    } else {
        (
            bounds.x.min(rect.x),
            (bounds.x + bounds.width).max(rect.x + rect.width) - bounds.x.min(rect.x),
        )
    };
    let (y, height) = if clip.clip_y {
        (rect.y, rect.height)
    } else {
        (
            bounds.y.min(rect.y),
            (bounds.y + bounds.height).max(rect.y + rect.height) - bounds.y.min(rect.y),
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
mod tests;
