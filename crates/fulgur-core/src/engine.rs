use crate::config::{Config, ConfigBuilder, Margin, PageSize};
use crate::error::Result;
use crate::pageable::Pageable;
use crate::render::render_to_pdf;
use std::path::Path;

/// Reusable PDF generation engine.
pub struct Engine {
    config: Config,
}

impl Engine {
    pub fn builder() -> EngineBuilder {
        EngineBuilder {
            config_builder: Config::builder(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Render a Pageable tree to PDF bytes.
    pub fn render_pageable(&self, root: Box<dyn Pageable>) -> Result<Vec<u8>> {
        render_to_pdf(root, &self.config)
    }

    /// Render HTML string to PDF bytes.
    pub fn render_html(&self, html: &str) -> Result<Vec<u8>> {
        let doc = crate::blitz_adapter::parse_and_layout(
            html,
            self.config.content_width(),
            self.config.content_height(),
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

    pub fn build(self) -> Engine {
        Engine {
            config: self.config_builder.build(),
        }
    }
}
