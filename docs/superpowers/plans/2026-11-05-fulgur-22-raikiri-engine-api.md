# FULGUR-22 Raikiri Engine API parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `render_template`, `render_batch` and a page-layout query work on an `Engine` rendered through `Backend::Raikiri`.

**Architecture:** Stacked on PR #947 (FULGUR-19). The sealed facade trait `fulgur::EngineExt` gains `_with(Backend, …)` twins; `Backend::Blitz` delegates to the existing `Engine` methods unchanged. Templates are expanded with MiniJinja first and the resulting HTML is rendered (same order as Blitz). The layout query returns a new engine-neutral `fulgur_core::PageLayout` (per-page resolved size and margins, PDF pt); Blitz's `LayoutOutput` (Blitz `Drawables` + geometry) stays Blitz-only.

**Tech Stack:** Rust, rayon (behind the existing `parallel` feature), minijinja (existing), raikiri-html 0.0.6.

**Spec:** Plane FULGUR-22 + `docs/superpowers/specs/2026-11-02-fulgur-19-engine-selection-design.md` (FULGUR-22 is listed there as the deferred Engine API parity work). Decisions made in this session, since the owner opted out of design questions:

- Stack on #947, do not reimplement it.
- Template: expand-then-parse, not stream-time injection.
- Layout: engine-neutral `PageLayout` in `fulgur-core`; no Raikiri → Blitz `Drawables` conversion. `Engine::layout` / `LayoutOutput` unchanged and Blitz-only.
- Margin boxes are not part of `PageLayout` (Blitz's `LayoutOutput` does not surface them either).

## Global Constraints

- `fulgur-core` must not depend on Blitz, Stylo, Taffy, Parley, or Raikiri.
- `fulgur-blitz` must not depend on `fulgur-raikiri`.
- Facade (`crates/fulgur`) holds no layout logic of its own: it dispatches.
- No behaviour change with the `raikiri` feature off; additions are additive (new methods, new types).
- Raikiri never silently ignores a request it cannot honour (keep the `has_custom_serialize_settings` refusal).
- Code comments in English; `cargo fmt --check` and `cargo clippy -D warnings` with and without `--features raikiri`.
- Use `BTreeMap`/ordered iteration where output is affected.

## Review Focus

1. `render_batch_with(Raikiri)` with a bundled font under `--features parallel,raikiri`: every output byte-equal to its sequential single render (test in Task 4).
2. `render_template_with(Raikiri)` with no template set: same `Error::Template("no template set")` as Blitz (test in Task 4).
3. `@page :first { size … }` / `:left` margins: per-page `PageLayout` entries differ by page for both engines (tests in Tasks 2 and 3).
4. `page_layout_with` page count equals the page count of the PDF the same engine renders, over the VRT fixture corpus (Task 5).
5. A document whose layout aborts / errors in Raikiri: `page_layout_with` returns `Err`, not an empty layout (Task 3 reuses the `LayoutStatus::Completed` check).

---

### Task 1: `fulgur_core::PageLayout`

**Files:**
- Create: `crates/fulgur-core/src/layout.rs`
- Modify: `crates/fulgur-core/src/lib.rs` (add `pub mod layout; pub use layout::{PageGeometry, PageLayout};`)

**Interfaces:**
- Produces:
  - `#[non_exhaustive] #[derive(Debug, Clone, PartialEq)] pub struct PageGeometry { pub size: PageSize, pub margin: Margin }` — PDF pt, orientation already applied.
  - `impl PageGeometry { pub fn new(size: PageSize, margin: Margin) -> Self; pub fn content_size(&self) -> PageSize }` (content = size minus margins, clamped at 0).
  - `#[non_exhaustive] #[derive(Debug, Clone, PartialEq, Default)] pub struct PageLayout { pub pages: Vec<PageGeometry> }`
  - `impl PageLayout { pub fn new(pages: Vec<PageGeometry>) -> Self; pub fn page_count(&self) -> usize }`

- [ ] Unit tests in `layout.rs`: `content_size_subtracts_margins`, `content_size_clamps_at_zero`, `page_count_counts_pages`.
- [ ] Implement, `cargo test -p fulgur-core --lib layout`, commit.

### Task 2: Blitz side (`fulgur-blitz/src/engine.rs`, `render.rs`)

**Interfaces:**
- Produces:
  - `pub fn Engine::expand_template(&self) -> Result<String>` — MiniJinja expansion only; `render_template` becomes `self.render(&self.expand_template()?)`. Error without template: `Error::Template("no template set")` (unchanged).
  - `pub fn Engine::page_layout(&self, html: &str) -> Result<fulgur_core::PageLayout>` — runs the same pass-1/pass-2 loop as `layout`, then `page_count = implied_page_count(&geometry).max(1)` and per page `resolve_page_settings(&gcpm.page_settings, page_num, page_count, &config, drawables.root_dir_rtl)` with landscape applied.
  - A `pub(crate) fn resolved_page_box(rules, page_num, page_count, config, rtl) -> (PageSize, Margin)` in `render.rs` (or `gcpm::page_settings`) used by both `render_v2` and `page_layout`, so the two cannot drift.

- [ ] Tests (engine.rs `mod tests`): `expand_template_returns_html`, `expand_template_without_template_errors`, `page_layout_first_page_size_override` (`@page { size: A5 } @page :first { size: A4 }` with enough content for 2+ pages: page 0 is A4, page 1 is A5), `page_layout_page_count_matches_pdf` (lopdf not available in blitz dev-deps → compare with `render` via counting `/Type /Page` objects or use existing helper in tests).
- [ ] Implement, `cargo test -p fulgur-blitz --lib engine`, commit.

### Task 3: Raikiri side (`fulgur-raikiri/src/lib.rs`)

**Interfaces:**
- Produces: `pub fn page_layout_html_with_options(html: &str, base_dir: Option<&Path>, config: &Config, options: &RenderOptions<'_>) -> Result<fulgur_core::PageLayout>` — `with_layout(Source::Html{..}, …)`, require `LayoutStatus::Completed` (same error as `draw`), map each `Page::geometry()` `page_box` width/height and `margins` (CSS px) to pt via `F32Units`.

- [ ] Tests (`src/tests.rs`): `page_layout_reports_configured_a4`, `page_layout_first_page_override` (same HTML as Task 2), `page_layout_count_matches_pdf_page_count` (lopdf dev-dep exists).
- [ ] Implement, `cargo test -p fulgur-raikiri`, commit.

### Task 4: Facade + CLI

**Files:** `crates/fulgur/src/backend.rs`, `crates/fulgur/tests/backend_selection.rs`, `crates/fulgur-cli/src/main.rs`, `crates/fulgur-cli/tests/engine_cli.rs`.

**Interfaces (added to sealed `EngineExt`):**
- `fn render_template_with(&self, backend: Backend) -> Result<Vec<u8>>` — Blitz: `render_template()`; Raikiri: `render_html_with(Raikiri, &self.expand_template()?)`.
- `fn render_batch_with<S: AsRef<str> + Sync>(&self, backend: Backend, htmls: &[S]) -> Vec<Result<Vec<u8>>>` — Blitz: `render_batch`; Raikiri: `render_html_with` per item, `par_iter` under `#[cfg(feature = "parallel")]`.
- `fn page_layout_with(&self, backend: Backend, html: &str) -> Result<PageLayout>` — Blitz: `Engine::page_layout`; Raikiri: `fulgur_raikiri::page_layout_html_with_options` with the same options as `render_html_with`, and the same serialize-settings refusal.
- `render_html_with(Raikiri)` drops the `has_template()` refusal (Blitz `render(html)` ignores a registered template; match it).
- Re-export `PageLayout`, `PageGeometry` from the facade (`fulgur_core` via `fulgur_blitz::*` if `fulgur-blitz` re-exports them; otherwise explicitly).
- CLI: `--engine raikiri --data` renders through `render_template_with(Raikiri)`; remove its error.

- [ ] Tests (`backend_selection.rs`, raikiri feature): `raikiri_render_template_matches_expanded_render` (bytes equal to `render_html_with(Raikiri, expanded)`), `raikiri_render_template_without_template_errors`, `raikiri_render_batch_matches_sequential` (3 docs, bundled font from `examples/.fonts`, each equal to single render; order preserved; one invalid item fails alone), `page_layout_with_both_engines_agree_on_simple_a4`, `raikiri_page_layout_refuses_custom_serialize_settings`. Feature-off: `blitz_*_with_delegates`. Update the #947 test that asserted the template refusal; CLI test: `--engine raikiri --data` produces a PDF containing the expanded text.
- [ ] `cargo test -p fulgur --features raikiri,parallel`, `cargo test -p fulgur`, `cargo test -p fulgur-cli --features raikiri`, commit.

### Task 5: Corpus check

**Files:** `crates/fulgur/tests/raikiri_corpus_parity.rs` (`#![cfg(feature = "raikiri")]`).

- [ ] For every `crates/fulgur-vrt/fixtures/**/*.html` (sorted): for each backend whose `render_html_with` succeeds, assert `page_layout_with(..).page_count()` equals the PDF's page count (lopdf). Cross-engine differences (Blitz vs Raikiri page count/size) are printed as a summary, not asserted — `fulgur-vrt engine_compare` already measures visual parity.
- [ ] Run with `FONTCONFIG_FILE=$PWD/examples/.fontconfig/fonts.conf`, commit.

### Task 6: Docs + verification

- [ ] rustdoc on new APIs; CHANGELOG `Unreleased` entry; `fulgur-cli` help for `--data` no longer says Blitz-only.
- [ ] `cargo fmt --check`, `cargo clippy --workspace -D warnings` (feature off), `cargo clippy -p fulgur -p fulgur-cli -p fulgur-raikiri --features fulgur/raikiri,fulgur/parallel -D warnings`.
