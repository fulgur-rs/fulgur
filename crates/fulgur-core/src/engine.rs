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
    pub fn render_html(&self, html: &str) -> Result<Vec<u8>> {
        let final_html = if let Some(assets) = &self.assets {
            let combined_css = assets.combined_css();
            if combined_css.is_empty() {
                html.to_string()
            } else {
                // Inject CSS into the HTML
                let style_block = format!("<style>{}</style>", combined_css);
                if let Some(pos) = html.find("</head>") {
                    format!("{}{}{}", &html[..pos], style_block, &html[pos..])
                } else if let Some(pos) = html.find("<body") {
                    format!("{}{}{}", &html[..pos], style_block, &html[pos..])
                } else {
                    format!("{}{}", style_block, html)
                }
            }
        } else {
            html.to_string()
        };

        let fonts = self.assets.as_ref()
            .map(|a| a.fonts.as_slice())
            .unwrap_or(&[]);
        let doc = crate::blitz_adapter::parse_and_layout(
            &final_html,
            self.config.content_width(),
            self.config.content_height(),
            fonts,
        );
        let root = crate::convert::dom_to_pageable(&doc);
        self.render_pageable(root)
    }

    /// Render HTML string to a PDF file.
    pub fn render_html_to_file(
        &self,
        html: &str,
        path: impl AsRef<Path>,
    ) -> Result<()> {
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
