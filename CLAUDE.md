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

- **Coordinate system and unit conversion**: fulgur uses three distinct unit spaces
  (Blitz/Taffy in CSS px, Pageable/Krilla in PDF pt, `PageSize::custom` in mm).
  Forgetting a conversion is the most common source of scale bugs (4/3× or 3/4× off).
  See `.claude/rules/coordinate-system.md` for the full rules, conversion helpers,
  and known pitfalls (Krilla Y-down, Stylo px basis, CSS transform composition,
  PDF text-space operators).
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
- **Worktree sparse-checkout**: `git worktree add` inherits a `/.beads/`-only
  sparse-checkout pattern, which makes `git add` refuse modifications to
  source files (`paths ... outside of your sparse-checkout definition`).
  Two ways to deal with this:
  - Right after `git worktree add <path> -b <branch>`, run
    `git -C <path> sparse-checkout disable`. This is the recommended fix —
    it sets `core.sparseCheckout=false` for that worktree only.
  - If you've already started work and only need a one-off commit, use
    `git add --sparse <files>` to force the index update.
  The `EnterWorktree` tool's PostToolUse hook in `.claude/settings.json`
  handles this automatically when it's used to enter a worktree, but
  Bash-driven `git worktree add` (used by the `using-git-worktrees` skill)
  doesn't trigger that hook.
- Use `BTreeMap` (not `HashMap`) for iteration that affects PDF output (determinism)
- Blitz: `!important` unreliable, `padding-top` on inline roots ignored (use `margin-top`)
- `cargo fmt --check` enforced by CI
- **Coverage scope**: CI の coverage job は `cargo llvm-cov nextest --workspace --exclude fulgur-vrt`
  で動いている (`.github/workflows/ci.yml`)。`crates/fulgur-vrt` は別ジョブで実行されるため、
  **VRT reftest だけでカバーした draw 経路は codecov の patch coverage に乗らない**。
  新しい draw / convert / pageable ロジックを書くときは VRT に加えて lib 側にもテストを置く:
  - 純関数 (helper, fixup, math) → 当該モジュールの `#[cfg(test)] mod tests` に unit test
  - レンダリング経路 (`draw_background_layer` の match arm 等、`Engine::render_html` を通って初めて
    叩かれる箇所) → `crates/fulgur/tests/render_smoke.rs` に end-to-end smoke test
    (`Engine::builder().build().render_html(html)` で `assert!(!pdf.is_empty())`)
  VRT を後付けで足すと codecov に再指摘されて lib 側 smoke test を追加する手戻りが発生する
  (PR #244 で実例)。最初から両方書くこと。
- **`Engine` is a builder**: `Engine::builder().page_size(PageSize::A4).base_path(root).build()` + single-arg `render_html(html)`. There is no `Engine::new().with_*()`.
- **VRT は PDF byte 比較**: `crates/fulgur-vrt` は HTML → PDF を生成して `goldens/fulgur/**/*.pdf` と byte-wise 比較する (`crates/fulgur-cli/tests/examples_determinism.rs` と同じ哲学)。pdftocairo は失敗時の diff 画像生成のみで使う。golden 更新は `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt`。
