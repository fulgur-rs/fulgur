pub mod counter;
pub mod margin_box;
pub mod page_settings;
pub mod parser;
pub mod running;
pub mod string_set;

use margin_box::MarginBoxPosition;

/// A simple CSS selector parsed from a style rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedSelector {
    /// A class selector, e.g. `.header`
    Class(String),
    /// An ID selector, e.g. `#title`
    Id(String),
    /// A tag name selector, e.g. `header`
    Tag(String),
}

/// Maps a CSS selector to a running element name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningMapping {
    /// The parsed CSS selector.
    pub parsed: ParsedSelector,
    /// The name from `position: running(name)`.
    pub running_name: String,
}

/// Policy for selecting which value of a named string to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringPolicy {
    /// The value inherited at the start of the current page
    /// (i.e. the last assignment from a previous page).
    Start,
    /// The first assignment on the current page, falling back to `Start`
    /// if no assignment happens on this page.
    First,
    /// The last assignment on the current page.
    Last,
    /// Like `First`, but returns the empty string on pages where the
    /// string is assigned (showing only the inherited value on pages
    /// that don't reset it).
    FirstExcept,
}

/// Policy for `element(name, <policy>)` — determines which running element
/// instance to show on a given page. Parallels [`StringPolicy`] but applies
/// to running elements extracted via `position: running(name)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ElementPolicy {
    /// First instance assigned on the current page; if no assignment occurs
    /// on the current page, falls back to the most recent prior assignment.
    /// This is the default when `element(name)` is written without a second
    /// argument.
    #[default]
    First,
    /// The element in effect at the start of the current page — i.e., the
    /// last instance from the most recent *preceding* page. Unlike `First`,
    /// `Start` ignores any assignments on the current page, so a heading
    /// that switches mid-page does not affect the start-of-page value.
    Start,
    /// Last instance assigned on the current page; if no assignment occurs
    /// on the current page, falls back to the most recent prior assignment.
    Last,
    /// Returns the empty value on pages that contain an assignment to this
    /// running element; otherwise falls back to the most recent prior
    /// assignment (same as `First` on unassigned pages).
    FirstExcept,
}

/// A single value component within a `string-set` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringSetValue {
    /// The text content of the element.
    ContentText,
    /// The `::before` pseudo-element content.
    ContentBefore,
    /// The `::after` pseudo-element content.
    ContentAfter,
    /// The value of the named attribute.
    Attr(String),
    /// A literal string value.
    Literal(String),
}

/// Maps a CSS selector to a named string via `string-set`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringSetMapping {
    /// The parsed CSS selector that triggers this mapping.
    pub parsed: ParsedSelector,
    /// The name of the string being set.
    pub name: String,
    /// The value components to concatenate.
    pub values: Vec<StringSetValue>,
}

/// Parsed `size` declaration from `@page`.
#[derive(Debug, Clone, PartialEq)]
pub enum PageSizeDecl {
    /// A named page size, e.g. `A4`, `letter`.
    Keyword(String),
    /// A named page size with orientation, e.g. `A4 landscape`.
    KeywordWithOrientation(String, bool),
    /// Explicit width × height, e.g. `210mm 297mm`. Values in points.
    Custom(f32, f32),
    /// `auto` — use Config default.
    Auto,
}

/// A parsed `@page { size: ...; margin: ...; }` settings rule.
#[derive(Debug, Clone, PartialEq)]
pub struct PageSettingsRule {
    /// Optional page selector (e.g. `:first`, `:left`). `None` means all pages.
    pub page_selector: Option<String>,
    /// Parsed `size` declaration, if present.
    pub size: Option<PageSizeDecl>,
    /// Parsed `margin` declaration, if present. Values in points.
    pub margin: Option<crate::config::Margin>,
}

/// A single counter operation from counter-reset, counter-increment, or counter-set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CounterOp {
    Reset { name: String, value: i32 },
    Increment { name: String, value: i32 },
    Set { name: String, value: i32 },
}

/// Maps a CSS selector to counter operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterMapping {
    pub parsed: ParsedSelector,
    pub ops: Vec<CounterOp>,
}

/// Pseudo-element type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
}

/// Maps a CSS selector + pseudo-element to content items containing counter().
#[derive(Debug, Clone, PartialEq)]
pub struct ContentCounterMapping {
    pub parsed: ParsedSelector,
    pub pseudo: PseudoElement,
    pub content: Vec<ContentItem>,
}

/// A single content item inside a margin box rule's `content` property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentItem {
    /// A running element reference, e.g. `element(title)` or `element(title, first)`.
    Element {
        /// The running element name.
        name: String,
        /// The policy for selecting which instance to show.
        policy: ElementPolicy,
    },
    /// A counter reference, e.g. `counter(page)` or `counter(chapter, upper-roman)`.
    Counter {
        /// Counter name — "page", "pages", or a custom name.
        name: String,
        /// Display style.
        style: CounterStyle,
    },
    /// A literal string, e.g. `"Page "`.
    String(String),
    /// A named string reference, e.g. `string(chapter-title, first)`.
    StringRef {
        /// The name of the string to reference.
        name: String,
        /// The policy for selecting the string value.
        policy: StringPolicy,
    },
}

/// Counter display styles (CSS `list-style-type` subset for counters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CounterStyle {
    /// Decimal numerals (1, 2, 3, ...).
    #[default]
    Decimal,
    /// Upper-case roman numerals (I, II, III, ...).
    UpperRoman,
    /// Lower-case roman numerals (i, ii, iii, ...).
    LowerRoman,
    /// Upper-case alphabetic (A, B, C, ..., Z, AA, AB, ...).
    UpperAlpha,
    /// Lower-case alphabetic (a, b, c, ..., z, aa, ab, ...).
    LowerAlpha,
}

/// A parsed `@page { @<position> { ... } }` margin box rule.
#[derive(Debug, Clone, PartialEq)]
pub struct MarginBoxRule {
    /// Optional page selector (e.g. `:first`, `:left`). `None` means all pages.
    pub page_selector: Option<String>,
    /// Which margin box this rule targets.
    pub position: MarginBoxPosition,
    /// Parsed content items from the `content` property.
    pub content: Vec<ContentItem>,
    /// Raw CSS declarations (excluding `content`) for future use.
    pub declarations: String,
}

/// Aggregated GCPM context extracted from a stylesheet.
#[derive(Debug, Clone)]
pub struct GcpmContext {
    /// All margin box rules found in `@page` rules.
    pub margin_boxes: Vec<MarginBoxRule>,
    /// Mappings from CSS selectors to running element names.
    pub running_mappings: Vec<RunningMapping>,
    /// Mappings from CSS selectors to named strings via `string-set`.
    pub string_set_mappings: Vec<StringSetMapping>,
    /// Page settings rules parsed from `@page { size: ...; margin: ...; }`.
    pub page_settings: Vec<PageSettingsRule>,
    /// Mappings from CSS selectors to counter operations.
    pub counter_mappings: Vec<CounterMapping>,
    /// Mappings from CSS selectors + pseudo-elements to content items with counter().
    pub content_counter_mappings: Vec<ContentCounterMapping>,
    /// The CSS with GCPM constructs stripped, suitable for normal rendering.
    pub cleaned_css: String,
}

impl GcpmContext {
    /// Returns `true` if no GCPM features were found.
    pub fn is_empty(&self) -> bool {
        self.margin_boxes.is_empty()
            && self.running_mappings.is_empty()
            && self.string_set_mappings.is_empty()
            && self.page_settings.is_empty()
            && self.counter_mappings.is_empty()
            && self.content_counter_mappings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gcpm_context_is_empty() {
        let ctx = GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![],
            string_set_mappings: vec![],
            page_settings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            cleaned_css: String::new(),
        };
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_gcpm_context_not_empty_with_margin_box() {
        let ctx = GcpmContext {
            margin_boxes: vec![MarginBoxRule {
                page_selector: None,
                position: MarginBoxPosition::TopCenter,
                content: vec![ContentItem::Counter {
                    name: "page".into(),
                    style: CounterStyle::Decimal,
                }],
                declarations: String::new(),
            }],
            running_mappings: vec![],
            string_set_mappings: vec![],
            page_settings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            cleaned_css: String::new(),
        };
        assert!(!ctx.is_empty());
    }

    #[test]
    fn test_gcpm_context_not_empty_with_running_name() {
        let ctx = GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![RunningMapping {
                parsed: ParsedSelector::Class("header".to_string()),
                running_name: "header".to_string(),
            }],
            string_set_mappings: vec![],
            page_settings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            cleaned_css: String::new(),
        };
        assert!(!ctx.is_empty());
    }
}
