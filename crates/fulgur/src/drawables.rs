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
    pub style: crate::draw_primitives::BlockStyle,
    pub opacity: f32,
    pub visible: bool,
    pub id: Option<std::sync::Arc<String>>,
    /// Taffy-computed border-box size (pt). Preferred when set; falls
    /// back to the fragment's width/height (CSS px → pt) at render
    /// time when absent.
    pub layout_size: Option<crate::draw_primitives::Size>,
    /// Strict descendant `NodeId`s that must paint INSIDE this block's
    /// `push_clip_path` / `pop` group. Populated by
    /// `extract_drawables_from_pageable` only when
    /// `style.has_overflow_clip()` is true — non-clipping blocks leave
    /// this empty so the dispatcher's main loop handles them with the
    /// regular shared-node_id pattern.
    ///
    /// Mirrors the `TransformEntry.descendants` shape: render time
    /// emits bg / border / shadow first (outside the clip), then pushes
    /// the clip path, dispatches each descendant fragment, and pops.
    pub clip_descendants: Vec<NodeId>,
    /// Strict descendant `NodeId`s that must paint INSIDE this block's
    /// `draw_with_opacity` group. Populated by
    /// `extract_drawables_from_pageable` only when `opacity < 1.0`
    /// AND the block does NOT have overflow clip (clip's
    /// `draw_under_clip` already wraps its descendants in
    /// `draw_with_opacity` so the dual case is covered there).
    ///
    /// Mirrors v1's `BlockPageable::draw` ordering: opacity wraps
    /// EVERYTHING — bg/border/shadow + descendants — so a
    /// `<div style="opacity:0.4"><svg>..</svg></div>` produces a
    /// single transparency group. v2's flat dispatch without this
    /// scope tracking would emit the svg outside the parent's
    /// opacity wrap, dropping the parent's opacity from the svg.
    pub opacity_descendants: Vec<NodeId>,
}

/// Paragraph draw payload for v2. Holds the shaped lines that
/// `paragraph::draw_shaped_lines` consumes verbatim — no re-shaping
/// at render time. Mirrors the per-paragraph fields from
/// `ParagraphRender` that survive draw.
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

/// Image draw payload for v2. Mirrors the fields `ImageRender` holds.
#[derive(Debug, Clone)]
pub struct ImageEntry {
    pub image_data: std::sync::Arc<Vec<u8>>,
    pub format: crate::image::ImageFormat,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub visible: bool,
}

/// SVG draw payload for v2. Mirrors the fields `SvgRender` holds.
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
/// already, multi-page tables follow in a later PR.
#[derive(Debug, Clone)]
pub struct TableEntry {
    pub style: crate::draw_primitives::BlockStyle,
    pub opacity: f32,
    pub visible: bool,
    pub id: Option<std::sync::Arc<String>>,
    pub layout_size: Option<crate::draw_primitives::Size>,
    pub width: f32,
    pub cached_height: f32,
    /// Strict descendant `node_id`s (cell blocks + their children) when
    /// `style.has_overflow_clip()` is true. Mirrors `BlockEntry::clip_descendants`
    /// so the dispatcher can push the table's clip path once and
    /// dispatch every cell inside it. Empty when the table doesn't clip.
    pub clip_descendants: Vec<NodeId>,
}

/// Image marker contents — either a raster image or a parsed SVG tree.
#[derive(Clone)]
pub enum ImageMarker {
    Raster(ImageEntry),
    Svg(SvgEntry),
}

/// List-item marker variants. Exactly one variant holds valid content per
/// list item, enforced by the type system. `None` is used for the second
/// fragment after a page-break split (the marker only appears on the first
/// fragment).
#[derive(Clone)]
pub enum ListItemMarker {
    /// Text marker with shaped glyph runs extracted from Blitz/Parley.
    Text {
        lines: Vec<crate::paragraph::ShapedLine>,
        width: f32,
    },
    /// Image marker (`list-style-image: url(...)`) — raster or SVG.
    Image {
        marker: ImageMarker,
        /// Display width after clamp (pt).
        width: f32,
        /// Display height after clamp (pt).
        height: f32,
    },
    /// No marker — split trailing fragment or `list-style-type: none`.
    None,
}

/// List-item marker payload for v2. The body block paints itself
/// through `BlockEntry`; `ListItemEntry` only carries the marker
/// (text / image / svg / none) and the line-height needed to
/// vertically centre image markers.
#[derive(Clone)]
pub struct ListItemEntry {
    pub marker: ListItemMarker,
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

/// One source paragraph distributed across columns of a multicol
/// container. Built from `multicol_layout::ParagraphSplitEntry`. The
/// per-slice `lines` are pre-rebased so each slice's first line has
/// `baseline = ascent` (i.e. `y=0` is the slice's top edge), matching
/// the rebase convention `paragraph::ParagraphPageable::split` uses
/// for second fragments after a page-break (see commit 9c0e092).
#[derive(Debug, Clone, Default)]
pub struct ParagraphSlicesEntry {
    /// Multicol container's NodeId. `render_v2` looks up the
    /// container's body-relative position via `block_styles[container_node_id]`
    /// to anchor the slices at correct page coordinates.
    pub container_node_id: NodeId,
    /// One slice per non-empty column. Empty columns are filtered out
    /// at construction time (Task 7), so iterating `slices` skips
    /// holes that `multicol_layout::ParagraphSplitEntry::column_slices`
    /// padded with `Default`.
    pub slices: Vec<ParagraphSlice>,
}

/// One column-bound slice of a paragraph rendered inside a multicol.
#[derive(Clone)]
pub struct ParagraphSlice {
    /// Slice top-left in PDF pt, relative to the multicol container's
    /// border-box top-left. Render adds the container's body-relative
    /// position to obtain final page coordinates.
    pub origin_pt: (f32, f32),
    /// Slice size — `col_w × Σ line_height(slice_lines)` in pt.
    pub size_pt: (f32, f32),
    /// Lines of this slice, baseline-rebased so the slice's first line
    /// renders at `y = baseline` from the slice top.
    pub lines: Vec<crate::paragraph::ShapedLine>,
}

impl std::fmt::Debug for ParagraphSlice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParagraphSlice")
            .field("origin_pt", &self.origin_pt)
            .field("size_pt", &self.size_pt)
            .field("lines", &self.lines.len())
            .finish()
    }
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
    pub matrix: crate::draw_primitives::Affine2D,
    pub origin: crate::draw_primitives::Point2,
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
    /// NodeId of the `<html>` root element when present.
    ///
    /// v1 painted html's own `background` BEFORE recursing into body.
    /// v2's flat dispatch never visits html — the fragmenter only records
    /// body and its descendants in `geometry` — so `render_v2` paints html
    /// as a pre-pass at the page's top-left margin using
    /// `block_styles[root_id].layout_size` as the rect dimensions.
    pub root_id: Option<NodeId>,
    /// NodeId of the `<body>` element when present.
    ///
    /// v1 paints body's `background` on EVERY page because each
    /// page's sliced root pageable still calls body's draw method.
    /// v2's main dispatch sees body via the fragmenter's single
    /// fragment on page 0 only, so multi-page documents would lose
    /// body's bg fill on continuation pages. `render_v2` mirrors v1
    /// by painting body as a pre-pass on every page (using
    /// `block_styles[body_id].layout_size` for the rect dimensions
    /// and `body_offset_pt` for the margin offset), then skipping
    /// body in the main dispatch loop to avoid double-painting.
    pub body_id: Option<NodeId>,
    pub block_styles: BTreeMap<NodeId, BlockEntry>,
    pub paragraphs: BTreeMap<NodeId, ParagraphEntry>,
    /// Per-source-paragraph multicol slicing emitted by
    /// `convert::convert_multicol_paragraph_slices` from
    /// `multicol_layout::MulticolGeometry::paragraph_splits`. When a
    /// `NodeId` has an entry, `render_v2`'s paragraph dispatcher renders
    /// one entry per non-empty column slice at the slice origin instead of
    /// the default single-rectangle path that uses `paragraphs[node_id]`.
    pub paragraph_slices: BTreeMap<NodeId, ParagraphSlicesEntry>,
    pub images: BTreeMap<NodeId, ImageEntry>,
    pub svgs: BTreeMap<NodeId, SvgEntry>,
    pub tables: BTreeMap<NodeId, TableEntry>,
    pub list_items: BTreeMap<NodeId, ListItemEntry>,
    pub multicol_rules: BTreeMap<NodeId, MulticolRuleEntry>,
    pub transforms: BTreeMap<NodeId, TransformEntry>,
    pub bookmark_anchors: BTreeMap<NodeId, BookmarkAnchorEntry>,
    pub link_spans: Vec<(NodeId, LinkSpanEntry)>,
    /// Tagged-PDF semantic classification keyed by source NodeId
    /// (fulgur-izp.3). Convert-side populates this from element local
    /// names; render integration arrives in fulgur-izp.4 and tag-tree
    /// assembly in fulgur-izp.5. Empty when tagging-related conversion
    /// records nothing — for example a fixture with `<custom-tag>`
    /// only — so byte-identical output is preserved while the data
    /// layer lands in isolation.
    pub semantics: BTreeMap<NodeId, crate::tagging::SemanticEntry>,
    /// PR 8g: NodeIds the v2 dispatcher's main loop must skip because
    /// they belong to inline-box content (or its descendants) dispatched
    /// explicitly by `paragraph::draw_shaped_lines` under an offset
    /// transform. Membership in this set means "do not dispatch at the
    /// geometry-recorded body-relative position; the paragraph render
    /// path owns this NodeId and will translate it to inline-flow
    /// position before invoking the standard dispatcher."
    pub inline_box_subtree_skip: std::collections::BTreeSet<NodeId>,
    /// PR 8g: per-inline-box-content descendant list. Keyed by the
    /// inline-box content's root NodeId; values are the strict
    /// descendant NodeIds the paragraph render path dispatches under
    /// the same offset transform. Both the key and values appear in
    /// `inline_box_subtree_skip`. `BTreeMap`/`Vec` keep iteration
    /// deterministic for PDF byte-equality.
    pub inline_box_subtree_descendants: BTreeMap<NodeId, Vec<NodeId>>,
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
            && self.paragraph_slices.is_empty()
            && self.images.is_empty()
            && self.svgs.is_empty()
            && self.tables.is_empty()
            && self.list_items.is_empty()
            && self.multicol_rules.is_empty()
            && self.transforms.is_empty()
            && self.bookmark_anchors.is_empty()
            && self.link_spans.is_empty()
            && self.semantics.is_empty()
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
    fn drawables_default_paragraph_slices_is_empty() {
        let d = Drawables::new();
        assert!(d.paragraph_slices.is_empty());
        assert!(d.is_empty());
    }

    #[test]
    fn drawables_new_matches_default() {
        let a = Drawables::new();
        let b = Drawables::default();
        assert_eq!(a.is_empty(), b.is_empty());
    }

    #[test]
    fn paragraph_entry_debug_formats_summary_fields() {
        let entry = ParagraphEntry {
            lines: Vec::new(),
            opacity: 0.5,
            visible: true,
            id: Some(std::sync::Arc::new("anchor".to_string())),
        };
        let s = format!("{:?}", entry);
        assert!(s.contains("ParagraphEntry"));
        assert!(s.contains("lines"));
        assert!(s.contains("opacity"));
        assert!(s.contains("visible"));
        assert!(s.contains("id"));
    }

    #[test]
    fn list_item_entry_debug_formats_summary_fields() {
        let entry = ListItemEntry {
            marker: ListItemMarker::Text {
                lines: Vec::new(),
                width: 0.0,
            },
            marker_line_height: 12.0,
            opacity: 1.0,
            visible: true,
        };
        let s = format!("{:?}", entry);
        assert!(s.contains("ListItemEntry"));
        assert!(s.contains("marker_line_height"));
        assert!(s.contains("opacity"));
        assert!(s.contains("visible"));
    }
}
