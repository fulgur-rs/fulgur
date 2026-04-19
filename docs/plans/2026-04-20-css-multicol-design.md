# CSS Multi-column Layout Design

**Date**: 2026-04-20
**Epic**: fulgur-qkg
**Status**: Design approved, Phase A starting

## Goal

Support CSS Multi-column Layout in fulgur so that newspaper- and magazine-style
page layouts render correctly. Scope is delivered in phases to keep each change
reviewable and to derisk the pagination interaction.

## Phased scope

### Phase A (MVP)

- `column-count`, `column-width`, `column-gap`
- `column-fill: auto` (fill the first column fully before advancing)
- `column-span: all`
- Page-spanning: a multicol container larger than one page keeps flowing across
  subsequent pages
- `column-rule-*` (`style`, `color`, `width`, shorthand)
- `break-inside: avoid`

### Phase B

- `column-fill: balance` (CSS default; balance column heights)
- `break-before`, `break-after` with `column | page | avoid`

### Phase C (follow-up epics)

- Nested multicol
- `double` / `groove` / `ridge` / `inset` / `outset` column-rule styles
- `column-span: <integer>` (Multi-column Level 2)

## Why fulgur implements this itself

- `taffy 0.9.2` has no multicol `Display` type; its `column_count` APIs are for
  CSS Grid tracks, not multicol.
- `stylo 0.8.0` parses the `column-*` properties but `blitz-dom 0.2.4` does
  not lay them out — a multicol container is treated as a regular block.
- So fulgur must own the column layout between Blitz (which gives us the
  container's content box) and Krilla (which gets the final positioned
  fragments).

## Architecture

```text
HTML
  ↓ Blitz (parse/style/layout)
Styled DOM        ← column-* parsed by stylo, ignored by taffy
  ↓ convert.rs
Pageable tree     ← NEW: MulticolPageable
  ↓ paginate.rs   (unchanged)
Paginated fragments
  ↓ render.rs
Krilla PDF
```

`paginate.rs` is not modified. `MulticolPageable` honours the existing
`Pageable` trait contract (`wrap → split → draw`), so pagination treats it like
any other block.

## Types

```rust
enum Segment {
    ColumnGroup(Vec<Box<dyn Pageable>>),  // flows across N columns
    SpanAll(Box<dyn Pageable>),           // full-width strip
}

struct ColumnRule {
    width: f32,                           // pt
    style: BorderStyle,                   // solid | dashed | dotted in Phase A
    color: Color,
}

struct MulticolPageable {
    // Declared properties
    column_count: Option<u32>,
    column_width: Option<f32>,
    column_gap: f32,
    column_rule: Option<ColumnRule>,
    segments: Vec<Segment>,
    // Filled by wrap()
    resolved_count: u32,
    resolved_col_w: f32,
    measured: Option<MeasuredSegments>,
}
```

### Convert-time decomposition

`convert.rs` detects a multicol container (any block with `column-count` or
`column-width` set) and walks its subtree. Children are grouped into
`ColumnGroup` segments, split at descendants that carry `column-span: all` —
those become their own `SpanAll` segment.

Rules:

- Nested `column-span: all` inside a `SpanAll` subtree is ignored (per spec,
  span resolves against the outermost multicol only).
- Empty `ColumnGroup` segments (e.g. when the first child is span-all) are not
  emitted.
- Child Pageables are constructed here but not measured. Measurement happens
  in `MulticolPageable::wrap()` once the column width is known.

### Column sizing

```rust
fn resolve_column_layout(
    container_content_w: f32,
    count: Option<u32>,
    width: Option<f32>,
    gap: f32,
) -> (u32, f32) {
    match (count, width) {
        (Some(n), None) => (n, (container_content_w - gap * (n as f32 - 1.0)) / n as f32),
        (None, Some(w)) => {
            let n = ((container_content_w + gap) / (w + gap)).floor().max(1.0) as u32;
            (n, (container_content_w - gap * (n as f32 - 1.0)) / n as f32)
        }
        (Some(n), Some(w)) => {
            let n_cap = ((container_content_w + gap) / (w + gap)).floor().max(1.0) as u32;
            let used = n.min(n_cap);
            (used, (container_content_w - gap * (used as f32 - 1.0)) / used as f32)
        }
        (None, None) => unreachable!("caller filters non-multicol blocks"),
    }
}
```

`.floor()` is used explicitly to keep the computation deterministic and
platform-independent.

## Pageable trait semantics

### `wrap(content_width) -> Size`

1. Resolve `(n, col_w)` from `resolve_column_layout`.
2. For each `ColumnGroup`, call `child.wrap(col_w)` on every child and sum to
   get the group's natural height. The wrap-reported height is approximated as
   `ceil(natural_h / n)`.
3. For each `SpanAll`, call `child.wrap(content_width)` (full width).
4. Return `Size { width: content_width, height: sum(segment heights) }`.

The returned height is the theoretical minimum; actual placement is decided by
`split`.

### `split(available_height) -> (Fragment, Option<Self>)`

Greedy `column-fill: auto`:

1. Walk segments in order.
2. `SpanAll`: defer to `child.split(available_height)`. If the child returns a
   remainder, emit a new `MulticolPageable` with that remainder + all
   subsequent segments.
3. `ColumnGroup`:
   - Start at column 1 with budget `available_height`.
   - For each child:
     - If it fits: place it, decrement budget.
     - If not and `child.avoid_break_inside()` is true and `col_idx < n - 1`:
       advance to the next column, retry.
     - If not and we can split it: `child.split(budget)` → place the filled
       part, carry the remainder into the next column.
     - If we run out of columns with content remaining: emit the remainder as
       a new `MulticolPageable` for the next page.
4. Degenerate case: if `avoid_break_inside` cannot be honoured anywhere
   (natural height > full page), fall back to a normal split. This is
   spec-conformant and avoids infinite loops.

### `draw(surface, fragment)`

- For each column, draw its pieces at `(col_idx * (col_w + gap), piece.y)`.
- For each `SpanAll` strip, draw at `x = 0` with full width.
- Draw column rules between adjacent columns where **both** have content. The
  rule spans `[max(top_i-1, top_i), min(bot_i-1, bot_i)]` (i.e. the shorter of
  the two column content extents).

## Page-spanning

Page-spanning falls out for free from `split`'s remainder contract:

```text
Page 1 (800pt budget)           Page 2 (800pt budget)
┌──┬──┬──┐                      ┌──┬──┬──┐
│A │A │B │  ← split(800)        │C │C │D │  ← remainder.split(800)
│  │  │B │     fragment            │  │  │     fragment
│  │  │  │     remainder = {C,D}   │  │  │     remainder = None
└──┴──┴──┘                      └──┴──┴──┘
```

The `remainder` reuses `resolved_count` / `resolved_col_w`; it does not
re-wrap. This assumes page width is constant across the document, which is
currently a fulgur-wide invariant. A docstring on the remainder path records
this so future per-page-size support knows where to trigger re-wrapping.

## column-rule rendering

Per CSS spec, the rule is drawn in the centre of the gap, **only between
columns that both carry content**, and its vertical extent is the shorter of
the two adjacent column content heights.

```rust
fn draw_column_rules(&self, surface: &mut Surface, frag: &MulticolFragment) {
    let Some(rule) = &self.column_rule else { return };
    for i in 1..frag.columns.len() {
        if !frag.columns[i - 1].has_content || !frag.columns[i].has_content {
            continue;
        }
        let x = frag.col_x(i) - self.column_gap / 2.0;
        let y_top = frag.columns[i - 1].content_top.max(frag.columns[i].content_top);
        let y_bot = frag.columns[i - 1].content_bot.min(frag.columns[i].content_bot);
        draw_border_line(surface, x, y_top, y_bot, rule.width, rule.style, rule.color);
    }
}
```

Phase A supports `solid` / `dashed` / `dotted`. Other border styles are
deferred.

## Edge cases

- `column-count: 1` → behaves as a regular block. Phase A accepts it without a
  dedicated fast path.
- Empty multicol → returns zero height, draws nothing.
- Nested multicol → **not supported in Phase A**. An inner multicol container
  is laid out as a plain block (its `column-*` props are ignored) with a log
  warning. Re-evaluated in a follow-up epic.
- `column-width: W` with `W > content_w` → clamp to 1 column, width =
  `content_w`.

## Testing

### Unit tests

- `resolve_column_layout`: nine combinations of (count, width, gap) plus
  edge cases (`content_w < col_w`, `gap > content_w`).
- `MulticolPageable::wrap`: basic / with SpanAll / empty segments.
- `split` branches: fits in one column / splits mid-child / break-inside
  avoid promotes to next column / promotes to next page / SpanAll remainder.
- `draw_column_rules`: both-have-content / one empty / SpanAll-adjacent.

### Integration tests

`crates/fulgur/tests/multicol_integration.rs`, mirroring
`gcpm_integration.rs`:

- basic 2-column, basic 3-column
- `column-width` driven
- with `column-span: all`
- with `break-inside: avoid`
- multi-page spanning

### VRT

`crates/fulgur-vrt` with at least six fixtures for Phase A:

1. `multicol-basic-2col`
2. `multicol-rule-solid`
3. `multicol-span-all`
4. `multicol-page-spanning`
5. `multicol-break-inside-avoid`
6. `multicol-column-width-resolution`

Rendered via `pdftocairo -png -r 100 -f 1 -l 1`, snapshotted and diffed.

## Determinism

Column sizing is pure arithmetic with `.floor()`. All per-segment data lives
in `Vec`/`BTreeMap` (no `HashMap` for iteration-order-sensitive state). The
font-fallback caveat documented in `CLAUDE.md` still applies.

## Phase A work breakdown

Will be filed as sub-issues under epic fulgur-qkg:

| Seq | Title | Type |
|---|---|---|
| A-0 | multicol: verify stylo parsing + thread column-* through blitz_adapter | task |
| A-1 | multicol: `MulticolPageable` scaffold + `resolve_column_layout` + convert integration | feature |
| A-2 | multicol: `wrap` / `split` / `draw` for `column-fill: auto` | feature |
| A-3 | multicol: `column-span: all` segment decomposition + page-spanning | feature |
| A-4 | multicol: `column-rule-*` rendering | feature |
| A-5 | multicol: `break-inside: avoid` handling | feature |
| A-6 | multicol: VRT fixtures + integration tests + examples | task |

Dependencies: `A-0 → A-1 → A-2 → {A-3, A-4, A-5}` (parallelisable) `→ A-6`.

## Phase B work breakdown (draft — to be refined after Phase A lands)

| Seq | Title | Type |
|---|---|---|
| B-1 | multicol: `column-fill: balance` | feature |
| B-2 | multicol: `break-before` / `break-after` (`column`, `page`, `avoid`) | feature |
| B-3 | multicol: Phase B VRT fixtures | task |
