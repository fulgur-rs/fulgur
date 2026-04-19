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
//! ## This file in Phase A-1
//!
//! Contains the type scaffolding and `resolve_column_layout`. The `Pageable`
//! impl currently delegates to an inner `BlockPageable` fallback so that HTML
//! with `column-count` keeps rendering as a normal block (matching pre-feature
//! behaviour). Phase A-2 replaces the delegation with the real column-fill:
//! auto implementation.

use std::sync::Arc;

use crate::pageable::{
    BlockPageable, BlockStyle, Canvas, DestinationRegistry, Pageable, Pt, Size, SplitResult,
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

/// Multi-column container Pageable.
pub struct MulticolPageable {
    pub props: MulticolProps,
    pub segments: Vec<Segment>,
    pub column_rule: Option<ColumnRule>,
    pub style: BlockStyle,
    pub opacity: f32,
    pub visible: bool,
    pub id: Option<Arc<String>>,
    /// Resolved column count. Filled by `wrap()`; `1` before that.
    pub resolved_count: u32,
    /// Resolved column width in Pt. Filled by `wrap()`; `0.0` before that.
    pub resolved_col_w: Pt,
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
            style: self.style.clone(),
            opacity: self.opacity,
            visible: self.visible,
            id: self.id.clone(),
            resolved_count: self.resolved_count,
            resolved_col_w: self.resolved_col_w,
            fallback: self.fallback.clone(),
        }
    }
}

impl MulticolPageable {
    /// Construct a new multicol container.
    ///
    /// `fallback` is the plain-block rendition of the container; Phase A-1
    /// delegates `Pageable` methods to it. Phase A-2 will take over wrap /
    /// split / draw and leave `fallback` unused (retaining it for
    /// debug / regression safety during the transition).
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
            style,
            opacity,
            visible,
            id,
            resolved_count: 1,
            resolved_col_w: 0.0,
            fallback: Box::new(fallback),
        }
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
        let gap = if self.props.column_gap > 0.0 {
            self.props.column_gap
        } else {
            0.0
        };
        let (n, col_w) = resolve_column_layout(
            avail_width,
            self.props.column_count,
            self.props.column_width,
            gap,
        );
        self.resolved_count = n;
        self.resolved_col_w = col_w;
        // Phase A-1: delegate measurement to the block-shaped fallback.
        self.fallback.wrap(avail_width, avail_height)
    }

    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        // Phase A-1: delegate to fallback. Phase A-2 implements column-fill: auto.
        self.fallback.split(avail_width, avail_height)
    }

    fn split_boxed(self: Box<Self>, avail_width: Pt, avail_height: Pt) -> SplitResult {
        // Avoid cloning the segments when delegating; take the fallback.
        match self.fallback.split(avail_width, avail_height) {
            Some(pair) => Ok(pair),
            None => Err(self.clone_box()),
        }
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        // Phase A-1: delegate to fallback. Phase A-2 positions columns and
        // draws column-rule lines.
        self.fallback.draw(canvas, x, y, avail_width, avail_height);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.fallback.height()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn collect_ids(
        &self,
        x: Pt,
        y: Pt,
        avail_width: Pt,
        avail_height: Pt,
        registry: &mut DestinationRegistry,
    ) {
        self.fallback
            .collect_ids(x, y, avail_width, avail_height, registry);
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
}
