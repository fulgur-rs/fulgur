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
#[derive(Debug, Clone, Copy, PartialEq)]
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

/// Tracks which Config fields were explicitly set by the caller (CLI/API).
/// When true, the field takes precedence over CSS @page declarations.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConfigOverrides {
    pub page_size: bool,
    pub margin: bool,
    pub landscape: bool,
}

/// PDF generation configuration
#[derive(Debug, Clone)]
pub struct Config {
    pub page_size: PageSize,
    pub margin: Margin,
    pub landscape: bool,
    pub overrides: ConfigOverrides,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub description: Option<String>,
    pub keywords: Vec<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub lang: Option<String>,
    /// Generate PDF bookmarks (outline) from h1–h6 headings.
    pub bookmarks: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            page_size: PageSize::A4,
            margin: Margin::default(),
            landscape: false,
            overrides: ConfigOverrides::default(),
            title: None,
            authors: vec![],
            description: None,
            keywords: vec![],
            creator: None,
            producer: Some(format!("fulgur v{}", env!("CARGO_PKG_VERSION"))),
            creation_date: None,
            lang: None,
            bookmarks: false,
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
        self.config.overrides.page_size = true;
        self
    }

    pub fn margin(mut self, margin: Margin) -> Self {
        self.config.margin = margin;
        self.config.overrides.margin = true;
        self
    }

    pub fn landscape(mut self, landscape: bool) -> Self {
        self.config.landscape = landscape;
        self.config.overrides.landscape = true;
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.config.title = Some(title.into());
        self
    }

    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.config.authors.push(author.into());
        self
    }

    pub fn authors(mut self, authors: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.config
            .authors
            .extend(authors.into_iter().map(|a| a.into()));
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.config.description = Some(description.into());
        self
    }

    pub fn keywords(mut self, keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.config
            .keywords
            .extend(keywords.into_iter().map(|k| k.into()));
        self
    }

    pub fn creator(mut self, creator: impl Into<String>) -> Self {
        self.config.creator = Some(creator.into());
        self
    }

    pub fn producer(mut self, producer: impl Into<String>) -> Self {
        self.config.producer = Some(producer.into());
        self
    }

    pub fn creation_date(mut self, creation_date: impl Into<String>) -> Self {
        self.config.creation_date = Some(creation_date.into());
        self
    }

    pub fn lang(mut self, lang: impl Into<String>) -> Self {
        self.config.lang = Some(lang.into());
        self
    }

    pub fn bookmarks(mut self, enabled: bool) -> Self {
        self.config.bookmarks = enabled;
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
    fn test_config_overrides_default() {
        let config = Config::default();
        assert!(!config.overrides.page_size);
        assert!(!config.overrides.margin);
        assert!(!config.overrides.landscape);
    }

    #[test]
    fn test_config_builder_tracks_overrides() {
        let config = Config::builder()
            .page_size(PageSize::LETTER)
            .margin(Margin::uniform_mm(10.0))
            .build();
        assert!(config.overrides.page_size);
        assert!(config.overrides.margin);
        assert!(!config.overrides.landscape);
    }

    #[test]
    fn test_custom_mm_size() {
        let size = PageSize::custom(210.0, 297.0); // A4 in mm
        assert!((size.width - 595.28).abs() < 0.2);
        assert!((size.height - 841.89).abs() < 0.2);
    }
}
