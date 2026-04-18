# Changelog

All notable changes to this project will be documented in this file.

## [0.5.0] - 2026-04-18

### Bug Fixes

- use license table syntax for PyPI compatibility
- address AI review feedback on placeholder packages
- address coderabbit review feedback
- convert viewport input from pt to CSS px (fulgur-9ul)
- convert Taffy layout output from CSS px to pt (fulgur-9ul)
- convert border/padding layout values from CSS px to pt (fulgur-9ul)
- convert get_body_child_dimension to pt (fulgur-9ul)
- close remaining pt/px boundary gaps from AI review
- address Task 1 review feedback
- address AI review feedback (coderabbit + devin + gemini)
- address CodeRabbit re-review
- use RbSys::ExtensionTask for cross-compile
- make cross-compile work in cross-gem-action mount
- single-platform cross_platform + gemspec injection
- use fulgur::asset::AssetBundle full path
- address CodeRabbit/Devin review feedback on PR #103
- defer Windows precompile, fix source gem smoke
- strip rb_sys dep from native gem spec

### CI

- sync pyfulgur and fulgur-ruby versions
- add release-python.yml for PyPI OIDC publish
- fix critical boolean input and harden install steps
- add release-ruby.yml for RubyGems OIDC publish
- restore Ruby 3.1 and 3.4 smoke-test coverage
- switch to RubyGems Trusted Publishers (no role-to-assume)
- address CodeRabbit review on release workflows
- fix missed fromJSON(inputs.dry_run) on publish job
- smoke-test sdist and source gem before publish
- address CodeRabbit review (round 3) — supply chain + musl + ref scoping
- smoke-test sdist/source-gem on minimum supported versions
- migrate to oxidize-rb/actions/cross-gem@v1
- restore ruby/setup-ruby for cross-gem action

### Documentation

- add not-available note above planned API examples
- update README for MVP release and CHANGELOG
- add pyfulgur binding MVP implementation plan
- add viewport pt/px fix plan (fulgur-9ul)
- add entry for pt/px unit fix (fulgur-9ul)
- fix broken placeholder link in pt/px entry
- add README + CHANGELOG
- clarify write_to_path description (no binmode concept)
- flag placeholder and dry_run limitation inline
- add RELEASE_SETUP.md for Trusted Publisher config
- link release setup guide
- record PyPI/RubyGems publish CI implementation plan

### Features

- wire PyO3 extension crate into workspace
- add PageSize class with A4/LETTER/A3 + custom/landscape
- add Margin class with uniform/symmetric/uniform_mm
- add AssetBundle with css/font/image registration
- add RenderError and map_fulgur_error helper
- add EngineBuilder with chainable config methods
- add Engine.render_html with GIL release
- add Engine.render_html_to_file
- add Engine(**kwargs) Pythonic constructor
- add __version__ and integration tests
- scaffold gem + crate skeleton
- add error mapping (Fulgur::{Error,RenderError,AssetError} + Errno::ENOENT)
- add PageSize wrapper (A4/LETTER/A3 + custom + landscape)
- add Margin wrapper (positional + kwargs + factory)
- add AssetBundle wrapper + long/short aliases
- add Engine + EngineBuilder (kwargs + chain)
- add Pdf result object (to_s/bytesize/to_base64/to_data_uri) + render_html
- add Pdf#write_to_path + #write_to_io (64KB chunked, binmode)
- release GVL during render_html
- add render_html_to_file + integration specs
- enable abi3-py39 for single wheel across Python 3.9+

### Miscellaneous

- add placeholder packages for PyPI (pyfulgur) and RubyGems (fulgur)
- fix fmt + silence pyo3 0.22 macro lints
- regenerate example PDFs
- loosen required_ruby_version to 3.1.0

### Refactor

- add layout_in_pt helper (no behavior change) (fulgur-9ul)
- remove dead viewport fields from PassContext (fulgur-9ul)
- simplify code after review

### Testing

- scaffold unit semantics integration tests (fulgur-9ul)
- add oracle tests for layout unit semantics (RED) (fulgur-9ul)
- regenerate goldens after pt/px unit fix (fulgur-9ul)
- update transform integration test + regenerate example PDFs (fulgur-9ul)

### Build

- switch to maturin, add smoke test

## [0.4.5] - 2026-04-16

### Bug Fixes

- keep sharp corners sharp and round color components
- clip shadow to exclude border-box so transparent elements render correctly
- forward pagination through HeadingMarkerWrapperPageable
- suppress collector during margin-box draws
- descend into HeadingMarkerWrapperPageable in walkers
- harden WOFF2 decoding and assert font embedding
- reject oversized WOFF2 header before invoking decoder
- gate font file size before reading into memory
- escape form feed in escape_css_url; cover \r and \f in tests
- guard element_text recursion depth and resolve per-page margins in GCPM destination pre-pass
- insert space at block/br boundaries in element_text
- add MAX_DOM_DEPTH guard to resolve_enclosing_anchor
- capture id on TablePageable for anchor destinations
- skip outline entry when resolved label is empty
- resolve rebase conflicts with main's link feature
- emit orphan marker for flattened / zero-size elements
- escape comment pattern in mise.toml for TOML compatibility
- address coderabbit review feedback on PR #94
- address code review feedback on transform tracking
- reject degenerate quads after transform in push_rect
- collapse nested if into match guard (clippy::collapsible_match)
- inject inside marker for non-inline-root <li> elements
- address code review feedback on inside marker implementation
- handle injection failures and empty paragraph edge case
- use per-character text_range in shape_marker_with_skrifa
- apply content_inset to empty <li> and support list-style-image

### Documentation

- add box-shadow v0.4.5 implementation plan
- add CSS property support reference with box-shadow
- clarify that inset shadows are excluded upstream, not filtered
- note blur rect expansion requirement for future blur impl
- replace branch reference with PR number
- document --bookmarks CLI flag in README
- add PDF bookmarks implementation plan (fulgur-6e6)
- document WOFF2 font support
- add WOFF2 font support implementation plan
- add fulgur-2ai link-media rewrite implementation plan
- add link-media example for <link media=print>
- rework link-media example to reflect current screen-media behaviour
- note <link media> handling and GCPM double-count limit (fulgur-2ai)
- satisfy markdownlint (blanks around lists/fences, code lang)
- note Engine builder API, pdftocairo helper, and fulgur --lib split
- implementation plan for fulgur-d5k
- clarify pseudo-element handling in GCPM parser
- add implementation plan for fulgur-yqi
- add bookmark CSS property example

### Features

- add BoxShadow type and BlockStyle.box_shadows field
- extract box-shadow from stylo computed values
- render outer box-shadow behind background layer
- add HeadingMarkerPageable and HeadingCollector
- derive PartialEq on HeadingEntry
- add HeadingMarkerWrapperPageable
- wrap h1-h6 with HeadingMarkerWrapperPageable
- add Config.bookmarks flag and builder method
- add outline tree builder
- wire HeadingCollector into render paths and emit outline
- add --bookmarks CLI flag
- add detect_font_format helper with magic byte detection
- add add_font_bytes API with format auto-detection
- decode WOFF2 fonts to TTF at ingestion
- collect <link rel=stylesheet media=X> candidates from DOM
- add escape_css_url helper for @import URL safety
- rewrite <link media=X> to <style>@import url() X;</style>
- wire LinkMediaRewrite into parse_html_with_local_resources
- add LinkTarget/LinkSpan types and link field on text items
- attach LinkSpan to glyph runs inside <a href>
- collect block id→(page,y) registry for destinations
- capture id on paragraph-level headings for destinations
- LinkCollector collects per-page link rects during draw
- emit URI actions and XYZ destinations as PDF link annotations
- add BookmarkLevel / BookmarkMapping types
- parse bookmark-level (integer | none)
- parse bookmark-label (content-list)
- add FULGUR_UA_CSS for h1-h6 default bookmarks
- add BookmarkPass for DOM → outline resolution
- plumb bookmark_by_node through ConvertContext
- wrap CSS-driven bookmark elements in convert_node
- wire FULGUR_UA_CSS and BookmarkPass into engine
- add Affine2D::transform_point
- add Quad type and Affine2D::transform_rect
- add transform stack to LinkCollector, use Quad instead of Rect
- add transform stack to DestinationRegistry, store x+y coords
- wire transform push/pop in TransformWrapperPageable

### Miscellaneous

- regenerate example PDFs
- regenerate example PDFs
- regenerate example PDFs
- regenerate example PDFs
- regenerate example PDFs

### Performance

- LinkCollector take_page API avoids O(P×L) filter

### Refactor

- hardcode box-shadow inset=false since inset shadows are filtered above
- reuse gcpm::string_set::extract_text_content
- drop unused img fixture and document soft-pin semantics
- skip tests when pdftocairo missing; pin blitz version in doc
- address coderabbit — drop duplicate fs imports, dead write, format CSS
- rename Heading* types to Bookmark* (fulgur-yqi)
- remove h1-h6 hardcode path, rely on UA stylesheet

### Styling

- apply cargo fmt
- fix formatting

### Testing

- add box-shadow fixture, golden, and example
- fix box-shadow fixture to use block-level boxes
- update box-shadow golden after EvenOdd clip fix
- use tempfile::TempDir for CLI tests
- add WOFF2 integration test via PDF render
- pin CSS-relative url() resolution against stylesheet directory
- add failing test for <link media=print> exclusion
- cover comma-separated media attribute in collector
- pin fulgur-owa GCPM duplication with #[ignore] and doc note
- cover nested @import under a print-only <link>
- pin rewrite-path liveness with media=screen
- multi-line anchor emits /QuadPoints and page-crossing emits per-page annotation
- verify GCPM render path and anchor-wrapping image behavior
- cover combined / level-only / label-only / absent rule cases
- cover BookmarkPass cascade, suppression, and label resolution
- verify h1 auto-bookmarks via UA stylesheet (e2e)
- cover suppression, custom CSS, mixed outline, counter graceful skip, label fallback

### Build

- support per-example fulgur.args for CLI flags
- bump thin-vec in the cargo group across 1 directory

### Deps

- add woff2-patched crate and error variants for WOFF support

### Release

- v0.4.5

## [0.4.4] - 2026-04-12

### Bug Fixes

- use bash shebang in mise update-examples task
- regenerate example PDFs in release-prepare workflow
- wrap left/right margin box content in div for height measurement
- include height in render cache key for left/right margin boxes
- update docstring and add defensive first-except handling
- propagate pagination from child in maybe_prepend_string_set
- address coderabbit review on PR #56
- avoid eating style rule closing brace after string-set
- preserve string-set on zero-size elements and skip script/style
- propagate Taffy coordinates to orphan string-set markers
- address coderabbit review on PR #57
- address second-pass review on PR #57
- address code review — remove dead code, handle margin:0, reject negative size
- address AI review feedback on @page settings
- address second-round AI review feedback
- use config.page_size for `size: auto` keyword instead of hardcoded A4
- case-insensitive CSS units and reject trailing tokens
- path traversal defense, panic-safe fd handling, recursion limits
- broader .env gitignore + depth guard in collect_positioned_children
- serialize concurrent access via static BLITZ_LOCK mutex
- gate engine.rs DomPass calls via apply_single_pass
- revert BLITZ_LOCK, remove suppress_stdout fd 1 race
- retry StdoutIsolator::write_all on EINTR
- address coderabbit review on #62
- address AI review — counter edge cases and consistency
- remove duplicate PageSizeDecl/PageSettingsRule definitions in mod.rs
- address AI review on counter-reset PR
- route CounterPass/InjectCssPass through apply_single_pass
- flush stdout buffer in StdoutIsolator::Drop
- push parent GcpmContext after nested @import fetches
- address AI review feedback on PR #68
- sort read_dir output before passing --image args
- add lang attribute and title to VRT fixtures
- wire overflow clip through convert + TablePageable
- address AI review feedback on PR #70
- 0x0 block leaf with pseudo image no longer dropped
- defer zero-size check until after axis expansion
- address integration test review issues
- address AI review feedback
- broaden Point2 doc + cover non-zero draw position
- SVG px→pt conversion and decode_dimensions fallback
- update stale doc comment and reject invalid URLs early
- match background-image pattern for ComputedUrl::Invalid
- guard resolve_list_marker against zero line_height
- address AI review feedback
- use per-line font metrics for vertical-align
- split y_acc for correct multi-line baseline rebase
- add log::warn for SVG background-image parse and draw failures
- correct off-by-one in page count byte scan
- revert off-by-one change that introduced index out-of-bounds risk
- align comment with >= 2 assertion in page-spanning test
- honour explicit line-height in image-only list marker fallback
- correct @media double-wrapping in rewrite_marker_content_url
- handle statement at-rules and quoted URL parens in marker rewrite
- guard against empty selector from bare ::marker rewrite
- skip ListItemPageable for inside-positioned list markers
- shift existing x_offsets when injecting inside image marker
- inject pseudo images before inside marker to preserve CSS order
- exclude inside-positioned items from list-item fallback guard
- use is_none_or instead of map_or(true, ...) for clippy

### CI

- run fulgur-vrt visual regression tests in a dedicated job
- add manual workflow skeleton for chrome golden updates

### Documentation

- add implementation plan for GCPM string-set/string()
- clarify ElementPolicy semantics and add Default derive
- document Start/First equivalence, invalid policy asymmetry, and marker split limitation
- add implementation plan for GCPM element() 4-policy support
- align resolve_element_policy docstring with Start split
- mark blitz thread-safety action items A-D as completed
- fix markdownlint errors in plan file
- add examples/svg with shapes, chart, gradient, opacity
- link stylesheet in examples/svg/index.html
- direct callers to parse_html_with_local_resources, harden test
- unify <link rel=stylesheet> across all examples
- correct font-update checklist (no golden hashes exist)
- add crate README with workflow instructions
- record VRT infrastructure implementation plan
- add overflow-hidden example
- record overflow:hidden implementation plan
- add CSS transform example
- CSS transform implementation plan
- fix markdownlint violations in CSS transform plan
- correct matrix_test_util visibility comment
- add list-style-image example with PNG/SVG bullets
- record list-style-image implementation plan (fulgur-507)
- fix markdownlint command to use repo-standard glob
- add threat model for SaaS multi-tenant use case (en + ja)
- add implementation plan for content: url() Phase 3
- document inside marker limitation for non-inline-root <li>

### Features

- add height measurement for left/right margin boxes
- integrate left/right margin boxes into rendering pipeline
- add StringSet data types and StringRef content item for GCPM string-set support
- parse string-set property and string() function in GCPM parser
- add StringSetStore and StringSetPass for GCPM string-set extraction
- add StringSetPageable zero-size marker for string-set tracking
- insert StringSetPageable markers during DOM-to-Pageable conversion
- collect per-page string set states during pagination
- resolve string() references in counter content resolution
- wire string-set states into margin box rendering
- wire StringSetPass into render_html pipeline
- add ElementPolicy enum and restructure ContentItem::Element
- parse element() policy second argument
- rewrite RunningElementStore with instance-list storage
- record node_id when registering running element instances
- add RunningElementMarkerPageable zero-size marker
- emit RunningElementMarkerPageable at running element source positions
- add collect_running_element_states for per-page instance tracking
- add resolve_element_policy and wire into resolve_content_to_html
- wire per-page running element states through rendering pipeline
- add parse_css_length helper for CSS unit to pt conversion
- add PageSettingsRule, PageSizeDecl, and PageMarginDecl types
- parse @page size/margin declarations into PageSettingsRule
- add ConfigOverrides to track explicit CLI/API settings
- add resolve_page_settings with CLI-priority override logic
- apply resolved @page size/margin per page in GCPM pipeline
- CSS counter-reset / counter-increment / counter-set support
- add SvgPageable skeleton with unit tests
- implement SvgPageable::draw via krilla-svg
- wire inline <svg> elements through convert_svg
- FulgurNetProvider for <link> / @import GCPM parity
- pin font environment for byte-deterministic PDFs
- scaffold dev-only VRT crate
- manifest.toml parser with tolerance defaults
- pixel diff engine with diff image output
- render fulgur PDF to RGBA via pdftocairo
- scaffold chrome-golden screenshot adapter stub
- runner with fulgur golden compare and update modes
- add Overflow enum and BlockStyle overflow fields
- compute_overflow_clip_path helper
- apply overflow clipping in BlockPageable::draw
- read CSS overflow-x/y from stylo in convert
- add extract_pseudo_image_url helper
- emit block-display pseudo content: url() images
- add Affine2D value type for CSS transform
- add TransformWrapperPageable with atomic split
- compute_transform helper reading stylo transform/origin
- wire TransformWrapperPageable into convert_node
- add clamp_marker_size helper for list-style-image
- AssetKind::detect for raster/SVG classification
- list-style-image raster marker support
- list-style-image SVG marker support
- draw inline images in shaped lines
- vertical-align line box recalculation
- inject inline pseudo content: url() images
- support SVG rendering in draw_background_layer via BgImageContent
- detect SVG in background-image via AssetKind and build Svg layer
- support content: url() on normal elements (Phase 3)
- support list-style-image when list-style-type is none
- add rewrite_marker_content_url CSS transformer
- integrate marker content url rewrite into render pipeline
- inject inline image marker for inside list-style-image

### Miscellaneous

- regenerate example PDFs
- regenerate example PDFs
- regenerate example PDFs
- silence pre-existing clippy warnings under -D warnings
- regenerate example PDFs
- regenerate example PDFs
- regenerate example PDFs
- loosen log pin to 0.4
- regenerate example PDFs
- regenerate example PDFs
- fix rustfmt formatting in convert.rs and background_test.rs

### Refactor

- rename distribute_widths to distribute_sizes for axis-independence
- extract axis-independent slot layout in compute_edge_layout
- simplify margin box code with edge() method and unified helpers
- simplify after string-set review
- extract shared parse_policy_ident helper
- tighten RunningElementStore API per review
- conditionally compute running_states matching string_set_states pattern
- cleanup per simplify review
- replace PageMarginDecl with Margin, fix measure_cache key
- use std::mem::discriminant for counter ops dedupe
- extract wrap_replaced_in_block_style helper
- address AI review feedback on PR #66
- remove dead GenericImageView import from diff.rs
- extract make_image_pageable sizing helper
- single-walk pseudo image lookup, avoid O(n) insert
- simplify per code review
- extract_marker_lines returns line_height
- ListItemMarker enum (Text/None)
- replace glyph_runs with LineItem enum
- extract resolve_image_dimensions, clean up review items
- introduce BgImageContent enum for Raster/SVG background layers
- deduplicate background-layer attribute computation
- narrow compute_overflow_clip_path visibility to pub(crate)
- rename extract_pseudo_image_url to extract_content_image_url
- extract list-item body builder and clean up fallback path
- use PX_TO_PT constant and eliminate redundant style lookups

### Styling

- fix rustfmt and revert clippy-redundant guard
- fix rustfmt formatting
- fix rustfmt import ordering for CI
- fix rustfmt long-string wrapping in counter ops test
- add PartialEq, Debug, Copy derives per review

### Testing

- add integration tests for left/right margin boxes
- add regression test for asymmetric side margin boxes
- add integration tests for GCPM string-set/string() pipeline
- integration tests for element() policy across multiple pages
- add integration tests for @page size/margin declarations
- verify border and padding wrapping for <svg>
- strengthen border/padding assertion via plain-SVG baseline
- verify multiple SVGs render on same page
- verify SVG is atomic (no page split)
- verify opacity and visibility propagation
- gate committed PDF match check to Linux
- add initial fixture set and fulgur goldens
- add overflow-hidden VRT fixtures
- end-to-end integration tests for CSS transform
- split keeps image marker on first list fragment only
- tighten list-style-image SVG assertion
- add unit test for SVG background layer resolve_size
- add integration test for SVG background-image rendering
- add page-spanning overflow:hidden integration test
- add edge case tests for content: url() normal element
- add integration tests for marker content url with image asset
- add integration tests for list-style-position: inside
- add integration test for inside position + list-style-image
- add mixed-mode regression tests for list-style-position

### Deps

- bump krilla to 0.7, add krilla-svg and usvg

### Example

- add block pseudo content: url() example
- add inline pseudo and vertical-align cases

### Release

- v0.4.4

## [0.4.3] - 2026-04-04

### Bug Fixes

- rename format filter to numformat, add built-in filter list to help
- correct zero-padding for negative numbers in numformat filter

### Documentation

- add template filter reference to render --help

### Features

- add Python-style format filter to MiniJinja templates

### Release

- v0.4.3

## [0.4.2] - 2026-04-04

### Bug Fixes

- avoid double opacity and add opacity to ListItemPageable
- correct visibility semantics and add TablePageable opacity support
- propagate visibility to synthetic children in styled wrappers
- resolve Stylo absolute URLs for background-image asset lookup
- correct JPEG SOF offset and add border-radius clip to background images
- guard against infinite loop on malformed JPEG segment length
- suppress clippy result_unit_err warning on to_krilla_image
- early return on non-positive clip box in draw_background_layer
- compute inner border-radii for background-image clip
- correct modulo operand order in background repeat tiling
- address code review issues in schema extraction
- suppress large_enum_variant clippy warning on Commands enum
- address AI review feedback on schema extraction
- recurse into GetAttr base expr when path resolution fails
- address coderabbit review round 3
- independent if-branch scoping and List/Map expression collection
- process remaining stmts with each if-branch scope independently

### Documentation

- add MiniJinja and template schema hint to --data help

### Features

- add opacity and visible fields to ParagraphPageable
- add opacity and visible fields to ImagePageable
- add opacity and visible fields to BlockPageable
- extract opacity and visibility from CSS computed styles
- add background image data structures to pageable module
- extract CSS background-image properties from Stylo computed styles
- add background.rs with image layer rendering (size, position, repeat, clip)
- enable minijinja unstable_machinery for AST access
- add AST-based schema extraction from MiniJinja templates
- add sample JSON data matching for precise type inference
- add fulgur template schema CLI command
- support stdin for --data in template schema command

### Miscellaneous

- regenerate example PDFs

### Refactor

- extract draw_with_opacity helper and merge extract functions

### Styling

- fix formatting in convert.rs
- fix formatting in pageable.rs draw_background calls
- apply cargo fmt to compute_inner_radii

### Testing

- add integration tests for opacity and visibility
- verify PDF Transparency Group presence in opacity tests
- add regression tests for visibility propagation to synthetic children
- add transparency group assertion to list item visibility test

### Release

- v0.4.2

## [0.4.1] - 2026-04-02

### Bug Fixes

- remove invalid diff datastore from octocov config
- pin third-party Actions to commit SHAs
- fail on missing lcov artifact, run octocov on main push for baseline
- remove duplicate test execution in coverage path
- remove cross-repo push, use artifact-only for central mode
- address review feedback on update-examples workflow
- pass --image flags when regenerating example PDFs
- skip non-visual elements in RunningElementPass walk
- fallback to cwd when input has no parent dir, add http:// test
- use case-insensitive token matching for rel="stylesheet"
- address code review feedback
- correct template API usage in README example

### CI

- add workflow to auto-update example PDFs on PRs

### Documentation

- add octocov coverage badges to README
- add running element DomPass implementation plan
- add link-stylesheet example demonstrating <link> CSS loading
- add template engine example with invoice template
- add template engine design and implementation plan
- add generated PDF for template example
- update README with template engine and improved positioning
- add 3-value margin shorthand to options table

### Features

- add octocov for PR coverage comments
- add detailed octocov reporting (code-to-test ratio, diff coverage)
- send coverage report to octocovs central repo for badge generation
- add RunningElementPass to DomPass pipeline
- integrate RunningElementPass into DomPass pipeline
- add base_path field to Engine for resolving relative stylesheet paths
- add LinkStylesheetPass to resolve local <link> stylesheets
- integrate LinkStylesheetPass into Engine pipeline
- auto-set base_path in CLI for resolving linked stylesheets
- add minijinja and serde_json dependencies for template engine
- add Template error variant
- add template.rs with MiniJinja render_template function
- add template/data support to Engine and EngineBuilder
- add --data flag for template mode in CLI

### Miscellaneous

- regenerate example PDFs
- regenerate example PDFs

### Refactor

- separate build and test steps in CI for clearer timing
- remove running element detection from convert.rs
- remove unnecessary Vec clone in walk_tree
- extract get_attr and inject_style_node helpers in blitz_adapter
- simplify template engine code

### Testing

- add template engine integration tests
- add error handling tests for template engine

### Release

- v0.4.1

## [0.4.0] - 2026-03-25

### Bug Fixes

- correct text overlap caused by double-counted baseline in draw_shaped_lines
- correct text overlap caused by double-counted baseline in draw_shaped_lines
- register injected CSS with Stylo via upsert_stylesheet_for_node
- rebase absolute baselines in ParagraphPageable::split second fragment
- add default split_boxed impl and table split_boxed regression test

### Features

- add DomPass trait and parse/resolve split to blitz_adapter
- implement InjectCssPass for DOM-based CSS injection
- add font data cache to ConvertContext for Arc sharing
- add split_boxed to Pageable trait for zero-copy page splitting

### Miscellaneous

- exclude CHANGELOG.md from markdownlint (auto-generated by git-cliff)

### Refactor

- replace text-matching CSS injection with DomPass pipeline
- deduplicate parse_and_layout by delegating to parse + resolve
- address code review feedback for font cache
- extract find_split_point to deduplicate split/split_boxed logic

### Release

- v0.4.0

## [0.3.1] - 2026-03-22

### Bug Fixes

- wire --language to EngineBuilder, validate margin tokens
- strict parsing in parse_datetime, reject invalid components
- address code review feedback on markdownlint setup
- address code review feedback
- preserve Taffy-computed height for styled container nodes
- address second round of code review feedback

### Documentation

- add CLI expansion implementation plan
- update README with image support and deterministic output

### Features

- add metadata fields to Config (description, keywords, creator, producer, creation_date)
- add metadata setters to EngineBuilder
- add --margin, metadata flags, and stdout output to CLI
- implement creation_date parsing for PDF metadata
- add image key normalization to AssetBundle
- detect <img> elements and create ImagePageable from AssetBundle
- add --image CLI flag and image rendering example

### Miscellaneous

- disable markdownlint for docs/plans/ in CodeRabbit
- add Claude Code settings with worktree sparse-checkout hook
- add markdownlint-cli2 with CI integration
- add Claude Code rule for markdownlint

### Refactor

- extract build_metadata helper, add new metadata fields to PDF output
- introduce ConvertContext to bundle conversion state
- extract get_attr helper, simplify image key normalization

### Testing

- add integration tests for <img> rendering

### Release

- v0.3.1

## [0.3.0] - 2026-03-22

### Bug Fixes

- use --unreleased instead of --latest for release notes generation
- correct crate name in README (fulgur_core → fulgur)
- use imported Engine instead of fully-qualified path in README
- address PR review — stray } in cleaned_css, case-insensitive at-rules
- reject compound/group selectors, add regression tests and PDF header checks
- skip running extraction for unsupported selectors to prevent stale display:none

### Documentation

- add cssparser rewrite implementation plan

### Features

- add ParsedSelector and RunningMapping types, replace running_names in GcpmContext

### Miscellaneous

- stop tracking .beads/issues.jsonl
- enable beads sync-branch config
- add cssparser as direct dependency for GCPM parser

### Refactor

- rewrite GCPM parser to use cssparser crate
- simplify QualifiedRuleParser prelude type and remove dead code
- switch convert.rs DOM matching from class names to ParsedSelector

### Styling

- apply cargo fmt

### Testing

- add edge case tests for cssparser-based GCPM parser
- add integration tests for ID and tag selector running elements

### Release

- v0.3.0

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

### Release

- v0.2.0

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


