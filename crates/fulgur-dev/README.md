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
