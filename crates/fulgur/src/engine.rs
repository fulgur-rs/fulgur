use crate::asset::AssetBundle;
use crate::config::{Config, ConfigBuilder, Margin, PageSize};
use crate::error::Result;
use crate::pageable::Pageable;
use crate::render::render_to_pdf;
use std::path::Path;

/// Reusable PDF generation engine.
pub struct Engine {
    config: Config,
    assets: Option<AssetBundle>,
}

impl Engine {
    pub fn builder() -> EngineBuilder {
        EngineBuilder {
            config_builder: Config::builder(),
            assets: None,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
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

        let final_html = if css_to_inject.is_empty() {
            html.to_string()
        } else {
            let style_block = format!("<style>{}</style>", css_to_inject);
            if let Some(pos) = html.find("</head>") {
                format!("{}{}{}", &html[..pos], style_block, &html[pos..])
            } else if let Some(pos) = html.find("<body") {
                format!("{}{}{}", &html[..pos], style_block, &html[pos..])
            } else {
                format!("{}{}", style_block, html)
            }
        };

        let fonts = self
            .assets
            .as_ref()
            .map(|a| a.fonts.as_slice())
            .unwrap_or(&[]);
        let doc = crate::blitz_adapter::parse_and_layout(
            &final_html,
            self.config.content_width(),
            self.config.content_height(),
            fonts,
        );

        let gcpm_opt = if gcpm.is_empty() { None } else { Some(&gcpm) };
        let mut running_store = crate::gcpm::running::RunningElementStore::new();
        let root = crate::convert::dom_to_pageable(&doc, gcpm_opt, &mut running_store);

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

    pub fn assets(mut self, assets: AssetBundle) -> Self {
        self.assets = Some(assets);
        self
    }

    pub fn build(self) -> Engine {
        Engine {
            config: self.config_builder.build(),
            assets: self.assets,
        }
    }
}
