use crate::asset::AssetBundle;
use crate::config::{Config, ConfigBuilder, Margin, PageSize};
use crate::convert::ConvertContext;
use crate::error::Result;
use crate::pageable::Pageable;
use crate::render::render_to_pdf;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::path::{Path, PathBuf};

/// Reusable PDF generation engine.
pub struct Engine {
    config: Config,
    assets: Option<AssetBundle>,
    base_path: Option<PathBuf>,
    template: Option<(String, String)>,
    data: Option<serde_json::Value>,
}

impl Engine {
    pub fn builder() -> EngineBuilder {
        EngineBuilder {
            config_builder: Config::builder(),
            assets: None,
            base_path: None,
            template: None,
            data: None,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn base_path(&self) -> Option<&Path> {
        self.base_path.as_deref()
    }

    /// Render a Pageable tree to PDF bytes.
    pub fn render_pageable(&self, root: Box<dyn Pageable>) -> Result<Vec<u8>> {
        render_to_pdf(root, &self.config)
    }

    pub fn assets(&self) -> Option<&AssetBundle> {
        self.assets.as_ref()
    }

    /// Render HTML string to PDF bytes.
    /// If an AssetBundle is set, its CSS will be injected as a <style> block.
    /// When GCPM constructs (margin boxes, running elements) are detected in the CSS,
    /// a 2-pass rendering pipeline is used: pass 1 paginates body content, pass 2
    /// renders each page with resolved margin boxes.
    pub fn render_html(&self, html: &str) -> Result<Vec<u8>> {
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

        // Prepend UA CSS bookmark mappings so author-CSS rules (appearing
        // later in `bookmark_mappings`) override them via last-match
        // cascade. Skipped when bookmarks are disabled to avoid unnecessary
        // CSS parsing and DOM traversal.
        if self.config.bookmarks {
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

        // Extract string-set values via DomPass
        let string_set_store = if !gcpm.string_set_mappings.is_empty() {
            let pass = crate::blitz_adapter::StringSetPass::new(gcpm.string_set_mappings.clone());
            crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
            pass.into_store()
        } else {
            crate::gcpm::string_set::StringSetStore::new()
        };

        let bookmark_by_node: HashMap<usize, crate::blitz_adapter::BookmarkInfo> =
            if self.config.bookmarks && !gcpm.bookmark_mappings.is_empty() {
                let pass = crate::blitz_adapter::BookmarkPass::new(gcpm.bookmark_mappings.clone());
                crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
                pass.into_results().into_iter().collect()
            } else {
                HashMap::new()
            };

        // Extract counter operations and resolve body content
        let (counter_ops_by_node_vec, counter_css) =
            if !gcpm.counter_mappings.is_empty() || !gcpm.content_counter_mappings.is_empty() {
                let pass = crate::blitz_adapter::CounterPass::new(
                    gcpm.counter_mappings.clone(),
                    gcpm.content_counter_mappings.clone(),
                );
                crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
                pass.into_parts()
            } else {
                (Vec::new(), String::new())
            };

        // Inject counter-resolved CSS for ::before/::after
        if !counter_css.is_empty() {
            let inject_pass = crate::blitz_adapter::InjectCssPass { css: counter_css };
            crate::blitz_adapter::apply_single_pass(&inject_pass, &mut doc, &ctx);
        }

        crate::blitz_adapter::resolve(&mut doc);

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

        let mut convert_ctx = ConvertContext {
            running_store: &running_store,
            assets: self.assets.as_ref(),
            font_cache: HashMap::new(),
            string_set_by_node,
            counter_ops_by_node: counter_ops_map,
            bookmark_by_node,
            column_styles,
            multicol_geometry,
            link_cache: Default::default(),
        };
        let root = crate::convert::dom_to_pageable(&doc, &mut convert_ctx);

        if gcpm.is_empty() {
            self.render_pageable(root)
        } else {
            crate::render::render_to_pdf_with_gcpm(root, &self.config, &gcpm, &running_store, fonts)
        }
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

    /// Build a Pageable tree from HTML for integration tests.
    ///
    /// This helper **skips** all GCPM passes (CSS Generated Content for
    /// Paged Media — running elements, counters, string-set, `content:`
    /// resolution). It is only appropriate for tests that do not depend on
    /// GCPM-rendered content. For transform tests in particular, no GCPM
    /// state is needed because `transform` is independent of content
    /// generation.
    ///
    /// Concretely, the following are skipped relative to `render_html`:
    ///
    /// - `InjectCssPass` for CSS produced by the GCPM parser
    /// - `RunningElementPass` / `StringSetPass` / `CounterPass`
    /// - `content:` (`::before` / `::after`) resolution via
    ///   counter-generated CSS injection
    ///
    /// The resulting tree can therefore **diverge from the production
    /// tree** whenever the HTML uses counters, running elements, or
    /// `content:` in a `<style>` block. Use this helper only for geometric
    /// / structural assertions on constructs that do not touch GCPM.
    #[doc(hidden)]
    pub fn build_pageable_for_testing_no_gcpm(&self, html: &str) -> Box<dyn Pageable> {
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
            self.base_path.as_deref(),
        );

        let ctx = crate::blitz_adapter::PassContext { font_data: fonts };
        let passes: Vec<Box<dyn crate::blitz_adapter::DomPass>> = Vec::new();
        crate::blitz_adapter::apply_passes(&mut doc, &passes, &ctx);

        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::blitz_adapter::extract_column_style_table(&doc);
        let multicol_geometry = crate::multicol_layout::run_pass(doc.deref_mut(), &column_styles);

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
            link_cache: Default::default(),
        };
        crate::convert::dom_to_pageable(&doc, &mut convert_ctx)
    }

    /// Render a Pageable tree to a PDF file.
    pub fn render_pageable_to_file(
        &self,
        root: Box<dyn Pageable>,
        path: impl AsRef<Path>,
    ) -> Result<()> {
        let pdf = self.render_pageable(root)?;
        std::fs::write(path, pdf)?;
        Ok(())
    }
}

pub struct EngineBuilder {
    config_builder: ConfigBuilder,
    assets: Option<AssetBundle>,
    base_path: Option<PathBuf>,
    template: Option<(String, String)>,
    data: Option<serde_json::Value>,
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

    pub fn build(self) -> Engine {
        Engine {
            config: self.config_builder.build(),
            assets: self.assets,
            base_path: self.base_path,
            template: self.template,
            data: self.data,
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

    #[test]
    fn test_render_html_resolves_link_stylesheet() {
        let dir = tempfile::tempdir().unwrap();
        let css_path = dir.path().join("test.css");
        std::fs::write(&css_path, "p { color: red; }").unwrap();

        let html = r#"<html><head><link rel="stylesheet" href="test.css"></head><body><p>Hello</p></body></html>"#;

        let engine = Engine::builder().base_path(dir.path()).build();
        let result = engine.render_html(html);
        assert!(result.is_ok());
    }

    #[test]
    fn test_render_html_link_stylesheet_with_gcpm() {
        // <link>-loaded CSS that contains @page / running / counter rules
        // must produce a PDF identical in structure to the same CSS passed
        // via --css. Specifically the running header div should NOT appear
        // as body content.
        let dir = tempfile::tempdir().unwrap();
        let css_path = dir.path().join("style.css");
        std::fs::write(
            &css_path,
            r#"
            .pageHeader { position: running(pageHeader); }
            @page { @top-center { content: element(pageHeader); } }
            body { font-family: sans-serif; }
            "#,
        )
        .unwrap();

        let html = r#"<!DOCTYPE html>
<html><head><link rel="stylesheet" href="style.css"></head>
<body>
<div class="pageHeader">RUNNING HEADER TEXT</div>
<h1>Body Heading</h1>
<p>Body paragraph.</p>
</body></html>"#;

        let engine = Engine::builder().base_path(dir.path()).build();
        let pdf = engine.render_html(html).expect("render");

        // Crude check: the PDF should have at least one page and not be
        // empty. A more thorough comparison would require pdf parsing in
        // tests, which we skip; the PR's verification step renders the
        // header-footer example and visually compares against the
        // --css output.
        assert!(!pdf.is_empty());
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn test_render_html_link_stylesheet_with_import() {
        // @import within a <link>-loaded stylesheet should also be
        // resolved by FulgurNetProvider via Blitz/stylo's StylesheetLoader.
        // The imported file is also fed through the GCPM parser, so
        // running elements declared inside an @import target are honoured.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("base.css"),
            r#"@import "header.css"; body { font-family: serif; }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("header.css"),
            r#"
            .pageHeader { position: running(pageHeader); }
            @page { @top-center { content: element(pageHeader); } }
            "#,
        )
        .unwrap();

        let html = r#"<!DOCTYPE html>
<html><head><link rel="stylesheet" href="base.css"></head>
<body>
<div class="pageHeader">FROM IMPORT</div>
<p>Body.</p>
</body></html>"#;

        let engine = Engine::builder().base_path(dir.path()).build();
        let pdf = engine.render_html(html).expect("render");
        assert!(!pdf.is_empty());
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn test_render_html_link_stylesheet_rejects_path_traversal() {
        // A <link href="../secret.css"> outside the base_path must be
        // ignored even if the file exists on disk. We can't easily verify
        // "no styles applied" without parsing the PDF, but we can verify
        // the engine doesn't error out and produces output.
        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().join("base");
        std::fs::create_dir(&base).unwrap();
        std::fs::write(parent.path().join("secret.css"), "body { color: red; }").unwrap();

        let html = r#"<!DOCTYPE html>
<html><head><link rel="stylesheet" href="../secret.css"></head>
<body><p>Hi</p></body></html>"#;

        let engine = Engine::builder().base_path(&base).build();
        let pdf = engine.render_html(html).expect("render");
        assert!(!pdf.is_empty());
    }

    #[test]
    fn test_render_html_marker_content_url_does_not_panic() {
        let html = r#"<!doctype html>
<html><head><style>
li::marker { content: url("bullet.png"); }
</style></head>
<body><ul><li>Item</li></ul></body></html>"#;
        let engine = Engine::builder().build();
        let pdf = engine.render_html(html).expect("render should not panic");
        assert!(!pdf.is_empty());
    }

    #[test]
    fn test_render_html_marker_content_url_with_image() {
        // 1x1 red PNG (valid, generated with correct CRC checksums)
        let png_data: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92,
            0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        let mut bundle = AssetBundle::default();
        bundle.add_css(r#"li::marker { content: url("bullet.png"); }"#);
        bundle.add_image("bullet.png", png_data);

        let html = r#"<!doctype html>
<html><body><ul><li>Item 1</li><li>Item 2</li></ul></body></html>"#;

        let engine = Engine::builder().assets(bundle).build();
        let pdf = engine
            .render_html(html)
            .expect("render should succeed with marker image");
        assert!(!pdf.is_empty(), "PDF should be non-empty");
    }

    /// `repeating-linear-gradient` を end-to-end で render し、`draw_background_layer`
    /// の `LinearGradient { repeating: true }` 経路 (uniform-grid → tiling pattern) を
    /// coverage 上カバーする。VRT 側で同等の reftest はあるが、CI が `--exclude fulgur-vrt`
    /// で coverage 計測しているため lib 側にも smoke test が必要。
    #[test]
    fn test_render_repeating_linear_gradient_smoke() {
        let html = r#"<!doctype html>
<html><body>
<div style="width:200px;height:100px;background:repeating-linear-gradient(to right, red 0%, blue 25%);"></div>
</body></html>"#;
        let pdf = Engine::builder()
            .build()
            .render_html(html)
            .expect("render repeating-linear-gradient");
        assert!(!pdf.is_empty());
    }

    /// `repeating-radial-gradient` の end-to-end smoke test。`RadialGradient { repeating: true }`
    /// 経路をカバーする。
    #[test]
    fn test_render_repeating_radial_gradient_smoke() {
        let html = r#"<!doctype html>
<html><body>
<div style="width:200px;height:200px;background:repeating-radial-gradient(circle 100px at center, red 0px, blue 25px);"></div>
</body></html>"#;
        let pdf = Engine::builder()
            .build()
            .render_html(html)
            .expect("render repeating-radial-gradient");
        assert!(!pdf.is_empty());
    }

    /// `linear-gradient(to top right, ...)` (Corner direction) の smoke test。
    /// `draw_background_layer` の `LinearGradientDirection::Corner` 経路は既存だが
    /// `repeating` 追加に伴い destructure を含む match arm を再書きしたため、
    /// patch coverage を満たすために lib 側にも end-to-end カバーを置いておく。
    #[test]
    fn test_render_linear_gradient_corner_direction_smoke() {
        let html = r#"<!doctype html>
<html><body>
<div style="width:200px;height:100px;background:linear-gradient(to top right, red, blue);"></div>
</body></html>"#;
        let pdf = Engine::builder()
            .build()
            .render_html(html)
            .expect("render corner-direction linear gradient");
        assert!(!pdf.is_empty());
    }

    /// `background-size` で複数タイルを生成して `try_uniform_grid` Some パスを
    /// 通す smoke test。これで linear gradient の uniform-grid → tiling pattern
    /// 経路が coverage に乗る。
    #[test]
    fn test_render_linear_gradient_tiled_smoke() {
        let html = r#"<!doctype html>
<html><body>
<div style="width:200px;height:100px;background:linear-gradient(red, blue);background-size:50px 50px;"></div>
</body></html>"#;
        let pdf = Engine::builder()
            .build()
            .render_html(html)
            .expect("render tiled linear gradient");
        assert!(!pdf.is_empty());
    }
}
