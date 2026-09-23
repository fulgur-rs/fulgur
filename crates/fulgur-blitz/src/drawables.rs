//! Data shape produced by `convert::dom_to_drawables` and consumed by
//! `render::render_v2`; each map holds per-NodeId state for one draw
//! concern (background, paragraph, image, etc.). The render path walks
//! `pagination_layout::PaginationGeometryTable` per page and looks up
//! the node's data in the appropriate map — no trait dispatch, no
//! central `DrawOp` enum.

use std::collections::BTreeMap;

/// Blitz DOM node id, keyed throughout `Drawables`. Same shape as
/// `pagination_layout::PaginationGeometryTable`'s key.
pub type NodeId = usize;

/// A `BTreeMap<NodeId, V>` that also records the *order* keys were inserted,
/// so a caller can recover "which NodeIds were inserted since an earlier
/// point" in O(inserted-since) time instead of scanning the whole map.
///
/// ## Why this exists (fulgur-vrkv)
///
/// Several convert passes need the set of drawable NodeIds produced while
/// converting one node's subtree — the clip / opacity / transform *descendant*
/// lists and the inline-box skip table. The original implementation
/// snapshot-diffed *every* drawable map before and after the recursion
/// (`collect_drawables_node_ids`): O(total drawables) per scope, and therefore
/// O(N²) across a document with N such scopes (e.g. N sibling
/// `opacity < 1` blocks). `fulgur-v1cm` gated the transform snapshot and
/// `fulgur-vrkv` replaced the boolean "did anything change?" probes with the
/// O(1) `drawables_total_len`, but the passes that need the actual *set* of
/// new NodeIds were left on the quadratic path.
///
/// `TrackedMap` makes that set recoverable in O(inserted-since):
/// [`insert`](TrackedMap::insert) appends to an insertion log,
/// [`Drawables::draw_mark`] captures the current log lengths, and
/// [`Drawables::drawn_since`] unions the six logs' tails.
///
/// ## Invariants that keep PDF output byte-identical
///
/// - `insert` is the *only* way to add a key: there is no `DerefMut`, so any
///   other mutation path (`.entry`, `.extend`, `iter_mut`) fails to compile
///   rather than silently bypassing the log. `get_mut` mutates an existing
///   entry's value and never adds a key, so it does not log.
/// - Convert never removes entries, so the maps are append-only and a log's
///   tail after a mark is exactly the keys inserted since that mark.
/// - [`Drawables::drawn_since`] returns keys **sorted ascending,
///   deduplicated** — matching the old `BTreeSet::difference` output so the
///   descendant `Vec`s (which drive painter-order PDF emission) keep the same
///   byte layout.
#[derive(Debug, Clone)]
pub struct TrackedMap<V> {
    map: BTreeMap<NodeId, V>,
    /// Append-only log of every key passed to `insert`, in call order. May
    /// contain duplicates when a key is re-inserted (overwritten); readers of
    /// a tail deduplicate via `Drawables::drawn_since`.
    order: Vec<NodeId>,
}

impl<V> Default for TrackedMap<V> {
    fn default() -> Self {
        Self {
            map: BTreeMap::new(),
            order: Vec::new(),
        }
    }
}

impl<V> std::ops::Deref for TrackedMap<V> {
    type Target = BTreeMap<NodeId, V>;
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl<V> TrackedMap<V> {
    /// Insert `value` for `key`, recording the insertion in the order log.
    /// Deliberately shadows `BTreeMap::insert` (otherwise reachable only via a
    /// `DerefMut` this type intentionally does not provide) so every
    /// insertion — current or future — is logged automatically.
    pub fn insert(&mut self, key: NodeId, value: V) -> Option<V> {
        self.order.push(key);
        self.map.insert(key, value)
    }

    /// Mutable access to an existing entry's value. Adds no key, so it is not
    /// logged. Returns `None` when `key` is absent.
    pub fn get_mut(&mut self, key: &NodeId) -> Option<&mut V> {
        self.map.get_mut(key)
    }

    /// Current insertion-log length (a mark for [`Self::since`]). Distinct
    /// from the map's `len` — the log counts insertion *events*, including
    /// re-inserts of an existing key.
    fn mark(&self) -> usize {
        self.order.len()
    }

    /// Keys inserted after `mark`, in raw call order (may contain duplicates).
    fn since(&self, mark: usize) -> &[NodeId] {
        &self.order[mark..]
    }
}

/// Opaque snapshot of every tracked map's insertion-log length, captured by
/// [`Drawables::draw_mark`] and consumed by [`Drawables::drawn_since`].
#[derive(Debug, Clone, Copy)]
pub struct DrawMark {
    block_styles: usize,
    paragraphs: usize,
    images: usize,
    svgs: usize,
    tables: usize,
    list_items: usize,
}

// ── Entry types ──────────────────────────────────────────────────────

/// Block draw payload: backgrounds, borders, box-shadow, overflow clip,
/// opacity, and the anchor id used by `DestinationRegistry`.
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
    /// CSS `opacity` semantics: the block's opacity wraps EVERYTHING —
    /// bg / border / shadow + descendants — so a
    /// `<div style="opacity:0.4"><svg>..</svg></div>` produces a
    /// single transparency group. `render_v2`'s flat dispatch would
    /// otherwise emit the svg outside the parent's opacity wrap and
    /// drop the parent's opacity from the svg.
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
    pub width: crate::units::Pt,
    pub height: crate::units::Pt,
    pub opacity: f32,
    pub visible: bool,
}

/// SVG draw payload for v2. Mirrors the fields `SvgRender` holds.
///
/// `tree` is `Arc<usvg::Tree>` — an external-crate type. Consumers that
/// construct, inspect, or pattern-match on `usvg::Tree` directly (rather
/// than treating it as opaque) must depend on the same `usvg` version
/// range this crate resolves (see the `usvg` entry in this crate's
/// `Cargo.toml`). Rust type identity is per-resolved-crate-instance: a
/// `usvg::Tree` from a differently-resolved `usvg` dependency is a
/// distinct type and will not interoperate with fulgur's, even across a
/// nominally-minor 0.x bump.
#[derive(Debug, Clone)]
pub struct SvgEntry {
    pub tree: std::sync::Arc<usvg::Tree>,
    pub width: crate::units::Pt,
    pub height: crate::units::Pt,
    pub opacity: f32,
    pub visible: bool,
}

/// Table draw payload for v2. Holds the border-box paint state
/// (background / borders / shadow) applied to the table's outer
/// frame. Cell content (`<th>` / `<td>`) lives as separate
/// `BlockEntry` / `ParagraphEntry` keyed by the cell's own NodeId
/// and paints through the standard per-NodeId dispatch.
///
/// Multi-page header repetition is represented by pagination geometry.
#[derive(Debug, Clone)]
pub struct TableEntry {
    pub style: crate::draw_primitives::BlockStyle,
    pub opacity: f32,
    pub visible: bool,
    pub id: Option<std::sync::Arc<String>>,
    pub layout_size: Option<crate::draw_primitives::Size>,
    pub width: crate::units::Pt,
    pub cached_height: crate::units::Pt,
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
        width: crate::units::Pt,
    },
    /// Image marker (`list-style-image: url(...)`) — raster or SVG.
    Image {
        marker: ImageMarker,
        /// Display width after clamp (pt).
        width: crate::units::Pt,
        /// Display height after clamp (pt).
        height: crate::units::Pt,
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
    pub marker_line_height: crate::units::Pt,
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

/// Per-column-group geometry for painting a multicol `column-rule`, in PDF
/// pt (already converted from `multicol_layout::ColumnGroupGeometry`'s CSS
/// px by `convert::record_multicol_rule`). A distinct pt-typed carrier so
/// the px source struct is no longer reused across two unit spaces.
#[derive(Debug, Clone)]
pub struct ColumnRuleGeometry {
    /// Horizontal offset from the container border-box left to column 0.
    pub x_offset: crate::units::Pt,
    /// Vertical offset from the container border-box top (incl. padding-top
    /// + border-top) to this group.
    pub y_offset: crate::units::Pt,
    /// Width of a single column.
    pub col_w: crate::units::Pt,
    /// Gap between adjacent columns.
    pub gap: crate::units::Pt,
    /// Number of columns this group balances across.
    pub n: u32,
    /// Per-column filled height; length == `n`.
    pub col_heights: Vec<crate::units::Pt>,
}

/// Multicol column-rule paint spec + per-column-group geometry.
/// Rendered at the container's location after children paint,
/// partitioning `groups` per page based on the container's fragment
/// cumulative heights.
#[derive(Debug, Clone)]
pub struct MulticolRuleEntry {
    pub rule: crate::column_css::ColumnRuleSpec,
    pub groups: Vec<ColumnRuleGeometry>,
}

/// One source paragraph distributed across columns of a multicol
/// container. Built from `multicol_layout::ParagraphSplitEntry`. The
/// per-slice `lines` are pre-rebased so each slice's first line has
/// `baseline = ascent` (i.e. `y=0` is the slice's top edge), matching
/// the baseline-rebase convention used for second fragments after a
/// page-break (see commit 9c0e092).
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
    pub origin_pt: (crate::units::Pt, crate::units::Pt),
    /// Slice size — `col_w × Σ line_height(slice_lines)` in pt.
    pub size_pt: (crate::units::Pt, crate::units::Pt),
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
/// `render_v2`'s flat dispatch loop has no implicit scope: to keep the
/// transform in effect for every descendant fragment, convert records
/// every descendant `node_id` of the wrapper here so the render loop
/// can dispatch the wrapper's own payload + every descendant inside
/// one push/pop pair.
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
#[derive(Debug, Clone)]
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
    pub body_offset_pt: (crate::units::Pt, crate::units::Pt),
    /// `true` when the root element (`<html>`) has `direction: rtl`.
    /// CSS Paged Media §5 specifies that when the root element is RTL
    /// the first page is a `:left` page instead of `:right`.
    pub root_dir_rtl: bool,
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
    pub block_styles: TrackedMap<BlockEntry>,
    pub paragraphs: TrackedMap<ParagraphEntry>,
    /// Per-source-paragraph multicol slicing emitted by
    /// `convert::convert_multicol_paragraph_slices` from
    /// `multicol_layout::MulticolGeometry::paragraph_splits`. When a
    /// `NodeId` has an entry, `render_v2`'s paragraph dispatcher renders
    /// one entry per non-empty column slice at the slice origin instead of
    /// the default single-rectangle path that uses `paragraphs[node_id]`.
    pub paragraph_slices: BTreeMap<NodeId, ParagraphSlicesEntry>,
    pub images: TrackedMap<ImageEntry>,
    pub svgs: TrackedMap<SvgEntry>,
    pub tables: TrackedMap<TableEntry>,
    pub list_items: TrackedMap<ListItemEntry>,
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
    /// 合成 NodeId カウンタ。usize::MAX / 2 から降順に割り当て。
    /// DOM NodeId（通常 < 100_000）との衝突を避けるため大きな値から開始する。
    pub synthetic_id_counter: usize,
    /// li NodeId → Lbl 合成 NodeId（render pass のマーカータグ付け用）
    pub li_lbl_ids: BTreeMap<NodeId, NodeId>,
    /// li NodeId → LBody 合成 NodeId（inline-root li の body タグ付け用）
    pub li_lbody_ids: BTreeMap<NodeId, NodeId>,
}

impl Default for Drawables {
    fn default() -> Self {
        Self {
            body_offset_pt: (crate::units::Pt::ZERO, crate::units::Pt::ZERO),
            root_dir_rtl: false,
            root_id: None,
            body_id: None,
            block_styles: TrackedMap::default(),
            paragraphs: TrackedMap::default(),
            paragraph_slices: BTreeMap::new(),
            images: TrackedMap::default(),
            svgs: TrackedMap::default(),
            tables: TrackedMap::default(),
            list_items: TrackedMap::default(),
            multicol_rules: BTreeMap::new(),
            transforms: BTreeMap::new(),
            bookmark_anchors: BTreeMap::new(),
            link_spans: Vec::new(),
            semantics: BTreeMap::new(),
            inline_box_subtree_skip: std::collections::BTreeSet::new(),
            inline_box_subtree_descendants: BTreeMap::new(),
            synthetic_id_counter: usize::MAX / 2,
            li_lbl_ids: BTreeMap::new(),
            li_lbody_ids: BTreeMap::new(),
        }
    }
}

impl Drawables {
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture the current insertion-log position of every tracked map. Pass
    /// the result to [`Self::drawn_since`] after converting a subtree to
    /// recover the NodeIds inserted in between. O(1).
    pub fn draw_mark(&self) -> DrawMark {
        DrawMark {
            block_styles: self.block_styles.mark(),
            paragraphs: self.paragraphs.mark(),
            images: self.images.mark(),
            svgs: self.svgs.mark(),
            tables: self.tables.mark(),
            list_items: self.list_items.mark(),
        }
    }

    /// NodeIds inserted into any tracked map since `mark`, **sorted ascending
    /// and deduplicated**.
    ///
    /// Drop-in replacement for the old
    /// `collect_drawables_node_ids(out).difference(&before)` snapshot diff:
    /// identical result set *and* order (both yield ascending unique keys),
    /// but O(inserted-since) instead of O(total drawables). The append-only
    /// invariant (convert never removes entries) makes the log tail equal to
    /// the set difference; see [`TrackedMap`]. Callers still apply their own
    /// `id != node_id` / skip-set filters on top.
    pub fn drawn_since(&self, mark: DrawMark) -> Vec<NodeId> {
        // Collect the six tails into a flat `Vec`, then `sort_unstable` +
        // `dedup` to get the same ascending-unique result a `BTreeSet` would
        // — but with one contiguous allocation instead of a per-node B-tree
        // allocation, which matters precisely because this runs once per
        // clip/opacity/transform scope. Tails are typically small.
        let mut ids: Vec<NodeId> = Vec::new();
        ids.extend(self.block_styles.since(mark.block_styles).iter().copied());
        ids.extend(self.paragraphs.since(mark.paragraphs).iter().copied());
        ids.extend(self.images.since(mark.images).iter().copied());
        ids.extend(self.svgs.since(mark.svgs).iter().copied());
        ids.extend(self.tables.since(mark.tables).iter().copied());
        ids.extend(self.list_items.since(mark.list_items).iter().copied());
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Minimum paragraph `NodeId` inserted since `mark`. `None` if no paragraph
    /// was inserted after `mark`. **O(paragraphs inserted since mark)**.
    ///
    /// Drop-in replacement for the old
    /// `paragraphs.keys().copied().find(|id| !pre.contains(id))` scan (which was
    /// O(all paragraphs)) — same NodeId under the convert invariant that each
    /// DOM NodeId is inserted at most once per map, so the `since` tail cannot
    /// contain an id that was already in the map before `mark`. See [`TrackedMap`].
    pub fn min_paragraph_since(&self, mark: DrawMark) -> Option<NodeId> {
        self.paragraphs.since(mark.paragraphs).iter().copied().min()
    }

    /// 合成 NodeId を 1 つ割り当てる。
    /// `usize::MAX / 2` から始まり降順に割り当てるので DOM NodeId と衝突しない。
    pub fn alloc_synthetic_id(&mut self) -> NodeId {
        let id = self.synthetic_id_counter;
        self.synthetic_id_counter = self.synthetic_id_counter.saturating_sub(1);
        id
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
    use crate::units::F32Units;

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
                width: 0.0_f32.as_pt(),
            },
            marker_line_height: 12.0_f32.as_pt(),
            opacity: 1.0,
            visible: true,
        };
        let s = format!("{:?}", entry);
        assert!(s.contains("ListItemEntry"));
        assert!(s.contains("marker_line_height"));
        assert!(s.contains("opacity"));
        assert!(s.contains("visible"));
    }

    #[test]
    fn alloc_synthetic_id_starts_at_usize_max_div_2() {
        let mut d = Drawables::default();
        let first = d.alloc_synthetic_id();
        assert_eq!(first, usize::MAX / 2);
    }

    #[test]
    fn alloc_synthetic_id_returns_unique_decreasing_ids() {
        let mut d = Drawables::default();
        let id1 = d.alloc_synthetic_id();
        let id2 = d.alloc_synthetic_id();
        let id3 = d.alloc_synthetic_id();
        assert!(id1 > id2, "IDs must decrease");
        assert!(id2 > id3, "IDs must decrease");
        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
    }

    // ── TrackedMap / draw_mark / drawn_since (fulgur-vrkv) ─────────────

    fn para() -> ParagraphEntry {
        ParagraphEntry {
            lines: Vec::new(),
            opacity: 1.0,
            visible: true,
            id: None,
        }
    }

    fn list_item() -> ListItemEntry {
        ListItemEntry {
            marker: ListItemMarker::Text {
                lines: Vec::new(),
                width: 0.0_f32.as_pt(),
            },
            marker_line_height: 12.0_f32.as_pt(),
            opacity: 1.0,
            visible: true,
        }
    }

    #[test]
    fn drawn_since_returns_only_the_tail_sorted_and_unique() {
        let mut d = Drawables::new();
        d.paragraphs.insert(7, para());
        d.paragraphs.insert(3, para());
        let mark = d.draw_mark();
        // Insert descendants out of key order; a duplicate re-insert too.
        d.paragraphs.insert(30, para());
        d.paragraphs.insert(10, para());
        d.paragraphs.insert(30, para()); // re-insert same key
        // Only post-mark keys, ascending, deduplicated — matches the old
        // `BTreeSet::difference` output the consumers relied on.
        assert_eq!(d.drawn_since(mark), vec![10, 30]);
    }

    #[test]
    fn drawn_since_is_independent_of_pre_mark_size() {
        // The anti-quadratic guarantee: work done per scope depends only on
        // what was inserted *since* the mark, never on how much accumulated
        // before it. A document of N such scopes is therefore O(total nodes),
        // not O(N²). Two marks over identical post-mark inserts but wildly
        // different pre-mark sizes must yield the same result.
        let mut small = Drawables::new();
        small.paragraphs.insert(1, para());
        let m_small = small.draw_mark();
        small.paragraphs.insert(1000, para());

        let mut big = Drawables::new();
        for id in 0..500 {
            big.paragraphs.insert(id, para());
        }
        let m_big = big.draw_mark();
        big.paragraphs.insert(1000, para());

        assert_eq!(small.drawn_since(m_small), vec![1000]);
        assert_eq!(big.drawn_since(m_big), vec![1000]);
    }

    #[test]
    fn drawn_since_unions_all_tracked_maps() {
        let mut d = Drawables::new();
        let mark = d.draw_mark();
        d.paragraphs.insert(5, para());
        d.list_items.insert(2, list_item());
        d.paragraphs.insert(9, para());
        // Keys from different maps merge into one ascending unique list.
        assert_eq!(d.drawn_since(mark), vec![2, 5, 9]);
    }

    #[test]
    fn drawn_since_empty_when_nothing_inserted_after_mark() {
        let mut d = Drawables::new();
        d.paragraphs.insert(1, para());
        let mark = d.draw_mark();
        assert!(d.drawn_since(mark).is_empty());
    }

    #[test]
    fn tracked_map_reads_pass_through_via_deref() {
        let mut d = Drawables::new();
        d.paragraphs.insert(42, para());
        // len / get / contains_key / keys reach BTreeMap through Deref.
        assert_eq!(d.paragraphs.len(), 1);
        assert!(d.paragraphs.contains_key(&42));
        assert!(d.paragraphs.get(&42).is_some());
        assert_eq!(d.paragraphs.keys().copied().collect::<Vec<_>>(), vec![42]);
    }

    // ── min_paragraph_since (fulgur-un8f) ─────────────────────────────

    fn block_entry() -> BlockEntry {
        BlockEntry {
            style: crate::draw_primitives::BlockStyle::default(),
            opacity: 1.0,
            visible: true,
            id: None,
            layout_size: None,
            clip_descendants: vec![],
            opacity_descendants: vec![],
        }
    }

    #[test]
    fn min_paragraph_since_none_when_no_paragraphs_inserted_after_mark() {
        let mut d = Drawables::default();
        d.paragraphs.insert(5, para());
        let mark = d.draw_mark();
        // 挿入なし → None
        assert_eq!(d.min_paragraph_since(mark), None);
    }

    #[test]
    fn min_paragraph_since_returns_lowest_new_node_id() {
        let mut d = Drawables::default();
        d.paragraphs.insert(1, para()); // pre-mark
        let mark = d.draw_mark();
        d.paragraphs.insert(10, para()); // 挿入順に依存せず
        d.paragraphs.insert(5, para());
        d.paragraphs.insert(20, para());
        // 挿入順 (10, 5, 20) だが min は 5
        assert_eq!(d.min_paragraph_since(mark), Some(5));
    }

    #[test]
    fn min_paragraph_since_ignores_other_maps() {
        let mut d = Drawables::default();
        let mark = d.draw_mark();
        d.block_styles.insert(1, block_entry());
        d.paragraphs.insert(7, para());
        // block_styles は無視、paragraphs のみ
        assert_eq!(d.min_paragraph_since(mark), Some(7));
    }
}
