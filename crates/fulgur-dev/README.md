# fulgur-dev

Unpublished development CLI for selecting Fulgur's layout backend.

```sh
cargo run -p fulgur-dev -- render input.html --engine blitz -o output.pdf
cargo run -p fulgur-dev -- render input.html --engine raikiri -o output.pdf
```

The default engine is `blitz`. Both the input file and `-o` output file are
required. The CLI selects an independent backend entry point with the contract
**input file path → PDF bytes**, then writes the bytes to the output file only
after the backend succeeds.

Blitz uses the existing Fulgur PDF renderer. Relative resources are resolved
against the input file's parent directory.

Raikiri lays out the document with Raikiri and draws the PDF with Fulgur's
Krilla painter for that backend. Default page settings yield to author CSS.
Relative resources are resolved against the input file's parent directory, and
files outside it are not read. The output file is written only on success.

This CLI is separate from the published `fulgur` CLI and bindings. It does not
add a shared backend trait or change the published facade.

Page settings can be supplied explicitly for either engine:

```sh
cargo run -p fulgur-dev -- render input.html --engine raikiri --size 200x300pt --margin "10 15" -o output.pdf
```

`--size` accepts page keywords or custom dimensions with mm/cm/in/pt/px
units. `--margin` accepts one to four non-negative millimetre values in CSS
shorthand order. Explicit settings override author page rules; omitted
settings preserve CSS priority. Invalid geometry fails before writing output.
`--landscape` requires `--size`; orientation-only CSS and landscape-only
overrides remain outside the current Raikiri API support.

Additional CSS and fonts can be supplied in registration order:

```sh
cargo run -p fulgur-dev -- render input.html --engine raikiri --css print.css --font NotoSans-Regular.woff2 --no-system-fonts -o output.pdf
```

`--css` and `--font` are repeatable. Core asset loading enforces byte limits
and decodes WOFF2; the development CLI rejects invalid font files before
rendering. Font family names come from the font, not the file name. Raikiri rejects
TTC/OTC collections because its pinned API cannot select a collection face;
use individual TTF/OTF fonts or WOFF2.
`--no-system-fonts` requires a bundled font. Raikiri registers bundle CSS as
user stylesheets based on the input document URL; a CSS argument's file path
is not retained as stylesheet provenance. Linked stylesheets resolve imports
from their own URLs. Image bundles are unsupported by the Raikiri painter.

PDF metadata supports title, repeated authors and keywords, description,
language, creator, producer, and creation date. The development CLI exposes
`--title`, `--author`, `--description`, `--keyword` (alias `--keywords`),
`--language`, `--creator`, `--producer`, and `--creation-date` for both engines.
Raikiri validates dates in `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, or
`YYYY-MM-DDThh:mm:ss` (optional `Z`) form, including calendar validity.
An omitted creation date does not insert the current time. Tagged PDF and
PDF/UA requests through Raikiri return an explicit error.

Raikiri PDF links retain all quads supplied by the page API in one annotation
per link per page. Fragment links use
rendered anchors (including percent-encoded names and the first duplicate
anchor); missing targets are omitted. File-name and absolute URL links to the same document use internal
destinations too; links resolve against the effective document base URL,
including HTML `base`, then compare against the original input URL. Destinations and link rectangles convert CSS px to pt,
with the PDF coordinate transform applied once by Krilla.

`--bookmarks` enables heading outlines. Raikiri receives resolved level and
label values through render-local consumer callbacks, then builds destinations
and hierarchy only after layout completes. Author CSS overrides the heading
defaults; supported labels are literal strings, `attr(...)`, and
`content(text)`. Invalid levels, empty labels, and non-rendered headings are
omitted. Empty or missing drawable boxes use the first rendered descendant.
Krilla 0.7's default outline state collapses child levels. CSS bookmark-state,
counter/string labels, and general display:contents box suppression are not
implemented in this backend. Aborted layout or callback errors return no PDF.

Raikiri pages whose text nodes each map to one paint event use page paint order
for boxes and text, with opacity composited as groups. Rounded and axis-aware
clips retain the existing geometry. Generated content, duplicate or missing
text events, and unknown run sources use the legacy rendering for the entire
page, preserving text. Replaced/image content is still unsupported. The
current pinned API exposes no ellipsis source; existing text-overflow output
is preserved without claiming ellipsis rendering support.


For pages whose events cover all ordinary text runs, Raikiri follows
`Page::paint_order()` and composites opacity groups. It keeps the existing
rounded and per-axis overflow geometry. Pages with generated text, repeated
text events, or missing run events use the previous painter for the whole page
to preserve their text. The pinned API has no ellipsis source variant; current
`text-overflow:ellipsis` output remains at its existing baseline. Replaced
content, SVG, background images, margin boxes, and generated-content ordering
remain outside this development backend's supported API.

## Development validation

| Capability | Validation |
| --- | --- |
| Page settings and CSS priority | CLI/PDF MediaBox assertions |
| Bundle CSS, TTF/WOFF2, fixed fonts | CLI and resource tests |
| Metadata, links, anchors, heading outlines | Parsed PDF values and destinations |
| Paint order, group opacity, rounded/axis clips | PDF operations and fixed-color Linux raster assertions |
| Fixed-font/date reproducibility | Three separate CLI runs with identical PDF bytes |

Linux tests require Poppler's `pdftocairo`; CI installs `poppler-utils` and
runs these tests through the workspace test suite. They fail if the rasterizer
is missing. Run the focused checks with:

```sh
cargo test -p fulgur-dev --test paint_order_raster --test determinism
python3 -m unittest discover -s scripts -p compare_raikiri_dev_tests.py
```

The comparison tool writes development PDFs, PPMs, and `comparison.json` only
to the selected output directory. It records fixture/font/binary hashes,
revision, rasterizer versions, page counts, extracted text, per-page RGB
pixel differences, and repeated-run PDF hashes. Backend differences are
observations; the correctness assertions above supply independent expectations.
Production VRT goldens are not updated.

```sh
cargo build -p fulgur-dev
python3 scripts/compare-raikiri-dev.py \
  --binary target/debug/fulgur-dev \
  --font crates/fulgur-ruby/spec/fixtures/noto_sans.ttf \
  --output-dir "$HOME/tmp/fulgur-dev-comparison" \
  tests/fixtures/raikiri-dev/*.html
```

For same-condition performance measurements, pass `--measure --runs 3` and
`tests/fixtures/raikiri-dev/long-document.html`. This requires GNU
`/usr/bin/time -v` and records elapsed seconds and maximum RSS in KiB for each
engine/run. Compare the same build profile and revision; the debug command
above is a development baseline, not a release performance claim. The bundled
TTF's family name is `Noto Sans Mono`, as used by these fixtures. Record any
measurement conclusions before removing the temporary output directory.
