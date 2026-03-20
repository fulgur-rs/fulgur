# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Fulgur is an HTML/CSS to PDF conversion library and CLI tool written in Rust. It uses Blitz for HTML parsing/layout, Krilla for PDF generation, and Taffy/Parley for layout/text shaping.

## Common Commands

```bash
# Build
cargo build
cargo build --release

# Test (unit tests are in paginate.rs)
cargo test --lib
cargo test -p fulgur

# Lint
cargo clippy
cargo fmt --check

# Run CLI
cargo run --bin fulgur -- render input.html -o output.pdf
cargo run --bin fulgur -- render input.html --size A4 --landscape -o output.pdf
```

## Architecture

The processing pipeline flows:

```
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

### Design Principles

- **Offline-first**: No network access; all assets must be explicitly bundled
- **Deterministic**: Same input always produces same output
- **Hybrid layout**: Taffy pre-computes sizes, Pageable reuses them during pagination (no re-layout after splitting)
- **Adapter isolation**: Blitz API surface is contained in `blitz_adapter.rs`
