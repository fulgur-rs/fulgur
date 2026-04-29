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

/// PR 4 target: per-node block-level paint state (background layers,
/// borders, box-shadow, opacity, overflow clip).
#[derive(Debug, Clone, Default)]
pub struct BlockEntry;

/// PR 3 target: per-node shaped lines (`Vec<ShapedLine>`) reused from
/// the existing `paragraph::draw_shaped_lines` path.
#[derive(Debug, Clone, Default)]
pub struct ParagraphEntry;

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

/// PR 5 target: table layout state — header cell ids, body cell offsets,
/// header height. Per-page slicing happens at render time.
#[derive(Debug, Clone, Default)]
pub struct TableEntry;

/// PR 5 target: list item marker + body wrapper data.
#[derive(Debug, Clone, Default)]
pub struct ListItemEntry;

/// PR 6 target: column-rule paint spec + per-column-group geometry.
#[derive(Debug, Clone, Default)]
pub struct MulticolRuleEntry;

/// PR 6 target: CSS transform matrix + origin.
#[derive(Debug, Clone, Default)]
pub struct TransformEntry;

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
