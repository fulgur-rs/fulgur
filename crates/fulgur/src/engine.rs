use crate::asset::AssetBundle;
use crate::config::{Config, ConfigBuilder, Margin, PageSize};
use crate::convert::ConvertContext;
use crate::error::Result;
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
    pub fn render_html(&self, html: &str) -> Result<Vec<u8>> {
        // Pass 1: render once. `render_pass` parses the full GCPM context
        // (AssetBundle, <link>-loaded stylesheets, inline <style> blocks)
        // and reports `needs_pass_two` based on that parsed view, so
        // `target-counter()` / `target-counters()` / `target-text()`
        // declared in any of those locations is detected reliably.
        let (pdf, anchor_map, needs_pass_two) = self.render_pass(html, None)?;
        if !needs_pass_two {
            return Ok(pdf);
        }
        // Pass 2: re-render with the AnchorMap so `target-*` resolvers
        // substitute resolved values instead of fixed-width placeholders.
        let (pdf2, _, _) = self.render_pass(html, Some(&anchor_map))?;
        Ok(pdf2)
    }

    /// Single render pass. When `anchor_map` is `Some`, the supplied map
    /// is wired into [`CounterPass`] so `target-counter()` /
    /// `target-counters()` / `target-text()` inside `::before` / `::after`
    /// resolve against pass-1 anchor data, and is passed through to
    /// `render::render_v2` so margin-box `target-*` resolvers can do the
    /// same. When `None`, those resolvers fall back to placeholders /
    /// empty strings.
    ///
    /// The returned tuple is `(pdf_bytes, collected_anchor_map,
    /// needs_pass_two)`:
    /// - `collected_anchor_map` is the table built from this pass's
    ///   pagination geometry + counter snapshots when the parsed GCPM
    ///   context contains `target-*` references AND `anchor_map` is
    ///   `None` (i.e. this is pass 1 of a 2-pass render). Otherwise it
    ///   is empty — we skip the DOM walk to avoid `element_text` cost
    ///   on every id'd subtree on the fast path.
    /// - `needs_pass_two` mirrors that gate: `true` only for pass 1 of
    ///   a 2-pass render so `render_html` can decide whether to call
    ///   `render_pass` again with the populated map.
    fn render_pass(
        &self,
        html: &str,
        anchor_map: Option<&AnchorMap>,
    ) -> Result<(Vec<u8>, AnchorMap, bool)> {
        let html = crate::blitz_adapter::rewrite_marker_content_url_in_html(html);

        let combined_css = self
            .assets
            .as_ref()
            .map(|a| a.combined_css())
            .unwrap_or_default();
        let combined_css = crate::blitz_adapter::rewrite_marker_content_url(&combined_css);

        let mut gcpm = crate::gcpm::parser::parse_gcpm(&combined_css);
        let css_to_inject = gcpm.cleaned_css.clone();

        let fonts = self
            .assets
            .as_ref()
            .map(|a| a.fonts.as_slice())
            .unwrap_or(&[]);

        // Parse the HTML and resolve every <link rel="stylesheet"> /
        // @import file inside `base_path` in one shot. The returned
        // `link_gcpm` carries the GCPM constructs extracted from those
        // stylesheets, which we fold into the AssetBundle-derived
        // context below.
        //
        // `cleaned_css` is folded too: it is consumed by `render.rs` as
        // the sole stylesheet for the margin-box mini-documents (see
        // `render_to_pdf_with_gcpm` and `strip_display_none`). Without
        // it, declarations like `.pageHeader { font-size: 8px; }`
        // defined in a `<link>`-loaded stylesheet would never reach
        // the margin-box renderer, so headers/footers would appear in
        // default browser styles even though their content resolved
        // correctly.
        let (mut doc, link_gcpm) = crate::blitz_adapter::parse_html_with_local_resources(
            &html,
            crate::convert::pt_to_px(self.config.content_width()),
            crate::convert::pt_to_px(self.config.page_height()) as u32,
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
        let inline_gcpm = crate::blitz_adapter::extract_gcpm_from_inline_styles(&doc);
        gcpm.extend_from(inline_gcpm);

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
        let resolved_content_width_px = crate::convert::pt_to_px(resolved_content_width_pt);
        let resolved_content_height_px = crate::convert::pt_to_px(resolved_content_height_pt);
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
        let record_node_snapshots = (self.config.effective_bookmarks()
            && !gcpm.bookmark_mappings.is_empty())
            || gcpm.has_target_references();

        // Extract string-set values via DomPass.
        // Also harvest per-node `name -> latest value` snapshots that the
        // later BookmarkPass uses to resolve `string(name)` inside
        // `bookmark-label` (fulgur-70c).
        let (string_set_store, string_snapshots) = if !gcpm.string_set_mappings.is_empty() {
            let mut pass =
                crate::blitz_adapter::StringSetPass::new(gcpm.string_set_mappings.clone());
            if record_node_snapshots {
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
                if record_node_snapshots {
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
        // `target-*` are active simultaneously.
        let bookmark_active =
            self.config.effective_bookmarks() && !gcpm.bookmark_mappings.is_empty();
        let target_refs_active = gcpm.has_target_references();
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
        // `ColumnGroup` layout for Task 4's `MulticolRulePageable`; we
        // thread it through `ConvertContext` so the convert pass can
        // wrap multicol containers with the rule spec + geometry they
        // need to render. See docs/plans/2026-04-20-css-multicol-design.md
        // and docs/plans/2026-04-21-fulgur-v7a-column-rule.md.
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
        // their height would over-count and diverge from Pageable.
        //
        // fulgur-s67g Phase 2.6 (`@page` size / margin resolution):
        // resolve the page-1 size + margin from `gcpm.page_settings`
        // before driving the fragmenter, so its strip height matches
        // `render_to_pdf_with_gcpm`'s `content_height` exactly. Both
        // sides use the page-1 result for *all* pages — Pageable
        // does the same in `render.rs:283-291` and does not re-resolve
        // per-page size for `:left` / `:right` / named selectors.
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
        let needs_anchor_map_for_pass_two = anchor_map.is_none() && gcpm.has_target_references();
        let collected_anchor_map = if needs_anchor_map_for_pass_two {
            build_anchor_map(&doc, &pagination_geometry, &counter_snapshots_for_anchor)
        } else {
            AnchorMap::default()
        };

        // --- Convert DOM to Pageable and render ---
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
        let pdf = crate::render::render_v2(
            &self.config,
            &convert_ctx.pagination_geometry,
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
        )?;
        Ok((pdf, collected_anchor_map, needs_anchor_map_for_pass_two))
    }

    /// Render HTML string to a PDF file.
    pub fn render_html_to_file(&self, html: &str, path: impl AsRef<Path>) -> Result<()> {
        let pdf = self.render_html(html)?;
        std::fs::write(path, pdf)?;
        Ok(())
    }

    /// Render a template with data to PDF bytes.
    /// The template is expanded via MiniJinja, then passed to render_html().
    /// Returns an error if no template was set via the builder.
    pub fn render(&self) -> Result<Vec<u8>> {
        let (name, content) = self
            .template
            .as_ref()
            .ok_or_else(|| crate::error::Error::Template("no template set".into()))?;
        let data = self
            .data
            .as_ref()
            .map_or_else(|| serde_json::json!({}), Clone::clone);
        let html = crate::template::render_template(name, content, &data)?;
        self.render_html(&html)
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
        let fonts = self
            .assets
            .as_ref()
            .map(|a| a.fonts.as_slice())
            .unwrap_or(&[]);

        let (mut doc, _link_gcpm) = crate::blitz_adapter::parse_html_with_local_resources(
            html,
            crate::convert::pt_to_px(self.config.content_width()),
            crate::convert::pt_to_px(self.config.page_height()) as u32,
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
            crate::convert::pt_to_px(self.config.content_width()),
            crate::convert::pt_to_px(self.config.content_height()),
        );
        let column_styles = crate::blitz_adapter::extract_column_style_table(&doc);
        let multicol_geometry = crate::multicol_layout::run_pass(doc.deref_mut(), &column_styles);
        let pagination_geometry = crate::pagination_layout::run_pass_with_break_styles(
            doc.deref_mut(),
            crate::convert::pt_to_px(self.config.content_height()),
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
                crate::convert::pt_to_px(self.config.content_width()),
                crate::convert::pt_to_px(self.config.content_height()),
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
        let fonts = self
            .assets
            .as_ref()
            .map(|a| a.fonts.as_slice())
            .unwrap_or(&[]);

        let (mut doc, _link_gcpm) = crate::blitz_adapter::parse_html_with_local_resources(
            html,
            crate::convert::pt_to_px(self.config.content_width()),
            crate::convert::pt_to_px(self.config.page_height()) as u32,
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
            crate::convert::pt_to_px(self.config.content_width()),
            crate::convert::pt_to_px(self.config.content_height()),
        );
        let column_styles = crate::blitz_adapter::extract_column_style_table(&doc);
        let multicol_geometry = crate::multicol_layout::run_pass(doc.deref_mut(), &column_styles);
        let mut pagination_geometry = crate::pagination_layout::run_pass_with_break_styles(
            doc.deref_mut(),
            crate::convert::pt_to_px(self.config.content_height()),
            &column_styles,
        );

        // Mirror the production `render_html` path so test callers that
        // consult the returned geometry as a placement oracle see the
        // same `position: fixed` per-page repetition that the real
        // render emits (see the `append_position_fixed_fragments` block
        // in `render_html`). Without this, the helper would diverge
        // from `render_html` for documents with `position: fixed`.
        let content_w_px = crate::convert::pt_to_px(self.config.content_width());
        let content_h_px = crate::convert::pt_to_px(self.config.content_height());
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
        // The production `render_html` path already passes
        // `&convert_ctx.pagination_geometry` to `render_v2` after
        // convert, so this matches the production read order.
        (drawables, convert_ctx.pagination_geometry)
    }
}

use crate::blitz_adapter::get_attr;
use crate::gcpm::target_ref::{AnchorEntry, AnchorMap, page_for_node};
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
    if let Some(elem) = node.element_data() {
        if let Some(frag) = get_attr(elem, "id") {
            let page_num = page_for_node(geometry, node_id).unwrap_or(0);
            let counters = snapshots.get(&node_id).cloned().unwrap_or_default();
            let text = collect_text_content(doc, node_id);
            out.insert(
                frag.to_string(),
                AnchorEntry {
                    page_num,
                    counters,
                    text,
                },
            );
        }
    }
    let children: Vec<usize> = node.children.clone();
    for c in children {
        walk_anchors(doc, c, depth + 1, geometry, snapshots, out);
    }
}

fn collect_text_content(doc: &BaseDocument, node_id: usize) -> String {
    let raw = crate::blitz_adapter::element_text(doc, node_id);
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
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
        let result = engine.render();
        assert!(result.is_ok());
    }

    #[test]
    fn test_engine_render_without_template_errors() {
        let engine = Engine::builder().build();
        let result = engine.render();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Template"));
    }

    #[test]
    fn test_engine_render_without_data_uses_empty_object() {
        let engine = Engine::builder()
            .template("test.html", "<p>static</p>")
            .build();
        let result = engine.render();
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
            .render_html_to_file("<html><body><p>test</p></body></html>", &path)
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
}
