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

Raikiri PDF links use all quads supplied by the page API. Fragment links use
rendered anchors (including percent-encoded names and the first duplicate
anchor); missing targets are omitted. File-name and absolute URL links to the same document use internal
destinations too; links to other documents resolve against the document base URL. Destinations and link rectangles convert CSS px to pt,
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
