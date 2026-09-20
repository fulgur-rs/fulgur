/// Page size in points (1 point = 1/72 inch)
#[derive(Debug, Clone, Copy)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

/// Millimetres to PDF points, usable in a `const` initialiser.
const fn mm(v: f32) -> f32 {
    v * 72.0 / 25.4
}

/// Inches to PDF points, usable in a `const` initialiser.
const fn inch(v: f32) -> f32 {
    v * 72.0
}

impl PageSize {
    // A4 / LETTER / A3 keep their historical two-decimal literals rather than
    // being re-expressed through `mm()` / `inch()`: the rounded values are
    // baked into every VRT golden, and shifting them by a few thousandths of
    // a point would re-bless the whole suite for no visual gain. The sizes
    // added below have no goldens yet, so they are written exactly.
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
    pub const A5: Self = Self {
        width: mm(148.0),
        height: mm(210.0),
    };
    /// ISO B4 (250 × 353 mm) — distinct from `JIS_B4`.
    pub const B4: Self = Self {
        width: mm(250.0),
        height: mm(353.0),
    };
    /// ISO B5 (176 × 250 mm) — distinct from `JIS_B5`.
    pub const B5: Self = Self {
        width: mm(176.0),
        height: mm(250.0),
    };
    pub const JIS_B4: Self = Self {
        width: mm(257.0),
        height: mm(364.0),
    };
    pub const JIS_B5: Self = Self {
        width: mm(182.0),
        height: mm(257.0),
    };
    pub const LEGAL: Self = Self {
        width: inch(8.5),
        height: inch(14.0),
    };
    pub const LEDGER: Self = Self {
        width: inch(11.0),
        height: inch(17.0),
    };

    /// Resolve a CSS `@page { size: <page-size> }` keyword, case-insensitively.
    ///
    /// Covers the full keyword set of CSS Paged Media Level 3 §4.1.1. Returns
    /// `None` for anything else so the caller can decide how to report it —
    /// silently substituting a different sheet of paper is the failure mode
    /// this function exists to prevent (`fulgur-5oav`: `size: A5` used to come
    /// out as A4 with no diagnostic).
    ///
    /// `JIS-B4` / `JIS-B5` are also accepted spelled with an underscore, since
    /// that is how the constants are named.
    pub fn from_css_keyword(name: &str) -> Option<Self> {
        let normalised = name.trim().replace('_', "-").to_ascii_uppercase();
        Some(match normalised.as_str() {
            "A3" => Self::A3,
            "A4" => Self::A4,
            "A5" => Self::A5,
            "B4" => Self::B4,
            "B5" => Self::B5,
            "JIS-B4" => Self::JIS_B4,
            "JIS-B5" => Self::JIS_B5,
            "LETTER" => Self::LETTER,
            "LEGAL" => Self::LEGAL,
            "LEDGER" => Self::LEDGER,
            _ => return None,
        })
    }

    /// The keyword spellings [`Self::from_css_keyword`] accepts, for help
    /// text and diagnostics.
    pub const CSS_KEYWORDS: [&'static str; 10] = [
        "A3", "A4", "A5", "B4", "B5", "JIS-B4", "JIS-B5", "Letter", "Legal", "Ledger",
    ];

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
    /// Enable Tagged PDF output (PDF structure tree).
    pub enable_tagging: bool,
    /// Enable PDF/UA-1 conformance (implies enable_tagging).
    pub pdf_ua: bool,
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
            producer: Some("fulgur".to_string()),
            creation_date: None,
            lang: None,
            bookmarks: false,
            enable_tagging: false,
            pdf_ua: false,
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

    /// Physical page height in PDF pt (before subtracting margins).
    ///
    /// Used as the Blitz viewport height so that CSS `vh` units resolve to the
    /// full page height.  This is the correct value because WPT ref pages use
    /// `@page { margin: 0; }` together with `height: 100vh`, meaning the page
    /// content area equals the physical page height.
    pub fn page_height(&self) -> f32 {
        if self.landscape {
            self.page_size.landscape().height
        } else {
            self.page_size.height
        }
    }

    /// Returns true if tagging should be enabled in the PDF output.
    /// pdf_ua implies tagging.
    pub fn effective_tagging(&self) -> bool {
        self.enable_tagging || self.pdf_ua
    }

    /// Returns true if bookmarks should be generated.
    /// pdf_ua implies bookmarks (PDF/UA requires document outline).
    pub fn effective_bookmarks(&self) -> bool {
        self.bookmarks || self.pdf_ua
    }

    /// Rejects page size / margin combinations that are unsafe to feed into
    /// Blitz layout and pagination: non-finite or non-positive page
    /// dimensions, non-finite or negative margins, and margins that leave no
    /// content area. Called once at the top of the layout pipeline
    /// (`Engine::layout_to_drawables`) so bad values fail fast with a clear
    /// error instead of reaching Blitz/Taffy as e.g. a saturated `u32::MAX`
    /// viewport height.
    pub(crate) fn validate(&self) -> crate::Result<()> {
        let ps = if self.landscape {
            self.page_size.landscape()
        } else {
            self.page_size
        };
        if !ps.width.is_finite() || ps.width <= 0.0 || !ps.height.is_finite() || ps.height <= 0.0 {
            return Err(crate::Error::PdfGeneration(format!(
                "invalid page size: width and height must be finite and positive (got {}x{} pt)",
                ps.width, ps.height
            )));
        }
        for (name, v) in [
            ("top", self.margin.top),
            ("right", self.margin.right),
            ("bottom", self.margin.bottom),
            ("left", self.margin.left),
        ] {
            if !v.is_finite() || v < 0.0 {
                return Err(crate::Error::PdfGeneration(format!(
                    "invalid margin: {name} must be finite and non-negative (got {v})"
                )));
            }
        }
        if self.content_width() <= 0.0 || self.content_height() <= 0.0 {
            return Err(crate::Error::PdfGeneration(format!(
                "invalid margin: margins leave no content area ({}x{} pt page, {}x{} pt content)",
                ps.width,
                ps.height,
                self.content_width(),
                self.content_height()
            )));
        }
        Ok(())
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

    pub fn tagged(mut self, enabled: bool) -> Self {
        self.config.enable_tagging = enabled;
        self
    }

    pub fn pdf_ua(mut self, enabled: bool) -> Self {
        self.config.pdf_ua = enabled;
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
    fn validate_accepts_default_config() {
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn validate_rejects_nan_page_width() {
        let config = Config::builder()
            .page_size(PageSize::custom(f32::NAN, 297.0))
            .build();
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_infinite_page_height() {
        let config = Config::builder()
            .page_size(PageSize::custom(210.0, f32::INFINITY))
            .build();
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_non_positive_page_size() {
        assert!(
            Config::builder()
                .page_size(PageSize::custom(-210.0, 297.0))
                .build()
                .validate()
                .is_err()
        );
        assert!(
            Config::builder()
                .page_size(PageSize::custom(0.0, 297.0))
                .build()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn validate_rejects_negative_margin() {
        let config = Config::builder()
            .page_size(PageSize::A4)
            .margin(Margin::uniform(-1.0))
            .build();
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_non_finite_margin() {
        let config = Config::builder()
            .page_size(PageSize::A4)
            .margin(Margin {
                top: f32::NAN,
                right: 20.0,
                bottom: 20.0,
                left: 20.0,
            })
            .build();
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_margin_collapsing_content_area() {
        // A4 height is 841.89pt; a 1000pt uniform margin leaves no content
        // area on any side.
        let config = Config::builder()
            .page_size(PageSize::A4)
            .margin(Margin::uniform(1000.0))
            .build();
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_accepts_landscape_with_valid_content_area() {
        // Content-area validation must use the landscape-flipped page size,
        // not the portrait one, or a legitimate landscape config could be
        // rejected (or an invalid one wrongly accepted).
        let config = Config::builder()
            .page_size(PageSize::A4)
            .margin(Margin::uniform(72.0))
            .landscape(true)
            .build();
        assert!(config.validate().is_ok());
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

    #[test]
    fn test_default_producer_has_no_version() {
        let config = Config::default();
        let producer = config.producer.as_deref().unwrap_or("");
        assert_eq!(producer, "fulgur");
        assert!(!producer.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn config_enable_tagging_defaults_to_false() {
        let config = Config::default();
        assert!(!config.enable_tagging);
    }

    #[test]
    fn config_pdf_ua_defaults_to_false() {
        let config = Config::default();
        assert!(!config.pdf_ua);
    }

    #[test]
    fn config_effective_tagging_both_false() {
        let config = Config::default();
        assert!(!config.effective_tagging());
    }

    #[test]
    fn config_effective_tagging_enable_tagging() {
        let config = Config::builder().tagged(true).build();
        assert!(config.effective_tagging());
    }

    #[test]
    fn config_effective_tagging_pdf_ua() {
        let config = Config::builder().pdf_ua(true).build();
        assert!(config.effective_tagging());
    }

    #[test]
    fn config_builder_tagged_sets_flag() {
        let config = Config::builder().tagged(true).build();
        assert!(config.enable_tagging);
    }

    #[test]
    fn config_builder_pdf_ua_sets_flag() {
        let config = Config::builder().pdf_ua(true).build();
        assert!(config.pdf_ua);
    }

    #[test]
    fn config_effective_bookmarks_false_by_default() {
        let config = Config::default();
        assert!(!config.effective_bookmarks());
    }

    #[test]
    fn config_effective_bookmarks_with_bookmarks_flag() {
        let config = Config::builder().bookmarks(true).build();
        assert!(config.effective_bookmarks());
    }

    #[test]
    fn config_effective_bookmarks_with_pdf_ua() {
        let config = Config::builder().pdf_ua(true).build();
        assert!(config.effective_bookmarks());
    }

    // --- Margin::symmetric ---

    #[test]
    fn margin_symmetric_sets_vertical_and_horizontal() {
        let m = Margin::symmetric(10.0, 20.0);
        assert_eq!(m.top, 10.0);
        assert_eq!(m.bottom, 10.0);
        assert_eq!(m.right, 20.0);
        assert_eq!(m.left, 20.0);
    }

    #[test]
    fn margin_symmetric_equal_values_matches_uniform() {
        let sym = Margin::symmetric(15.0, 15.0);
        let uni = Margin::uniform(15.0);
        assert_eq!(sym, uni);
    }

    #[test]
    fn margin_symmetric_zero_horizontal() {
        let m = Margin::symmetric(5.0, 0.0);
        assert_eq!(m.top, 5.0);
        assert_eq!(m.bottom, 5.0);
        assert_eq!(m.right, 0.0);
        assert_eq!(m.left, 0.0);
    }

    // --- Config::page_height landscape branch ---

    #[test]
    fn page_height_portrait_returns_portrait_height() {
        let config = Config::builder()
            .page_size(PageSize::A4)
            .landscape(false)
            .build();
        // portrait: height = 841.89pt
        assert!((config.page_height() - PageSize::A4.height).abs() < 0.01);
    }

    #[test]
    fn page_height_landscape_returns_flipped_height() {
        let config = Config::builder()
            .page_size(PageSize::A4)
            .landscape(true)
            .build();
        // landscape: page is rotated, so physical height = A4 portrait width
        assert!((config.page_height() - PageSize::A4.width).abs() < 0.01);
    }

    #[test]
    fn page_height_portrait_letter() {
        let config = Config::builder()
            .page_size(PageSize::LETTER)
            .landscape(false)
            .build();
        assert!((config.page_height() - PageSize::LETTER.height).abs() < 0.01);
    }

    #[test]
    fn page_height_landscape_letter() {
        let config = Config::builder()
            .page_size(PageSize::LETTER)
            .landscape(true)
            .build();
        // Letter portrait width=612, height=792; landscape flips them
        assert!((config.page_height() - PageSize::LETTER.width).abs() < 0.01);
    }

    /// fulgur-5oav: the keyword table has to cover CSS Paged Media Level 3
    /// §4.1.1, and unknown keywords have to be distinguishable from known
    /// ones so the caller can warn instead of silently substituting A4.
    #[test]
    fn css_keyword_table_covers_the_spec_set() {
        let mm = |v: f32| v * 72.0 / 25.4;
        let cases: [(&str, PageSize); 10] = [
            ("A3", PageSize::A3),
            ("A4", PageSize::A4),
            ("A5", PageSize::A5),
            ("B4", PageSize::B4),
            ("B5", PageSize::B5),
            ("JIS-B4", PageSize::JIS_B4),
            ("JIS-B5", PageSize::JIS_B5),
            ("Letter", PageSize::LETTER),
            ("Legal", PageSize::LEGAL),
            ("Ledger", PageSize::LEDGER),
        ];
        for (keyword, want) in cases {
            let got = PageSize::from_css_keyword(keyword)
                .unwrap_or_else(|| panic!("`{keyword}` should be a known keyword"));
            assert!(
                (got.width - want.width).abs() < 0.01 && (got.height - want.height).abs() < 0.01,
                "`{keyword}` resolved to {} x {}",
                got.width,
                got.height
            );
        }
        // Spot-check one dimension against the spec rather than against our
        // own constant, so a typo in the constant is still caught.
        let a5 = PageSize::from_css_keyword("A5").expect("A5");
        assert!((a5.width - mm(148.0)).abs() < 0.01);
        assert!((a5.height - mm(210.0)).abs() < 0.01);
    }

    #[test]
    fn css_keyword_is_case_and_separator_insensitive() {
        let a5 = PageSize::from_css_keyword("A5").expect("A5");
        for spelling in ["a5", "  A5  ", "a5"] {
            let got = PageSize::from_css_keyword(spelling).expect(spelling);
            assert!((got.width - a5.width).abs() < 0.01);
        }
        // `JIS_B5` matches the constant's spelling; `JIS-B5` the CSS one.
        let dash = PageSize::from_css_keyword("JIS-B5").expect("JIS-B5");
        let underscore = PageSize::from_css_keyword("jis_b5").expect("jis_b5");
        assert!((dash.width - underscore.width).abs() < 0.01);
    }

    #[test]
    fn css_keyword_rejects_unknown_names() {
        for name in ["", "banana", "A99", "210mm"] {
            assert!(
                PageSize::from_css_keyword(name).is_none(),
                "`{name}` should not resolve to a page size"
            );
        }
    }

    #[test]
    fn css_keywords_list_matches_the_resolver() {
        for keyword in PageSize::CSS_KEYWORDS {
            assert!(
                PageSize::from_css_keyword(keyword).is_some(),
                "`{keyword}` is advertised in CSS_KEYWORDS but does not resolve"
            );
        }
    }
}
