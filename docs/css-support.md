# CSS Feature Support

This document tracks fulgur's CSS property support status and any
version-specific limitations.

fulgur has two layout engines: **Blitz**, the default engine used by the
published crates, CLI and bindings, and **Raikiri**, which is under
development in the `fulgur-raikiri` crate and can be selected
with `fulgur-dev render input.html --engine raikiri -o output.pdf`. The two tables below compare them
feature by feature. The per-property sections after them describe the Blitz
engine unless they name an engine.

Status values:

- **Supported**: works for the cases fulgur documents and tests.
- **Partial**: works with a known gap, described in the notes.
- **Unsupported**: not available in that engine.
- **Intentional difference**: the engines differ on purpose; see
  [Intentional differences](#intentional-differences).
- **Unverified**: not yet checked for that engine.

The Raikiri column describes the Raikiri revision that `fulgur-raikiri`
pins in its `Cargo.toml`. A fix that has landed in Raikiri but not in that
revision is listed as a gap until the pin is updated.

## Feature parity by engine

Rows marked **yes** in the *Required* column must be Supported or an
Intentional difference in Raikiri before Raikiri can become the default
engine.

| Feature | Required | Blitz | Raikiri | Notes |
|---|---|---|---|---|
| GCPM: margin boxes, `counter(page)` / `counter(pages)` | yes | Supported | Partial | Raikiri draws margin-box backgrounds, solid borders and text, but not text decorations inside margin boxes, which Blitz lays out like page content. (Raikiri also does not yet draw vertical-writing text in margin boxes; Blitz has no vertical writing at all.) Counters other than `page` / `pages` resolve to `0` in both engines (see [Counters in `@page` margin boxes](#counters-in-page-margin-boxes)). |
| GCPM: running elements (`position: running()` / `element()`) | yes | Partial | Supported | Blitz also paints the running element in the page body; Raikiri shows it only in the margin box (see [Intentional differences](#intentional-differences)). |
| GCPM: named strings (`string-set` / `string()`) | yes | Partial | Supported | Blitz does not accept `counter()` in `string-set`. Raikiri resolves `string()` to the value for each page. |
| GCPM: `target-counter()` / `leader()` tables of contents | yes | Supported | Supported | |
| GCPM: page selectors (`:first`, `:left`, `:right`) and named pages | yes | Partial | Supported | Blitz places `:right` page margin boxes incorrectly (see [Intentional differences](#intentional-differences)). |
| PDF bookmarks (outline from headings, `bookmark-level` / `bookmark-label`) | yes | Supported | Supported | Raikiri also honours `bookmark-state`; Blitz ignores it (see [Intentional differences](#intentional-differences)). Raikiri does not resolve page counters in bookmark labels. |
| Links (`<a href>` annotations, internal destinations) | yes | Supported | Supported | |
| Tagged PDF / PDF/UA-1 | yes | Supported | Supported | Both engines use the same HTML-to-structure mapping (`fulgur_core::tagging`). |
| Images (`<img>`, `url()` backgrounds) | yes | Supported | Supported | Blitz: PNG / JPEG / GIF. Raikiri: PNG / JPEG verified; its image resolver also decodes GIF and WebP. |
| SVG vector rendering (inline SVG) | yes | Supported | Supported | Raikiri draws inline SVG as PDF vector content and falls back to Krilla SVG for masks, patterns and other content outside its scene model. |
| Fonts (bundled TTF / OTF / TTC / WOFF2, system fallback) | yes | Supported | Supported | Both engines register every face of a TTC / OTC collection. Both start from `sans-serif` when the document sets no `font-family`. |
| Shadows (`box-shadow`, `text-shadow`) | yes | Partial | Partial | Blitz skips `inset` box shadows, does not paint shadows on generic inline-level boxes, and does not paint `text-shadow` (see [`box-shadow`](#box-shadow-v045)). Neither engine draws shadows in page-margin boxes. |
| `transform` | yes | Supported | Supported | |
| Gradients (linear, radial, conic, repeating) | yes | Supported | Partial | Gradient background tiling (`background-size` / `-position` / `-repeat`) matches Blitz. Raikiri draws gradients with color transition hints, but their colors still differ from Blitz. |
| Table header repeat across pages | yes | Supported | Supported | Raikiri also repeats headers over anonymous rows and cells, and repeats `table-footer-group` as well; Blitz repeats headers only. |
| Multi-column layout (`column-count`, `column-width`, `column-span`, `column-rule`) | yes | Supported | Supported | |
| Pagination (`break-*`, orphans / widows, `@page` size and margins) | yes | Supported | Supported | Border widths snap to whole CSS pixels as in Chromium's print layout, so boxes take the same height and break at the same place. |
| Templates (`render_template`, MiniJinja + JSON) | no | Supported | Unsupported | Not yet wired to the Raikiri engine. |
| Batch rendering (`render_batch`) and `Engine::layout` | no | Supported | Unsupported | Not yet wired to the Raikiri engine. |
| Streaming (paint each page while the input is still being read) | no | Unsupported | Supported | Raikiri only (`fulgur_raikiri::render_streaming`, `fulgur-dev render input.html --engine raikiri --stream -o output.pdf`). See the `fulgur-raikiri` README for its limits. |

## Intentional differences

Places where the engines differ on purpose: features Blitz does not have,
Blitz behaviour that Raikiri does not reproduce, and deliberate departures
from a specification that both engines share.

| Area | Blitz | Raikiri | Why |
|---|---|---|---|
| [Document counters in `@page` margin boxes](#counters-in-page-margin-boxes) | Resolves to `0` | Resolves to `0` | Deliberate difference from CSS Paged Media 3 §6.1, shared by both engines and Chromium, so switching engines does not change the output. |
| Running elements | Paints the element in the page body as well as in the margin box | Shows it only in the margin box | Blitz bug: CSS GCPM 3 removes a running element from the normal flow. Raikiri does not reproduce it. |
| `@page :right` margin boxes | Places them incorrectly; on the first page, `@top-right` content appears at the top left | Places them per CSS Paged Media 3 | Blitz bug that Raikiri does not reproduce. |
| `bookmark-state` | Ignored; the outline always starts collapsed | `open` (the CSS GCPM 3 initial value) or `closed` per entry | Raikiri follows CSS GCPM 3. |
| `box-shadow: inset` | Skipped with a warning | Painted inside the padding box | Feature Blitz does not have. |
| `text-shadow` | Not painted | Painted | Feature Blitz does not have. |
| Vertical writing (`writing-mode: vertical-rl` / `vertical-lr`) | Not supported | Supported in the page body; not yet drawn in page-margin boxes | Feature Blitz does not have. |
| Ruby (`<ruby>` / `<rt>` annotations) | Not laid out as ruby | Laid out as ruby | Feature Blitz does not have. |

Output differences that are still under investigation are not listed here.
In the visual regression comparison of the two engines, for example, some
multi-column fixtures and `svg/shapes.html` render differently in the two
engines, with Raikiri's output closer to Chromium's; until the cause is
known, those are not treated as intentional.

## Colors

Supported:

- `rgb()` / `rgba()` / `#hex` / named colors
- `hsl()` / `hsla()` / `hwb()`
- `lab()` / `lch()` / `oklab()` / `oklch()`
- `color(srgb ...)` and the other `color()` predefined spaces
- `color-mix()`
- `currentColor`

Stylo keeps a computed color in whatever space the author wrote it in, so
`AbsoluteColor::components` is only RGB for the sRGB-component spaces.
`convert/style/mod.rs::absolute_to_rgba` converts to sRGB
(`AbsoluteColor::into_srgb_legacy`) before reading the components; skipping
that step rendered every non-sRGB space as a different color entirely — a
regression tracked as `fulgur-xfzo`, where `hsl(0,100%,50%)` painted cyan and
`hsl(120,100%,50%)` painted white and vanished against the page.

Notes:

- PDF output is device RGB, so a wide-gamut color (`lab()`,
  `color(display-p3 ...)`) is clamped into sRGB rather than preserved.
- Alpha is carried through; see the `opacity` notes below for how it
  composites.

Regression coverage lives in `crates/fulgur/tests/color_space_test.rs`,
which byte-compares a document using a non-sRGB spelling against the same
document written as `rgb()`.

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

Not yet supported (Blitz engine; Raikiri paints both, see
[Feature parity by engine](#feature-parity-by-engine)):

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

## Paged Media

### `@page { size: <keyword> }`

Supported keywords (CSS Paged Media Level 3 §4.1.1, case-insensitive):

| Keyword | Dimensions |
|---|---|
| `A3` | 297 × 420 mm |
| `A4` | 210 × 297 mm |
| `A5` | 148 × 210 mm |
| `B4` | 250 × 353 mm (ISO) |
| `B5` | 176 × 250 mm (ISO) |
| `JIS-B4` | 257 × 364 mm |
| `JIS-B5` | 182 × 257 mm |
| `letter` | 8.5 × 11 in |
| `legal` | 8.5 × 14 in |
| `ledger` | 11 × 17 in |

Explicit dimensions (`size: 148mm 210mm`) and the `portrait` / `landscape`
suffix work alongside any of these.

An unrecognised keyword still falls back to A4 — a document has to be laid
out on some sheet — but it now emits a `log::warn!` naming the keyword.
Before `fulgur-5oav` the table stopped at A4/A3/Letter and the fallback was
silent, so `size: A5` produced A4 output with no diagnostic anywhere.

The same table backs the CLI's `--size`, via `PageSize::from_css_keyword`,
so the two cannot drift apart again. `--size` still takes priority over
`@page { size }` when both are present.

### Counters in `@page` margin boxes

In margin-box `content` (`@top-right`, `@bottom-center`, ...):

- `counter(page)` / `counter(pages)` (and the `counters()` forms) resolve to
  the current page number and the total page count.
- Any other name, including a counter the document resets or increments
  (for example one incremented by every `h2`), resolves to `0`, the same as a
  counter that does not exist. Chromium renders these the same way.
- `counter-reset` / `counter-increment` declared on the `@page` rule itself
  are not supported, so they do not define page-context counters either.

This is a deliberate difference from CSS Paged Media 3 §6.1, under which
a counter that the page and margin contexts neither reset nor increment
takes its value at the start of the page, so a document counter would show
there. Chromium and the Raikiri backend do not implement that rule and
print `0`, and both Fulgur engines now do the same, so switching engines
does not change the output. Earlier releases of the Blitz backend printed
the document counter value, so `content: "Ch. " counter(chapter)` showed
the current chapter number; it now prints `Ch. 0`.

To show a chapter number in a page header, carry it into a named string
with `string-set` and reference it with `string()`:

```css
h2 { string-set: chapter attr(data-chapter); }
@page { @top-right { content: "Ch. " string(chapter); } }
```

```html
<h2 data-chapter="2">Chapter Beta</h2>
```

`string-set` also accepts `content()` (the element's text) and quoted
strings. CSS GCPM 3 additionally allows `counter()` in `string-set`
(`h2 { string-set: chapter counter(chapter) }`), but the Blitz backend
does not support that value yet, and `content(before)` does not capture
generated `::before` text either, so the number has to come from the
element's text or an attribute for now.

The regression fixture is `crates/fulgur-vrt/fixtures/gcpm/counter-page-numbers.html`.
