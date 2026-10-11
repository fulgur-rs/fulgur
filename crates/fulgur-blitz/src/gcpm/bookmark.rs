//! Types for CSS GCPM `bookmark-*` properties (`bookmark-level`,
//! `bookmark-label`).
//!
//! See <https://www.w3.org/TR/css-gcpm-3/#bookmark-properties>.
//!
//! The parser (`gcpm::parser`) populates `BookmarkMapping` entries in
//! `GcpmContext::bookmark_mappings`. A later pass (`blitz_adapter`) walks
//! the DOM, matches each element against the mapping's selector, resolves
//! the label items against that element, and emits entries into the PDF
//! outline via `BookmarkCollector`.

use crate::gcpm::{ContentItem, ParsedSelector};

/// The value of `bookmark-level`.
///
/// A positive integer establishes the outline depth (1 = top-level).
/// `None_` maps to `bookmark-level: none`, which suppresses the bookmark
/// for that element even when `bookmark-label` is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkLevel {
    /// Explicit integer depth (1-based; 1 is the outermost).
    Integer(u8),
    /// `bookmark-level: none` — suppress the bookmark for this element.
    // Suffix underscore because `None` is reserved for the `Option::None`
    // pattern and shadowing it in match arms on `BookmarkLevel` would
    // surprise readers.
    None_,
}

/// A single bookmark mapping extracted from a CSS rule.
///
/// A mapping is emitted when a style rule carries `bookmark-level` and/or
/// `bookmark-label`. Either property is sufficient on its own — callers
/// treat missing values as defaults per the GCPM spec (unset level ⇒
/// continue the sibling sequence; unset label ⇒ use the element's
/// rendered text).
#[derive(Debug, Clone, PartialEq)]
pub struct BookmarkMapping {
    /// The parsed CSS selector that triggers this mapping.
    pub selector: ParsedSelector,
    /// Parsed `bookmark-level` value, if present.
    pub level: Option<BookmarkLevel>,
    /// Parsed `bookmark-label` content list, if present.
    pub label: Option<Vec<ContentItem>>,
}
