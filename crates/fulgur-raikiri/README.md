# fulgur-raikiri

Unpublished Raikiri backend for Fulgur development.

The backend accepts an HTML file path, or an HTML string with an optional base
directory, and a `fulgur_core::Config`, and returns
`Result<Vec<u8>>` with the PDF bytes. It reads, parses, and lays out the
document with Raikiri, then draws the pages with Krilla: page geometry, box
backgrounds and borders (including `url()` background images), text, text
decorations, PNG/JPEG images, and inline SVG. It does not fall back to Blitz.

The config's page size and margins act as defaults that the document's own
`@page` rules override; fields the caller set explicitly (`Config::overrides`)
win over `@page`. Linked stylesheets, `@import`s, and other referenced files
are read from the input file's directory; files outside it are not read.
`render_html` and `render_html_with_options` take the HTML as `&str` and use
the base directory in the same way. Without a base directory the document is
treated as `about:blank`: no file is read and only same-document fragment
links resolve.
`render_with_options` accepts `RenderOptions { assets, system_fonts }`.
Bundle CSS is registered as user stylesheets in order. Fonts use core asset
loading (including WOFF2 decoding), then their family names are extracted
and registered with Raikiri. Parsing, layout, and painting retain the same
resources and local-file provider. Disabling system fonts requires a bundled
font. Invalid fonts and TTC/OTC collections return an asset error. Each bundle
image is a file at the URL its name resolves to against the base directory
(`img/logo.png`), or at its name when that is an absolute URL; HTML without a
base directory is `about:blank`, so only absolute-URL names apply to it.
Documents reach bundle images with browser URL resolution: `<img src>`
against the document base URL, `url()` in a linked or imported stylesheet
against that stylesheet's URL. Bundle images are served ahead of the files in
the directory, for any resource kind. The pinned font
API cannot select a collection face; use
individual TTF/OTF fonts or WOFF2.
Bundle CSS has document URL provenance; linked stylesheet imports retain
stylesheet URL provenance.

```rust
let pdf = fulgur_raikiri::render(
    std::path::Path::new("input.html"),
    &fulgur_core::Config::default(),
)?;
let pdf = fulgur_raikiri::render_html(
    "<p>Hello</p>",
    Some(std::path::Path::new("assets")),
    &fulgur_core::Config::default(),
)?;
```

Raikiri is a Git dependency pinned to a specific revision. This crate is not
used by Fulgur's published facade, CLI, or bindings.

PDF metadata supports title, repeated authors and keywords, description,
language, creator, producer, and creation date. The development CLI exposes
`--title`, `--author`, `--description`, `--keyword` (alias `--keywords`),
`--language`, `--creator`, `--producer`, and `--creation-date` for both engines.
Raikiri validates dates in `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, or
`YYYY-MM-DDThh:mm:ss` (optional `Z`) form, including calendar validity.
An omitted creation date does not insert the current time.

Tagged PDF (`Config::tagged`) builds the structure tree from the DOM with the
same HTML mapping as the Blitz backend (`fulgur_core::tagging`): headings with
their text as title, paragraphs, generic containers as `Div`, lists with
`Lbl`/`LBody` items, tables with header scope, images as `Figure` with their
`alt`, and `<a href>` as `Link` holding its content and its tagged link
annotation. Text, images and list markers are marked content of their element,
in document order; text outside any classified element gets its own `P`.
Backgrounds, borders, text decorations, column rules and margin boxes
(`Header`/`Footer` for the top and bottom boxes) are artifacts, as are the
repeated copies of table headers and `position: fixed` boxes after the page
that first shows them. `Config::pdf_ua` adds Krilla's PDF/UA-1 validation and
implies tagging and bookmarks. It takes the title and language from `Config`,
falling back to `<title>` and `<html lang>`; rendering fails when no language
is available.

Raikiri PDF links use all quads supplied by the page API. Fragment links use
rendered anchors (including percent-encoded names and the first duplicate
anchor); missing targets are omitted. Other links are resolved against the
document base URL. Destinations and link rectangles convert CSS px to pt,
with the PDF coordinate transform applied once by Krilla.

`--bookmarks` enables heading outlines. Raikiri receives resolved level and
label values through render-local consumer callbacks, then builds destinations
and hierarchy only after layout completes. Author CSS overrides the heading
defaults; supported labels are literal strings, `attr(...)`,
`content(text)`, counters and named strings. Invalid levels, empty labels, and non-rendered headings are
omitted. Empty or missing drawable boxes use the first rendered descendant.
`bookmark-state: open | closed` sets each entry's initial expansion and
defaults to `open` as in CSS GCPM 3, unlike the Blitz backend, whose outline
always starts collapsed. Labels may also use `counter()`, `counters()` and
`string()`: counters take their value at the heading, and `string()` takes the
latest `string-set` assignment at or before it in document order. Page counters
and general display:contents box suppression are not implemented in this
backend. Aborted layout or callback errors return no PDF.

Raikiri pages use paint order with one text event per paragraph line from the
page's positioned runs. Source text, generated content and ellipses share their
ancestor opacity groups, which composite overlapping descendants once. Rounded
and axis-aware clips retain the existing geometry. Duplicate, unknown or missing
line events and unknown run sources use legacy rendering for the entire page.
Inline element opacity and other replaced content remain unsupported. Generated
text and ellipses have no decoration segments in this API version.

Multi-column rules use Raikiri's physical rectangles, used widths, styles,
colors, and complete pattern extents. Solid, double, dotted, dashed, ridge,
groove, inset and outset rules paint above the owner's background and below
its content and outside markers. Rules share ancestor clips, owner overflow
and opacity, while retaining their pattern phase across page slices.

Ordinary inline-formatting `::before` and `::after` backgrounds and borders
use the generated line pieces and pseudo styles, including split inline edges
and glyph clipping. Their text, counters and attributes retain the resolved
positioned runs and share the originating element's opacity and overflow.
Pseudo-element opacity, anonymous table pseudo cells and legacy generated
overlays remain unsupported.

Ordinary PNG/JPEG images use the layout-resolved URL, intrinsic dimensions,
`object-fit` and `object-position` from Raikiri. The PDF painter reads the
same cached pixels without fetching resources, and clips the object to its
content box and active ancestor overflow while sharing opacity groups.
Relative local URLs retain the input-directory boundary and resource limits.
Unavailable images produce an explicit resolver fallback and are omitted;
repeated sources share one PDF image resource. PNG list markers reuse the same
cache at Raikiri's outside or inline atomic placement, with first-fragment
ownership, ancestor clipping, and item opacity. Missing marker sources retain
text fallback, and explicit `::marker` content takes priority. Image bundle URLs
remain unsupported.

Inline SVG is drawn as PDF vector content through Krilla SVG, including paths,
gradients, and selectable text. Its viewport uses Raikiri's resolved content box,
including border, padding, fixed repeats, and page cuts. SVG fonts follow the
same bundled-font and system-font options as the HTML text. Resolved root font
sizes and complete family candidate lists preserve relative sizing and fallback.
Parent and SVG-root
opacity composite once while explicit inherited descendant opacity is retained.
Source preparation preserves selector matches before rewriting the root viewport
and color. The supported SVG subset follows Raikiri's admission checks; external
references and filter effects return errors rather than fetching resources or
silently dropping the SVG.

Used text decorations come directly from Raikiri's glyph runs, including
ancestor propagation, line endpoints, color, thickness, and the unsplit pattern
extent. Insets apply before font and color run slicing.
Underline and overline paint before glyphs, and line-through paints after
them. Solid, double, dotted, dashed, and wavy styles retain their pattern
phase across font and color run boundaries. Decorations share the text's
active clip and opacity group; transparent glyphs can still have visible
colored decorations. Margin-box decorations are not drawn.

Text shadows come from the glyph runs' used `text-shadow` list and paint below
the line's decorations and glyphs, the first declared shadow on top; each
shadow layer covers the whole line before the next, and neighboring runs that
share a shadow are drawn as one shape, so font and color changes leave no seam.
A sharp shadow is drawn as filled glyph outlines, so extracted text is not
repeated. PDF has no blur, so a blurred shadow is rasterized at three pixels
per CSS px (a Gaussian of half the blur radius, approximated by three box
blurs) and drawn as an image with a soft mask; the shadow rasters of a document
share a budget of 64M pixels. Shadows are not applied to text decorations,
page-margin box text has no shadows, and glyphs without outlines (bitmap, SVG
and color-only emoji glyphs) cast no shadow.

Decoration phases apply to each paragraph line independently. Raikiri's
line identities keep coincident lines distinct and join font/color slices
of the same line, so earlier decorations cannot cover later overlapping
text merely because both are in one paint batch.

Page-margin boxes come from Raikiri's per-page layout (`Page::margin_boxes`):
the sixteen slots with their used rectangles, resolved generated content
(`counter(page)`, `counter(pages)`, quotes, `string()` and `element()`), and
glyph runs of their text. Each box paints below the page body: background
color and `url()` image, solid borders, then its content clipped to the
border box. A box whose `content` has `element()` draws the running element the page selects
(`Page::margin_box_running_element`), laid out at the box's content width
and drawn like a page body, with its links clipped to the box. Other boxes,
including those that combine `element()` with other values, draw their text.
Vertical-writing text in margin boxes is not drawn yet.

Corner radii retain separate horizontal and vertical axes from Raikiri,
including slash shorthand, two-value corner longhands, and percentages of
the border box. Both painters use the shared used-value calculation, with
one scale factor when adjacent corners overlap. PDF backgrounds, borders,
and overflow clips follow the resulting ellipses; insets subtract their
adjacent border widths and fragment breaks keep their corners square.

Inner corner curves preserve the outer radius minus adjacent border or padding
widths. Thick opposite borders crop the ellipse at the padding/content rectangle;
they do not rescale it. An entirely excluded inner shape paints no background,
hides descendants when used as an overflow clip, and leaves the full outer
border ring.
When cropped diagonal inner arcs intersect, their common outline is computed
with adaptive vector segments (0.05px control-hull tolerance, bounded to 1024
segments per corner for extreme coordinates). Single-corner crops and ordinary
rounded rectangles retain cubic curves.
