//! AssetBundle for managing CSS, fonts, and images.

use crate::error::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Collection of external assets (CSS, fonts, images) for PDF generation.
pub struct AssetBundle {
    pub css: Vec<String>,
    pub fonts: Vec<Arc<Vec<u8>>>,
    pub images: HashMap<String, Arc<Vec<u8>>>,
}

impl AssetBundle {
    pub fn new() -> Self {
        Self {
            css: Vec::new(),
            fonts: Vec::new(),
            images: HashMap::new(),
        }
    }

    pub fn add_css(&mut self, css: impl Into<String>) {
        self.css.push(css.into());
    }

    pub fn add_css_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let css = std::fs::read_to_string(path)?;
        self.css.push(css);
        Ok(())
    }

    pub fn add_font_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let data = std::fs::read(path)?;
        self.fonts.push(Arc::new(data));
        Ok(())
    }

    pub fn add_image(&mut self, name: impl Into<String>, data: Vec<u8>) {
        self.images.insert(name.into(), Arc::new(data));
    }

    pub fn add_image_file(
        &mut self,
        name: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> Result<()> {
        let data = std::fs::read(path)?;
        self.images.insert(name.into(), Arc::new(data));
        Ok(())
    }

    pub fn get_image(&self, name: &str) -> Option<&Arc<Vec<u8>>> {
        self.images.get(name)
    }

    /// Build combined CSS from all added stylesheets.
    pub fn combined_css(&self) -> String {
        self.css.join("\n")
    }
}

impl Default for AssetBundle {
    fn default() -> Self {
        Self::new()
    }
}
