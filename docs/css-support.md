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

### Tables

Supported:

- Multi-page tables repeat computed `table-header-group` sections on each
  continuation page. Use `thead { display: table-row-group; }` to disable
  repetition for a specific header group.
- Only the first `table-header-group` is treated as a header; later ones
  are treated as if they had `display: table-row-group`, per CSS Tables
  Level 3.
- A table does not start on a page that has room for the repeated band
  but not for the first body row — an orphaned header carries no
  information and the same header is redrawn on the next page anyway.

Not supported:

- `border-collapse: collapse`. The upstream layout engine
  (`blitz-dom` 0.2.4) has no implementation, so adjacent cell borders are
  drawn as if `separate` and shared edges paint twice. Upstream issue:
  `DioxusLabs/blitz#386`. The VRT fixture below uses `border-collapse`,
  so its golden bakes in the current behaviour and will need regenerating
  when upstream lands it.
- Hoisting a header group to the top of its table. CSS 2.1 §17.5.1
  requires a `table-header-group` to be rendered before all other rows
  regardless of source position. That reordering belongs to layout, and
  the engine fulgur builds on does not perform it, so a header group
  written after a `tbody` keeps its in-flow position. fulgur declines to
  repeat such a group rather than reserve a band spanning the rows above
  it. WPT `css/CSS2/tables/table-header-group-005.xht` fails for this
  reason (tracked as `fulgur-naj7.13`).
- Fragmenting nested tables. A table inside a cell of another table does
  not split: both keep a single fragment and the content overflows the
  page. Taffy has no table layout algorithm — `taffy::compute` provides
  `block`, `flexbox`, `grid` and `leaf` — and tables are approximated on
  top of those (tracked as `fulgur-naj7.14`).
- A header band that itself fragments. A forced break or a page-name
  change inside a header cell would make the band span pages, which
  contradicts repeating it; fulgur declines to repeat and lets the table
  paginate normally, so the break is honoured.
- `repeat-on-break` (CSS Repeated Headers and Footers). Stylo 0.8 does
  not parse the property, so the `display: table-row-group` opt-out above
  is the only way to suppress repetition — at the cost of discarding the
  header semantics along with the repetition.

Note that CSS Tables Level 3 §6 (Fragmentation) has no web-platform-tests
coverage, and `css/CSS2/tables/table-header-group-004.xht` is flagged
`may paged` — it passes whether or not headers repeat. Regression coverage
for this area therefore lives in fulgur's own geometry probes
(`crates/fulgur/tests/repeating_table_header_probes.rs`) rather than in an external suite.

See `docs/plans/2026-08-11-repeating-table-headers.md` for the
fragmentation coordinator. The VRT golden lives at
`crates/fulgur-vrt/fixtures/layout/repeating-table-header.html`.
