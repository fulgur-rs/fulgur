# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Fulgur is an HTML/CSS to PDF conversion library and CLI tool written in Rust. It uses Blitz for HTML parsing/layout, Krilla for PDF generation, and Taffy/Parley for layout/text shaping.

## Common Commands

```bash
# Build
cargo build
cargo build --release

# Test
cargo test --lib                   # note: in the workspace root this runs only fulgur-vrt
cargo test -p fulgur --lib         # fulgur unit tests (~340)
cargo test -p fulgur
cargo test -p fulgur --test gcpm_integration

# Lint
cargo clippy
cargo fmt --check
npx markdownlint-cli2 '**/*.md'

# Run CLI
cargo run --bin fulgur -- render input.html -o output.pdf
cargo run --bin fulgur -- render input.html --size A4 --landscape -o output.pdf
```

## Architecture

The processing pipeline flows:

```text
HTML string → Blitz (parse/style/layout) → Pageable tree → Page splitting → Krilla PDF
```

### Workspace Structure

- `crates/fulgur/` — Library crate with the conversion engine
- `crates/fulgur-cli/` — CLI binary using clap

### Key Modules (fulgur)

- **engine.rs** — `Engine` builder: configures and executes `render_html()` / `render_pageable()`
- **blitz_adapter.rs** — Thin adapter isolating Blitz API changes from the rest of the codebase
- **convert.rs** — Transforms Blitz DOM nodes into `Pageable` trait objects
- **pageable.rs** — Core `Pageable` trait with `wrap()` (measure), `split()` (page break), `draw()` (render). Concrete types: `BlockPageable`, `ParagraphPageable`, `SpacerPageable`, `ImagePageable`
- **paginate.rs** — Page splitting algorithm that walks the Pageable tree
- **render.rs** — Draws paginated fragments onto Krilla surfaces
- **config.rs** — Page size, margins, orientation, metadata
- **asset.rs** — `AssetBundle` manages CSS, fonts, and images (offline-first, all assets explicitly registered)
- **paragraph.rs** — Text line layout and drawing
- **gcpm/** — CSS Generated Content for Paged Media: parser, margin boxes, running elements, counters

### Design Principles

- **Offline-first**: No network access; all assets must be explicitly bundled
- **Deterministic**: Same input always produces same output — see the font caveat below
- **Hybrid layout**: Taffy pre-computes sizes, Pageable reuses them during pagination (no re-layout after splitting)
- **Adapter isolation**: Blitz API surface is contained in `blitz_adapter.rs`

**Font determinism caveat**: `blitz-dom` 0.2.4 hardcodes
`fontdb::Database::load_system_fonts()` for inline `<svg>` parsing (see
`blitz-dom-0.2.4/src/util.rs`), and fulgur currently inherits Parley's
system font fallback for HTML text whenever no bundled fonts are supplied.
This means the same HTML can produce different PDFs on two hosts if their
installed `.ttf`/`.otf` set differs — the usual bite is `<text>` inside
SVG picking a fallback that happens to ship on one machine but not the
other. The regeneration scripts under `mise.toml` and
`.github/workflows/update-examples.yml` pin this via
`FONTCONFIG_FILE=examples/.fontconfig/fonts.conf`, which redirects
fontconfig to the bundled Noto Sans set in `examples/.fonts/`. When
editing fonts, CLI defaults, or the SVG pipeline, remember that library
callers don't get this guarantee by default — see the tracking issue
`fulgur-a8s` and the README's *Determinism and fonts* section.

### Gotchas

- **Blitz is thread-safe** (contrary to earlier belief). Multiple threads can
  call `blitz_adapter::parse` / `resolve` / `apply_passes` concurrently on
  independent documents. The previous "Blitz not thread-safe" note was based
  on a misdiagnosis — the real race was in fulgur's own `suppress_stdout`
  helper, which has been removed. See
  `docs/plans/2026-04-11-blitz-thread-safety-investigation.md` for the full
  root-cause analysis.
- **Blitz prints html5ever parse errors via `println!` to stdout** during
  `TreeSink::finish`. This is noise from dependencies, not fulgur.
  Policy by crate:
  - **`crates/fulgur` (core library)** must not touch fd 1 under any
    circumstance. `blitz_adapter::suppress_stdout` was removed for this
    reason (see
    `docs/plans/2026-04-11-blitz-thread-safety-investigation.md`).
  - **`crates/fulgur-cli`** is single-threaded during render and may
    manipulate fd 1 via `StdoutIsolator` — this is required for
    correctness (`-o -` writes PDF bytes to stdout; any noise corrupts
    the stream).
  - **`crates/pyfulgur`, `crates/fulgur-ruby`** are multi-threaded
    bindings. They must not manipulate fd 1 either: a global suppress
    mutex still races with `suppress=false` callers on the same process,
    and PDF bytes are returned via the function return value (not
    stdout), so noise is cosmetic, not a correctness issue. The
    canonical workaround for binding users is redirection at their own
    call site (e.g. `os.dup2` / `contextlib.redirect_stdout`) or running
    renders in a subprocess (`multiprocessing`). A future wrapper-style
    package that shells out to the CLI is on the roadmap for users who
    want clean stdout without doing this themselves.
  - Short version: **touch fd 1 only from a crate that can guarantee
    single-threaded semantics**. That's CLI today; bindings are
    multi-threaded by design and must leave fd 1 alone.
- Use `BTreeMap` (not `HashMap`) for iteration that affects PDF output (determinism)
- Blitz: `!important` unreliable, `padding-top` on inline roots ignored (use `margin-top`)
- `cargo fmt --check` enforced by CI
- **`Engine` is a builder**: `Engine::builder().page_size(PageSize::A4).base_path(root).build()` + single-arg `render_html(html)`. There is no `Engine::new().with_*()`.
- **PDF → PNG for visual tests**: `pdftocairo -png -r 100 -f 1 -l 1 <pdf> <prefix>` (poppler-utils). Installed in CI; gate with skip-if-missing for local dev. `fulgur-vrt::pdf_render::render_html_to_rgba` wraps this but does not accept `base_path`, so integration tests that load local CSS must inline the call.
