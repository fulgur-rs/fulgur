use crate::config::{Config, Margin, PageSize};
use crate::gcpm::{PageSettingsRule, PageSizeDecl, PartialMargin};

/// Map a CSS page-size keyword (case-insensitive) to a [`PageSize`].
///
/// The keyword table lives on [`PageSize::from_css_keyword`] so this path and
/// the CLI's `--size` cannot drift apart. An unrecognised keyword still falls
/// back to A4 — there is no page to render otherwise — but it now says so
/// (`fulgur-5oav`): the old silent fallback meant `size: A5` produced an A4
/// sheet with no diagnostic anywhere, which is the kind of thing that is only
/// noticed after the documents are printed.
fn keyword_to_page_size(name: &str) -> PageSize {
    // Bound how much of `name` gets processed at all, *before* even the
    // first resolution attempt: `PageSize::from_css_keyword` itself
    // normalises the whole string (trim/replace/uppercase) before
    // comparing it against the known-keyword table, so calling it with an
    // unbounded `name` would already do `O(name.len())` work on every
    // call, success or failure. `name` is attacker-controlled and
    // `resolve_page_settings` resolves it repeatedly across pages (see
    // render.rs:98, render.rs:190, engine.rs:238), so a single
    // pathologically long keyword would otherwise cost allocation/CPU
    // proportional to its length on every one of those calls. Every entry
    // in `PageSize::CSS_KEYWORDS` is well under `MAX_LOGGED_KEYWORD_LEN`
    // characters, so bounding first cannot change the outcome for a
    // legitimate keyword — only a keyword that was already going to fail
    // to resolve is ever affected.
    let (bounded, truncated) = bound_keyword(name);
    PageSize::from_css_keyword(bounded).unwrap_or_else(|| {
        // `resolve_page_settings` runs once per page (and more than once
        // per page across setup / per-page / destination passes), so a
        // document with an unrecognised keyword would otherwise log the
        // same warning hundreds of times. Dedup process-wide so each
        // distinct bad keyword is reported once per process rather than
        // once per resolve call.
        if should_warn_once(bounded) {
            // Built outside the macro on purpose: `log::warn!` only
            // evaluates its arguments when the level is enabled, and a
            // library's callers often install no logger at all. Doing the
            // join here keeps this cold fallback path executed (and
            // therefore covered) either way.
            let known = PageSize::CSS_KEYWORDS.join(", ");
            let shown = sanitize_for_log(bounded, truncated);
            log::warn!(
                "@page {{ size: {shown} }}: unknown page-size keyword, falling back to A4. \
                 Known keywords: {known}. For any other sheet, give explicit dimensions \
                 (e.g. `size: 148mm 210mm`)."
            );
        }
        PageSize::A4
    })
}

/// Longest prefix of an untrusted `@page { size }` keyword this module ever
/// processes — for [`sanitize_for_log`] and the warning-dedup key alike.
const MAX_LOGGED_KEYWORD_LEN: usize = 100;

/// Bounds how much of `name` the caller processes further, returning the
/// (possibly shortened) prefix plus whether it was actually shortened.
/// Cheap and independent of `name`'s length beyond the cutoff — `nth`
/// stops walking `char_indices` as soon as it finds the boundary, so an
/// oversized `name` costs the same either way.
fn bound_keyword(name: &str) -> (&str, bool) {
    match name.char_indices().nth(MAX_LOGGED_KEYWORD_LEN) {
        Some((byte_idx, _)) => (&name[..byte_idx], true),
        None => (name, false),
    }
}

/// Makes an already-[`bound_keyword`]-ed, untrusted CSS identifier safe to
/// interpolate into a log message.
///
/// `bounded` comes (by way of `bound_keyword`) from the document's
/// `@page { size }` declaration: CSS escapes such as `\A` / `\D` decode to
/// a literal newline / carriage return by the time it reaches here, so
/// printing it with `{bounded}` would let a crafted stylesheet forge
/// additional log lines (e.g. `size: bad\A [ERROR] forged-entry` could make
/// it look like a second, unrelated log record). Formatting with `{:?}`
/// (Debug) escapes control characters the same way a Rust string literal
/// would, so a decoded newline/CR/etc. becomes the visible two-character
/// sequence `\n` / `\r` rather than an actual line break. `truncated`
/// appends a trailing `…` to make the earlier shortening visible.
fn sanitize_for_log(bounded: &str, truncated: bool) -> String {
    let mut shown = format!("{bounded:?}");
    if truncated {
        shown.push('…');
    }
    shown
}

/// Cap on distinct unknown keywords tracked for the once-per-process warning
/// dedup in [`should_warn_once`]. `name` comes straight from the document's
/// `@page { size }` declaration, so it is attacker-controlled input to a
/// library that may run inside a long-lived service rendering many
/// untrusted documents. Bounding the set (and hashing rather than storing
/// the raw string) keeps a stream of distinct garbage keywords from growing
/// memory without limit.
///
/// Past the cap, `insert_bounded` evicts the oldest tracked key (FIFO)
/// instead of simply refusing to track new ones: a document that keeps
/// reusing the *same* keyword beyond the cap still gets it deduped, because
/// that keyword is always the most-recently inserted and therefore the
/// last one evicted. Only a genuinely wide spread of distinct bad keywords
/// (more than the cap, each recurring) degrades to warning more than once —
/// bounded degradation, not the unbounded-flood risk a "stop tracking past
/// the cap" policy would reintroduce.
const MAX_TRACKED_WARNING_KEYWORDS: usize = 256;

/// Inserts `key` into `seen` (tracked with `order` for FIFO eviction)
/// unless it's already present. Returns `true` when the caller should warn
/// (first sight of `key`) and `false` when `key` was already tracked. When
/// `seen` is at `max` capacity, the oldest key is evicted to make room —
/// `key` itself is always inserted, so an immediate repeat of the same key
/// still dedups. Pure and side-effect-free apart from its arguments, so the
/// capacity/eviction behavior is unit-testable without touching the
/// process-global state in [`should_warn_once`].
fn insert_bounded(
    seen: &mut std::collections::HashSet<u64>,
    order: &mut std::collections::VecDeque<u64>,
    key: u64,
    max: usize,
) -> bool {
    if seen.contains(&key) {
        return false;
    }
    if seen.len() >= max {
        if let Some(oldest) = order.pop_front() {
            seen.remove(&oldest);
        }
    }
    seen.insert(key);
    order.push_back(key);
    true
}

/// Returns `true` the first time a given (normalised) unknown keyword is
/// seen, `false` on every subsequent call — the dedup key for the
/// once-per-process warning in [`keyword_to_page_size`]. Callers should
/// pass an already-[`bound_keyword`]-ed `name`: the normalise-and-hash work
/// here is `O(name.len())`, run on every `resolve_page_settings` call
/// regardless of cache hit/miss, so an unbounded `name` would cost
/// allocation/CPU proportional to its length on every one of those calls.
fn should_warn_once(name: &str) -> bool {
    use std::collections::hash_map::DefaultHasher;
    use std::collections::{HashSet, VecDeque};
    use std::hash::{Hash, Hasher};
    use std::sync::{Mutex, OnceLock};

    static WARNED: OnceLock<Mutex<(HashSet<u64>, VecDeque<u64>)>> = OnceLock::new();
    let normalised = name.trim().replace('_', "-").to_ascii_uppercase();
    let mut hasher = DefaultHasher::new();
    normalised.hash(&mut hasher);
    let key = hasher.finish();

    let mut guard = WARNED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (seen, order) = &mut *guard;
    insert_bounded(seen, order, key, MAX_TRACKED_WARNING_KEYWORDS)
}

/// Returns `true` when `selector` matches the given page number.
///
/// Supported pseudo-selectors:
/// - `:first` — matches page 1 only
/// - `:left`  — even pages in LTR, odd pages in RTL (CSS Paged Media §5:
///   RTL documents start on a `:left` page)
/// - `:right` — odd pages in LTR, even pages in RTL
fn selector_matches(selector: &str, page_num: usize, first_page_is_left: bool) -> bool {
    match selector {
        ":first" => page_num == 1,
        ":left" => {
            if first_page_is_left {
                page_num % 2 == 1
            } else {
                page_num.is_multiple_of(2)
            }
        }
        ":right" => {
            if first_page_is_left {
                page_num.is_multiple_of(2)
            } else {
                page_num % 2 == 1
            }
        }
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
///
/// When `config.overrides.page_size` is set (i.e. `--size` / `builder.page_size`),
/// the CLI fully owns page geometry **including orientation**: CSS `@page`
/// orientation is intentionally ignored, and landscape is taken solely from
/// `config.landscape` (request it with `--landscape` / `builder.landscape(true)`).
pub fn resolve_page_settings(
    rules: &[PageSettingsRule],
    page_num: usize,
    _total_pages: usize,
    config: &Config,
    first_page_is_left: bool,
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
                if selector_matches(sel, page_num, first_page_is_left) {
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
        // CLI `--size` owns geometry, including orientation: CSS `@page`
        // orientation is ignored so `--size` is authoritative on both axes.
        // Landscape comes solely from `config.landscape` (default portrait;
        // set via `--landscape` / `builder.landscape(true)`).
        (config.page_size, config.landscape)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, PageSize};
    use crate::gcpm::{PageSettingsRule, PageSizeDecl, PartialMargin};

    /// `bound_keyword` has to shorten a pathologically long keyword using
    /// only cheap, length-bounded work (fulgur-5oav follow-up: the
    /// normalise-and-hash work downstream in `should_warn_once` runs on
    /// every `resolve_page_settings` call, so leaving that unbounded would
    /// let a single long keyword cost CPU/allocation proportional to its
    /// length on every page).
    #[test]
    fn bound_keyword_truncates_long_keywords_and_reports_it() {
        let long = "a".repeat(MAX_LOGGED_KEYWORD_LEN * 3);
        let (bounded, truncated) = bound_keyword(&long);
        assert!(
            truncated,
            "an oversized keyword must be reported as bounded"
        );
        assert!(
            bounded.len() < long.len(),
            "an oversized keyword must be shortened: got {} chars for a {}-char input",
            bounded.len(),
            long.len()
        );
    }

    #[test]
    fn bound_keyword_leaves_short_keywords_untouched() {
        let (bounded, truncated) = bound_keyword("banana");
        assert_eq!(bounded, "banana");
        assert!(
            !truncated,
            "a short keyword must not be reported as bounded"
        );
    }

    /// `keyword_to_page_size` must bound `name` *before* the first
    /// `PageSize::from_css_keyword` resolution attempt, not only inside the
    /// `unwrap_or_else` fallback: `from_css_keyword` itself normalises the
    /// whole string before comparing it against the keyword table, so an
    /// unbounded `name` would cost `O(name.len())` on every call whether or
    /// not it ends up being invalid. This is a behavioral check (still
    /// falls back to A4 for an oversized invalid keyword) standing in for
    /// that ordering, since the ordering itself isn't independently
    /// observable from outside `keyword_to_page_size`.
    #[test]
    fn keyword_to_page_size_falls_back_to_a4_for_oversized_invalid_keyword() {
        let huge_garbage = "z".repeat(MAX_LOGGED_KEYWORD_LEN * 10);
        assert_eq!(
            keyword_to_page_size(&huge_garbage).width,
            PageSize::A4.width
        );
        assert_eq!(
            keyword_to_page_size(&huge_garbage).height,
            PageSize::A4.height
        );
    }

    /// A legitimate keyword must still resolve correctly regardless of
    /// `keyword_to_page_size` now bounding `name` before resolving it —
    /// every `PageSize::CSS_KEYWORDS` entry is far shorter than the bound,
    /// so this must be a no-op for real keywords.
    #[test]
    fn keyword_to_page_size_still_resolves_real_keywords() {
        let a5 = keyword_to_page_size("A5");
        assert_eq!(a5.width, PageSize::A5.width);
        assert_eq!(a5.height, PageSize::A5.height);
    }

    /// A CSS `\A` / `\D` escape in a `@page { size }` keyword decodes to a
    /// literal newline / carriage return by the time it reaches
    /// `keyword_to_page_size`. `sanitize_for_log` must neutralize that
    /// (fulgur-5oav follow-up: log injection via crafted page-size names)
    /// so a crafted stylesheet cannot forge extra log lines.
    #[test]
    fn sanitize_for_log_escapes_control_characters() {
        let forged = "bad\nERROR: forged-entry";
        let sanitized = sanitize_for_log(forged, false);
        assert!(
            !sanitized.contains('\n'),
            "a literal newline must not survive sanitisation: {sanitized:?}"
        );
        assert!(
            sanitized.contains("\\n"),
            "the newline must show up as the escaped two-character sequence: {sanitized:?}"
        );
    }

    #[test]
    fn sanitize_for_log_marks_truncation_when_told_to() {
        let sanitized = sanitize_for_log("banana", true);
        assert!(
            sanitized.ends_with('…'),
            "the caller-reported truncation must be visible in the output: {sanitized:?}"
        );
    }

    #[test]
    fn sanitize_for_log_leaves_short_plain_keywords_readable() {
        let sanitized = sanitize_for_log("banana", false);
        assert!(
            sanitized.contains("banana"),
            "an ordinary keyword must still be readable: {sanitized:?}"
        );
    }

    /// The unknown-keyword warning must fire once per distinct (normalised)
    /// keyword and never again — otherwise a document with hundreds of pages
    /// and one typo'd `@page { size }` would log hundreds of identical
    /// warnings (see `keyword_to_page_size`'s call sites in render.rs /
    /// engine.rs). Unique keyword strings per assertion keep this
    /// independent of test execution order against the shared process-wide
    /// dedup set.
    #[test]
    fn should_warn_once_dedupes_per_normalised_keyword() {
        assert!(should_warn_once("totally-unique-test-keyword-1"));
        assert!(!should_warn_once("totally-unique-test-keyword-1"));
        assert!(
            !should_warn_once("TOTALLY-UNIQUE-TEST-KEYWORD-1"),
            "dedup key must be case-insensitive"
        );
        assert!(
            !should_warn_once("totally_unique_test_keyword_1"),
            "dedup key must treat `_` and `-` the same, like `from_css_keyword`"
        );
        assert!(should_warn_once("totally-unique-test-keyword-2"));
    }

    /// `insert_bounded` backs the dedup cap that guards against unbounded
    /// memory growth from a stream of distinct attacker-controlled keywords
    /// (`should_warn_once` is process-global, so exercising the cap through
    /// it directly would permanently pollute shared state for every other
    /// test in this binary — testing the pure helper against local
    /// collections avoids that).
    #[test]
    fn insert_bounded_caps_growth_and_keeps_deduping_existing_keys() {
        use std::collections::{HashSet, VecDeque};

        let mut seen: HashSet<u64> = HashSet::new();
        let mut order: VecDeque<u64> = VecDeque::new();
        let max = 3;

        assert!(
            insert_bounded(&mut seen, &mut order, 1, max),
            "first sight of 1 warns"
        );
        assert!(
            insert_bounded(&mut seen, &mut order, 2, max),
            "first sight of 2 warns"
        );
        assert!(
            insert_bounded(&mut seen, &mut order, 3, max),
            "first sight of 3 warns"
        );
        assert_eq!(seen.len(), max, "set stops growing at the cap");

        assert!(
            !insert_bounded(&mut seen, &mut order, 1, max),
            "1 is still tracked and dedups"
        );
        assert!(
            !insert_bounded(&mut seen, &mut order, 2, max),
            "2 is still tracked and dedups"
        );

        // The set is full: inserting a brand-new key 4 evicts the oldest
        // still-tracked key by insertion order (1 — eviction is FIFO, not
        // LRU, so the repeat lookups above did not move 1 or 2 to the
        // back) rather than refusing to track 4 at all.
        assert!(
            insert_bounded(&mut seen, &mut order, 4, max),
            "new key past the cap still warns"
        );
        assert_eq!(seen.len(), max, "set does not grow past the cap");
        assert!(
            !insert_bounded(&mut seen, &mut order, 4, max),
            "an immediate repeat of the newly inserted key still dedups — \
             this is what prevents a single reused bad keyword from \
             re-flooding the log once the cache is full"
        );
        assert!(
            insert_bounded(&mut seen, &mut order, 1, max),
            "1 was evicted to make room for 4, so it is treated as new again"
        );
    }

    #[test]
    fn test_no_page_settings_uses_config() {
        let config = Config::default();
        let (size, margin, landscape) = resolve_page_settings(&[], 1, 10, &config, false);
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
        let (size, _, _) = resolve_page_settings(&rules, 1, 10, &config, false);
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
        let (size, _, _) = resolve_page_settings(&rules, 1, 10, &config, false);
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
        let (_, margin_p1, _) = resolve_page_settings(&rules, 1, 10, &config, false);
        assert!((margin_p1.top - 50.0).abs() < 0.01);
        let (_, margin_p2, _) = resolve_page_settings(&rules, 2, 10, &config, false);
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
        let (_, m2, _) = resolve_page_settings(&rules, 2, 10, &config, false);
        assert!((m2.left - 10.0).abs() < 0.01);
        let (_, m3, _) = resolve_page_settings(&rules, 3, 10, &config, false);
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
        let (_, _, landscape) = resolve_page_settings(&rules, 1, 10, &config, false);
        assert!(landscape);
    }

    #[test]
    fn test_cli_override_ignores_css_landscape() {
        // `--size` (overrides.page_size) is authoritative on the orientation
        // axis too: a CSS `@page { size: <keyword> landscape }` must not rotate
        // the CLI-specified size. Without `--landscape`, config.landscape is
        // false, so the result stays portrait. (fulgur-u4k5)
        let config = Config::builder().page_size(PageSize::A4).build();
        let rules = vec![PageSettingsRule {
            page_selector: None,
            size: Some(PageSizeDecl::KeywordWithOrientation("A4".into(), true)),
            margin: PartialMargin::default(),
        }];
        let (_, _, landscape) = resolve_page_settings(&rules, 1, 10, &config, false);
        assert!(!landscape, "CLI --size must ignore CSS @page landscape");
    }

    #[test]
    fn test_cli_override_with_landscape_flag_is_landscape() {
        // `--size A4 --landscape` still yields landscape: orientation comes
        // solely from config.landscape in the override path. (fulgur-u4k5)
        let config = Config::builder()
            .page_size(PageSize::A4)
            .landscape(true)
            .build();
        let rules = vec![PageSettingsRule {
            page_selector: None,
            size: Some(PageSizeDecl::KeywordWithOrientation("A4".into(), false)),
            margin: PartialMargin::default(),
        }];
        let (_, _, landscape) = resolve_page_settings(&rules, 1, 10, &config, false);
        assert!(landscape, "--landscape must force landscape under --size");
    }

    #[test]
    fn test_custom_page_size() {
        let config = Config::default();
        let rules = vec![PageSettingsRule {
            page_selector: None,
            size: Some(PageSizeDecl::Custom(400.0, 600.0)),
            margin: PartialMargin::default(),
        }];
        let (size, _, landscape) = resolve_page_settings(&rules, 1, 10, &config, false);
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
        let (_, m, _) = resolve_page_settings(&rules, 1, 10, &config, false);
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
        let (_, m, _) = resolve_page_settings(&rules, 1, 10, &config, false);
        assert!((m.top - 100.0).abs() < 0.01);
        assert!((m.right - config.margin.right).abs() < 0.01);
        assert!((m.bottom - config.margin.bottom).abs() < 0.01);
        assert!((m.left - config.margin.left).abs() < 0.01);
    }

    #[test]
    fn test_left_right_selectors_rtl() {
        // In RTL documents the first page is :left (first_page_is_left = true).
        // Odd pages → :left, even pages → :right.
        let config = Config::default();
        let rules = vec![
            PageSettingsRule {
                page_selector: Some(":left".into()),
                size: None,
                margin: PartialMargin::from_uniform(11.0),
            },
            PageSettingsRule {
                page_selector: Some(":right".into()),
                size: None,
                margin: PartialMargin::from_uniform(22.0),
            },
        ];
        let (_, m1, _) = resolve_page_settings(&rules, 1, 4, &config, true);
        assert!(
            (m1.top - 11.0).abs() < 0.01,
            "page 1 should be :left in RTL"
        );
        let (_, m2, _) = resolve_page_settings(&rules, 2, 4, &config, true);
        assert!(
            (m2.top - 22.0).abs() < 0.01,
            "page 2 should be :right in RTL"
        );
        let (_, m3, _) = resolve_page_settings(&rules, 3, 4, &config, true);
        assert!(
            (m3.top - 11.0).abs() < 0.01,
            "page 3 should be :left in RTL"
        );
        let (_, m4, _) = resolve_page_settings(&rules, 4, 4, &config, true);
        assert!(
            (m4.top - 22.0).abs() < 0.01,
            "page 4 should be :right in RTL"
        );
    }
}
