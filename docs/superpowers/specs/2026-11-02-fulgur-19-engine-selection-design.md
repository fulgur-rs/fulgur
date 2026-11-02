# FULGUR-19: choose the layout engine (Cargo feature, runtime API, CLI `--engine`)

Status: design, 2026-11-02. Plane: FULGUR-19. Roadmap: M2 (selectable release, Blitz stays the default).

## Why

The owner generates OCR fixtures with `fulgur render`. Complex tables come out
misaligned because of Blitz bugs, for example `border-collapse`
(DioxusLabs/blitz#386: adjacent cell borders are added instead of collapsed,
and `border-collapse: separate` loses its spacing). Rendering the fixture from
that issue through `fulgur-dev render --engine raikiri` gives the Firefox
result. Raikiri is only reachable through the unpublished `fulgur-dev` today.
This change lets the regular `fulgur` CLI and library select it.

## Goals

1. A `raikiri` Cargo feature on the `fulgur` facade and on `fulgur-cli`. It is
   **off by default**. With it off, the dependency graph, the binary and every
   existing behaviour stay as they are.
2. A small runtime API in the `fulgur` facade that renders an already built
   `Engine` with a chosen backend.
3. `fulgur render --engine blitz|raikiri` in the CLI. The default is `blitz`.
4. Leaving out `--engine`, or passing `--engine blitz`, behaves exactly as
   today.

## Non-goals (deferred, by owner decision)

- **Shipping Raikiri in the distributed binaries** (npm packages, release
  binaries, cross-gem, wheel). That is FULGUR-20. The release workflow builds
  `fulgur-cli` without features, so npm users do not get Raikiri from this
  change.
- **Template rendering, `render_batch` and `layout` on Raikiri** (Engine API
  parity). That is FULGUR-22 (M3). The owner uses template data
  (`--data`), but accepted waiting for FULGUR-22.
- **Bindings** (pyfulgur, fulgur-ruby, fulgur-wasm). That is FULGUR-23 (M3).
- **User docs** (README, website, threat model, css-support engine notes).
  That is FULGUR-21. This change only updates CLI help text and rustdoc.
- **Making Raikiri the default.** That is M3 (FULGUR-24).
- **CI changes.** The owner decided that Raikiri stays in `raikiri.yml` (not
  a required check, excluded from the ci.yml coverage) until FULGUR-19 or a
  later issue decides otherwise. See "Left to the builder" for the one CI touch
  this change needs.

## Anti-goals

- No behaviour change for anyone who does not ask for Raikiri: the same
  output, the same dependencies with the feature off, and no API removed or
  changed.
- Raikiri must never ignore a request it cannot honour without saying so.
  Unsupported combinations fail with a clear error.
- `fulgur-blitz` must not depend on `fulgur-raikiri`, and `fulgur-core` stays
  backend-independent (CLAUDE.md).

## Constraints and current state

- `fulgur` (facade) is `pub use fulgur_blitz::*;`. `fulgur::Engine` and
  `fulgur::EngineBuilder` are `fulgur-blitz` types.
- `Engine` has the public accessors `config()`, `base_path()` and `assets()`.
  `system_fonts` is a private field with no accessor.
- `fulgur-raikiri` (published, core version group since #942) exposes:
  - `render_html_with_options(html, base_dir: Option<&Path>, &Config, &RenderOptions)`
  - `RenderOptions { assets: Option<&AssetBundle>, system_fonts: bool, svg_rendering, svg_raster_scale }`
    (`Clone + Copy + Default`; it is not `#[non_exhaustive]`)
  - `render_with_options(input: &Path, ...)`
  - `render_streaming(...)`
- `fulgur-dev` already switches engines with an `EngineChoice` ValueEnum. Its
  Raikiri call is the reference for which options get passed.
- `fulgur-cli` `render` builds an `Engine` with the builder (page size, margin,
  landscape, metadata, bookmarks, tagged, pdf-ua, base path, assets), then
  calls `engine.render_template()` when `--data` is given and
  `engine.render(&input_content)` otherwise. Input is read from a file or
  `--stdin`.
- crates.io and release-plz: `fulgur`, `fulgur-cli`, `fulgur-core`,
  `fulgur-blitz` and `fulgur-raikiri` are published in the `core` version
  group (0.41.0). An optional dependency on `fulgur-raikiri` has to carry
  `version` and `path`, like the other in-group dependencies.

## Decided design

### Cargo features

- `fulgur/Cargo.toml`:
  `fulgur-raikiri = { version = "0.41.0", path = "../fulgur-raikiri", optional = true }`
  and `[features] raikiri = ["dep:fulgur-raikiri"]`.
- `fulgur-cli/Cargo.toml`: `[features] raikiri = ["fulgur/raikiri"]`.
- Neither feature is in `default`.

### Runtime API (facade, `fulgur` crate)

The owner approved the minimal shape below. `Engine` is not changed.

- `fulgur::Backend`: an enum, `#[non_exhaustive]`, deriving
  `Clone, Copy, Debug, PartialEq, Eq, Default`. `Blitz` is always present and
  is the `#[default]`. `Raikiri` exists only with the `raikiri` feature
  (`#[cfg(feature = "raikiri")]`).
- An entry point that renders an HTML string with a built `Engine` and a
  `Backend`. `Backend::Blitz` gives exactly `engine.render(html)`.
  `Backend::Raikiri` gives
  `fulgur_raikiri::render_html_with_options(html, engine.base_path(), engine.config(), &RenderOptions { assets: engine.assets(), system_fonts: engine.system_fonts(), ..Default::default() })`.
  The function-versus-trait choice is the builder's (see below). The name
  carries the backend explicitly, as in `render_html_with(Backend, html)`.
- `fulgur-blitz` gains `pub fn system_fonts(&self) -> bool` on `Engine`. It
  is a read-only accessor, which is additive and non-breaking. The two read-only
  additions below are the only other `fulgur-blitz` changes. No existing
  signature changes.
- The Raikiri entry point does not render templates. When the `Engine` has a
  template set, the Raikiri path returns an error and does not render the
  raw template. The Blitz path keeps `render`'s current semantics.
  *(Designer's call, from the builder check:)* `template` is a private field
  of `Engine`, so `fulgur-blitz` also gains a read-only
  `pub fn has_template(&self) -> bool`, which is additive like
  `system_fonts()`.
- *(Designer's call, from the builder check:)* `EngineBuilder::serialize_settings`
  (krilla `SerializeSettings`: PDF/A validation, PDF version, compression) has
  no Raikiri counterpart. `fulgur-raikiri` builds its own settings. Following
  the anti-goal, the Raikiri path refuses an `Engine` whose serialize settings
  were set explicitly, instead of ignoring them.
  - krilla's type has no `PartialEq`, so the builder records whether
    `serialize_settings(...)` was called: a private flag, set to true by that
    builder method and stored on `Engine`.
  - The flag is exposed read-only, for example
    `pub fn has_custom_serialize_settings(&self) -> bool`.
  - Threading the settings through to `fulgur-raikiri` is out of scope, since
    it would widen that crate's public API.
  - The CLI never sets this option, so this affects library users only.

### CLI (`fulgur render`)

- `--engine <blitz|raikiri>`, a clap ValueEnum, defaulting to `blitz`.
- When `fulgur-cli` is built without the `raikiri` feature, `--engine raikiri`
  is still accepted by the parser. It fails at run time with an error saying
  that this build does not include Raikiri, and naming the feature to enable
  it (`cargo install fulgur-cli --features raikiri`). It is not a clap parse
  error that hides the option.
- `--engine raikiri --data ...` fails before rendering, with an error that
  template data needs the Blitz engine for now.
- With `--engine raikiri`, every other `render` option reaches Raikiri through
  the built `Engine`: size, landscape, margin, metadata (title, authors,
  description, keywords, language, creator, producer, creation date),
  `--font`, `--css`, `--image`, `--stdin`, `-o -`, `--bookmarks`, `--tagged`
  and `--pdf-ua`.
- Other subcommands (`inspect`, `template`, `schema`, plugins) stay Blitz-only
  and get no `--engine`.
- The Raikiri-only options of `fulgur-dev` (`--stream`, `--svg-rendering`,
  `--svg-raster-scale`) are not added.

## Left to the builder

- The exact name and form of the runtime entry point (extension trait on
  `Engine` or free function) and its module path, within the shape above.
- Error variants and messages. Reuse `fulgur_core::Error::Other` or add a
  variant. A new variant on a public enum must not break anyone: check whether
  `Error` is `#[non_exhaustive]`.
- Where the template check sits. It can be in the CLI, before rendering, or in
  the facade entry point, or in both, as long as both the CLI and the library
  refuse.
- Tests:
  - feature-off: `--engine raikiri` errors, and default output is
    byte-identical to today's path where tests already assert that
  - feature-on: a small HTML renders through Raikiri, options such as title
    or bookmarks reach the PDF, and `--data` with raikiri errors
  - a regression test with the blitz#386 table may be added as a Raikiri
    smoke check
  - `fulgur-raikiri` tests depend on the local fonts (two tests are sensitive
    to system versus pinned fonts); use the existing fixtures' approach.
- CI: the feature-on facade and CLI code must be compiled and tested
  somewhere. `raikiri.yml` already builds the Raikiri crates and is the
  natural place: add `-p fulgur -p fulgur-cli --features …/raikiri` to its
  clippy and test steps, and add the touched paths to its `paths:` filter if
  needed. Do not make it required. `ci.yml` keeps building the default
  (feature-off) configuration.
- The CHANGELOG and release-notes label (`release-notes:feature`).

## Verification the builder should run

- `cargo build -p fulgur-cli` (feature off): `cargo tree -p fulgur-cli` shows
  no raikiri crate.
- `cargo run -p fulgur-cli --features raikiri -- render <blitz#386 fixture> --engine raikiri -o out.pdf`
  renders collapsed borders, matching `fulgur-dev --engine raikiri`.
- The existing CLI and facade test suites pass unchanged with the feature
  off.
- `cargo clippy` with `-D warnings`, both with and without the feature, on
  stable.
