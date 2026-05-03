use crate::config::{Config, Margin, PageSize};
use crate::gcpm::{PageSettingsRule, PageSizeDecl, PartialMargin};

/// Map a CSS page-size keyword (case-insensitive) to a [`PageSize`].
/// Falls back to A4 for unrecognised keywords.
fn keyword_to_page_size(name: &str) -> PageSize {
    match name.to_uppercase().as_str() {
        "A4" => PageSize::A4,
        "A3" => PageSize::A3,
        "LETTER" => PageSize::LETTER,
        _ => PageSize::A4,
    }
}

/// Returns `true` when `selector` matches the given page number.
///
/// Supported pseudo-selectors:
/// - `:first` — matches page 1 only
/// - `:left`  — matches even page numbers (verso in LTR)
/// - `:right` — matches odd page numbers (recto in LTR)
fn selector_matches(selector: &str, page_num: usize) -> bool {
    match selector {
        ":first" => page_num == 1,
        ":left" => page_num % 2 == 0,
        ":right" => page_num % 2 == 1,
        _ => false,
    }
}

/// Resolve effective page size, margin, and landscape for a given page number.
///
/// Priority model (highest wins):
///
/// ```text
/// CLI override (config.overrides) > CSS @page selector match > CSS @page default > Config defaults
/// ```
pub fn resolve_page_settings(
    rules: &[PageSettingsRule],
    page_num: usize,
    _total_pages: usize,
    config: &Config,
) -> (PageSize, Margin, bool) {
    // --- Collect CSS declarations, separating default from selector-matched ---
    let mut default_size: Option<&PageSizeDecl> = None;
    let mut default_margin = PartialMargin::default();
    let mut matched_size: Option<&PageSizeDecl> = None;
    let mut matched_margin = PartialMargin::default();

    // Later rules override earlier ones per side; selector-matched layer
    // wins over the default layer.
    for rule in rules {
        match &rule.page_selector {
            None => {
                if rule.size.is_some() {
                    default_size = rule.size.as_ref();
                }
                default_margin.merge(&rule.margin);
            }
            Some(sel) => {
                if selector_matches(sel, page_num) {
                    if rule.size.is_some() {
                        matched_size = rule.size.as_ref();
                    }
                    matched_margin.merge(&rule.margin);
                }
            }
        }
    }

    let css_size = matched_size.or(default_size);

    // --- Resolve page size and landscape ---
    let (size, landscape) = if config.overrides.page_size {
        // CLI override for size — also respect CLI landscape override separately.
        let ls = if config.overrides.landscape {
            config.landscape
        } else {
            resolve_landscape_from_css(css_size, config.landscape)
        };
        (config.page_size, ls)
    } else {
        match css_size {
            Some(PageSizeDecl::Keyword(name)) => {
                // Keyword without orientation carries no landscape signal;
                // use config.landscape regardless of override flag.
                (keyword_to_page_size(name), config.landscape)
            }
            Some(PageSizeDecl::KeywordWithOrientation(name, is_landscape)) => {
                let ls = if config.overrides.landscape {
                    config.landscape
                } else {
                    *is_landscape
                };
                let size = if name == "auto" {
                    config.page_size
                } else {
                    keyword_to_page_size(name)
                };
                (size, ls)
            }
            Some(PageSizeDecl::Custom(w, h)) => {
                let ls = if config.overrides.landscape {
                    config.landscape
                } else {
                    false
                };
                (
                    PageSize {
                        width: *w,
                        height: *h,
                    },
                    ls,
                )
            }
            Some(PageSizeDecl::Auto) | None => {
                // No CSS size — fall back entirely to config defaults.
                (config.page_size, config.landscape)
            }
        }
    };

    // Margin cascade: config.margin → default @page → matched selector.
    // Each layer overlays per-side, so partial longhand declarations only
    // affect their own side.
    let margin = if config.overrides.margin {
        config.margin
    } else {
        let mut m = config.margin;
        default_margin.apply_to_margin(&mut m);
        matched_margin.apply_to_margin(&mut m);
        m
    };

    (size, margin, landscape)
}

/// Extract landscape flag from a CSS size declaration.
fn resolve_landscape_from_css(css_size: Option<&PageSizeDecl>, fallback: bool) -> bool {
    match css_size {
        Some(PageSizeDecl::KeywordWithOrientation(_, is_landscape)) => *is_landscape,
        Some(PageSizeDecl::Custom(_, _)) => false,
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, PageSize};
    use crate::gcpm::{PageSettingsRule, PageSizeDecl, PartialMargin};

    #[test]
    fn test_no_page_settings_uses_config() {
        let config = Config::default();
        let (size, margin, landscape) = resolve_page_settings(&[], 1, 10, &config);
        assert!((size.width - PageSize::A4.width).abs() < 0.01);
        assert!((margin.top - config.margin.top).abs() < 0.01);
        assert!(!landscape);
    }

    #[test]
    fn test_css_size_overrides_default_config() {
        let config = Config::default(); // overrides all false
        let rules = vec![PageSettingsRule {
            page_selector: None,
            size: Some(PageSizeDecl::Keyword("letter".into())),
            margin: PartialMargin::default(),
        }];
        let (size, _, _) = resolve_page_settings(&rules, 1, 10, &config);
        assert!((size.width - PageSize::LETTER.width).abs() < 0.01);
    }

    #[test]
    fn test_cli_override_beats_css() {
        let config = Config::builder().page_size(PageSize::A3).build();
        // config.overrides.page_size is true
        let rules = vec![PageSettingsRule {
            page_selector: None,
            size: Some(PageSizeDecl::Keyword("letter".into())),
            margin: PartialMargin::default(),
        }];
        let (size, _, _) = resolve_page_settings(&rules, 1, 10, &config);
        assert!((size.width - PageSize::A3.width).abs() < 0.01);
    }

    #[test]
    fn test_selector_first_matches_page_1() {
        let config = Config::default();
        let rules = vec![
            PageSettingsRule {
                page_selector: None,
                size: None,
                margin: PartialMargin::from_uniform(20.0),
            },
            PageSettingsRule {
                page_selector: Some(":first".into()),
                size: None,
                margin: PartialMargin::from_uniform(50.0),
            },
        ];
        let (_, margin_p1, _) = resolve_page_settings(&rules, 1, 10, &config);
        assert!((margin_p1.top - 50.0).abs() < 0.01);
        let (_, margin_p2, _) = resolve_page_settings(&rules, 2, 10, &config);
        assert!((margin_p2.top - 20.0).abs() < 0.01);
    }

    #[test]
    fn test_left_right_selectors() {
        let config = Config::default();
        let rules = vec![
            PageSettingsRule {
                page_selector: Some(":left".into()),
                size: None,
                margin: PartialMargin::from_sides(20.0, 30.0, 20.0, 10.0),
            },
            PageSettingsRule {
                page_selector: Some(":right".into()),
                size: None,
                margin: PartialMargin::from_sides(20.0, 10.0, 20.0, 30.0),
            },
        ];
        let (_, m2, _) = resolve_page_settings(&rules, 2, 10, &config);
        assert!((m2.left - 10.0).abs() < 0.01);
        let (_, m3, _) = resolve_page_settings(&rules, 3, 10, &config);
        assert!((m3.left - 30.0).abs() < 0.01);
    }

    #[test]
    fn test_page_size_landscape_from_css() {
        let config = Config::default();
        let rules = vec![PageSettingsRule {
            page_selector: None,
            size: Some(PageSizeDecl::KeywordWithOrientation("A4".into(), true)),
            margin: PartialMargin::default(),
        }];
        let (_, _, landscape) = resolve_page_settings(&rules, 1, 10, &config);
        assert!(landscape);
    }

    #[test]
    fn test_custom_page_size() {
        let config = Config::default();
        let rules = vec![PageSettingsRule {
            page_selector: None,
            size: Some(PageSizeDecl::Custom(400.0, 600.0)),
            margin: PartialMargin::default(),
        }];
        let (size, _, landscape) = resolve_page_settings(&rules, 1, 10, &config);
        assert!((size.width - 400.0).abs() < 0.01);
        assert!((size.height - 600.0).abs() < 0.01);
        assert!(!landscape);
    }

    #[test]
    fn test_partial_margin_inherits_unset_sides_from_default() {
        // Default `@page { margin: 0 }` should provide bottom and left when
        // a matched selector only sets top and right.
        let config = Config::default();
        let rules = vec![
            PageSettingsRule {
                page_selector: None,
                size: None,
                margin: PartialMargin::from_uniform(0.0),
            },
            PageSettingsRule {
                page_selector: Some(":right".into()),
                size: None,
                margin: PartialMargin {
                    top: Some(150.0),
                    right: Some(375.0),
                    bottom: None,
                    left: None,
                },
            },
        ];
        let (_, m, _) = resolve_page_settings(&rules, 1, 10, &config);
        assert!((m.top - 150.0).abs() < 0.01);
        assert!((m.right - 375.0).abs() < 0.01);
        assert!((m.bottom - 0.0).abs() < 0.01);
        assert!((m.left - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_partial_margin_falls_back_to_config_when_no_default_rule() {
        let config = Config::default();
        let rules = vec![PageSettingsRule {
            page_selector: Some(":first".into()),
            size: None,
            margin: PartialMargin {
                top: Some(100.0),
                right: None,
                bottom: None,
                left: None,
            },
        }];
        let (_, m, _) = resolve_page_settings(&rules, 1, 10, &config);
        assert!((m.top - 100.0).abs() < 0.01);
        assert!((m.right - config.margin.right).abs() < 0.01);
        assert!((m.bottom - config.margin.bottom).abs() < 0.01);
        assert!((m.left - config.margin.left).abs() < 0.01);
    }
}
