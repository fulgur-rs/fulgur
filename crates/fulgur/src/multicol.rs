//! CSS Multi-column Layout.
//!
//! taffy 0.9.2 has no multicol layout mode and blitz-dom 0.2.4 treats multicol
//! containers as regular blocks, so fulgur owns multicol layout between Blitz
//! (which supplies the container's content box) and Krilla (which renders the
//! final fragments).
//!
//! Design: `docs/plans/2026-04-20-css-multicol-design.md`. Phase A ships the
//! common case (`column-fill: auto`, `column-span: all`, page-spanning,
//! `column-rule-*`, `break-inside: avoid`); `column-fill: balance` and
//! `break-before`/`break-after` land in Phase B.
//!
//! ## Phase A-2 spike status
//!
//! `wrap` / `split` / `draw` now implement real `column-fill: auto` layout:
//! children are reshaped and re-wrapped at the resolved column width, then
//! greedily distributed across N columns against the available page height.
//! Text is re-broken through
//! [`crate::paragraph::ParagraphPageable::reshape_at`] so multicol content
//! flows within the narrower column boundary instead of inheriting the
//! container's Parley layout.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::pageable::{
    BlockPageable, BlockStyle, Canvas, DestinationRegistry, Pageable, PositionedChild, Pt, Size,
    SplitResult,
};

/// Resolved multicol container properties.
///
/// Re-exported from [`crate::blitz_adapter::MulticolProps`] for consumers of
/// this module that should not reach into the adapter.
pub use crate::blitz_adapter::MulticolProps;

/// A horizontal strip inside a multicol container.
///
/// A container is decomposed into alternating `ColumnGroup` / `SpanAll`
/// segments at children carrying `column-span: all`. Phase A-1 only emits a
/// single `ColumnGroup`; Phase A-3 adds `SpanAll` detection in `convert.rs`.
pub enum Segment {
    /// Flows across N columns.
    ColumnGroup(Vec<Box<dyn Pageable>>),
    /// Full-width strip (a descendant with `column-span: all`).
    SpanAll(Box<dyn Pageable>),
}

impl std::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Segment::ColumnGroup(children) => f
                .debug_struct("ColumnGroup")
                .field("child_count", &children.len())
                .finish(),
            Segment::SpanAll(_) => f.debug_struct("SpanAll").finish(),
        }
    }
}

impl Clone for Segment {
    fn clone(&self) -> Self {
        match self {
            Segment::ColumnGroup(children) => {
                Segment::ColumnGroup(children.iter().map(|c| c.clone_box()).collect())
            }
            Segment::SpanAll(child) => Segment::SpanAll(child.clone_box()),
        }
    }
}

/// `column-rule-*` resolved into render-ready form. Populated by the
/// custom CSS parser added in Phase A-4; Phase A-1 always leaves it `None`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColumnRule {
    pub width: Pt,
    pub style: crate::pageable::BorderStyleValue,
    pub color: [u8; 4],
}

/// Column-fill mode.
///
/// `column-fill: balance` is the CSS default and distributes content into
/// roughly equal columns. `column-fill: auto` fills the first column
/// completely before moving on. Phase A-2 ships balance as the default;
/// auto is the fallback used when content overflows the balance budget and
/// will be user-selectable once the A-4 custom CSS parser lands (stylo's
/// servo engine does not expose `column-fill`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColumnFill {
    #[default]
    Balance,
    Auto,
}

/// Multi-column container Pageable.
pub struct MulticolPageable {
    pub props: MulticolProps,
    pub segments: Vec<Segment>,
    pub column_rule: Option<ColumnRule>,
    pub fill: ColumnFill,
    pub style: BlockStyle,
    pub opacity: f32,
    pub visible: bool,
    pub id: Option<Arc<String>>,
    /// Resolved column count. Filled by `wrap()`; `1` before that.
    pub resolved_count: u32,
    /// Resolved column width in Pt. Filled by `wrap()`; `0.0` before that.
    pub resolved_col_w: Pt,
    /// Taffy-computed size for this container. Used as the explicit column
    /// budget when present (e.g. `<div style="column-count:2;height:200pt">`);
    /// see [`MulticolPageable::effective_column_budget`].
    pub layout_size: Option<Size>,
    /// Phase A-1 fallback. Phase A-2 replaces this with a real column layout.
    pub(crate) fallback: Box<BlockPageable>,
}

impl std::fmt::Debug for MulticolPageable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MulticolPageable")
            .field("props", &self.props)
            .field("segment_count", &self.segments.len())
            .field("column_rule", &self.column_rule)
            .field("resolved_count", &self.resolved_count)
            .field("resolved_col_w", &self.resolved_col_w)
            .finish()
    }
}

impl Clone for MulticolPageable {
    fn clone(&self) -> Self {
        Self {
            props: self.props,
            segments: self.segments.clone(),
            column_rule: self.column_rule,
            fill: self.fill,
            style: self.style.clone(),
            opacity: self.opacity,
            visible: self.visible,
            id: self.id.clone(),
            resolved_count: self.resolved_count,
            resolved_col_w: self.resolved_col_w,
            layout_size: self.layout_size,
            fallback: self.fallback.clone(),
        }
    }
}

impl MulticolPageable {
    /// Construct a new multicol container.
    ///
    /// `fallback` is retained for compatibility with A-1 call sites but no
    /// longer participates in rendering — A-2 lays out the segments
    /// directly. It stays around as a measurement aid (Taffy-reported
    /// natural size) until the convert path is tidied up in a follow-up.
    pub fn new(
        props: MulticolProps,
        segments: Vec<Segment>,
        style: BlockStyle,
        opacity: f32,
        visible: bool,
        id: Option<Arc<String>>,
        fallback: BlockPageable,
    ) -> Self {
        Self {
            props,
            segments,
            column_rule: None,
            fill: ColumnFill::default(),
            style,
            opacity,
            visible,
            id,
            resolved_count: 1,
            resolved_col_w: 0.0,
            layout_size: None,
            fallback: Box::new(fallback),
        }
    }

    /// Attach Taffy's computed size so the column-fill heuristics can use
    /// the explicit CSS `height` as a column budget. Without this, short
    /// multicol containers fall back to the page's available height which
    /// is usually much larger than the author intended.
    pub fn with_layout_size(mut self, size: Size) -> Self {
        self.layout_size = Some(size);
        self
    }

    /// Override the column-fill mode. Defaults to balance.
    pub fn with_fill(mut self, fill: ColumnFill) -> Self {
        self.fill = fill;
        self
    }

    /// Per-column height budget.
    ///
    /// We originally tried to respect `layout_size.height` (Taffy's
    /// computed box height) here so explicit `height: Xpt` on a multicol
    /// container would cap the column. The trouble is that Taffy sizes
    /// the box at *pre-reshape* single-column content height, which is
    /// typically smaller than the post-reshape total. Using that as the
    /// budget forces balance to fall back to auto and drop overflow on
    /// the floor. Until we can distinguish author-set `height` from
    /// Taffy's fit-content fallback, prefer the page budget and let
    /// balance find a per-column height that fits. A future A-4 stylesheet
    /// parse can flip this back when it knows the author set `height`.
    fn effective_column_budget(&self, avail_h: Pt) -> Pt {
        avail_h
    }

    /// Layout children into columns with the configured fill mode.
    ///
    /// `avail_h` is the per-column hard upper bound (page / container
    /// budget). For `ColumnFill::Balance`, the chosen per-column budget
    /// never exceeds `avail_h` but may be smaller so that all content fits
    /// in `n` roughly-equal columns. For `ColumnFill::Auto`, columns fill
    /// to `avail_h` in order.
    fn distribute_with_fill(
        children_source: &[Box<dyn Pageable>],
        n: u32,
        col_w: Pt,
        col_gap: Pt,
        avail_h: Pt,
        fill: ColumnFill,
    ) -> (Vec<PositionedChild>, Vec<Box<dyn Pageable>>) {
        // Reshape + measure children at col_w once up front; distribute
        // iterations below operate on clones and re-read heights but do
        // not re-shape.
        let mut measured: Vec<Box<dyn Pageable>> = children_source
            .iter()
            .map(|c| {
                let mut c = c.clone_box();
                c.reshape_for_width(col_w);
                c.wrap(col_w, 10000.0);
                c
            })
            .collect();

        let total: Pt = measured.iter().map(|c| c.height()).sum();

        let budget = match fill {
            ColumnFill::Auto => avail_h,
            ColumnFill::Balance => {
                // When content overflows `avail_h * n` the column-fill
                // spec lets us behave like auto; otherwise start at the
                // ideal `total / n` and grow until content fits in `n`
                // columns without overflow.
                if total >= avail_h * n as f32 {
                    avail_h
                } else {
                    Self::balanced_budget(&measured, n, col_w, col_gap, avail_h, total)
                }
            }
        };
        // Take ownership back to avoid re-cloning: distribute_children
        // clones again inside, but the helper takes `&[Box<dyn Pageable>]`
        // and we no longer need `measured` after this call.
        let children_refs = std::mem::take(&mut measured);
        Self::distribute_children(&children_refs, n, col_w, col_gap, budget)
    }

    /// Search for the smallest per-column budget (within `[total/n, avail_h]`)
    /// that keeps the greedy distribution to `n` columns with no overflow.
    /// Linear scan at 5% of `avail_h` increments; cheap in practice (≤20
    /// iterations) and stable vs floating-point edge cases.
    fn balanced_budget(
        children: &[Box<dyn Pageable>],
        n: u32,
        col_w: Pt,
        col_gap: Pt,
        avail_h: Pt,
        total_h: Pt,
    ) -> Pt {
        let ideal = (total_h / n as f32).ceil().max(1.0);
        let step = (avail_h / 20.0).max(1.0);
        let mut budget = ideal;
        while budget <= avail_h {
            let (_, overflow) = Self::distribute_children(children, n, col_w, col_gap, budget);
            if overflow.is_empty() {
                return budget;
            }
            budget += step;
        }
        avail_h
    }

    /// Greedy `column-fill: auto` distribution primitive used by both the
    /// auto path directly and as the inner iteration step for balance.
    ///
    /// Takes children *already reshaped / measured at `col_w`* and places
    /// them into `n` columns of height `avail_h`, splitting mid-child
    /// where supported. Returns the placed children with absolute column
    /// offsets plus whatever did not fit.
    fn distribute_children(
        children_source: &[Box<dyn Pageable>],
        n: u32,
        col_w: Pt,
        col_gap: Pt,
        avail_h: Pt,
    ) -> (Vec<PositionedChild>, Vec<Box<dyn Pageable>>) {
        let mut queue: VecDeque<Box<dyn Pageable>> =
            children_source.iter().map(|c| c.clone_box()).collect();

        // After a split, the remainder has no cached_size / layout_size
        // (BlockPageable::split returns fresh children). Re-wrap at col_w
        // before placing so the fragment draws its background at the
        // column width, not the container width.
        let rewrap_at_col = |child: &mut Box<dyn Pageable>| {
            child.wrap(col_w, 10000.0);
        };

        let mut placed: Vec<PositionedChild> = Vec::new();
        for col_idx in 0..n {
            let col_x = col_idx as f32 * (col_w + col_gap);
            let mut col_y: Pt = 0.0;
            while let Some(mut child) = queue.pop_front() {
                // Ensure child has cached_size at col_w so its background /
                // border draw at the column width (split fragments come in
                // with no size info).
                rewrap_at_col(&mut child);
                let h = child.height();
                if col_y + h <= avail_h {
                    placed.push(PositionedChild {
                        child,
                        x: col_x,
                        y: col_y,
                    });
                    col_y += h;
                    continue;
                }
                let budget = (avail_h - col_y).max(0.0);
                if budget > 0.0 {
                    if let Some((mut first, mut rest)) = child.split(col_w, budget) {
                        rewrap_at_col(&mut first);
                        rewrap_at_col(&mut rest);
                        placed.push(PositionedChild {
                            child: first,
                            x: col_x,
                            y: col_y,
                        });
                        queue.push_front(rest);
                        break; // column full, advance
                    }
                }
                // Can't split — at col_y=0 we place the oversized child to
                // avoid losing content; otherwise push back and advance.
                if col_y == 0.0 {
                    placed.push(PositionedChild {
                        child,
                        x: col_x,
                        y: col_y,
                    });
                    break;
                }
                queue.push_front(child);
                break;
            }
            if queue.is_empty() {
                break;
            }
        }

        (placed, queue.into_iter().collect())
    }

    /// Gather the set of children currently owned by ColumnGroup segments.
    /// In Phase A-2 this is a single segment; A-3 concatenates across
    /// multiple segments (interleaving SpanAll which is not flowed).
    fn column_flow_children(&self) -> Vec<Box<dyn Pageable>> {
        let mut out = Vec::new();
        for seg in &self.segments {
            if let Segment::ColumnGroup(children) = seg {
                for c in children {
                    out.push(c.clone_box());
                }
            }
        }
        out
    }

    /// Horizontal border+padding inset (left + right) applied to the
    /// border-box so column widths use the actual content box.
    fn horizontal_inset(&self) -> Pt {
        self.style.border_widths[1]
            + self.style.border_widths[3]
            + self.style.padding[1]
            + self.style.padding[3]
    }

    fn resolved_col_params(&self, avail_w: Pt) -> (u32, Pt, Pt) {
        // Prefer the container's own width (from Taffy via `layout_size`)
        // when available. `layout_size.width` is the border box, so we
        // subtract padding + border to get the content box that columns
        // are laid out inside.
        let border_box_w = self
            .layout_size
            .map(|s| s.width)
            .filter(|w| *w > 0.0)
            .unwrap_or(avail_w);
        let content_w = (border_box_w - self.horizontal_inset()).max(0.0);
        let gap = self.props.column_gap.max(0.0);
        let (n, col_w) = resolve_column_layout(
            content_w,
            self.props.column_count,
            self.props.column_width,
            gap,
        );
        (n, col_w, gap)
    }
}

/// Resolve the CSS `column-count` / `column-width` pair into a concrete
/// `(used_count, used_column_width)` for the given content width.
///
/// Spec reference: <https://drafts.csswg.org/css-multicol/#cw>
///
/// - `column-count: N`, `column-width: auto` → N columns.
/// - `column-count: auto`, `column-width: W` → floor((content+gap)/(W+gap))
///   columns, at least one.
/// - Both present → used count capped by the column-width derived maximum.
/// - Neither present → caller should not invoke this (the container is not a
///   multicol).
///
/// Uses `.floor()` explicitly to keep the calculation deterministic across
/// platforms. A degenerate `content_w < W` still yields at least one column
/// whose width collapses to `content_w`.
pub fn resolve_column_layout(
    content_w: Pt,
    count: Option<u32>,
    width: Option<Pt>,
    gap: Pt,
) -> (u32, Pt) {
    let capped = |raw_n: u32, total_w: f32, gap: f32| -> (u32, f32) {
        let n = raw_n.max(1);
        let col_w = (total_w - gap * (n as f32 - 1.0)) / n as f32;
        (n, col_w.max(0.0))
    };
    // A column-width of 0 (or negative) would either blow up or yield a
    // runaway count; fall back to "width unconstrained".
    let width = width.filter(|&w| w > 0.0);
    match (count, width) {
        (Some(n), None) => capped(n, content_w, gap),
        (None, Some(w)) => {
            let denom = w + gap;
            let raw = if denom > 0.0 {
                ((content_w + gap) / denom).floor() as u32
            } else {
                1
            };
            capped(raw, content_w, gap)
        }
        (Some(n), Some(w)) => {
            let denom = w + gap;
            let cap = if denom > 0.0 {
                ((content_w + gap) / denom).floor() as u32
            } else {
                1
            };
            let used = n.min(cap.max(1));
            capped(used, content_w, gap)
        }
        (None, None) => (1, content_w.max(0.0)),
    }
}

impl Pageable for MulticolPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        let (n, col_w, _gap) = self.resolved_col_params(avail_width);
        self.resolved_count = n;
        self.resolved_col_w = col_w;

        // Reshape + re-wrap children inside segments at col_w so their
        // cached heights reflect the narrow column measurement used below.
        for seg in &mut self.segments {
            match seg {
                Segment::ColumnGroup(children) => {
                    for child in children {
                        child.reshape_for_width(col_w);
                        child.wrap(col_w, 10000.0);
                    }
                }
                Segment::SpanAll(child) => {
                    child.reshape_for_width(avail_width);
                    child.wrap(avail_width, 10000.0);
                }
            }
        }

        let total_body_h: Pt = self
            .segments
            .iter()
            .map(|seg| match seg {
                Segment::ColumnGroup(children) => children.iter().map(|c| c.height()).sum(),
                Segment::SpanAll(c) => c.height(),
            })
            .sum();
        // column-fill: auto — with N columns, the container's reported
        // height caps at the available page height once content spans
        // across columns. Shorter content just uses col 0's height.
        let height = if total_body_h > avail_height {
            avail_height
        } else {
            total_body_h
        };
        Size {
            width: avail_width,
            height,
        }
    }

    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        let (n, col_w, gap) = self.resolved_col_params(avail_width);
        let budget = self.effective_column_budget(avail_height);
        let children = self.column_flow_children();
        let (mut placed, overflow) =
            Self::distribute_with_fill(&children, n, col_w, gap, budget, self.fill);
        if overflow.is_empty() {
            // Everything fits — `draw()` will re-run the distribution.
            return None;
        }
        // Shift columns into the content box so the fragment's style can
        // paint border/padding around them.
        let (inset_x, inset_y) = self.style.content_inset();
        for pc in &mut placed {
            pc.x += inset_x;
            pc.y += inset_y;
        }
        // First fragment: styled BlockPageable with column-positioned
        // children for the current page.
        let mut fragment = BlockPageable::with_positioned_children(placed)
            .with_style(self.style.clone())
            .with_opacity(self.opacity)
            .with_visible(self.visible)
            .with_id(self.id.clone());
        fragment.wrap(avail_width, avail_height);
        // Remainder: new multicol container carrying the unplaced flow.
        let remainder = MulticolPageable {
            props: self.props,
            segments: vec![Segment::ColumnGroup(overflow)],
            column_rule: self.column_rule,
            fill: self.fill,
            style: self.style.clone(),
            opacity: self.opacity,
            visible: self.visible,
            id: self.id.clone(),
            resolved_count: self.resolved_count,
            resolved_col_w: self.resolved_col_w,
            layout_size: self.layout_size,
            fallback: self.fallback.clone(),
        };
        Some((Box::new(fragment), Box::new(remainder)))
    }

    fn split_boxed(self: Box<Self>, avail_width: Pt, avail_height: Pt) -> SplitResult {
        match self.split(avail_width, avail_height) {
            Some(pair) => Ok(pair),
            None => Err(self.clone_box()),
        }
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        let (n, col_w, gap) = self.resolved_col_params(avail_width);
        let budget = self.effective_column_budget(avail_height);
        let children = self.column_flow_children();
        let (mut placed, _overflow) =
            Self::distribute_with_fill(&children, n, col_w, gap, budget, self.fill);
        // Shift columns inside the content box (past border + padding).
        let (inset_x, inset_y) = self.style.content_inset();
        for pc in &mut placed {
            pc.x += inset_x;
            pc.y += inset_y;
        }
        // Render via a throwaway BlockPageable so we inherit background /
        // border / opacity handling without duplicating that code here.
        let mut frame = BlockPageable::with_positioned_children(placed)
            .with_style(self.style.clone())
            .with_opacity(self.opacity)
            .with_visible(self.visible)
            .with_id(self.id.clone());
        frame.wrap(avail_width, avail_height);
        frame.draw(canvas, x, y, avail_width, avail_height);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        // Prefer the measured total-body height so paginate knows whether
        // a split is required. Zero when wrap() has not been called yet.
        self.segments
            .iter()
            .map(|seg| match seg {
                Segment::ColumnGroup(children) => children.iter().map(|c| c.height()).sum(),
                Segment::SpanAll(c) => c.height(),
            })
            .sum()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn reshape_for_width(&mut self, _avail_width: Pt) {
        // Multicol re-derives column width from its own props on every
        // wrap() call, so no-op here.
    }

    fn collect_ids(
        &self,
        x: Pt,
        y: Pt,
        avail_width: Pt,
        avail_height: Pt,
        registry: &mut DestinationRegistry,
    ) {
        if let Some(id) = &self.id
            && !id.is_empty()
        {
            registry.record(id, x, y);
        }
        // Anchor resolution still walks the undistributed tree — good
        // enough for A-2. A follow-up can record per-column positions.
        for seg in &self.segments {
            match seg {
                Segment::ColumnGroup(children) => {
                    for c in children {
                        c.collect_ids(x, y, avail_width, avail_height, registry);
                    }
                }
                Segment::SpanAll(c) => {
                    c.collect_ids(x, y, avail_width, avail_height, registry);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_column_layout: count only ───────────────────────────
    #[test]
    fn count_only_three_columns() {
        let (n, w) = resolve_column_layout(300.0, Some(3), None, 10.0);
        assert_eq!(n, 3);
        // (300 - 20) / 3 = 93.33..
        assert!((w - 93.333_33).abs() < 1e-3, "got {w}");
    }

    #[test]
    fn count_only_one_column_no_gap_subtraction() {
        let (n, w) = resolve_column_layout(400.0, Some(1), None, 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 400.0);
    }

    #[test]
    fn count_only_zero_clamps_to_one() {
        // column-count: 0 is invalid CSS but we defend against it.
        let (n, w) = resolve_column_layout(400.0, Some(0), None, 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 400.0);
    }

    // ── resolve_column_layout: width only ───────────────────────────
    #[test]
    fn width_only_derives_count() {
        // (400 + 10) / (180 + 10) = 2.157 → floor = 2
        let (n, w) = resolve_column_layout(400.0, None, Some(180.0), 10.0);
        assert_eq!(n, 2);
        // (400 - 10) / 2 = 195
        assert!((w - 195.0).abs() < 1e-3);
    }

    #[test]
    fn width_only_too_wide_collapses_to_one() {
        let (n, w) = resolve_column_layout(200.0, None, Some(400.0), 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 200.0);
    }

    #[test]
    fn width_only_zero_gap() {
        // content=300, W=100, gap=0 → floor(300/100)=3 cols of 100.
        let (n, w) = resolve_column_layout(300.0, None, Some(100.0), 0.0);
        assert_eq!(n, 3);
        assert!((w - 100.0).abs() < 1e-3);
    }

    // ── resolve_column_layout: both present ─────────────────────────
    #[test]
    fn both_count_wins_when_narrower() {
        // count=2 vs width-derived-max = floor((600+10)/(100+10)) = 5 → 2 used.
        let (n, w) = resolve_column_layout(600.0, Some(2), Some(100.0), 10.0);
        assert_eq!(n, 2);
        assert!((w - 295.0).abs() < 1e-3);
    }

    #[test]
    fn both_width_cap_wins_when_count_too_high() {
        // count=10 vs width-derived-max = floor((400+10)/(180+10)) = 2 → 2 used.
        let (n, w) = resolve_column_layout(400.0, Some(10), Some(180.0), 10.0);
        assert_eq!(n, 2);
        assert!((w - 195.0).abs() < 1e-3);
    }

    #[test]
    fn both_extremely_narrow_container_still_one_column() {
        let (n, w) = resolve_column_layout(50.0, Some(4), Some(200.0), 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 50.0);
    }

    // ── resolve_column_layout: edge cases ───────────────────────────
    #[test]
    fn neither_present_falls_back_to_single_column() {
        let (n, w) = resolve_column_layout(400.0, None, None, 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 400.0);
    }

    #[test]
    fn zero_content_width_never_produces_negative() {
        let (n, w) = resolve_column_layout(0.0, Some(3), None, 10.0);
        assert_eq!(n, 3);
        assert!(w >= 0.0, "column width must be clamped non-negative");
    }

    #[test]
    fn gap_exceeds_content_width_clamps_col_width_to_zero() {
        // content=50, cols=3, gap=40 → total gaps=80 > 50 → col_w clamped to 0.
        let (n, w) = resolve_column_layout(50.0, Some(3), None, 40.0);
        assert_eq!(n, 3);
        assert_eq!(w, 0.0);
    }

    #[test]
    fn width_zero_degenerates_safely() {
        // column-width: 0 would divide by gap only; guard against it.
        let (n, w) = resolve_column_layout(300.0, None, Some(0.0), 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 300.0);
    }

    // ── A-2 spike: end-to-end text reshape through multicol ─────────
    //
    // The spike proves that inline-root paragraph content can be
    // re-broken at the column width when a MulticolPageable wraps it.
    // We render the same text with and without a multicol container and
    // assert the multicol version has strictly more lines (because
    // col_w < container_w forces more line breaks).

    fn count_paragraph_lines(tree: &dyn Pageable) -> Option<usize> {
        use crate::paragraph::ParagraphPageable;
        if let Some(pg) = tree.as_any().downcast_ref::<ParagraphPageable>() {
            return Some(pg.lines.len());
        }
        if let Some(b) = tree.as_any().downcast_ref::<BlockPageable>() {
            for pc in &b.children {
                if let Some(n) = count_paragraph_lines(&*pc.child) {
                    return Some(n);
                }
            }
        }
        if let Some(mc) = tree.as_any().downcast_ref::<MulticolPageable>() {
            for seg in &mc.segments {
                if let Segment::ColumnGroup(children) = seg {
                    for c in children {
                        if let Some(n) = count_paragraph_lines(&**c) {
                            return Some(n);
                        }
                    }
                }
            }
        }
        None
    }

    fn render_tree(html: &str) -> Box<dyn Pageable> {
        use crate::convert::ConvertContext;
        use crate::gcpm::running::RunningElementStore;
        use std::collections::HashMap;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let running_store = RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
        };
        crate::convert::dom_to_pageable(&doc, &mut ctx)
    }

    const LOREM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.";

    #[test]
    fn multicol_text_reshape_produces_more_lines_at_narrower_column() {
        let without_mc = format!(
            r#"<!doctype html><html><body style="font-family: serif;">
                <div><p>{LOREM}</p></div>
            </body></html>"#
        );
        let with_mc = format!(
            r#"<!doctype html><html><body style="font-family: serif;">
                <div style="column-count: 2; column-gap: 0;"><p>{LOREM}</p></div>
            </body></html>"#
        );
        let tree_no_mc = render_tree(&without_mc);
        let tree_mc = render_tree(&with_mc);

        let lines_no_mc = count_paragraph_lines(&*tree_no_mc).expect("paragraph found");
        let lines_mc = count_paragraph_lines(&*tree_mc).expect("paragraph found");

        assert!(
            lines_mc > lines_no_mc,
            "text inside multicol should re-break at column width: \
             container_lines={lines_no_mc}, col_lines={lines_mc}"
        );
        // Sanity: roughly double (2 columns means col_w ≈ half), allowing
        // slack because break points are not linear in width.
        assert!(
            lines_mc as f32 >= lines_no_mc as f32 * 1.5,
            "expected ~2x line count at col_w, got {lines_no_mc} → {lines_mc}"
        );
    }
}
