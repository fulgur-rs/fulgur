use crate::asset::AssetBundle;
use crate::config::{Config, ConfigBuilder, Margin, PageSize};
use crate::convert::ConvertContext;
use crate::error::Result;
use crate::units::F32Units;
use krilla::SerializeSettings;
use std::collections::{BTreeMap, HashMap};
use std::ops::DerefMut;
use std::path::{Path, PathBuf};

/// Reusable PDF generation engine.
pub struct Engine {
    config: Config,
    assets: Option<AssetBundle>,
    base_path: Option<PathBuf>,
    template: Option<(String, String)>,
    data: Option<serde_json::Value>,
    serialize_settings: SerializeSettings,
    system_fonts: bool,
}

/// Renderer-agnostic layout result produced by [`Engine::layout`].
///
/// Carries the parse → style → layout → `Drawables` output that both PDF
/// rendering and out-of-core consumers (image rasterization, OCR label
/// generation) build on, without pulling PDF serialization into core.
///
/// Unit contract: `drawables` coordinates are PDF pt; `geometry` fragments are
/// CSS px (`units::Px`). See the crate `units` module and
/// `.claude/rules/coordinate-system.md`.
///
/// # Limitations
///
/// This carries only **body** layout. CSS Paged Media constructs that
/// [`render`](Engine::render) paints from the render-side GCPM state —
/// `@page` margin boxes, `position: running()` headers/footers, and the page
/// numbers rendered inside them — are **not** included in `drawables` /
/// `geometry`; an image / OCR consumer composing from `LayoutOutput` alone
/// will omit them (fulgur-2map design doc §1/§9).
///
/// Likewise, when author CSS overrides the page box (`@page { size / margin }`,
/// including `:left` / `:right` / `:first`), the pipeline paginates against the
/// **resolved** content box, but the resolved page size / margin is not yet
/// surfaced here — a consumer must not assume [`Engine::config`] alone
/// describes the canvas. Surfacing margin boxes and resolved page geometry is a
/// tracked follow-up ([fulgur-2map.10] notes).
///
/// This struct is `#[non_exhaustive]`: it is only ever returned by
/// [`Engine::layout`] (consumers read fields, never construct it), so those
/// follow-up fields can be added without a breaking change.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LayoutOutput {
    pub drawables: crate::drawables::Drawables,
    pub geometry: crate::pagination_layout::PaginationGeometryTable,
}

/// Full per-pass output of [`Engine::layout_to_drawables`] — a superset of the
/// public [`LayoutOutput`] holding every side-channel `render::render_v2`
/// needs. `fonts` / `system_fonts` are intentionally absent: both are re-derived
/// from `&self` at the render call site, byte-identically to the old inline path.
struct LayoutArtifacts {
    drawables: crate::drawables::Drawables,
    pagination_geometry: crate::pagination_layout::PaginationGeometryTable,
    gcpm: crate::gcpm::GcpmContext,
    running_store: crate::gcpm::running::RunningElementStore,
    string_set_for_render: HashMap<usize, Vec<(String, String)>>,
    counter_ops_for_render: BTreeMap<usize, Vec<crate::gcpm::CounterOp>>,
    html_title: Option<String>,
    implicit_href_map: BTreeMap<usize, String>,
    collected_anchor_map: AnchorMap,
    needs_pass_two: bool,
}

impl Engine {
    pub fn builder() -> EngineBuilder {
        EngineBuilder {
            config_builder: Config::builder(),
            assets: None,
            base_path: None,
            template: None,
            data: None,
            serialize_settings: SerializeSettings::default(),
            system_fonts: true,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn base_path(&self) -> Option<&Path> {
        self.base_path.as_deref()
    }

    pub fn assets(&self) -> Option<&AssetBundle> {
        self.assets.as_ref()
    }

    /// Font byte-blobs registered on the [`AssetBundle`], or an empty slice
    /// when no assets are set. Shared by every layout / render entry point so
    /// they all parse against the identical font set.
    fn fonts(&self) -> &[std::sync::Arc<Vec<u8>>] {
        self.assets
            .as_ref()
            .map(|a| a.fonts.as_slice())
            .unwrap_or(&[])
    }

    /// Render HTML string to PDF bytes.
    /// If an AssetBundle is set, its CSS will be injected as a <style> block.
    /// When GCPM constructs (margin boxes, running elements) are detected in the CSS,
    /// a 2-pass rendering pipeline is used: pass 1 paginates body content, pass 2
    /// renders each page with resolved margin boxes.
    ///
    /// `target-counter()` / `target-counters()` / `target-text()` add a
    /// second axis of 2-pass rendering: pass 1 paginates the document and
    /// builds an `AnchorMap` (fragment id → page / counter / text); pass
    /// 2 re-renders with that map so the resolvers in
    /// `gcpm::counter::resolve_content_to_*_with_anchor` and
    /// `CounterPass::with_anchor_map` substitute real values instead of
    /// fixed-width placeholders.
    pub fn render(&self, html: &str) -> Result<Vec<u8>> {
        // Pass 1: layout only. `layout_to_drawables` parses the full GCPM
        // context (AssetBundle, `<link>`-loaded stylesheets, inline `<style>`
        // blocks) and reports `needs_pass_two` based on that parsed view, so
        // `target-counter()` / `target-counters()` / `target-text()` declared
        // in any of those locations is detected reliably.
        //
        // Pass 1 does NOT invoke `render_v2` — its PDF output would be
        // discarded on the 2-pass path (fulgur-6vl0). Mirrors `layout()`'s
        // 2-pass loop; the final PDF serialization happens exactly once.
        let pass1 = self.layout_to_drawables(html, None)?;
        if !pass1.needs_pass_two {
            return self.render_artifacts(pass1, None);
        }
        // Pass 2: re-lay-out with the pass-1 AnchorMap so `target-*` resolvers
        // substitute resolved values instead of fixed-width placeholders, then
        // serialize once.
        let LayoutArtifacts {
            collected_anchor_map: anchor_map,
            ..
        } = pass1;
        let pass2 = self.layout_to_drawables(html, Some(&anchor_map))?;
        self.render_artifacts(pass2, Some(&anchor_map))
    }

    /// Run the full parse → style → layout → convert pipeline for a single
    /// pass and return the resolved [`LayoutArtifacts`] (drawables, pagination
    /// geometry, and the side-channel data `render::render_v2` needs) —
    /// **without** serializing a PDF.
    ///
    /// When `anchor_map` is `Some`, the supplied pass-1 map is wired into
    /// [`CounterPass`] so `target-counter()` / `target-counters()` /
    /// `target-text()` inside `::before` / `::after` resolve against pass-1
    /// anchor data, and is carried in the returned artifacts so margin-box
    /// `target-*` resolvers in `render::render_v2` can do the same. When
    /// `None`, those resolvers fall back to placeholders / empty strings.
    ///
    /// This is the single shared layout path: both [`render`](Engine::render)
    /// (which feeds the artifacts to `render_artifacts` for PDF serialization)
    /// and the public [`layout`](Engine::layout) build on it. See
    /// [`LayoutArtifacts`] for the returned fields.
    fn layout_to_drawables(
        &self,
        html: &str,
        anchor_map: Option<&AnchorMap>,
    ) -> Result<LayoutArtifacts> {
        // Reject non-finite/non-positive page size or margin values before
        // they reach Blitz parsing below — an unvalidated f32 here can
        // otherwise saturate to e.g. a u32::MAX viewport height.
        self.config.validate()?;

        let html = crate::blitz_adapter::rewrite_marker_content_url_in_html(html);

        let combined_css = self
            .assets
            .as_ref()
            .map(|a| a.combined_css())
            .unwrap_or_default();
        let combined_css = crate::blitz_adapter::rewrite_marker_content_url(&combined_css);

        let mut gcpm = crate::gcpm::parser::parse_gcpm(&combined_css);
        // `css_to_inject` drives `InjectCssPass` below, which writes a
        // plain, unconditional `<style>` into the document — there is no
        // way to attach a `media` attribute to it. AssetBundle / `--css`
        // CSS has no media scoping to begin with, so snapshotting it here
        // (before the `<link>` fold below) is correct and required: unlike
        // `<link>`-sourced CSS, it's fine for this to be unconditional.
        let mut css_to_inject = gcpm.cleaned_css.clone();

        let fonts = self.fonts();

        // Parse the HTML and resolve every <link rel="stylesheet"> /
        // @import file inside `base_path` in one shot. The returned
        // `link_gcpm` carries the GCPM constructs extracted from those
        // stylesheets, which we fold into the AssetBundle-derived
        // context below.
        //
        // `cleaned_css` is folded into `gcpm.cleaned_css` too — but
        // deliberately NOT into `css_to_inject` above. `gcpm.cleaned_css`
        // is consumed by `render.rs` as the sole stylesheet for the
        // margin-box mini-documents (see `render_to_pdf_with_gcpm` and
        // `strip_display_none`), where declarations like
        // `.pageHeader { font-size: 8px; }` defined in a `<link>`-loaded
        // stylesheet need to reach the margin-box renderer. But
        // `<link>`-sourced CSS is *also* independently served straight to
        // Blitz's native cascade — cleaned and already media-aware — by
        // `net::FulgurNetProvider::fetch` (it runs `parse_gcpm` per fetched
        // stylesheet and hands Blitz the cleaned text, respecting whatever
        // `media` rewrite `apply_link_media_rewrites` applied). Folding
        // `link_gcpm.cleaned_css` into `css_to_inject` here as well would
        // inject it a second time via `InjectCssPass`, which writes an
        // unconditional `<style>` with no media attribute — bypassing
        // `<link media="print">` exclusion on screen renders (regression
        // caught by `link_media_attribute.rs`'s
        // `link_media_print_does_not_apply_on_screen`).
        let (mut doc, link_gcpm) = crate::blitz_adapter::parse_html_with_local_resources(
            &html,
            self.config.content_width().as_pt().in_px().to_f32(),
            self.config.page_height().as_pt().in_px().to_f32() as u32,
            fonts,
            self.system_fonts,
            self.base_path.as_deref(),
        );
        gcpm.extend_from(link_gcpm);

        // Inline `<style>` blocks in the HTML are parsed by stylo for
        // regular CSS but never passed through `parse_gcpm`. Walk the
        // DOM to collect any `@page`, margin-box, running-element, and
        // counter constructs declared inline so they are honored
        // alongside the AssetBundle / link-loaded contexts (fulgur-mq5).
        //
        // Unlike `<link>`, inline `<style>` has no interception point
        // equivalent to `net::FulgurNetProvider::fetch` — it goes through
        // Blitz's native HTML parser untouched. `InjectCssPass` /
        // `css_to_inject` is therefore the ONLY place that can apply
        // `parse_gcpm`'s `display: none` rewrite for
        // `position: running(name)` declared inline. Omitting it used to
        // mean the rewrite never reached the DOM for
        // inline-`<style>`-sourced running elements — the "real" copy
        // rendered a second time alongside its `@page` margin-box copy
        // (fulgur-css-flag-running, follow-up to the --css +
        // hidden-ancestor running-element fix).
        //
        // Inject ONLY the generated `display: none` rules, not
        // `inline_gcpm.cleaned_css`. `parse_gcpm` preserves all non-GCPM
        // CSS verbatim in `cleaned_css`, so folding the whole string in
        // re-injects the author's entire inline stylesheet as the last
        // child of `<head>`, so a `<link>` that followed the `<style>` in
        // source order loses specificity ties it should win. Regression
        // coverage:
        // `render_smoke.rs::inline_style_before_link_keeps_cascade_order`.
        //
        // It would also strip any `<style media="...">` scoping, since
        // `InjectCssPass` writes a plain `<style>` with no media
        // attribute. That one is currently moot — blitz-dom 0.2.4 ignores
        // `media` on inline `<style>` just as it does on `<link>` (which
        // is why `LinkMediaRewritePass` exists), so the author's own copy
        // is unscoped too. Injecting only the generated rules keeps this
        // path from becoming a second thing to fix if inline `media`
        // support lands.
        //
        // Concatenation mirrors `GcpmContext::extend_from`'s
        // newline-joining.
        let inline_gcpm = crate::blitz_adapter::extract_gcpm_from_inline_styles(&doc);
        let inline_running_css =
            crate::blitz_adapter::build_running_display_none_css(&inline_gcpm.running_mappings);
        if !inline_running_css.is_empty() {
            if !css_to_inject.is_empty() {
                css_to_inject.push('\n');
            }
            css_to_inject.push_str(&inline_running_css);
        }
        gcpm.extend_from(inline_gcpm);

        // Cache the predicate once gcpm is fully populated. It feeds three
        // gates inside this pass (snapshot recording, the bookmark+anchor
        // counter-snapshots split, and the AnchorMap-build gate); recomputing
        // it would re-walk every margin-box rule and content-counter mapping
        // each time.
        let has_target_refs = gcpm.has_target_references();

        // fulgur-lv0a: resolve the page-1 `@page` size + margin NOW so we can
        // update Blitz's viewport BEFORE the first `resolve()` pass. The
        // viewport originally passed to `parse_html_with_local_resources`
        // (line 79) used `self.config.page_height()` because `@page` overrides
        // were not yet known — Stylo would otherwise bind viewport-relative
        // units (`vh` / `vw` / `vmin` / `vmax`) to the full page area, ignoring
        // the @page margin. With the viewport corrected to the resolved
        // content area, `100vh` resolves to the actual content box used by
        // pagination / fixed-element layout / margin-box rendering.
        let (resolved_page_size, resolved_page_margin, resolved_landscape) =
            crate::gcpm::page_settings::resolve_page_settings(
                &gcpm.page_settings,
                1,
                0,
                &self.config,
                false, // RTL not yet known at viewport setup; LTR assumed
            );
        let resolved_page_size = if resolved_landscape {
            resolved_page_size.landscape()
        } else {
            resolved_page_size
        };
        // Clamp to non-negative — defensive against pathological CSS like
        // `@page { margin: 1000mm }` where margins exceed the page size.
        // Without the clamp the resulting negative value would silently flip
        // sign across `as u32` (saturating to 0 for Stylo's viewport) but
        // remain negative when fed to Taffy / `viewport_size_px`, causing
        // divergence between the layers (CodeRabbit on PR #338).
        let resolved_content_width_pt =
            (resolved_page_size.width - resolved_page_margin.left - resolved_page_margin.right)
                .max(0.0);
        let resolved_content_height_pt =
            (resolved_page_size.height - resolved_page_margin.top - resolved_page_margin.bottom)
                .max(0.0);
        // Compute the px-space content box once and reuse the same f32
        // values for every downstream consumer (Stylo viewport,
        // `relayout_position_fixed`, the pagination fragmenter, the v2
        // ConvertContext) so they all see an identical content area.
        // `set_viewport_size_px` truncates to u32 internally for Blitz's
        // `Viewport.window_size`; Taffy keeps the f32 sub-pixel precision
        // it needs for its layout cache.
        let resolved_content_width_px = resolved_content_width_pt.as_pt().in_px().to_f32();
        let resolved_content_height_px = resolved_content_height_pt.as_pt().in_px().to_f32();
        crate::blitz_adapter::set_viewport_size_px(
            &mut doc,
            resolved_content_width_px,
            resolved_content_height_px,
        );

        // Prepend UA CSS bookmark mappings so author-CSS rules (appearing
        // later in `bookmark_mappings`) override them via last-match
        // cascade. Skipped when bookmarks are disabled to avoid unnecessary
        // CSS parsing and DOM traversal.
        if self.config.effective_bookmarks() {
            let ua_gcpm = crate::gcpm::parser::parse_gcpm(crate::gcpm::ua_css::FULGUR_UA_CSS);
            let mut combined_bookmarks = ua_gcpm.bookmark_mappings;
            combined_bookmarks.extend(gcpm.bookmark_mappings);
            gcpm.bookmark_mappings = combined_bookmarks;
        }

        // Build and apply DOM passes
        let mut passes: Vec<Box<dyn crate::blitz_adapter::DomPass>> = Vec::new();

        if !css_to_inject.is_empty() {
            passes.push(Box::new(crate::blitz_adapter::InjectCssPass {
                css: css_to_inject,
            }));
        }

        // Restructure `<table><caption>` before layout so Blitz lays the
        // caption out as a normal block (it otherwise drops it during table
        // box construction). This runs *after* InjectCssPass so the pass's
        // pre-resolve (which reads computed `caption-side`) sees engine- and
        // AssetBundle-injected CSS, not just document `<style>`/`<link>`.
        // See blitz_adapter::CaptionRestructurePass.
        passes.push(Box::new(crate::blitz_adapter::CaptionRestructurePass));

        let ctx = crate::blitz_adapter::PassContext { font_data: fonts };
        crate::blitz_adapter::apply_passes(&mut doc, &passes, &ctx);

        // Extract running elements via DomPass (before resolve)
        let running_store = if !gcpm.running_mappings.is_empty() {
            let pass = crate::blitz_adapter::RunningElementPass::new(gcpm.running_mappings.clone());
            crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
            pass.into_running_store()
        } else {
            crate::gcpm::running::RunningElementStore::new()
        };

        // BookmarkPass downstream consumes per-node snapshots from
        // StringSetPass and CounterPass when (and only when) bookmarks
        // will actually be emitted. The 2-pass `target-*` path
        // (`gcpm.has_target_references()`) also needs the per-node
        // counter snapshots so `build_anchor_map` can populate
        // `AnchorEntry.counters` for pass 2 — without this the
        // `target-counter(attr(href), section)` family resolves to
        // empty strings even though `attr(href), page` still works
        // via `AnchorEntry.page_num`. Compute the gate once here so
        // each pass can opt out of the per-element clone otherwise.
        let bookmark_active =
            self.config.effective_bookmarks() && !gcpm.bookmark_mappings.is_empty();
        // Counter snapshots feed BOTH bookmark `counter()` resolution and
        // the target-ref anchor map, so record them for either trigger.
        let record_counter_snapshots = bookmark_active || has_target_refs;
        // String snapshots are consumed ONLY by BookmarkPass's `string()`
        // resolution — `build_anchor_map` reads counter snapshots, not
        // string ones — so recording them for a target-ref-only render is
        // pure waste and, before the `MAX_STRING_SNAPSHOT_BYTES` budget,
        // an attacker-reachable amplification sink via inline `target-*`
        // CSS with no bookmark opt-in. Gate them on bookmarks alone.
        // NOTE: if a future feature makes `string()` resolve in a
        // target-ref / `running()` context (fulgur-ejw9), widen this gate.
        let record_string_snapshots = bookmark_active;

        // Extract string-set values via DomPass.
        // Also harvest per-node `name -> latest value` snapshots that the
        // later BookmarkPass uses to resolve `string(name)` inside
        // `bookmark-label` (fulgur-70c).
        let (string_set_store, string_snapshots) = if !gcpm.string_set_mappings.is_empty() {
            let mut pass =
                crate::blitz_adapter::StringSetPass::new(gcpm.string_set_mappings.clone());
            if record_string_snapshots {
                pass = pass.with_snapshot_recording();
            }
            crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
            let snapshots = pass.take_node_snapshots();
            (pass.into_store(), snapshots)
        } else {
            (
                crate::gcpm::string_set::StringSetStore::new(),
                BTreeMap::new(),
            )
        };

        // Extract counter operations and resolve body content.
        // Also harvest per-node counter snapshots for BookmarkPass
        // (`counter(name)` / `counters(name, sep)` inside
        // `bookmark-label`, fulgur-70c / fulgur-vsv). Each snapshot
        // value is the full nesting chain (`Vec<i32>`, outer-to-inner)
        // per CSS Lists 3 §4.5, so both `counter()` (innermost) and
        // `counters()` (joined) can resolve directly.
        let (counter_ops_by_node_vec, counter_css, counter_snapshots) =
            if !gcpm.counter_mappings.is_empty() || !gcpm.content_counter_mappings.is_empty() {
                let mut pass = crate::blitz_adapter::CounterPass::new(
                    gcpm.counter_mappings.clone(),
                    gcpm.content_counter_mappings.clone(),
                );
                if record_counter_snapshots {
                    pass = pass.with_snapshot_recording();
                }
                if let Some(map) = anchor_map {
                    pass = pass.with_anchor_map(map.clone());
                }
                crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
                let snapshots = pass.take_node_snapshots();
                let (ops, css) = pass.into_parts();
                (ops, css, snapshots)
            } else {
                (Vec::new(), String::new(), BTreeMap::new())
            };

        // Inject counter-resolved CSS for ::before/::after. Must happen
        // before BookmarkPass's selector matching so any `data-fulgur-cid`
        // attributes added by CounterPass are visible.
        if !counter_css.is_empty() {
            let inject_pass = crate::blitz_adapter::InjectCssPass { css: counter_css };
            crate::blitz_adapter::apply_single_pass(&inject_pass, &mut doc, &ctx);
        }

        // Inject flattened static pseudo-content (multi-item string `content:`
        // lists). One selector-level rule per mapping — no per-node expansion,
        // so this stays O(mappings) even for a hostile document. Injected after
        // the author stylesheet so the flattened rule wins by source order and
        // renders in full where blitz-dom would truncate to `items[0]`
        // (fulgur-2ykw). Works for AssetBundle / `<link>` CSS *and* inline
        // `<style>` (whose original text is never rewritten to `cleaned_css`).
        if !gcpm.static_content_mappings.is_empty() {
            let static_css =
                crate::blitz_adapter::build_static_content_css(&gcpm.static_content_mappings);
            if !static_css.is_empty() {
                let inject_pass = crate::blitz_adapter::InjectCssPass { css: static_css };
                crate::blitz_adapter::apply_single_pass(&inject_pass, &mut doc, &ctx);
            }
        }

        // BookmarkPass runs AFTER CounterPass and StringSetPass so it can
        // resolve `counter()` / `string()` inside `bookmark-label` against
        // the per-node snapshots harvested above (fulgur-70c).
        //
        // The 2-pass `target-*` path also needs `counter_snapshots` —
        // `build_anchor_map` reads them later to populate
        // `AnchorEntry.counters` so `target-counter(href, section)` etc.
        // resolve to the chain at the destination. `BookmarkPass`
        // consumes the map by value, so when both gates fire we have to
        // clone first; the clone cost is paid only when bookmarks +
        // `target-*` are active simultaneously. `bookmark_active` is
        // computed once above (shared with the snapshot-recording gates).
        let target_refs_active = has_target_refs;
        let (counter_snapshots_for_bookmark, counter_snapshots_for_anchor) =
            match (bookmark_active, target_refs_active) {
                (true, true) => (counter_snapshots.clone(), counter_snapshots),
                (true, false) => (counter_snapshots, BTreeMap::new()),
                (false, true) => (BTreeMap::new(), counter_snapshots),
                (false, false) => (BTreeMap::new(), BTreeMap::new()),
            };
        let bookmark_by_node: HashMap<usize, crate::blitz_adapter::BookmarkInfo> =
            if bookmark_active {
                let pass = crate::blitz_adapter::BookmarkPass::new_with_snapshots(
                    gcpm.bookmark_mappings.clone(),
                    counter_snapshots_for_bookmark,
                    string_snapshots,
                );
                crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
                pass.into_results().into_iter().collect()
            } else {
                HashMap::new()
            };

        crate::blitz_adapter::resolve(&mut doc);

        // The `@page` size / margin and resolved content box were computed
        // earlier (right after `extract_gcpm_from_inline_styles`) so the
        // first `resolve()` above already cascaded against the corrected
        // viewport. The bindings — `resolved_page_size`,
        // `resolved_page_margin`, `resolved_landscape`,
        // `resolved_content_width_pt` / `_height_pt`, and the px-space
        // `resolved_content_width_px` / `_height_px` — are reused here for
        // `relayout_position_fixed`, the pagination fragmenter, and
        // downstream margin-box rendering.

        // Second layout pass: re-run Taffy on every `position: fixed` subtree
        // with the page area as available space. Without this, stylo_taffy
        // collapses Fixed → Absolute and lays each fixed element out against
        // its nearest positioned ancestor, producing wrong sizes whenever the
        // fixed element is nested inside a shrink-to-fit abs (fixedpos-002 et
        // al.). The position math itself is corrected later inside
        // `convert::positioned::build_absolute_*_children` via the body cb_h
        // viewport fallback in `resolve_cb_for_absolute`.
        crate::blitz_adapter::relayout_position_fixed(
            &mut doc,
            resolved_content_width_px,
            resolved_content_height_px,
        );

        // Harvest Phase A `column-*` properties (column-fill, column-rule-*)
        // that stylo 0.8.0 gates behind its gecko engine. The side-table is
        // consumed first by the multicol layout hook (for column-fill) and
        // then by the convert pass (for column-rule wrapping).
        let column_styles = crate::blitz_adapter::extract_column_style_table(&doc);
        // Blitz treats multicol containers as plain blocks; route them
        // through fulgur's Taffy hook so columns balance and siblings
        // shift in lockstep. The returned geometry table captures per-
        // `ColumnGroup` layout for column-rule rendering; we thread it
        // through `ConvertContext` so the convert pass can wrap multicol
        // containers with the rule spec + geometry they need to render.
        // See docs/plans/2026-04-20-css-multicol-design.md and
        // docs/plans/2026-04-21-fulgur-v7a-column-rule.md.
        let multicol_geometry = crate::multicol_layout::run_pass(doc.deref_mut(), &column_styles);

        // Run the pagination_layout fragmenter (fulgur-4cbc). Walks
        // body's children's existing `final_layout` (populated by
        // `resolve()` and `multicol_layout::run_pass`) and produces a
        // per-node `PaginationGeometryTable`. fulgur-cj6u Phase 1.1
        // captures the result on `ConvertContext` so future consumers
        // — parity assertion, counter / string-set replacement,
        // per-page fixed repetition redesign — can read it without
        // re-walking layout.
        //
        // Side-effect safety: `run_pass_with_break_and_running` is a
        // read-only walk of `final_layout` via
        // `fragment_pagination_root` — it does not re-drive Taffy or
        // mutate any node's layout. The wrapper's `LayoutPartialTree`
        // / `RoundTree` / `CacheTree` / `TraversePartialTree` impls
        // are kept compile-time live as scaffolding for a future
        // per-strip-constrained variant and are exercised at runtime
        // only by the test-gated `drive_taffy_root_layout` (see
        // `pagination_layout.rs` module docs). VRT /
        // examples_determinism / WPT all stay byte-identical with
        // this call inserted.
        //
        // fulgur-s67g Phase 2.2: thread `running_store` so the
        // fragmenter skips `position: running()` named children. They
        // belong in `@page` margin boxes, not body flow, so including
        // their height would over-count body-flow strip height.
        //
        // fulgur-s67g Phase 2.6 (`@page` size / margin resolution):
        // resolve the page-1 size + margin from `gcpm.page_settings`
        // before driving the fragmenter, so its strip height matches
        // `render_to_pdf_with_gcpm`'s `content_height` exactly. Both
        // sides use the page-1 result for *all* pages and do not
        // re-resolve per-page size for `:left` / `:right` / named
        // selectors.
        // This lets the parity gates drop the
        // `(content_height - config.content_height()).abs() < 0.001`
        // skip: documents that override page size / margin via
        // `@page { size: ...; margin: ...; }` now feed the fragmenter a
        // matching strip height by construction.
        // The page-1 `@page` size / margin was resolved above (before
        // `relayout_position_fixed`). Reuse those resolved dimensions
        // here so the fragmenter, fixed-element layout, and viewport
        // sizing all share a single content box — see the resolve block
        // up at the start of this function.
        let mut pagination_geometry = crate::pagination_layout::run_pass_with_break_and_running(
            doc.deref_mut(),
            resolved_content_height_px,
            &column_styles,
            &running_store,
        );

        // fulgur-rpvu: append per-page fragments for every `position:
        // fixed` element so v2's geometry-driven dispatch repeats them
        // on every page. The fragmenter itself skips out-of-flow nodes
        // (`fragment_pagination_root` `continue` for `Pos::Fixed`), so
        // without this pass `position: fixed` elements never reach
        // `dispatch_fragment` under v2. v1's `PositionedChild::is_fixed`
        // slice path provides the same per-page repetition until PR 8
        // deletes v1; both paths produce equivalent observable output.
        // The added geometry entries set `is_repeat = true` so paragraph
        // / block slicers know each fragment carries the *full* content
        // rather than a slice (see `PaginationGeometry::is_split`).
        let total_pages = crate::pagination_layout::implied_page_count(&pagination_geometry).max(1);
        crate::pagination_layout::append_position_fixed_fragments(
            &mut pagination_geometry,
            doc.deref_mut(),
            total_pages,
            resolved_content_width_px,
            resolved_content_height_px,
        );
        // fulgur-a8m5: emit fragments for body-direct
        // `position: absolute` children whose effective CB is the
        // viewport (body's box collapses to zero when every child is
        // out-of-flow — CSS 2.1 §10.1.5). The fragmenter skips them
        // unconditionally, so without this pass they never reach
        // `dispatch_fragment` and ref-side renders blank for WPT
        // fixedpos-001/002/008.
        crate::pagination_layout::append_position_absolute_body_direct_fragments(
            &mut pagination_geometry,
            doc.deref_mut(),
            total_pages,
            resolved_content_width_px,
            resolved_content_height_px,
            Some(&running_store),
        );
        let expanded_total_pages =
            crate::pagination_layout::implied_page_count(&pagination_geometry).max(1);
        if expanded_total_pages > total_pages {
            crate::pagination_layout::append_position_fixed_fragments(
                &mut pagination_geometry,
                doc.deref_mut(),
                expanded_total_pages,
                resolved_content_width_px,
                resolved_content_height_px,
            );
        }

        // Build the AnchorMap for `target-*` cross-references only when
        // pass 2 will actually consume it (parsed GCPM context contains
        // `target-*` AND this is pass 1, i.e. `anchor_map.is_none()`).
        // On the fast path (no `target-*` anywhere) we skip the DOM walk
        // entirely to avoid the `element_text` cost on every id'd
        // subtree. On pass 2 we hand `render_v2` the caller-supplied
        // `anchor_map` directly — the local one would be redundant.
        //
        // The walk runs against `pagination_geometry` after `position:
        // fixed` / body-direct absolute fragments have been appended,
        // so anchor pages reflect the final paginated layout.
        // `walk_anchors` short-circuits on `MAX_DOM_DEPTH`.
        let needs_anchor_map_for_pass_two = anchor_map.is_none() && has_target_refs;
        let collected_anchor_map = if needs_anchor_map_for_pass_two {
            build_anchor_map(&doc, &pagination_geometry, &counter_snapshots_for_anchor)
        } else {
            AnchorMap::default()
        };

        // Per-page implicit `href` for `target-*(attr(href), ...)` in
        // `@page` margin boxes (fulgur-qgy7). Only pass 2 consults the
        // map — pass 1's resolver returns empty without an `anchor_map`
        // regardless of `implicit_href` — and the map is irrelevant
        // unless a margin-box rule actually references
        // `target-*(attr(href), ...)`. Gating on both keeps the
        // single-pass / non-margin-box-target fast path free of the DOM
        // walk.
        let needs_implicit_href_map = anchor_map.is_some()
            && gcpm.margin_boxes.iter().any(|r| {
                r.content
                    .iter()
                    .any(crate::gcpm::ContentItem::is_target_reference)
            });
        let implicit_href_map = if needs_implicit_href_map {
            build_implicit_href_map(&doc, &pagination_geometry)
        } else {
            BTreeMap::new()
        };

        // --- Convert DOM to Drawables and render ---
        // Build string-set lookup map
        let string_set_by_node: HashMap<usize, Vec<(String, String)>> = {
            let mut map: HashMap<usize, Vec<(String, String)>> = HashMap::new();
            for entry in string_set_store.entries() {
                map.entry(entry.node_id)
                    .or_default()
                    .push((entry.name.clone(), entry.value.clone()));
            }
            map
        };

        // Build counter_ops_by_node map
        let counter_ops_map: HashMap<usize, Vec<crate::gcpm::CounterOp>> = {
            let mut map = HashMap::new();
            for (node_id, ops) in counter_ops_by_node_vec {
                map.insert(node_id, ops);
            }
            map
        };

        // PR 8i: `convert::dom_to_drawables` no longer drains
        // `string_set_by_node` / `counter_ops_by_node`, but we keep the
        // pre-convert clones for the fragmenter-driven `collect_*_states`
        // calls in `render_v2` so those side-channel maps remain
        // explicitly readable after convert returns. Each clone is small
        // (one `Vec` per node that declares the property).
        let string_set_for_render = string_set_by_node.clone();
        let counter_ops_for_render: BTreeMap<usize, Vec<crate::gcpm::CounterOp>> = counter_ops_map
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();

        let mut convert_ctx = ConvertContext {
            running_store: &running_store,
            assets: self.assets.as_ref(),
            font_cache: HashMap::new(),
            string_set_by_node,
            counter_ops_by_node: counter_ops_map,
            bookmark_by_node,
            column_styles,
            multicol_geometry,
            pagination_geometry,
            link_cache: Default::default(),
            // Use the resolved `@page` content box so percentage-based
            // fixed/abs descendants size against the same viewport that
            // pagination geometry and `relayout_position_fixed` use.
            viewport_size_px: Some((resolved_content_width_px, resolved_content_height_px)),
        };

        let drawables = crate::convert::dom_to_drawables(&doc, &mut convert_ctx);
        let html_title = crate::blitz_adapter::extract_html_title(&doc);
        // Reclaim the post-convert geometry without partially moving
        // `convert_ctx`, then drop it so its `&running_store` borrow ends and
        // `running_store` can be moved into the artifacts.
        let pagination_geometry = std::mem::take(&mut convert_ctx.pagination_geometry);
        drop(convert_ctx);
        Ok(LayoutArtifacts {
            drawables,
            pagination_geometry,
            gcpm,
            running_store,
            string_set_for_render,
            counter_ops_for_render,
            html_title,
            implicit_href_map,
            collected_anchor_map,
            needs_pass_two: needs_anchor_map_for_pass_two,
        })
    }

    /// Serialize a fully-laid-out [`LayoutArtifacts`] into a PDF via
    /// [`render::render_v2`]. On pass 2 of a 2-pass render, callers pass
    /// `anchor_map = Some(&pass1_map)` so `target-*` resolvers substitute
    /// real values; on the 1-pass path (no `target-*`) they pass `None`.
    fn render_artifacts(
        &self,
        artifacts: LayoutArtifacts,
        anchor_map: Option<&AnchorMap>,
    ) -> Result<Vec<u8>> {
        let LayoutArtifacts {
            drawables,
            pagination_geometry,
            gcpm,
            running_store,
            string_set_for_render,
            counter_ops_for_render,
            html_title,
            implicit_href_map,
            ..
        } = artifacts;

        // Re-derive fonts / system_fonts from `&self` — byte-identical to the
        // value `layout_to_drawables` used for parsing.
        let fonts = self.fonts();

        crate::render::render_v2(
            &self.config,
            &pagination_geometry,
            &drawables,
            &gcpm,
            &running_store,
            fonts,
            self.system_fonts,
            &string_set_for_render,
            &counter_ops_for_render,
            html_title,
            self.serialize_settings.clone(),
            anchor_map,
            &implicit_href_map,
        )
    }

    /// Render an HTML string to a PDF file.
    pub fn render_file(&self, html: &str, path: impl AsRef<Path>) -> Result<()> {
        let pdf = self.render(html)?;
        std::fs::write(path, pdf)?;
        Ok(())
    }

    /// Lay out `html` and return the renderer-agnostic per-node draw payloads
    /// (`drawables`) plus the pagination geometry, without serializing a PDF.
    ///
    /// Page shape (canvas size, margins, orientation) starts from the builder
    /// configuration and is then resolved against author CSS exactly as
    /// [`render`](Engine::render) resolves it — so `@page { size / margin }`
    /// overrides (including `:left` / `:right` / `:first`) drive pagination
    /// here too. The resolved page box itself is not surfaced on
    /// [`LayoutOutput`] yet; see its `# Limitations`. Documents with
    /// `target-counter()` / `target-counters()` / `target-text()` run the same
    /// internal 2-pass resolution as `render`, so the returned `drawables`
    /// carry resolved cross-reference values, not fixed-width placeholders.
    ///
    /// This is the shared layout path behind `render`; a downstream image
    /// rasterizer or OCR-label generator can consume [`LayoutOutput`] without
    /// pulling PDF serialization into core.
    pub fn layout(&self, html: &str) -> Result<LayoutOutput> {
        // Mirror `render`'s 2-pass loop: pass 1 always runs; when it reports
        // `target-*` cross-references, pass 2 re-lays-out with the pass-1
        // AnchorMap so those resolve. Project the final artifacts once.
        let pass1 = self.layout_to_drawables(html, None)?;
        let artifacts = if pass1.needs_pass_two {
            // Retain only the AnchorMap across passes (as `render`'s loop
            // does — its pass-1 drawables/geometry are dropped when only
            // `collected_anchor_map` is destructured out). Destructure
            // `pass1` so its drawables, geometry, and GCPM stores are freed
            // before pass 2 allocates, keeping peak memory single-pass-sized
            // for large raster/OCR inputs.
            let LayoutArtifacts {
                collected_anchor_map,
                ..
            } = pass1;
            self.layout_to_drawables(html, Some(&collected_anchor_map))?
        } else {
            pass1
        };
        Ok(LayoutOutput {
            drawables: artifacts.drawables,
            geometry: artifacts.pagination_geometry,
        })
    }

    /// Render a template with data to PDF bytes.
    ///
    /// The template is expanded via MiniJinja, then passed to [`render`](Engine::render).
    /// Returns an error if no template was set via the builder.
    ///
    /// **Migration note:** This method was previously named `render()` (no arguments).
    /// Because the new [`render`](Engine::render) method occupies that name with a
    /// different signature, no `#[deprecated]` alias can be provided — existing calls
    /// to `engine.render()` with no arguments will produce a compile error and must be
    /// updated to `engine.render_template()`.
    pub fn render_template(&self) -> Result<Vec<u8>> {
        let (name, content) = self
            .template
            .as_ref()
            .ok_or_else(|| crate::error::Error::Template("no template set".into()))?;
        let data = self
            .data
            .as_ref()
            .map_or_else(|| serde_json::json!({}), Clone::clone);
        let html = crate::template::render_template(name, content, &data)?;
        self.render(&html)
    }

    /// Render multiple HTML strings to PDFs, sharing this engine's parsed fonts.
    ///
    /// Returns one `Result<Vec<u8>>` per input in the same order as the input
    /// slice. A failure in one item does not abort the rest.
    ///
    /// With the `parallel` feature enabled the items are processed concurrently
    /// via rayon's global thread pool. Callers managing many concurrent batches
    /// should consider [`rayon::ThreadPoolBuilder`] to bound concurrency.
    /// Without the feature items run sequentially.
    pub fn render_batch<S: AsRef<str> + Sync>(&self, htmls: &[S]) -> Vec<Result<Vec<u8>>> {
        #[cfg(feature = "parallel")]
        {
            use rayon::prelude::*;
            htmls.par_iter().map(|h| self.render(h.as_ref())).collect()
        }
        #[cfg(not(feature = "parallel"))]
        {
            htmls.iter().map(|h| self.render(h.as_ref())).collect()
        }
    }

    /// Renamed to [`render`](Engine::render).
    #[deprecated(since = "0.19.0", note = "renamed to `render`")]
    pub fn render_html(&self, html: &str) -> Result<Vec<u8>> {
        self.render(html)
    }

    /// Renamed to [`render_file`](Engine::render_file).
    #[deprecated(since = "0.19.0", note = "renamed to `render_file`")]
    pub fn render_html_to_file(&self, html: &str, path: impl AsRef<Path>) -> Result<()> {
        self.render_file(html, path)
    }

    /// Build a `Drawables` map from HTML for integration tests.
    ///
    /// This helper **skips** all GCPM passes (CSS Generated Content for
    /// Paged Media — running elements, counters, string-set, `content:`
    /// resolution). It is only appropriate for tests that do not depend on
    /// GCPM-rendered content. The resulting drawables can therefore
    /// **diverge from the production output** whenever the HTML uses
    /// counters, running elements, or `content:` in a `<style>` block.
    /// Use this helper only for geometric / structural assertions on
    /// constructs that do not touch GCPM.
    #[doc(hidden)]
    pub fn build_drawables_for_testing_no_gcpm(&self, html: &str) -> crate::drawables::Drawables {
        let fonts = self.fonts();

        let (mut doc, _link_gcpm) = crate::blitz_adapter::parse_html_with_local_resources(
            html,
            self.config.content_width().as_pt().in_px().to_f32(),
            self.config.page_height().as_pt().in_px().to_f32() as u32,
            fonts,
            self.system_fonts,
            self.base_path.as_deref(),
        );

        let ctx = crate::blitz_adapter::PassContext { font_data: fonts };
        let passes: Vec<Box<dyn crate::blitz_adapter::DomPass>> = Vec::new();
        crate::blitz_adapter::apply_passes(&mut doc, &passes, &ctx);

        crate::blitz_adapter::resolve(&mut doc);
        crate::blitz_adapter::relayout_position_fixed(
            &mut doc,
            self.config.content_width().as_pt().in_px().to_f32(),
            self.config.content_height().as_pt().in_px().to_f32(),
        );
        let column_styles = crate::blitz_adapter::extract_column_style_table(&doc);
        let multicol_geometry = crate::multicol_layout::run_pass(doc.deref_mut(), &column_styles);
        let pagination_geometry = crate::pagination_layout::run_pass_with_break_styles(
            doc.deref_mut(),
            self.config.content_height().as_pt().in_px(),
            &column_styles,
        );

        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut convert_ctx = ConvertContext {
            running_store: &running_store,
            assets: self.assets.as_ref(),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles,
            multicol_geometry,
            pagination_geometry,
            link_cache: Default::default(),
            viewport_size_px: Some((
                self.config.content_width().as_pt().in_px().to_f32(),
                self.config.content_height().as_pt().in_px().to_f32(),
            )),
        };
        crate::convert::dom_to_drawables(&doc, &mut convert_ctx)
    }

    /// Build a `Drawables` map together with the per-NodeId
    /// `PaginationGeometryTable` for integration tests that need to
    /// reason about both the per-node draw payload and its absolute
    /// page-relative placement.
    ///
    /// Same GCPM caveat as `build_drawables_for_testing_no_gcpm` —
    /// margin boxes / running elements / counters / `content:`
    /// resolution are skipped.
    #[doc(hidden)]
    pub fn build_drawables_and_geometry_for_testing_no_gcpm(
        &self,
        html: &str,
    ) -> (
        crate::drawables::Drawables,
        crate::pagination_layout::PaginationGeometryTable,
    ) {
        let fonts = self.fonts();

        let (mut doc, _link_gcpm) = crate::blitz_adapter::parse_html_with_local_resources(
            html,
            self.config.content_width().as_pt().in_px().to_f32(),
            self.config.page_height().as_pt().in_px().to_f32() as u32,
            fonts,
            self.system_fonts,
            self.base_path.as_deref(),
        );

        let ctx = crate::blitz_adapter::PassContext { font_data: fonts };
        let passes: Vec<Box<dyn crate::blitz_adapter::DomPass>> = Vec::new();
        crate::blitz_adapter::apply_passes(&mut doc, &passes, &ctx);

        crate::blitz_adapter::resolve(&mut doc);
        crate::blitz_adapter::relayout_position_fixed(
            &mut doc,
            self.config.content_width().as_pt().in_px().to_f32(),
            self.config.content_height().as_pt().in_px().to_f32(),
        );
        let column_styles = crate::blitz_adapter::extract_column_style_table(&doc);
        let multicol_geometry = crate::multicol_layout::run_pass(doc.deref_mut(), &column_styles);
        let mut pagination_geometry = crate::pagination_layout::run_pass_with_break_styles(
            doc.deref_mut(),
            self.config.content_height().as_pt().in_px(),
            &column_styles,
        );

        // Mirror the production `render` path so test callers that
        // consult the returned geometry as a placement oracle see the
        // same `position: fixed` per-page repetition that the real
        // render emits (see the `append_position_fixed_fragments` block
        // in `render`). Without this, the helper would diverge
        // from `render` for documents with `position: fixed`.
        let content_w_px = self.config.content_width().as_pt().in_px().to_f32();
        let content_h_px = self.config.content_height().as_pt().in_px().to_f32();
        let total_pages = crate::pagination_layout::implied_page_count(&pagination_geometry).max(1);
        crate::pagination_layout::append_position_fixed_fragments(
            &mut pagination_geometry,
            doc.deref_mut(),
            total_pages,
            content_w_px,
            content_h_px,
        );
        crate::pagination_layout::append_position_absolute_body_direct_fragments(
            &mut pagination_geometry,
            doc.deref_mut(),
            total_pages,
            content_w_px,
            content_h_px,
            None,
        );
        let expanded_total_pages =
            crate::pagination_layout::implied_page_count(&pagination_geometry).max(1);
        if expanded_total_pages > total_pages {
            crate::pagination_layout::append_position_fixed_fragments(
                &mut pagination_geometry,
                doc.deref_mut(),
                expanded_total_pages,
                content_w_px,
                content_h_px,
            );
        }

        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut convert_ctx = ConvertContext {
            running_store: &running_store,
            assets: self.assets.as_ref(),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles,
            multicol_geometry,
            pagination_geometry,
            link_cache: Default::default(),
            viewport_size_px: Some((content_w_px, content_h_px)),
        };
        let drawables = crate::convert::dom_to_drawables(&doc, &mut convert_ctx);
        // PR 8i regression fix: read geometry AFTER convert. Convert
        // can write override fragments into `pagination_geometry`
        // (textless `content: url(...)` abs pseudos with `right` /
        // `bottom` insets), so cloning before convert would hide
        // those corrections from tests that drive
        // `pseudo_absolute_content_url::
        // absolute_pseudo_with_right_bottom_offsets_by_image_size`.
        // The production path already reads `convert_ctx.pagination_geometry`
        // after convert — `layout_to_drawables` `mem::take`s it into a
        // `pagination_geometry` local (the same value) that `render_artifacts`
        // then hands to `render_v2` — so this matches the production read order.
        (drawables, convert_ctx.pagination_geometry)
    }
}

use crate::blitz_adapter::get_attr;
use crate::gcpm::target_ref::{AnchorEntry, AnchorMap, fragment_id_from_href, page_for_node};
use crate::pagination_layout::PaginationGeometryTable;
use blitz_dom::BaseDocument;

fn build_anchor_map(
    doc: &BaseDocument,
    pagination_geometry: &PaginationGeometryTable,
    counter_snapshots: &BTreeMap<usize, BTreeMap<String, Vec<i32>>>,
) -> AnchorMap {
    let mut map = AnchorMap::new();
    walk_anchors(
        doc,
        doc.root_element().id,
        0,
        pagination_geometry,
        counter_snapshots,
        &mut map,
    );
    map
}

fn walk_anchors(
    doc: &BaseDocument,
    node_id: usize,
    depth: usize,
    geometry: &PaginationGeometryTable,
    snapshots: &BTreeMap<usize, BTreeMap<String, Vec<i32>>>,
    out: &mut AnchorMap,
) {
    if depth >= crate::MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    // Skip nodes that the fragmenter never assigned a page to (out-of-flow
    // or non-laid-out subtrees). Registering an entry with `page_num = 0`
    // would surface as a literal "0" through `target-counter(..., page)`,
    // which is wrong — better to drop the anchor entirely so callers see
    // an empty resolution and can fall back to plain text.
    if let Some(elem) = node.element_data()
        && let Some(frag) = get_attr(elem, "id")
        && let Some(page_num) = page_for_node(geometry, node_id)
    {
        let counters = snapshots.get(&node_id).cloned().unwrap_or_default();
        let text = collect_text_content(doc, node_id);
        let before_text = collect_pseudo_text(doc, node.before, elem);
        let after_text = collect_pseudo_text(doc, node.after, elem);
        out.insert(
            frag.to_string(),
            AnchorEntry {
                page_num,
                counters,
                text,
                before_text,
                after_text,
            },
        );
    }
    let children: Vec<usize> = node.children.clone();
    for c in children {
        walk_anchors(doc, c, depth + 1, geometry, snapshots, out);
    }
}

/// Capture the resolved text of a `::before` / `::after` pseudo node for
/// `target-text(url, before|after)`.
///
/// Read-only post-cascade read via `primary_styles().get_counters().content`
/// (cf. `blitz_adapter::extract_content_image_url`). Runs at `walk_anchors`
/// time — after CounterPass + InjectCssPass + pagination — so the cascade
/// carries both fulgur-left-to-Blitz `attr()`/string content and
/// CounterPass's injected resolved-counter overlay (a `String` item). One
/// path covers cascade-only and counter-tracked pseudo content; no CSS is
/// injected and Blitz rendering is never overridden.
///
/// Stylo 0.8 may keep `attr()` deferred as `ContentItem::Attr` rather
/// than substituting it at computed time; both the resolved-`String` and
/// the deferred-`Attr` shapes are handled (the latter resolved against
/// `parent_elem`, falling back to `a.fallback`). Counter / Counters /
/// Image / quote items are skipped — text items only, then internal
/// whitespace runs are collapsed while leading/trailing whitespace is
/// preserved (CSS string literals such as `content: attr(x) ": "` keep
/// their separator space; HTML whitespace collapsing does not apply to
/// generated content). A `counter()` pseudo that GCPM did not
/// counter-track has no injected `String` overlay and therefore captures
/// as empty here.
fn collect_pseudo_text(
    doc: &BaseDocument,
    pseudo_id: Option<usize>,
    parent_elem: &blitz_dom::node::ElementData,
) -> String {
    use style::values::generics::counters::{Content, ContentItem as Sci};
    let Some(pid) = pseudo_id else {
        return String::new();
    };
    let Some(node) = doc.get_node(pid) else {
        return String::new();
    };
    let Some(styles) = node.primary_styles() else {
        return String::new();
    };
    let content = &styles.get_counters().content;
    let Content::Items(item_data) = content else {
        return String::new();
    };
    let mut out = String::new();
    // Only the "main" items (before `alt_start`); content after is
    // alt-text in CSS Level 3 Content.
    for item in &item_data.items[..item_data.alt_start] {
        match item {
            Sci::String(s) => out.push_str(s.as_ref()),
            Sci::Attr(a) => {
                let name = a.attribute.as_ref();
                match get_attr(parent_elem, name) {
                    Some(v) => out.push_str(v),
                    None => out.push_str(a.fallback.as_ref()),
                }
            }
            _ => {}
        }
    }
    collapse_ws_keep_edges(&out)
}

/// Collapse internal whitespace runs to a single space while preserving a
/// single leading/trailing space if the original had any. Unlike
/// `split_whitespace().join(" ")` (used by `collect_text_content` for HTML
/// text, where boundary whitespace is collapsed away by HTML rules), CSS
/// generated content keeps author-written separator spaces — e.g.
/// `content: attr(data-tag) ": "` must yield `APP: ` so a referencing
/// `target-text(..., before)` does not jam against following text.
fn collapse_ws_keep_edges(s: &str) -> String {
    let leading = s.chars().next().is_some_and(char::is_whitespace);
    let trailing = s.chars().next_back().is_some_and(char::is_whitespace);
    let core = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if core.is_empty() {
        // All-whitespace (or empty) input: a lone space if any ws was present.
        return if leading || trailing {
            " ".to_string()
        } else {
            String::new()
        };
    }
    let mut r = String::with_capacity(core.len() + 2);
    if leading {
        r.push(' ');
    }
    r.push_str(&core);
    if trailing {
        r.push(' ');
    }
    r
}

fn collect_text_content(doc: &BaseDocument, node_id: usize) -> String {
    let raw = crate::blitz_adapter::element_text(doc, node_id);
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Per-page implicit `href` for `target-*(attr(href), ...)` evaluated
/// inside `@page` margin boxes. CSS GCPM does not define `attr(href)`
/// for margin-box content (no link element exists in that context), so
/// we adopt the de-facto rule used by other paged-media engines: the
/// implicit reference for page *N* is the **first `<a href="#...">`
/// element whose enclosing block lands on page N**. Pages with no such
/// anchor have no entry in the returned map; callers fall back to an
/// empty string.
///
/// Inline anchors carry no geometry of their own — the block-only
/// fragmenter only records block-level nodes — so we attribute each
/// `<a>` to the first page of its **nearest paginated ancestor**. An
/// `<a>` inline in a `<p>` that spans pages 1→2 is therefore attributed
/// to page 1 only; line-level placement is not available without
/// re-driving inline layout, and keeping the rule cheap matches the
/// "spec coverage at low marginal cost" framing of fulgur-qgy7.
fn build_implicit_href_map(
    doc: &BaseDocument,
    pagination_geometry: &PaginationGeometryTable,
) -> BTreeMap<usize, String> {
    let mut map = BTreeMap::new();
    walk_implicit_href(
        doc,
        doc.root_element().id,
        0,
        None,
        pagination_geometry,
        &mut map,
    );
    map
}

fn walk_implicit_href(
    doc: &BaseDocument,
    node_id: usize,
    depth: usize,
    inherited_page: Option<u32>,
    geometry: &PaginationGeometryTable,
    out: &mut BTreeMap<usize, String>,
) {
    if depth >= crate::MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    // Block-level ancestors get their own fragments; inline descendants
    // (notably `<a>`) do not. Inherit the deepest seen block page so
    // inline children resolve against the same page the fragmenter
    // assigned to their nearest paginated ancestor.
    let resolved_page = page_for_node(geometry, node_id).or(inherited_page);
    if let Some(elem) = node.element_data()
        && elem.name.local.as_ref() == "a"
        && let Some(href) = get_attr(elem, "href")
        && fragment_id_from_href(href).is_some()
        && let Some(page_num) = resolved_page
    {
        let page_idx = page_num.saturating_sub(1) as usize;
        out.entry(page_idx).or_insert_with(|| href.to_string());
    }
    let children: Vec<usize> = node.children.clone();
    for c in children {
        walk_implicit_href(doc, c, depth + 1, resolved_page, geometry, out);
    }
}

pub struct EngineBuilder {
    config_builder: ConfigBuilder,
    assets: Option<AssetBundle>,
    base_path: Option<PathBuf>,
    template: Option<(String, String)>,
    data: Option<serde_json::Value>,
    serialize_settings: SerializeSettings,
    system_fonts: bool,
}

impl EngineBuilder {
    pub fn page_size(mut self, size: PageSize) -> Self {
        self.config_builder = self.config_builder.page_size(size);
        self
    }

    pub fn margin(mut self, margin: Margin) -> Self {
        self.config_builder = self.config_builder.margin(margin);
        self
    }

    pub fn landscape(mut self, landscape: bool) -> Self {
        self.config_builder = self.config_builder.landscape(landscape);
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.title(title);
        self
    }

    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.author(author);
        self
    }

    pub fn lang(mut self, lang: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.lang(lang);
        self
    }

    pub fn bookmarks(mut self, enabled: bool) -> Self {
        self.config_builder = self.config_builder.bookmarks(enabled);
        self
    }

    pub fn tagged(mut self, enabled: bool) -> Self {
        self.config_builder = self.config_builder.tagged(enabled);
        self
    }

    pub fn pdf_ua(mut self, enabled: bool) -> Self {
        self.config_builder = self.config_builder.pdf_ua(enabled);
        self
    }

    pub fn authors(mut self, authors: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.config_builder = self.config_builder.authors(authors);
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.description(description);
        self
    }

    pub fn keywords(mut self, keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.config_builder = self.config_builder.keywords(keywords);
        self
    }

    pub fn creator(mut self, creator: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.creator(creator);
        self
    }

    pub fn producer(mut self, producer: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.producer(producer);
        self
    }

    pub fn creation_date(mut self, date: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.creation_date(date);
        self
    }

    pub fn assets(mut self, assets: AssetBundle) -> Self {
        self.assets = Some(assets);
        self
    }

    pub fn system_fonts(mut self, enabled: bool) -> Self {
        self.system_fonts = enabled;
        self
    }

    pub fn base_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.base_path = Some(path.into());
        self
    }

    pub fn template(mut self, name: impl Into<String>, template: impl Into<String>) -> Self {
        self.template = Some((name.into(), template.into()));
        self
    }

    pub fn data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn serialize_settings(mut self, settings: SerializeSettings) -> Self {
        self.serialize_settings = settings;
        self
    }

    pub fn build(mut self) -> Engine {
        // When both base_path and assets are set, propagate the canonical
        // file:// base URL to the bundle so get_image can normalize
        // Stylo-resolved absolute file paths back to relative asset names.
        if let (Some(bundle), Some(path)) = (&mut self.assets, &self.base_path) {
            if let Some(url_str) = crate::blitz_adapter::canonical_directory_url(path) {
                bundle.set_base_url(&url_str);
            }
        }

        Engine {
            config: self.config_builder.build(),
            assets: self.assets,
            base_path: self.base_path,
            template: self.template,
            data: self.data,
            serialize_settings: self.serialize_settings,
            system_fonts: self.system_fonts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::F32Units;

    #[test]
    fn collect_pseudo_text_resolves_string_and_attr() {
        // `walk_anchors` (and therefore `collect_pseudo_text`) only runs
        // when GCPM has an active `target-*` reference, so the CSS must
        // include a real `target-text(...)` or this exercises nothing.
        let html = r##"<!doctype html><html><head><style>
          #t::before { content: attr(data-x) " "; }
          #t::after  { content: "AFT"; }
          .ref::after { content: target-text(attr(href), before); }
        </style></head><body>
          <p><a class="ref" href="#t"></a></p>
          <h2 id="t" data-x="DX">Title</h2>
        </body></html>"##;
        let pdf = Engine::builder().build().render(html).unwrap();
        assert!(!pdf.is_empty());
    }

    #[test]
    fn builder_bookmarks_defaults_to_false() {
        let engine = Engine::builder().build();
        assert!(!engine.config().bookmarks);
    }

    #[test]
    fn builder_bookmarks_opt_in() {
        let engine = Engine::builder().bookmarks(true).build();
        assert!(engine.config().bookmarks);
    }

    #[test]
    fn test_engine_builder_base_path() {
        let engine = Engine::builder().base_path("/tmp/test").build();
        assert_eq!(engine.base_path(), Some(std::path::Path::new("/tmp/test")));
    }

    #[test]
    fn test_engine_builder_no_base_path() {
        let engine = Engine::builder().build();
        assert_eq!(engine.base_path(), None);
    }

    #[test]
    fn test_engine_render_template() {
        let engine = Engine::builder()
            .template("test.html", "<h1>{{ title }}</h1>")
            .data(serde_json::json!({"title": "Hello"}))
            .build();
        let result = engine.render_template();
        assert!(result.is_ok());
    }

    #[test]
    fn test_engine_render_without_template_errors() {
        let engine = Engine::builder().build();
        let result = engine.render_template();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Template"));
    }

    #[test]
    fn test_engine_render_without_data_uses_empty_object() {
        let engine = Engine::builder()
            .template("test.html", "<p>static</p>")
            .build();
        let result = engine.render_template();
        assert!(result.is_ok());
    }

    // ── Builder: config fields and override flags ──────────────────────────

    #[test]
    fn builder_page_size_stores_size_and_sets_override() {
        let engine = Engine::builder().page_size(PageSize::LETTER).build();
        let config = engine.config();
        assert!((config.page_size.width - 612.0).abs() < 0.01);
        assert!((config.page_size.height - 792.0).abs() < 0.01);
        assert!(config.overrides.page_size);
    }

    #[test]
    fn builder_margin_stores_margin_and_sets_override() {
        let engine = Engine::builder().margin(Margin::uniform(36.0)).build();
        let config = engine.config();
        assert_eq!(config.margin, Margin::uniform(36.0));
        assert!(config.overrides.margin);
    }

    #[test]
    fn builder_landscape_stores_flag_and_sets_override() {
        let engine = Engine::builder().landscape(true).build();
        let config = engine.config();
        assert!(config.landscape);
        assert!(config.overrides.landscape);
    }

    // ── Builder: metadata fields ───────────────────────────────────────────

    #[test]
    fn builder_author_appends_each_call() {
        // author() pushes rather than overwrites — verify both entries land.
        let engine = Engine::builder().author("Alice").author("Bob").build();
        let authors = &engine.config().authors;
        assert_eq!(authors, &["Alice", "Bob"]);
    }

    #[test]
    fn builder_authors_extends_from_iterator() {
        let engine = Engine::builder().authors(["Alice", "Bob", "Carol"]).build();
        assert_eq!(
            engine.config().authors,
            vec!["Alice".to_string(), "Bob".to_string(), "Carol".to_string()]
        );
    }

    #[test]
    fn builder_keywords_extends_from_iterator() {
        let engine = Engine::builder().keywords(["pdf", "html", "css"]).build();
        assert_eq!(
            engine.config().keywords,
            vec!["pdf".to_string(), "html".to_string(), "css".to_string()]
        );
    }

    #[test]
    fn builder_metadata_fields_round_trip() {
        let engine = Engine::builder()
            .title("My Report")
            .lang("en-US")
            .description("A test document")
            .creator("Test Suite")
            .producer("fulgur-test")
            .creation_date("2026-05-01")
            .build();
        let cfg = engine.config();
        assert_eq!(cfg.title.as_deref(), Some("My Report"));
        assert_eq!(cfg.lang.as_deref(), Some("en-US"));
        assert_eq!(cfg.description.as_deref(), Some("A test document"));
        assert_eq!(cfg.creator.as_deref(), Some("Test Suite"));
        assert_eq!(cfg.producer.as_deref(), Some("fulgur-test"));
        assert_eq!(cfg.creation_date.as_deref(), Some("2026-05-01"));
    }

    // ── Builder: assets getter ─────────────────────────────────────────────

    #[test]
    fn engine_assets_is_none_without_bundle() {
        let engine = Engine::builder().build();
        assert!(engine.assets().is_none());
    }

    #[test]
    fn engine_assets_is_some_after_bundle_set() {
        let mut bundle = AssetBundle::default();
        bundle.add_css("body { color: red; }");
        let engine = Engine::builder().assets(bundle).build();
        assert!(engine.assets().is_some());
    }

    // ── Render methods ─────────────────────────────────────────────────────

    #[test]
    fn render_html_to_file_writes_valid_pdf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.pdf");
        Engine::builder()
            .build()
            .render_file("<html><body><p>test</p></body></html>", &path)
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
    }

    #[test]
    fn builder_tagged_defaults_to_false() {
        let engine = Engine::builder().build();
        assert!(!engine.config().enable_tagging);
    }

    #[test]
    fn builder_pdf_ua_defaults_to_false() {
        let engine = Engine::builder().build();
        assert!(!engine.config().pdf_ua);
    }

    #[test]
    fn builder_tagged_opt_in() {
        let engine = Engine::builder().tagged(true).build();
        assert!(engine.config().enable_tagging);
    }

    #[test]
    fn builder_pdf_ua_opt_in() {
        let engine = Engine::builder().pdf_ua(true).build();
        assert!(engine.config().pdf_ua);
    }

    #[test]
    fn builder_pdf_ua_implies_effective_tagging() {
        let engine = Engine::builder().pdf_ua(true).build();
        assert!(engine.config().effective_tagging());
    }

    // ── fulgur-qgy7: implicit-href map for margin-box target-* ─────────────

    /// Locate every `<a>` element in document order. Used by the
    /// implicit-href tests to drive synthetic `PaginationGeometryTable`
    /// fixtures that don't run a real fragmenter.
    fn collect_anchor_node_ids(doc: &blitz_dom::BaseDocument) -> Vec<usize> {
        fn walk(doc: &blitz_dom::BaseDocument, node_id: usize, out: &mut Vec<usize>) {
            let Some(node) = doc.get_node(node_id) else {
                return;
            };
            if let Some(elem) = node.element_data()
                && elem.name.local.as_ref() == "a"
            {
                out.push(node_id);
            }
            let children: Vec<usize> = node.children.clone();
            for c in children {
                walk(doc, c, out);
            }
        }
        let mut out = Vec::new();
        walk(doc, doc.root_element().id, &mut out);
        out
    }

    fn frag_on_page(page_index: u32) -> crate::pagination_layout::Fragment {
        crate::pagination_layout::Fragment {
            page_index,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 0.0_f32.as_px(),
            height: 0.0_f32.as_px(),
        }
    }

    fn geometry_with_anchor_pages(
        anchor_ids: &[usize],
        page_indices: &[u32],
    ) -> PaginationGeometryTable {
        assert_eq!(anchor_ids.len(), page_indices.len());
        let mut table = PaginationGeometryTable::new();
        for (&node_id, &page_index) in anchor_ids.iter().zip(page_indices) {
            table.insert(
                node_id,
                crate::pagination_layout::PaginationGeometry {
                    fragments: vec![frag_on_page(page_index)],
                    is_repeat: false,
                    ..Default::default()
                },
            );
        }
        table
    }

    #[test]
    fn implicit_href_first_anchor_per_page_wins() {
        let html = r##"<html><body>
            <a href="#a">first</a>
            <a href="#b">second</a>
        </body></html>"##;
        let doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        let anchors = collect_anchor_node_ids(&doc);
        assert_eq!(anchors.len(), 2, "expected two <a> elements");
        // Both anchors land on page 0 — document order means `#a` wins.
        let geometry = geometry_with_anchor_pages(&anchors, &[0, 0]);
        let map = build_implicit_href_map(&doc, &geometry);
        assert_eq!(map.get(&0).map(String::as_str), Some("#a"));
        assert!(!map.contains_key(&1));
    }

    #[test]
    fn implicit_href_records_one_entry_per_page() {
        let html = r##"<html><body>
            <a href="#a">page-one</a>
            <a href="#b">page-two</a>
            <a href="#c">page-two-second</a>
        </body></html>"##;
        let doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        let anchors = collect_anchor_node_ids(&doc);
        assert_eq!(anchors.len(), 3);
        let geometry = geometry_with_anchor_pages(&anchors, &[0, 1, 1]);
        let map = build_implicit_href_map(&doc, &geometry);
        assert_eq!(map.get(&0).map(String::as_str), Some("#a"));
        // First-on-page wins; `#c` is dropped.
        assert_eq!(map.get(&1).map(String::as_str), Some("#b"));
    }

    #[test]
    fn implicit_href_skips_external_and_hashless_hrefs() {
        // `href="#"` is a no-op anchor: `fragment_id_from_href` strips
        // the leading `#` and returns `None` for the empty remainder,
        // so it must not poison the map and clobber the later
        // `<a href="#real">` via first-on-page-wins.
        let html = r##"<html><body>
            <a href="https://example.com/">external</a>
            <a href="page2.html">relative</a>
            <a>missing-href</a>
            <a href="#">empty-fragment</a>
            <a href="#real">fragment</a>
        </body></html>"##;
        let doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        let anchors = collect_anchor_node_ids(&doc);
        assert_eq!(anchors.len(), 5);
        let geometry = geometry_with_anchor_pages(&anchors, &[0, 0, 0, 0, 0]);
        let map = build_implicit_href_map(&doc, &geometry);
        // Only the real fragment `<a>` contributes; `#` is dropped.
        assert_eq!(map.get(&0).map(String::as_str), Some("#real"));
    }

    #[test]
    fn implicit_href_skips_anchors_without_geometry() {
        // An `<a>` whose subtree was never paginated (out-of-flow,
        // `display: none`, …) has no entry in the geometry table and
        // must not poison the implicit-href map for any page.
        let html = r##"<html><body><a href="#orphan">x</a></body></html>"##;
        let doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        let geometry = PaginationGeometryTable::new();
        let map = build_implicit_href_map(&doc, &geometry);
        assert!(map.is_empty());
    }

    #[test]
    fn implicit_href_empty_when_document_has_no_anchors() {
        let html = "<html><body><p>no anchors here</p></body></html>";
        let doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        let geometry = PaginationGeometryTable::new();
        let map = build_implicit_href_map(&doc, &geometry);
        assert!(map.is_empty());
    }

    #[test]
    fn implicit_href_inherits_block_ancestor_page() {
        // The block-only fragmenter records geometry for block-level
        // nodes only; inline `<a>` elements inherit their page from
        // the nearest paginated ancestor. Real renders trigger this
        // path because `<p><a href="#x">` has geometry on the `<p>`,
        // not on the `<a>` itself.
        let html = r##"<html><body><p><a href="#sec">link</a></p></body></html>"##;
        let doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        // Locate the `<p>` (block-level) by walking; the `<a>` inside
        // it is inline and intentionally has no geometry entry.
        fn find_first_tag(doc: &blitz_dom::BaseDocument, tag: &str) -> Option<usize> {
            fn walk(doc: &blitz_dom::BaseDocument, node_id: usize, tag: &str) -> Option<usize> {
                let n = doc.get_node(node_id)?;
                if let Some(elem) = n.element_data()
                    && elem.name.local.as_ref() == tag
                {
                    return Some(node_id);
                }
                for c in n.children.iter().copied() {
                    if let Some(found) = walk(doc, c, tag) {
                        return Some(found);
                    }
                }
                None
            }
            walk(doc, doc.root_element().id, tag)
        }
        let p_id = find_first_tag(&doc, "p").expect("find <p>");
        let mut geometry = PaginationGeometryTable::new();
        geometry.insert(
            p_id,
            crate::pagination_layout::PaginationGeometry {
                fragments: vec![frag_on_page(2)],
                is_repeat: false,
                ..Default::default()
            },
        );
        let map = build_implicit_href_map(&doc, &geometry);
        // `<a>` inherits page 2 (0-based index) from its `<p>` parent.
        assert_eq!(map.get(&2).map(String::as_str), Some("#sec"));
    }

    // ── collapse_ws_keep_edges: pure-function unit tests ──────────────────

    #[test]
    fn collapse_ws_empty_string_returns_empty() {
        assert_eq!(collapse_ws_keep_edges(""), "");
    }

    #[test]
    fn collapse_ws_all_whitespace_returns_single_space() {
        // All-whitespace input: leading is true, core is empty → single space.
        assert_eq!(collapse_ws_keep_edges("   "), " ");
    }

    #[test]
    fn collapse_ws_only_newlines_returns_single_space() {
        assert_eq!(collapse_ws_keep_edges("\n\t "), " ");
    }

    #[test]
    fn collapse_ws_leading_space_preserved() {
        // Leading whitespace is present and non-empty core → prefix space.
        assert_eq!(collapse_ws_keep_edges(" hello"), " hello");
    }

    #[test]
    fn collapse_ws_trailing_space_preserved() {
        assert_eq!(collapse_ws_keep_edges("hello "), "hello ");
    }

    #[test]
    fn collapse_ws_both_edges_and_internal_collapse() {
        // Leading, trailing and multiple internal spaces all handled.
        assert_eq!(collapse_ws_keep_edges(" hello   world "), " hello world ");
    }

    #[test]
    fn collapse_ws_internal_only_collapses_no_edge_space() {
        assert_eq!(collapse_ws_keep_edges("foo  bar"), "foo bar");
    }

    #[test]
    fn collapse_ws_separator_pattern_preserved() {
        // Typical CSS attr/string separator: `attr(tag) ": "`.
        // Leading and trailing spaces survive, internal is already single.
        assert_eq!(collapse_ws_keep_edges(" TAG: "), " TAG: ");
    }

    // ── render_batch: public API smoke test ───────────────────────────────

    #[test]
    fn render_batch_returns_one_result_per_input() {
        let engine = Engine::builder().build();
        let htmls: &[&str] = &[
            "<html><body><p>first</p></body></html>",
            "<html><body><p>second</p></body></html>",
        ];
        let results = engine.render_batch(htmls);
        assert_eq!(results.len(), 2);
        for r in results {
            let pdf = r.expect("render_batch item should succeed");
            assert!(pdf.starts_with(b"%PDF"), "each item should be a PDF");
        }
    }

    #[test]
    fn render_batch_empty_slice_returns_empty_vec() {
        let engine = Engine::builder().build();
        let htmls: &[&str] = &[];
        let results = engine.render_batch(htmls);
        assert!(results.is_empty());
    }

    // ── Builder: system_fonts and serialize_settings ──────────────────────

    #[test]
    fn system_fonts_false_still_produces_pdf() {
        // system_fonts(false) disables Blitz system-font loading; a plain
        // document with no explicit fonts should still render to a valid PDF.
        let pdf = Engine::builder()
            .system_fonts(false)
            .build()
            .render("<html><body><p>test</p></body></html>")
            .unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn serialize_settings_builder_produces_pdf() {
        let settings = SerializeSettings::default();
        let pdf = Engine::builder()
            .serialize_settings(settings)
            .build()
            .render("<html><body><p>test</p></body></html>")
            .unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── Builder: build() with assets + base_path triggers base_url setup ──

    #[test]
    fn build_with_assets_and_base_path_sets_base_url() {
        // When both assets and base_path are provided, build() calls
        // canonical_directory_url and propagates it to the bundle.
        let mut bundle = AssetBundle::default();
        bundle.add_css("p { color: red; }");
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::builder()
            .assets(bundle)
            .base_path(dir.path())
            .build();
        assert_eq!(engine.base_path(), Some(dir.path()));
        assert!(engine.assets().is_some());
    }

    // ── @page landscape via CSS (resolved_landscape branch) ───���───────────

    #[test]
    fn render_page_with_css_landscape_size_produces_pdf() {
        // `@page { size: A4 landscape; }` triggers the `resolved_landscape =
        // true` branch in `layout_to_drawables`.
        let html = "<!doctype html><html><head><style>\
            @page { size: A4 landscape; }\
            </style></head><body><p>landscape</p></body></html>";
        let pdf = Engine::builder().build().render(html).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── GCPM: running elements (lines 246-248) ────────────────────────────

    #[test]
    fn render_with_running_elements_css() {
        // `position: running(name)` triggers the RunningElementPass branch
        // (engine.rs:246-248).
        let mut assets = AssetBundle::new();
        assets.add_css(
            ".hdr { position: running(pageHdr); }\
             @page { @top-center { content: element(pageHdr); } }",
        );
        let html = "<body><div class=\"hdr\">Header</div><p>Content</p></body>";
        let pdf = Engine::builder()
            .assets(assets)
            .build()
            .render(html)
            .unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ���─ GCPM: string-set with snapshot recording (lines 272-279) ─────────

    #[test]
    fn render_with_string_set_css() {
        // `string-set: name content()` triggers the StringSetPass branch
        // (engine.rs:272-279).
        let mut assets = AssetBundle::new();
        assets.add_css(
            "h1 { string-set: chap content(text); }\
             @page { @top-center { content: string(chap); } }",
        );
        let html = "<body><h1>Chapter One</h1><p>Body text.</p></body>";
        let pdf = Engine::builder()
            .assets(assets)
            .build()
            .render(html)
            .unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── GCPM: target-counter triggers 2-pass render (lines 87, 93) ────────

    #[test]
    fn render_target_counter_in_margin_box_triggers_two_pass() {
        // `target-counter()` inside a @page margin box sets
        // `has_target_refs = true`, which causes `needs_pass_two = true` and
        // drives the render() method through its pass-2 branch (lines 87/93).
        let mut assets = AssetBundle::new();
        assets.add_css("@page { @bottom-center { content: target-counter(attr(href), page); } }");
        let html = "<body>\
            <p><a href=\"#sec\">jump to section</a></p>\
            <h2 id=\"sec\">Section</h2>\
            </body>";
        let pdf = Engine::builder()
            .assets(assets)
            .build()
            .render(html)
            .unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── GCPM: static-content mappings (lines 330-335) ────────────────────

    #[test]
    fn render_with_static_content_mapping_css() {
        // Multi-value plain-string content: list triggers static_content_mappings
        // (engine.rs:330-335). All items must be String(_) and len > 1; mixing in
        // counter() would classify it as dynamic and route to CounterPass instead.
        let mut assets = AssetBundle::new();
        assets.add_css("h1::before { content: \"Ch. \" \"1\"; }");
        let html = "<body><h1>Title</h1><p>Text</p></body>";
        let pdf = Engine::builder()
            .assets(assets)
            .build()
            .render(html)
            .unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── layout: PDF化なしの公開レイアウトAPI (engine.rs:757-781) ──────────────

    #[test]
    fn layout_returns_non_empty_output() {
        let output = Engine::builder()
            .build()
            .layout("<p>hello world</p>")
            .expect("layout should succeed");
        assert!(
            !output.drawables.is_empty() || !output.geometry.is_empty(),
            "layout should produce drawables or geometry for a non-empty document"
        );
    }

    #[test]
    fn layout_two_pass_with_target_text() {
        // target-text() triggers needs_pass_two, exercising the two-pass branch
        // inside layout() (engine.rs:762-773).
        let html = r##"<!doctype html><html><head><style>
          .ref::after { content: target-text(attr(href), before); }
        </style></head><body>
          <p><a class="ref" href="#sec"></a></p>
          <h2 id="sec">Section</h2>
        </body></html>"##;
        let output = Engine::builder()
            .build()
            .layout(html)
            .expect("two-pass layout should succeed");
        assert!(!output.geometry.is_empty(), "layout must produce geometry");
    }

    // ── render Errパス: 不正なページサイズ (engine.rs:133, 172) ──────────────

    #[test]
    fn render_returns_err_on_zero_page_width() {
        let result = Engine::builder()
            .page_size(PageSize {
                width: 0.0,
                height: 841.89,
            })
            .build()
            .render("<p>test</p>");
        assert!(result.is_err(), "zero-width page must produce an error");
    }

    #[test]
    fn render_returns_err_on_nan_page_height() {
        let result = Engine::builder()
            .page_size(PageSize {
                width: 595.28,
                height: f32::NAN,
            })
            .build()
            .render("<p>test</p>");
        assert!(result.is_err(), "NaN page height must produce an error");
    }

    // ── Builder: base_path + assetsでset_base_urlが呼ばれる (engine.rs:1361) ─

    #[test]
    fn builder_with_assets_and_base_path_renders_successfully() {
        let bundle = crate::AssetBundle::default();
        let pdf = Engine::builder()
            .assets(bundle)
            .base_path(std::env::temp_dir())
            .build()
            .render("<p>hello</p>")
            .expect("render with base_path + assets should succeed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── スナップショット記録: string-set + bookmarks (engine.rs:348-349) ──────

    #[test]
    fn render_with_string_set_and_bookmarks_enabled() {
        // bookmarks(true) + string-set CSS の組み合わせで record_string_snapshots=true
        // になり、StringSetPassがスナップショット記録付きで実行される (engine.rs:349)。
        let mut assets = AssetBundle::new();
        assets.add_css(
            "h1 { string-set: chap content(text); }\
             @page { @top-center { content: string(chap); } }",
        );
        let html = "<body><h1>Chapter One</h1><p>Body text.</p></body>";
        let pdf = Engine::builder()
            .assets(assets)
            .bookmarks(true)
            .build()
            .render(html)
            .expect("string-set + bookmarks render should succeed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── スナップショット記録: counter + bookmarks (engine.rs:376) ─────────────

    #[test]
    fn render_with_counter_css_and_bookmarks_enabled() {
        // bookmarks(true) で record_counter_snapshots=true になり、
        // CounterPassがスナップショット記録付きで実行される (engine.rs:376)。
        let mut assets = AssetBundle::new();
        assets.add_css(
            "body { counter-reset: section; }\
             h2::before { counter-increment: section; content: counter(section) \". \"; }",
        );
        let html = "<body><h2>Section A</h2><h2>Section B</h2></body>";
        let pdf = Engine::builder()
            .assets(assets)
            .bookmarks(true)
            .build()
            .render(html)
            .expect("counter + bookmarks render should succeed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── (true, true) matchケース: bookmarks AND target-refs同時有効 (engine.rs:427) ─

    #[test]
    fn render_with_bookmarks_and_target_refs_simultaneously() {
        // bookmarks=true かつ target-counter() で has_target_refs=true になり、
        // match (bookmark_active, target_refs_active) の (true, true) アームを通る
        // (engine.rs:427)。
        let mut assets = AssetBundle::new();
        assets.add_css("@page { @bottom-center { content: target-counter(attr(href), page); } }");
        let html = "<body>\
            <p><a href=\"#sec\">go to section</a></p>\
            <h2 id=\"sec\">Section</h2>\
            </body>";
        let pdf = Engine::builder()
            .assets(assets)
            .bookmarks(true)
            .build()
            .render(html)
            .expect("bookmarks + target-refs render should succeed");
        assert!(pdf.starts_with(b"%PDF"));
    }
}
