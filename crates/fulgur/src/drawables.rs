//! Phase 4 (fulgur-9t3z): node-keyed side-channel maps that replace the
//! `Pageable` trait + 17 impls.
//!
//! `Drawables` is the data shape that `convert::dom_to_drawables` produces
//! and `render::render_v2` consumes. Each map holds per-NodeId state for
//! one draw concern (background, paragraph, image, etc.). The render path
//! walks `pagination_layout::PaginationGeometryTable` per page and looks
//! up the node's data in the appropriate map — no trait dispatch, no
//! central `DrawOp` enum.
//!
//! See `docs/plans/2026-04-30-phase4-design.md` for the full design.
//!
//! ## PR sequence note
//!
//! This struct is introduced in PR 1 with empty placeholder fields. Each
//! subsequent PR replaces one placeholder with the real data type:
//!
//! - PR 2: `images`, `svgs`, plus the five marker types collapse into
//!   `bookmark_anchors` (the rest are deleted from the draw path).
//! - PR 3: `paragraphs`.
//! - PR 4: `block_styles`.
//! - PR 5: `tables`, `list_items`.
//! - PR 6: `transforms`, `multicol_rules`, plus marker wrappers vanish.

use std::collections::BTreeMap;

/// Blitz DOM node id, keyed throughout `Drawables`. Same shape as
/// `pagination_layout::PaginationGeometryTable`'s key.
pub type NodeId = usize;

// ── Placeholder entry types ──────────────────────────────────────────
//
// Each PR in the Phase 4 sequence replaces one of these with the real
// per-node data extracted from convert. The placeholder is empty so that
// the `Drawables` struct compiles before any draw migration starts; the
// shadow harness can already exercise the pipeline plumbing.

/// Block draw payload for v2. Mirrors the fields `BlockPageable`
/// holds for paint dispatch — backgrounds, borders, box-shadow,
/// overflow clip, opacity, and the anchor id used by
/// `DestinationRegistry`.
#[derive(Debug, Clone)]
pub struct BlockEntry {
    pub style: crate::pageable::BlockStyle,
    pub opacity: f32,
    pub visible: bool,
    pub id: Option<std::sync::Arc<String>>,
    /// Taffy-computed border-box size (pt). Preferred when set; falls
    /// back to the fragment's width/height (CSS px → pt) at render
    /// time when absent.
    pub layout_size: Option<crate::pageable::Size>,
}

/// Paragraph draw payload for v2. Holds the shaped lines that
/// `paragraph::draw_shaped_lines` consumes verbatim — no re-shaping
/// at render time. Mirrors the per-paragraph fields from
/// `ParagraphPageable` that survive draw.
#[derive(Clone)]
pub struct ParagraphEntry {
    pub lines: Vec<crate::paragraph::ShapedLine>,
    pub opacity: f32,
    pub visible: bool,
    /// Anchor id (`id="..."` on the inline root) — drives
    /// `DestinationRegistry` for `href="#..."` resolution.
    pub id: Option<std::sync::Arc<String>>,
}

impl std::fmt::Debug for ParagraphEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParagraphEntry")
            .field("lines", &self.lines.len())
            .field("opacity", &self.opacity)
            .field("visible", &self.visible)
            .field("id", &self.id)
            .finish()
    }
}

/// Image draw payload for v2. Mirrors the fields `ImagePageable` holds.
#[derive(Debug, Clone)]
pub struct ImageEntry {
    pub image_data: std::sync::Arc<Vec<u8>>,
    pub format: crate::image::ImageFormat,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub visible: bool,
}

/// SVG draw payload for v2. Mirrors the fields `SvgPageable` holds.
#[derive(Debug, Clone)]
pub struct SvgEntry {
    pub tree: std::sync::Arc<usvg::Tree>,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub visible: bool,
}

/// Table draw payload for v2. Holds the border-box paint state
/// (background / borders / shadow) applied to the table's outer
/// frame. Cell content (`<th>` / `<td>`) lives as separate
/// `BlockEntry` / `ParagraphEntry` keyed by the cell's own NodeId
/// and paints through the standard per-NodeId dispatch.
///
/// Multi-page header repetition (`<thead>` cloned on continuation
/// pages) is **not** modelled in PR 5; single-page tables byte-eq
/// already, multi-page tables follow in a later PR alongside the
/// per-block clip work.
#[derive(Debug, Clone)]
pub struct TableEntry {
    pub style: crate::pageable::BlockStyle,
    pub opacity: f32,
    pub visible: bool,
    pub id: Option<std::sync::Arc<String>>,
    pub layout_size: Option<crate::pageable::Size>,
    pub width: f32,
    pub cached_height: f32,
}

/// List-item marker payload for v2. The body block paints itself
/// through `BlockEntry`; `ListItemEntry` only carries the marker
/// (text / image / svg / none) and the line-height needed to
/// vertically centre image markers.
#[derive(Clone)]
pub struct ListItemEntry {
    pub marker: crate::pageable::ListItemMarker,
    pub marker_line_height: f32,
    pub opacity: f32,
    pub visible: bool,
}

impl std::fmt::Debug for ListItemEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListItemEntry")
            .field("marker_line_height", &self.marker_line_height)
            .field("opacity", &self.opacity)
            .field("visible", &self.visible)
            .finish()
    }
}

/// Multicol column-rule paint spec + per-column-group geometry.
/// Mirrors the fields `MulticolRulePageable` carries — render at the
/// container's location after children paint, partitioning `groups`
/// per page based on the container's fragment cumulative heights.
#[derive(Debug, Clone)]
pub struct MulticolRuleEntry {
    pub rule: crate::column_css::ColumnRuleSpec,
    pub groups: Vec<crate::multicol_layout::ColumnGroupGeometry>,
}

/// CSS transform matrix + origin for a node (and its descendants).
///
/// Mirrors `TransformWrapperPageable`. v1 pushes the surface transform
/// before drawing `inner.draw(...)` and pops after; v2's flat dispatch
/// emulates this by recording every descendant `node_id` of the wrapper
/// at convert time so the render loop can dispatch the wrapper's own
/// payload + every descendant inside one push/pop pair.
#[derive(Debug, Clone)]
pub struct TransformEntry {
    pub matrix: crate::pageable::Affine2D,
    pub origin: crate::pageable::Point2,
    /// Every strict descendant `NodeId` whose fragment must paint
    /// inside this transform's `push_transform`/`pop` group. Does NOT
    /// include the wrapper's own `node_id` (the entry's key) — the
    /// render loop dispatches the wrapper node separately before
    /// iterating descendants (see
    /// `render::draw_under_transform`). Stored as a `Vec` for
    /// deterministic iteration — order matches the depth-first walk
    /// produced by `extract_drawables_from_pageable`.
    pub descendants: Vec<NodeId>,
}

/// Bookmark anchor (level + label) keyed by source node. First-fragment-only
/// emission is enforced at render time by reading `geometry.fragments[0]`.
#[derive(Debug, Clone)]
pub struct BookmarkAnchorEntry {
    pub level: u8,
    pub label: String,
}

/// PR 3 target: link span (target + alt text) covering one or more
/// glyph runs in a paragraph. `Vec<(NodeId, LinkSpan)>` lets a single
/// node carry multiple spans.
#[derive(Debug, Clone, Default)]
pub struct LinkSpanEntry;

// ── Drawables ─────────────────────────────────────────────────────────

/// Node-keyed side-channel maps consumed by `render::render_v2`.
///
/// Phase 4 PR 1 ships this with all maps empty — the v2 render path
/// walks geometry but emits no content for any node. Subsequent PRs
/// fill each map by migrating one Pageable type at a time.
#[derive(Debug, Default, Clone)]
pub struct Drawables {
    /// `body_layout.location.x/y` in pt. Captures the html → body
    /// offset that CSS margin collapsing folds onto the body element.
    /// `render_v2` adds this to every per-fragment `(x, y)` so v2 paint
    /// matches v1's `html → body @ pc=(body.x, body.y)` chain exactly.
    /// Pre-Phase-4 the fragmenter intentionally records body's own
    /// fragment at `y=0` in body-content-area-relative coordinates and
    /// downstream slicing logic depends on that — keeping it relative
    /// in geometry but absolute on Drawables avoids touching the
    /// fragmenter contract.
    pub body_offset_pt: (f32, f32),
    pub block_styles: BTreeMap<NodeId, BlockEntry>,
    pub paragraphs: BTreeMap<NodeId, ParagraphEntry>,
    pub images: BTreeMap<NodeId, ImageEntry>,
    pub svgs: BTreeMap<NodeId, SvgEntry>,
    pub tables: BTreeMap<NodeId, TableEntry>,
    pub list_items: BTreeMap<NodeId, ListItemEntry>,
    pub multicol_rules: BTreeMap<NodeId, MulticolRuleEntry>,
    pub transforms: BTreeMap<NodeId, TransformEntry>,
    pub bookmark_anchors: BTreeMap<NodeId, BookmarkAnchorEntry>,
    pub link_spans: Vec<(NodeId, LinkSpanEntry)>,
}

impl Drawables {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when no draw payload has been registered for any node.
    /// PR 1 always returns `true` because the convert side has not
    /// migrated yet.
    ///
    /// `body_offset_pt` is intentionally excluded — it is a global
    /// coordinate offset (e.g. `body { margin: 8px }`), not a per-node
    /// draw payload, so an empty `<body>` with default browser margins
    /// should still report `true`.
    pub fn is_empty(&self) -> bool {
        self.block_styles.is_empty()
            && self.paragraphs.is_empty()
            && self.images.is_empty()
            && self.svgs.is_empty()
            && self.tables.is_empty()
            && self.list_items.is_empty()
            && self.multicol_rules.is_empty()
            && self.transforms.is_empty()
            && self.bookmark_anchors.is_empty()
            && self.link_spans.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawables_default_is_empty() {
        let d = Drawables::default();
        assert!(d.is_empty());
        assert_eq!(d.block_styles.len(), 0);
        assert_eq!(d.link_spans.len(), 0);
    }

    #[test]
    fn drawables_new_matches_default() {
        let a = Drawables::new();
        let b = Drawables::default();
        assert_eq!(a.is_empty(), b.is_empty());
    }
}
