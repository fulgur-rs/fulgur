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
cargo test --lib
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
  `TreeSink::finish`. This is noise from dependencies, not fulgur. Library
  callers that need clean stdout must redirect fd 1 at their own call site —
  `fulgur-cli` does this via `StdoutIsolator` for the render command so the
  `-o -` mode does not corrupt PDF output. Multi-threaded callers must not
  manipulate fd 1 process-wide; there is no thread-safe way to suppress
  blitz's output in-process short of an upstream patch.
- Use `BTreeMap` (not `HashMap`) for iteration that affects PDF output (determinism)
- Blitz: `!important` unreliable, `padding-top` on inline roots ignored (use `margin-top`)
- `cargo fmt --check` enforced by CI
