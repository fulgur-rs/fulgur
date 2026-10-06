# fulgur-raikiri

Unpublished Raikiri backend for Fulgur development.

The backend accepts an HTML file path and a `fulgur_core::Config`, and returns
`Result<Vec<u8>>` with the PDF bytes. It reads, parses, and lays out the
document with Raikiri, then draws the pages with Krilla: page geometry, box
backgrounds and borders, and text. It does not fall back to Blitz.

The config's page size and margins act as defaults that the document's own
`@page` rules override; fields the caller set explicitly (`Config::overrides`)
win over `@page`. Linked stylesheets, `@import`s, and other referenced files
are read from the input file's directory; files outside it are not read.
`render_with_options` accepts `RenderOptions { assets, system_fonts }`.
Bundle CSS is registered as user stylesheets in order. Fonts use core asset
loading (including WOFF2 decoding), then their family names are extracted
and registered with Raikiri. Parsing, layout, and painting retain the same
resources and local-file provider. Disabling system fonts requires a bundled
font. Invalid fonts, TTC/OTC collections, and non-empty image bundles return
an asset error. The pinned font API cannot select a collection face; use
individual TTF/OTF fonts or WOFF2.
Bundle CSS has document URL provenance; linked stylesheet imports retain
stylesheet URL provenance.

```rust
let pdf = fulgur_raikiri::render(
    std::path::Path::new("input.html"),
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
An omitted creation date does not insert the current time. Tagged PDF and
PDF/UA requests through Raikiri return an explicit error.
