pub mod counter;
pub mod margin_box;
pub mod parser;
pub mod running;

use std::collections::HashSet;

use margin_box::MarginBoxPosition;

/// A single content item inside a margin box rule's `content` property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentItem {
    /// A running element reference, e.g. `element(title)`.
    Element(String),
    /// A counter reference, e.g. `counter(page)`.
    Counter(CounterType),
    /// A literal string, e.g. `"Page "`.
    String(String),
}

/// Counter types supported by GCPM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CounterType {
    /// Current page number.
    Page,
    /// Total page count.
    Pages,
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
    /// Names referenced by `position: running(...)` in the stylesheet.
    pub running_names: HashSet<String>,
    /// The CSS with GCPM constructs stripped, suitable for normal rendering.
    pub cleaned_css: String,
}

impl GcpmContext {
    /// Returns `true` if no GCPM features were found.
    pub fn is_empty(&self) -> bool {
        self.margin_boxes.is_empty() && self.running_names.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gcpm_context_is_empty() {
        let ctx = GcpmContext {
            margin_boxes: vec![],
            running_names: HashSet::new(),
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
                content: vec![ContentItem::Counter(CounterType::Page)],
                declarations: String::new(),
            }],
            running_names: HashSet::new(),
            cleaned_css: String::new(),
        };
        assert!(!ctx.is_empty());
    }

    #[test]
    fn test_gcpm_context_not_empty_with_running_name() {
        let mut names = HashSet::new();
        names.insert("header".to_string());
        let ctx = GcpmContext {
            margin_boxes: vec![],
            running_names: names,
            cleaned_css: String::new(),
        };
        assert!(!ctx.is_empty());
    }
}
