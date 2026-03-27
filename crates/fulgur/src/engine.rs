use crate::asset::AssetBundle;
use crate::blitz_adapter::DomPass;
use crate::config::{Config, ConfigBuilder, Margin, PageSize};
use crate::convert::ConvertContext;
use crate::error::Result;
use crate::pageable::Pageable;
use crate::render::render_to_pdf;
use std::collections::HashMap;
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
        let combined_css = self
            .assets
            .as_ref()
            .map(|a| a.combined_css())
            .unwrap_or_default();

        let gcpm = crate::gcpm::parser::parse_gcpm(&combined_css);
        let css_to_inject = &gcpm.cleaned_css;

        // --- Pipeline: parse → DomPass → resolve ---
        let fonts = self
            .assets
            .as_ref()
            .map(|a| a.fonts.as_slice())
            .unwrap_or(&[]);

        let mut doc = crate::blitz_adapter::parse(html, self.config.content_width(), fonts);

        // Build and apply DOM passes
        let mut passes: Vec<Box<dyn crate::blitz_adapter::DomPass>> = Vec::new();

        // Resolve <link rel="stylesheet"> before CSS injection
        if let Some(ref base_path) = self.base_path {
            passes.push(Box::new(crate::blitz_adapter::LinkStylesheetPass {
                base_path: base_path.clone(),
            }));
        }

        if !css_to_inject.is_empty() {
            passes.push(Box::new(crate::blitz_adapter::InjectCssPass {
                css: css_to_inject.clone(),
            }));
        }

        let ctx = crate::blitz_adapter::PassContext {
            viewport_width: self.config.content_width(),
            viewport_height: self.config.content_height(),
            font_data: fonts,
        };
        crate::blitz_adapter::apply_passes(&mut doc, &passes, &ctx);

        // Extract running elements via DomPass (before resolve)
        let mut running_store = if !gcpm.is_empty() {
            let pass = crate::blitz_adapter::RunningElementPass::new(gcpm.clone());
            pass.apply(&mut doc, &ctx);
            pass.into_running_store()
        } else {
            crate::gcpm::running::RunningElementStore::new()
        };

        crate::blitz_adapter::resolve(&mut doc);

        // --- Convert DOM to Pageable and render ---
        let mut convert_ctx = ConvertContext {
            running_store: &mut running_store,
            assets: self.assets.as_ref(),
            font_cache: HashMap::new(),
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
}
