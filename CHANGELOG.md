# Changelog

All notable changes to this project will be documented in this file.

## [0.2.0] - 2026-03-21

### Bug Fixes

- adjust decoration line positions using cap_height and font_size ratio
- address PR review feedback
- offset inline root children by padding+border in styled blocks
- use cached width for background/border drawing, handle empty styled elements
- proportional radius scaling per CSS spec, darken example backgrounds
- separate layout_size from cached_size, guard non-uniform border widths
- BlockPageable::height() now respects layout_size
- recursive child split in BlockPageable and re-wrap after split
- address PR review — table borders, running elements, split edge cases
- preserve table width across splits, use width field instead of avail_width
- snap table split to row boundary instead of cell boundary
- use strict > 0.0 instead of is_sign_positive for child_avail guard
- correct groove/ridge normal direction per side
- correct groove/ridge normal direction for all four border sides

### Documentation

- add text-decoration example
- add text-align example

### Features

- add TextDecoration types and skrifa dependency
- extract text-decoration from Stylo computed values
- draw text-decoration lines with font metrics
- extract border-radius from Stylo into BlockStyle
- draw rounded rectangle backgrounds and borders
- add TablePageable with header repeat on page split
- detect table elements and build TablePageable with header/body groups
- border-style support (dashed, dotted, double, none)
- 3D border styles (groove, ridge, inset, outset)

### Miscellaneous

- regenerate example PDFs
- add mise task for regenerating example PDFs

### Refactor

- extract shared helpers and eliminate duplication
- extract draw_block_border helper, regenerate examples

### Styling

- add margins to border-radius example for better spacing

### Testing

- add text-decoration visual test fixture
- add border-radius integration tests and example
- add table header repeat tests and example

## [0.1.1] - 2026-03-21

### Bug Fixes

- use full text range for glyph text_range to avoid multi-byte boundary panic
- correct glyph positioning, Taffy layout coordinates, and table rendering
- suppress Blitz HTML parser noise and fix unused variable warning
- recursive page splitting for single-child overflow
- apply border/background styles to inline root nodes (th, td, p)
- drop macOS amd64 from CI matrix (macos-13 deprecated)
- resolve clippy warnings (collapsible_if, field_reassign_with_default)
- handle list items that are inline roots (marker was lost)
- update test files to use renamed fulgur crate
- handle non-ASCII CSS, skip comments/strings in parser, apply page_selector
- inject CSS into margin boxes, apply @page selector specificity
- center margin box positioning when left is undefined
- measure max-content width with inline-block, escape declarations
- deterministic margin box rendering order
- layout margin boxes at confirmed width, not content_width
- escape HTML attributes fully, update design doc
- extract git-cliff to /tmp to avoid committing tarball contents

### CI

- add release workflow with crates.io publish and binary distribution
- create draft GitHub Release in prepare, publish on merge
- validate semver input, retry fulgur-cli publish
- fix script injection, add tag guard, update docs
- use prebuilt git-cliff, handle re-runs, add release fallback
- pass all inputs via env vars to prevent script injection
- fix git-cliff download URL, validate version in release.yml

### Documentation

- add fulgur design document and implementation plan
- add README with usage, API, and architecture overview
- add list marker rendering design document
- add list marker design and implementation plan
- add margin box width distribution design
- update CLAUDE.md with gcpm module and gotchas

### Features

- scaffold cargo workspace with core types and pagination
- add PDF rendering, Engine API, and CLI render subcommand
- integrate Blitz for HTML parsing and layout, CLI accepts HTML input
- add ParagraphPageable with text rendering via Parley->Krilla bridge
- add background/border rendering, ImagePageable, and AssetBundle
- support bundled fonts via AssetBundle and CLI --font flag
- add ListItemPageable struct with wrap/split/draw
- add extract_marker_lines for list marker glyph extraction
- wire ListItemPageable into DOM conversion for li elements
- add margin box types, GcpmContext, and ContentItem
- implement page counter resolution
- implement CSS parser for @page margin boxes and running()
- add RunningElementStore and DOM serializer
- add running element detection and exclusion in convert.rs
- integrate 2-pass rendering pipeline with margin box support
- implement flex-based margin box width distribution
- integrate flex-based margin box layout into render pipeline
- flex-based margin box width distribution + declarations support

### Miscellaneous

- add CI, licenses, and CLAUDE.md for OSS setup
- initialize beads issue tracking
- add .gitignore with target, tmp, and worktrees
- prepare packages for crates.io publishing
- remove unused header_html/footer_html from Config

### Refactor

- extract draw_shaped_lines from ParagraphPageable

### Styling

- apply cargo fmt across codebase
- fix indentation after collapsible_if refactor
- apply cargo fmt formatting
- apply cargo fmt formatting

### Testing

- add integration tests for list marker rendering
- add integration tests for header/footer with GCPM
- add deterministic output test for GCPM margin boxes

### Release

- v0.1.1


