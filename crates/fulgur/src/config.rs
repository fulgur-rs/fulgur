/// Page size in points (1 point = 1/72 inch)
#[derive(Debug, Clone, Copy)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

impl PageSize {
    pub const A4: Self = Self {
        width: 595.28,
        height: 841.89,
    };
    pub const LETTER: Self = Self {
        width: 612.0,
        height: 792.0,
    };
    pub const A3: Self = Self {
        width: 841.89,
        height: 1190.55,
    };

    pub fn custom(width_mm: f32, height_mm: f32) -> Self {
        Self {
            width: width_mm * 72.0 / 25.4,
            height: height_mm * 72.0 / 25.4,
        }
    }

    pub fn landscape(self) -> Self {
        Self {
            width: self.height,
            height: self.width,
        }
    }
}

/// Margin in points
#[derive(Debug, Clone, Copy)]
pub struct Margin {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Margin {
    pub fn uniform(pt: f32) -> Self {
        Self {
            top: pt,
            right: pt,
            bottom: pt,
            left: pt,
        }
    }

    pub fn symmetric(vertical: f32, horizontal: f32) -> Self {
        Self {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }

    pub fn uniform_mm(mm: f32) -> Self {
        Self::uniform(mm * 72.0 / 25.4)
    }
}

impl Default for Margin {
    fn default() -> Self {
        Self::uniform_mm(20.0)
    }
}

/// PDF generation configuration
#[derive(Debug, Clone)]
pub struct Config {
    pub page_size: PageSize,
    pub margin: Margin,
    pub landscape: bool,
    pub title: Option<String>,
    pub author: Option<String>,
    pub lang: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            page_size: PageSize::A4,
            margin: Margin::default(),
            landscape: false,
            title: None,
            author: None,
            lang: None,
        }
    }
}

impl Config {
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::default()
    }

    /// Content area width (page width minus left and right margins)
    pub fn content_width(&self) -> f32 {
        let ps = if self.landscape {
            self.page_size.landscape()
        } else {
            self.page_size
        };
        ps.width - self.margin.left - self.margin.right
    }

    /// Content area height (page height minus top and bottom margins)
    pub fn content_height(&self) -> f32 {
        let ps = if self.landscape {
            self.page_size.landscape()
        } else {
            self.page_size
        };
        ps.height - self.margin.top - self.margin.bottom
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConfigBuilder {
    config: Config,
}

impl ConfigBuilder {
    pub fn page_size(mut self, size: PageSize) -> Self {
        self.config.page_size = size;
        self
    }

    pub fn margin(mut self, margin: Margin) -> Self {
        self.config.margin = margin;
        self
    }

    pub fn landscape(mut self, landscape: bool) -> Self {
        self.config.landscape = landscape;
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.config.title = Some(title.into());
        self
    }

    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.config.author = Some(author.into());
        self
    }

    pub fn lang(mut self, lang: impl Into<String>) -> Self {
        self.config.lang = Some(lang.into());
        self
    }

    pub fn build(self) -> Config {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a4_dimensions() {
        let size = PageSize::A4;
        assert!((size.width - 595.28).abs() < 0.01);
        assert!((size.height - 841.89).abs() < 0.01);
    }

    #[test]
    fn test_landscape() {
        let size = PageSize::A4.landscape();
        assert!((size.width - 841.89).abs() < 0.01);
        assert!((size.height - 595.28).abs() < 0.01);
    }

    #[test]
    fn test_content_area() {
        let config = Config::builder()
            .page_size(PageSize::A4)
            .margin(Margin::uniform(72.0)) // 1 inch
            .build();
        assert!((config.content_width() - (595.28 - 144.0)).abs() < 0.01);
        assert!((config.content_height() - (841.89 - 144.0)).abs() < 0.01);
    }

    #[test]
    fn test_content_area_landscape() {
        let config = Config::builder()
            .page_size(PageSize::A4)
            .margin(Margin::uniform(72.0))
            .landscape(true)
            .build();
        assert!((config.content_width() - (841.89 - 144.0)).abs() < 0.01);
        assert!((config.content_height() - (595.28 - 144.0)).abs() < 0.01);
    }

    #[test]
    fn test_custom_mm_size() {
        let size = PageSize::custom(210.0, 297.0); // A4 in mm
        assert!((size.width - 595.28).abs() < 0.2);
        assert!((size.height - 841.89).abs() < 0.2);
    }
}
