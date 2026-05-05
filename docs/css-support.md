# CSS Feature Support

This document tracks fulgur's CSS property support status and any
version-specific limitations.

## Effects

### `box-shadow` (v0.4.5+)

Supported:

- Outer shadows with `offset-x`, `offset-y`, `spread-radius`, `color`
  (including `rgba()` alpha, `transparent`, `currentColor`)
- Multiple comma-separated shadows (painted front-to-back per CSS spec)
- Combination with `border-radius` (shadow follows rounded corners; spread
  expands radii per spec)
- Negative `spread-radius` (corners clamp sharp per CSS spec)
- Gaussian `blur-radius > 0` (gradient-based 9-slice approximation)

Not yet supported:

- `inset` shadows: skipped with a `log::warn!` diagnostic.
- `box-shadow` on inline-level elements: shadow drawing currently dispatches
  through the block and table draw paths, so generic inline-level backgrounds,
  borders, and shadows are not painted today. Use `display: block` (or
  `inline-block`, which routes through the block draw path) to get shadows
  on generic boxes.

See PR `#83` and `docs/plans/2026-04-14-box-shadow.md` for implementation
details.

## Layout

### `overflow` / `overflow-x` / `overflow-y`

Supported:

- `overflow: hidden` and `overflow: clip`: paint is clipped to the
  padding-box of the element. The clip is applied at draw time
  (`render.rs` push/pop_clip_path) and follows `border-radius` when
  present.
- `overflow: scroll` and `overflow: auto`: PDF has no scroll concept, so
  these collapse to the same padding-box clip as `hidden`.
- `overflow-x` / `overflow-y` per axis: each axis is honoured
  independently. Per CSS Overflow Module Level 3 §3, when one axis is
  `visible` and the other is non-`visible`, the `visible` value is
  promoted to `auto` — fulgur defers to Stylo for that promotion and
  ends up clipping both axes in that combination.
- Inline-level boxes: `display: inline-block` participates in clipping
  via the block draw path (see fulgur-tsp / PR #131).
- Tables: `<table style="overflow:hidden">` clips its cells to the
  outer table border-box.
- Nested overflow: each `overflow:hidden|clip` ancestor pushes its own
  clip; descendant clips compose correctly across transform and
  opacity contexts.

Not yet supported:

- `overflow: visible` interaction with pagination: clipping is a pure
  visual effect — the layout box and pagination follow the element's
  computed size, so overflowing children stay anchored inside their
  parent's page slot rather than being split across pages. Documents
  that rely on `overflow: hidden` to constrain page breaks should
  instead pin a fixed `height` on the container.
- `overflow: clip-path` and `clip-path` shapes: not implemented;
  rectangles and `border-radius` are the only supported clip shapes.
- Scroll UI (scrollbars, focus ring on scroll containers): irrelevant
  in static PDF.
- Multicol interaction: `overflow:hidden` inside a `column-count`
  container has not been validated; behaviour may change once
  multicol pagination (fulgur-qkg) lands.

Related follow-ups:

- `text-overflow: ellipsis` is tracked separately as fulgur-2cy.
- `white-space: nowrap` is tracked separately as fulgur-5rj.

VRT goldens covering this feature live under
`crates/fulgur-vrt/fixtures/layout/overflow-*.html`.
