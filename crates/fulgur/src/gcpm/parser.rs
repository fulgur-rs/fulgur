use cssparser::{
    AtRuleParser, BasicParseErrorKind, CowRcStr, DeclarationParser, ParseError, Parser,
    ParserInput, QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser, Token,
};

use super::margin_box::MarginBoxPosition;
use super::{
    ContentCounterMapping, ContentItem, CounterMapping, CounterOp, CounterStyle, ElementPolicy,
    GcpmContext, MarginBoxRule, PageSettingsRule, PageSizeDecl, ParsedSelector, PseudoElement,
    RunningMapping, StringPolicy, StringSetMapping, StringSetValue,
};
use crate::config::Margin;

// ---------------------------------------------------------------------------
// Top-level result types
// ---------------------------------------------------------------------------

/// A parsed item from the top-level stylesheet scan.
/// The variants carry no data; results are accumulated via mutable references.
enum TopLevelItem {
    /// An `@page` rule was found.
    PageRule,
    /// A qualified rule (style rule) was found.
    StyleRule,
}

// ---------------------------------------------------------------------------
// 1. Top-level parser (GcpmSheetParser)
// ---------------------------------------------------------------------------

/// Collects byte-offset spans of `@page` rules and `position: running(...)` declarations
/// so that `cleaned_css` can be assembled from the original source.
struct GcpmSheetParser<'a> {
    /// Byte ranges to remove (for `@page` blocks) or replace (for running decls).
    edits: &'a mut Vec<CssEdit>,
    margin_boxes: &'a mut Vec<MarginBoxRule>,
    running_mappings: &'a mut Vec<RunningMapping>,
    string_set_mappings: &'a mut Vec<StringSetMapping>,
    counter_mappings: &'a mut Vec<CounterMapping>,
    content_counter_mappings: &'a mut Vec<ContentCounterMapping>,
    page_settings: &'a mut Vec<PageSettingsRule>,
}

/// Describes a region in the original CSS to edit when building `cleaned_css`.
enum CssEdit {
    /// Remove the byte range entirely (used for `@page` blocks).
    Remove { start: usize, end: usize },
    /// Replace the byte range with the given text (used for `position: running(...)`).
    Replace {
        start: usize,
        end: usize,
        replacement: String,
    },
}

impl<'i, 'a> AtRuleParser<'i> for GcpmSheetParser<'a> {
    type Prelude = Option<String>; // page selector
    type AtRule = TopLevelItem;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, ()>> {
        if !name.eq_ignore_ascii_case("page") {
            return Err(input.new_error(BasicParseErrorKind::AtRuleInvalid(name)));
        }

        // Optional page selector like `:first`
        let page_selector = input
            .try_parse(|input| -> Result<String, ParseError<'i, ()>> {
                input.expect_colon()?;
                let ident = input.expect_ident()?.clone();
                Ok(format!(":{}", &*ident))
            })
            .ok();

        Ok(page_selector)
    }

    fn parse_block<'t>(
        &mut self,
        page_selector: Self::Prelude,
        start: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::AtRule, ParseError<'i, ()>> {
        let mut boxes = Vec::new();
        let mut size = None;
        let mut margin = None;
        parse_page_block(input, &page_selector, &mut boxes, &mut size, &mut margin);

        // Record the full @page rule span for removal.
        let start_offset = start.position().byte_index();
        let end_offset = input.position().byte_index();
        self.edits.push(CssEdit::Remove {
            start: start_offset,
            end: end_offset,
        });

        self.margin_boxes.extend(boxes);

        if size.is_some() || margin.is_some() {
            self.page_settings.push(PageSettingsRule {
                page_selector,
                size,
                margin,
            });
        }

        Ok(TopLevelItem::PageRule)
    }
}

struct QualifiedPrelude {
    selector: ParsedSelector,
    pseudo: Option<PseudoElement>,
}

impl<'i, 'a> QualifiedRuleParser<'i> for GcpmSheetParser<'a> {
    type Prelude = Option<QualifiedPrelude>;
    type QualifiedRule = TopLevelItem;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, ()>> {
        // Skip leading whitespace
        let first = loop {
            match input.next_including_whitespace()?.clone() {
                Token::WhiteSpace(_) => continue,
                tok => break tok,
            }
        };

        let selector = match first {
            Token::Delim('.') => {
                let name = input.expect_ident()?.clone();
                ParsedSelector::Class(name.to_string())
            }
            Token::IDHash(ref name) => ParsedSelector::Id(name.to_string()),
            Token::Ident(ref name) => ParsedSelector::Tag(name.to_string()),
            _ => {
                while input.next_including_whitespace().is_ok() {}
                return Ok(None);
            }
        };

        // Try to detect ::before / ::after pseudo-element
        let pseudo = input
            .try_parse(|input| {
                input.expect_colon()?;
                input.expect_colon()?;
                let ident = input.expect_ident()?.clone();
                if ident.eq_ignore_ascii_case("before") {
                    Ok(PseudoElement::Before)
                } else if ident.eq_ignore_ascii_case("after") {
                    Ok(PseudoElement::After)
                } else {
                    Err(input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid))
                }
            })
            .ok();

        // Reject compound/group selectors — only simple selectors are supported.
        // If any non-whitespace tokens remain, this is not a simple selector.
        while let Ok(tok) = input.next_including_whitespace() {
            match tok {
                Token::WhiteSpace(_) => {}
                _ => return Ok(None),
            }
        }
        Ok(Some(QualifiedPrelude { selector, pseudo }))
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::QualifiedRule, ParseError<'i, ()>> {
        // Only scan for GCPM declarations if the selector is supported.
        // Otherwise, skip the block to avoid replacing declarations with
        // `display: none` for elements that won't be registered as running.
        let Some(qp) = prelude else {
            while input.next().is_ok() {}
            return Ok(TopLevelItem::StyleRule);
        };

        let selector = qp.selector;
        let pseudo = qp.pseudo;

        let mut running_name: Option<String> = None;
        let mut string_set: Option<(String, Vec<StringSetValue>)> = None;
        let mut counter_ops: Vec<CounterOp> = Vec::new();
        let mut content_items: Option<Vec<ContentItem>> = None;

        let mut parser = StyleRuleParser {
            edits: self.edits,
            running_name: &mut running_name,
            string_set: &mut string_set,
            counter_ops: &mut counter_ops,
            content_items: &mut content_items,
            has_pseudo: pseudo.is_some(),
        };
        let iter = RuleBodyParser::new(input, &mut parser);
        for item in iter {
            let _ = item;
        }

        if let Some(running_name) = running_name {
            self.running_mappings.push(RunningMapping {
                parsed: selector.clone(),
                running_name,
            });
        }

        if let Some((name, values)) = string_set {
            self.string_set_mappings.push(StringSetMapping {
                parsed: selector.clone(),
                name,
                values,
            });
        }

        if !counter_ops.is_empty() {
            self.counter_mappings.push(CounterMapping {
                parsed: selector.clone(),
                ops: counter_ops,
            });
        }

        if let Some(items) = content_items {
            if let Some(pseudo) = pseudo {
                self.content_counter_mappings.push(ContentCounterMapping {
                    parsed: selector.clone(),
                    pseudo,
                    content: items,
                });
            }
        }

        Ok(TopLevelItem::StyleRule)
    }
}

// ---------------------------------------------------------------------------
// CSS length unit → points converter
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// @page size/margin value parsers
// ---------------------------------------------------------------------------

/// Convert a CSS dimension value+unit to PDF points.
fn css_unit_to_pt(value: f32, unit: &str) -> Option<f32> {
    let factor = match () {
        _ if unit.eq_ignore_ascii_case("mm") => 72.0 / 25.4,
        _ if unit.eq_ignore_ascii_case("cm") => 72.0 / 2.54,
        _ if unit.eq_ignore_ascii_case("in") => 72.0,
        _ if unit.eq_ignore_ascii_case("pt") => 1.0,
        _ if unit.eq_ignore_ascii_case("px") => 72.0 / 96.0,
        _ => return None,
    };
    Some(value * factor)
}

/// Parse the value of an `@page { size: ... }` declaration.
///
/// Returns `None` if the value is invalid or has trailing tokens
/// (CSS requires invalid declarations to be ignored entirely).
fn parse_page_size_value(input: &mut Parser<'_, '_>) -> Option<PageSizeDecl> {
    let token = input.next().ok()?.clone();
    let result = match token {
        Token::Ident(ref name) => {
            if name.eq_ignore_ascii_case("auto") {
                Some(PageSizeDecl::Auto)
            } else if name.eq_ignore_ascii_case("landscape") {
                Some(PageSizeDecl::KeywordWithOrientation(
                    "auto".to_string(),
                    true,
                ))
            } else if name.eq_ignore_ascii_case("portrait") {
                Some(PageSizeDecl::KeywordWithOrientation(
                    "auto".to_string(),
                    false,
                ))
            } else {
                let keyword = name.to_string();
                // Try to read a second ident for orientation
                let orientation = input.try_parse(|input| {
                    let tok = input.next()?.clone();
                    match tok {
                        Token::Ident(ref orient) => {
                            if orient.eq_ignore_ascii_case("landscape") {
                                Ok(true)
                            } else if orient.eq_ignore_ascii_case("portrait") {
                                Ok(false)
                            } else {
                                Err(input
                                    .new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid))
                            }
                        }
                        _ => Err(input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid)),
                    }
                });
                match orientation {
                    Ok(landscape) => Some(PageSizeDecl::KeywordWithOrientation(keyword, landscape)),
                    Err(_) => Some(PageSizeDecl::Keyword(keyword)),
                }
            }
        }
        Token::Dimension { value, unit, .. } => {
            let w = css_unit_to_pt(value, &unit).filter(|v| *v > 0.0)?;
            // Try to read a second dimension for height
            let h = input
                .try_parse(|input| {
                    let tok = input.next()?.clone();
                    match tok {
                        Token::Dimension { value, unit, .. } => css_unit_to_pt(value, &unit)
                            .filter(|v| *v > 0.0)
                            .ok_or_else(|| {
                                input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid)
                            }),
                        _ => Err(input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid)),
                    }
                })
                .unwrap_or(w);
            Some(PageSizeDecl::Custom(w, h))
        }
        _ => None,
    };
    // Reject if trailing tokens remain (CSS: invalid declaration → ignore entirely)
    if result.is_some() && input.next().is_ok() {
        return None;
    }
    result
}

/// Parse the value of an `@page { margin: ... }` declaration (CSS shorthand).
///
/// Returns `None` if the value is invalid or has trailing tokens
/// (CSS requires invalid declarations to be ignored entirely).
fn parse_page_margin_value(input: &mut Parser<'_, '_>) -> Option<Margin> {
    let mut values = Vec::new();
    loop {
        let result = input.try_parse(|input| {
            let tok = input.next()?.clone();
            match tok {
                Token::Dimension { value, unit, .. } => {
                    css_unit_to_pt(value, &unit).ok_or_else(|| {
                        input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid)
                    })
                }
                Token::Number { value: 0.0, .. } => Ok(0.0_f32),
                _ => Err(input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid)),
            }
        });
        match result {
            Ok(v) => values.push(v),
            Err(_) => break,
        }
        if values.len() >= 4 {
            break;
        }
    }

    // Reject if trailing tokens remain (CSS: invalid declaration → ignore entirely)
    if input.next().is_ok() {
        return None;
    }

    match values.len() {
        1 => Some(Margin {
            top: values[0],
            right: values[0],
            bottom: values[0],
            left: values[0],
        }),
        2 => Some(Margin {
            top: values[0],
            right: values[1],
            bottom: values[0],
            left: values[1],
        }),
        3 => Some(Margin {
            top: values[0],
            right: values[1],
            bottom: values[2],
            left: values[1],
        }),
        4 => Some(Margin {
            top: values[0],
            right: values[1],
            bottom: values[2],
            left: values[3],
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 2. @page block parser (PageRuleParser) — uses RuleBodyParser
// ---------------------------------------------------------------------------

fn parse_page_block(
    input: &mut Parser<'_, '_>,
    page_selector: &Option<String>,
    boxes: &mut Vec<MarginBoxRule>,
    size: &mut Option<PageSizeDecl>,
    margin: &mut Option<Margin>,
) {
    let mut parser = PageRuleParser {
        page_selector,
        boxes,
        size,
        margin,
    };
    let iter = RuleBodyParser::new(input, &mut parser);
    for item in iter {
        let _ = item;
    }
}

struct PageRuleParser<'a> {
    page_selector: &'a Option<String>,
    boxes: &'a mut Vec<MarginBoxRule>,
    size: &'a mut Option<PageSizeDecl>,
    margin: &'a mut Option<Margin>,
}

impl<'i, 'a> AtRuleParser<'i> for PageRuleParser<'a> {
    type Prelude = MarginBoxPosition;
    type AtRule = ();
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, ()>> {
        MarginBoxPosition::from_at_keyword(&name)
            .ok_or_else(|| input.new_error(BasicParseErrorKind::AtRuleInvalid(name)))
    }

    fn parse_block<'t>(
        &mut self,
        position: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::AtRule, ParseError<'i, ()>> {
        let mut content_items = Vec::new();
        let mut declarations = String::new();

        let mut parser = MarginBoxParser {
            content: &mut content_items,
            declarations: &mut declarations,
        };
        let iter = RuleBodyParser::new(input, &mut parser);
        for item in iter {
            let _ = item;
        }

        self.boxes.push(MarginBoxRule {
            page_selector: self.page_selector.clone(),
            position,
            content: content_items,
            declarations,
        });

        Ok(())
    }
}

impl<'i, 'a> DeclarationParser<'i> for PageRuleParser<'a> {
    type Declaration = ();
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _start: &cssparser::ParserState,
    ) -> Result<(), ParseError<'i, ()>> {
        if name.eq_ignore_ascii_case("size") {
            if let Some(v) = parse_page_size_value(input) {
                *self.size = Some(v);
            }
        } else if name.eq_ignore_ascii_case("margin") {
            if let Some(v) = parse_page_margin_value(input) {
                *self.margin = Some(v);
            }
        } else {
            // Skip unknown declarations
            while input.next().is_ok() {}
        }
        Ok(())
    }
}

impl<'i, 'a> QualifiedRuleParser<'i> for PageRuleParser<'a> {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = ();
}

impl<'i, 'a> RuleBodyItemParser<'i, (), ()> for PageRuleParser<'a> {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// 3. Margin box block parser (MarginBoxParser)
// ---------------------------------------------------------------------------

struct MarginBoxParser<'a> {
    content: &'a mut Vec<ContentItem>,
    declarations: &'a mut String,
}

impl<'i, 'a> DeclarationParser<'i> for MarginBoxParser<'a> {
    type Declaration = ();
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _start: &cssparser::ParserState,
    ) -> Result<(), ParseError<'i, ()>> {
        if name.eq_ignore_ascii_case("content") {
            *self.content = parse_content_value(input);
        } else {
            // Accumulate other declarations as raw text
            let start_pos = input.position();
            while input.next_including_whitespace().is_ok() {}
            let value_str = input.slice_from(start_pos).trim();
            if !self.declarations.is_empty() {
                self.declarations.push_str("; ");
            }
            self.declarations.push_str(&name);
            self.declarations.push_str(": ");
            self.declarations.push_str(value_str);
        }
        Ok(())
    }
}

impl<'i, 'a> AtRuleParser<'i> for MarginBoxParser<'a> {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
}

impl<'i, 'a> QualifiedRuleParser<'i> for MarginBoxParser<'a> {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = ();
}

impl<'i, 'a> RuleBodyItemParser<'i, (), ()> for MarginBoxParser<'a> {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// 4. Style rule block parser (StyleRuleParser)
// ---------------------------------------------------------------------------

struct StyleRuleParser<'a> {
    edits: &'a mut Vec<CssEdit>,
    running_name: &'a mut Option<String>,
    string_set: &'a mut Option<(String, Vec<StringSetValue>)>,
    counter_ops: &'a mut Vec<CounterOp>,
    content_items: &'a mut Option<Vec<ContentItem>>,
    has_pseudo: bool,
}

impl<'i, 'a> DeclarationParser<'i> for StyleRuleParser<'a> {
    type Declaration = ();
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        decl_start: &cssparser::ParserState,
    ) -> Result<(), ParseError<'i, ()>> {
        if name.eq_ignore_ascii_case("position") {
            // Try to parse `running(<name>)`
            let result = input.try_parse(|input| {
                let fn_name = input.expect_function()?.clone();
                if !fn_name.eq_ignore_ascii_case("running") {
                    return Err(input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid));
                }
                input.parse_nested_block(|input| {
                    let ident = input.expect_ident()?.clone();
                    Ok(ident.to_string())
                })
            });

            if let Ok(running_name) = result {
                *self.running_name = Some(running_name);

                let decl_start_byte = decl_start.position().byte_index();
                let end_byte = input.position().byte_index();
                self.edits.push(CssEdit::Replace {
                    start: decl_start_byte,
                    end: end_byte,
                    replacement: "display: none".to_string(),
                });
            } else {
                while input.next().is_ok() {}
            }
        } else if name.eq_ignore_ascii_case("string-set") {
            // Parse `string-set: <name> <value>+`
            if let Ok((set_name, values)) = parse_string_set_value(input) {
                *self.string_set = Some((set_name, values));

                // Replace with an empty string rather than Remove: the skip-`}`
                // logic in build_cleaned_css is only correct for @page block
                // removals. string-set lives inside a style rule, so eating a
                // trailing `}` would corrupt the rule's closing brace when the
                // declaration has no terminating semicolon.
                let decl_start_byte = decl_start.position().byte_index();
                let end_byte = input.position().byte_index();
                self.edits.push(CssEdit::Replace {
                    start: decl_start_byte,
                    end: end_byte,
                    replacement: String::new(),
                });
            } else {
                while input.next().is_ok() {}
            }
        } else if name.eq_ignore_ascii_case("counter-reset")
            || name.eq_ignore_ascii_case("counter-increment")
            || name.eq_ignore_ascii_case("counter-set")
        {
            let ops = parse_counter_ops(input, &name);
            // Last-declaration-wins: replace, don't extend
            *self.counter_ops = ops;
            let start = decl_start.position().byte_index();
            let end = input.position().byte_index();
            self.edits.push(CssEdit::Replace {
                start,
                end,
                replacement: String::new(),
            });
        } else if name.eq_ignore_ascii_case("content") && self.has_pseudo {
            let items = parse_content_value(input);
            let has_counter = items
                .iter()
                .any(|item| matches!(item, ContentItem::Counter { .. }));
            // Last-declaration-wins: always update (clear if no counter)
            if has_counter {
                *self.content_items = Some(items);
            } else {
                *self.content_items = None;
            }
            // Always strip `content` from cleaned CSS for ::before/::after rules
            // we recognise — counter()/string()/element() are resolved by us, not
            // by Blitz, so leaving the original declaration would cause Blitz to
            // try (and fail) to render the unsupported function.
            let start = decl_start.position().byte_index();
            let end = input.position().byte_index();
            self.edits.push(CssEdit::Replace {
                start,
                end,
                replacement: String::new(),
            });
        } else {
            // Skip other declarations
            while input.next().is_ok() {}
        }

        Ok(())
    }
}

impl<'i, 'a> AtRuleParser<'i> for StyleRuleParser<'a> {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
}

impl<'i, 'a> QualifiedRuleParser<'i> for StyleRuleParser<'a> {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = ();
}

impl<'i, 'a> RuleBodyItemParser<'i, (), ()> for StyleRuleParser<'a> {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// 5. string-set value parser
// ---------------------------------------------------------------------------

/// Parse the value of a `string-set` declaration: `<name> <value>+`.
fn parse_string_set_value<'i, 't>(
    input: &mut Parser<'i, 't>,
) -> Result<(String, Vec<StringSetValue>), ParseError<'i, ()>> {
    let name = input.expect_ident()?.clone().to_string();
    let mut values = Vec::new();

    loop {
        if input.is_exhausted() {
            break;
        }

        let result: Result<(), ParseError<'_, ()>> = input.try_parse(|input| {
            let token = input.next_including_whitespace()?.clone();
            match token {
                Token::QuotedString(ref s) => {
                    values.push(StringSetValue::Literal(s.to_string()));
                }
                Token::Function(ref fn_name) => {
                    let fn_name = fn_name.clone();
                    input.parse_nested_block(|input| {
                        if fn_name.eq_ignore_ascii_case("content") {
                            let arg = input.expect_ident()?.clone();
                            match &*arg {
                                "text" => values.push(StringSetValue::ContentText),
                                "before" => values.push(StringSetValue::ContentBefore),
                                "after" => values.push(StringSetValue::ContentAfter),
                                _ => {}
                            }
                        } else if fn_name.eq_ignore_ascii_case("attr") {
                            let arg = input.expect_ident()?.clone();
                            values.push(StringSetValue::Attr(arg.to_string()));
                        }
                        Ok(())
                    })?;
                }
                Token::WhiteSpace(_) | Token::Comment(_) => {}
                _ => {}
            }
            Ok(())
        });

        if result.is_err() {
            break;
        }
    }

    if values.is_empty() {
        return Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid));
    }

    Ok((name, values))
}

// ---------------------------------------------------------------------------
// 6. Content value parser
// ---------------------------------------------------------------------------

/// Parse a GCPM string/element policy identifier, handling cssparser's
/// tokenization of `first-except` which may arrive either as a single ident
/// or as `first` + `-` + `except`.
///
/// `map_fn` converts the canonical lowercase identifier (`"first"`, `"start"`,
/// `"last"`, `"first-except"`) into the caller's typed policy enum. Returning
/// `None` from `map_fn` signals an unknown identifier.
fn parse_policy_ident<'i, T>(
    input: &mut Parser<'i, '_>,
    map_fn: impl Fn(&str) -> Option<T>,
) -> Result<T, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    let canonical: String = if ident.eq_ignore_ascii_case("first") {
        // Try to consume a trailing `-except` (cssparser may split the hyphenated ident).
        let has_except = input
            .try_parse(|input| {
                input.expect_delim('-')?;
                let next = input.expect_ident()?.clone();
                if next.eq_ignore_ascii_case("except") {
                    Ok(())
                } else {
                    Err(input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid))
                }
            })
            .is_ok();
        if has_except {
            "first-except".to_string()
        } else {
            "first".to_string()
        }
    } else {
        ident.to_ascii_lowercase()
    };

    map_fn(&canonical).ok_or_else(|| input.new_error(BasicParseErrorKind::QualifiedRuleInvalid))
}

/// Parse the policy argument of `string(name, <policy>)`.
fn parse_string_policy<'i>(input: &mut Parser<'i, '_>) -> Result<StringPolicy, ParseError<'i, ()>> {
    parse_policy_ident(input, |s| match s {
        "first" => Some(StringPolicy::First),
        "start" => Some(StringPolicy::Start),
        "last" => Some(StringPolicy::Last),
        "first-except" => Some(StringPolicy::FirstExcept),
        _ => None,
    })
}

/// Parse the style argument of `counter(name, <style>)`.
fn parse_counter_style<'i>(input: &mut Parser<'i, '_>) -> Result<CounterStyle, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    // Handle hyphenated idents: cssparser may split "upper-roman" into
    // "upper" + "-" + "roman".
    let full = if input.try_parse(|input| input.expect_delim('-')).is_ok() {
        let suffix = input.expect_ident()?.clone();
        format!("{}-{}", &*ident, &*suffix)
    } else {
        ident.to_string()
    };
    match full.to_ascii_lowercase().as_str() {
        "decimal" => Ok(CounterStyle::Decimal),
        "upper-roman" => Ok(CounterStyle::UpperRoman),
        "lower-roman" => Ok(CounterStyle::LowerRoman),
        "upper-alpha" | "upper-latin" => Ok(CounterStyle::UpperAlpha),
        "lower-alpha" | "lower-latin" => Ok(CounterStyle::LowerAlpha),
        _ => Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
    }
}

/// Parse the policy argument of `element(name, <policy>)`.
fn parse_element_policy<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<ElementPolicy, ParseError<'i, ()>> {
    parse_policy_ident(input, |s| match s {
        "first" => Some(ElementPolicy::First),
        "start" => Some(ElementPolicy::Start),
        "last" => Some(ElementPolicy::Last),
        "first-except" => Some(ElementPolicy::FirstExcept),
        _ => None,
    })
}

/// Parse counter operations from `counter-reset`, `counter-increment`, or `counter-set`.
fn parse_counter_ops(input: &mut Parser<'_, '_>, prop: &str) -> Vec<CounterOp> {
    let mut ops = Vec::new();
    loop {
        let name = match input.try_parse(|input| input.expect_ident().map(|s| s.to_string())) {
            Ok(n) => n,
            Err(_) => break,
        };
        if name.eq_ignore_ascii_case("none") {
            break;
        }
        let value = input.try_parse(|input| input.expect_integer()).ok();
        let default_value = if prop.eq_ignore_ascii_case("counter-increment") {
            1
        } else {
            0
        };
        let value = value.unwrap_or(default_value);
        let op = if prop.eq_ignore_ascii_case("counter-reset") {
            CounterOp::Reset { name, value }
        } else if prop.eq_ignore_ascii_case("counter-increment") {
            CounterOp::Increment { name, value }
        } else {
            CounterOp::Set { name, value }
        };
        ops.push(op);
    }
    ops
}

/// Parse a `content` property value into a list of `ContentItem`s using cssparser.
/// Handles: `element(<name>, <policy>)`, `counter(page)`, `counter(pages)`, `string(<name>, <policy>)`, `"string"`.
fn parse_content_value(input: &mut Parser<'_, '_>) -> Vec<ContentItem> {
    let mut items = Vec::new();

    loop {
        if input.is_exhausted() {
            break;
        }

        let result: Result<(), ParseError<'_, ()>> = input.try_parse(|input| {
            let token = input.next_including_whitespace()?.clone();
            match token {
                Token::QuotedString(ref s) => {
                    items.push(ContentItem::String(s.to_string()));
                }
                Token::Function(ref name) => {
                    let fn_name = name.clone();
                    input.parse_nested_block(|input| {
                        let arg = input.expect_ident()?.clone();
                        if fn_name.eq_ignore_ascii_case("element") {
                            let name = arg.to_string();
                            // Asymmetric with `string(name, <policy>)` below:
                            // invalid policy drops the item entirely here,
                            // whereas `string()` falls back to `First` via
                            // `unwrap_or`. Running element references are
                            // stricter so a typo surfaces as a missing margin
                            // box rather than silently defaulting.
                            let had_comma = input.try_parse(|input| input.expect_comma()).is_ok();
                            if had_comma {
                                if let Ok(policy) = parse_element_policy(input) {
                                    items.push(ContentItem::Element { name, policy });
                                }
                            } else {
                                items.push(ContentItem::Element {
                                    name,
                                    policy: ElementPolicy::First,
                                });
                            }
                        } else if fn_name.eq_ignore_ascii_case("counter") {
                            let name = arg.to_string();
                            let style = input
                                .try_parse(|input| {
                                    input.expect_comma()?;
                                    parse_counter_style(input)
                                })
                                .unwrap_or(CounterStyle::Decimal);
                            items.push(ContentItem::Counter { name, style });
                        } else if fn_name.eq_ignore_ascii_case("string") {
                            let name = arg.to_string();
                            let policy = input
                                .try_parse(|input| {
                                    input.expect_comma()?;
                                    parse_string_policy(input)
                                })
                                .unwrap_or(StringPolicy::First);
                            items.push(ContentItem::StringRef { name, policy });
                        }
                        Ok(())
                    })?;
                }
                Token::WhiteSpace(_) | Token::Comment(_) => {
                    // skip
                }
                _ => {
                    // skip unknown tokens
                }
            }
            Ok(())
        });

        if result.is_err() {
            // If we can't parse the next token at all, break out
            break;
        }
    }

    items
}

// ---------------------------------------------------------------------------
// 6. Main entry point
// ---------------------------------------------------------------------------

/// Parse a CSS string, extracting GCPM constructs and returning a `GcpmContext`.
///
/// - `position: running(<name>)` is replaced with `display: none` in cleaned_css
/// - `@page { @<position> { content: ...; } }` blocks are extracted as margin box rules
/// - All other CSS is preserved verbatim in `cleaned_css`
pub fn parse_gcpm(css: &str) -> GcpmContext {
    let mut margin_boxes = Vec::new();
    let mut running_mappings = Vec::new();
    let mut string_set_mappings = Vec::new();
    let mut counter_mappings = Vec::new();
    let mut content_counter_mappings = Vec::new();
    let mut page_settings = Vec::new();
    let mut edits: Vec<CssEdit> = Vec::new();

    // Run the cssparser-based parse to collect GCPM data and edit spans.
    {
        let mut input = ParserInput::new(css);
        let mut input = Parser::new(&mut input);

        let mut parser = GcpmSheetParser {
            edits: &mut edits,
            margin_boxes: &mut margin_boxes,
            running_mappings: &mut running_mappings,
            string_set_mappings: &mut string_set_mappings,
            counter_mappings: &mut counter_mappings,
            content_counter_mappings: &mut content_counter_mappings,
            page_settings: &mut page_settings,
        };

        let iter = StyleSheetParser::new(&mut input, &mut parser);
        for item in iter {
            let _ = item;
        }
    }

    // Build cleaned_css by applying edits to the original CSS.
    let cleaned_css = build_cleaned_css(css, &mut edits);

    GcpmContext {
        margin_boxes,
        running_mappings,
        string_set_mappings,
        counter_mappings,
        content_counter_mappings,
        page_settings,
        cleaned_css,
    }
}

/// Build `cleaned_css` from the original CSS and a list of edits.
/// Edits must not overlap. They are sorted by start position.
fn build_cleaned_css(css: &str, edits: &mut [CssEdit]) -> String {
    if edits.is_empty() {
        return css.to_string();
    }

    // Sort by start position
    edits.sort_by_key(|e| match e {
        CssEdit::Remove { start, .. } => *start,
        CssEdit::Replace { start, .. } => *start,
    });

    let mut result = String::with_capacity(css.len());
    let mut cursor = 0;

    for edit in edits.iter() {
        let (start, end) = match edit {
            CssEdit::Remove { start, end } => (*start, *end),
            CssEdit::Replace { start, end, .. } => (*start, *end),
        };

        // Copy verbatim text before this edit
        if cursor < start {
            result.push_str(&css[cursor..start]);
        }

        // Apply the edit
        match edit {
            CssEdit::Remove { .. } => {
                // For @page removal, insert a newline separator if needed
                if !result.is_empty() && !result.ends_with('\n') && !result.ends_with(' ') {
                    result.push('\n');
                }
            }
            CssEdit::Replace { replacement, .. } => {
                result.push_str(replacement);
            }
        }

        cursor = end;

        // For Remove edits, cssparser's parse_block ends before the closing '}'.
        // Skip the '}' that the framework consumes after parse_block returns.
        if matches!(edit, CssEdit::Remove { .. })
            && cursor < css.len()
            && css.as_bytes()[cursor] == b'}'
        {
            cursor += 1;
        }
    }

    // Copy any remaining text after the last edit
    if cursor < css.len() {
        result.push_str(&css[cursor..]);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_css() {
        let css = "body { color: red; }\np { margin: 0; }";
        let ctx = parse_gcpm(css);
        assert!(ctx.running_mappings.is_empty());
        assert!(ctx.margin_boxes.is_empty());
        assert_eq!(ctx.cleaned_css, css);
    }

    #[test]
    fn test_extract_running_name() {
        let css = ".header { position: running(pageHeader); font-size: 12px; }";
        let ctx = parse_gcpm(css);
        assert!(
            ctx.running_mappings
                .iter()
                .any(|m| m.running_name == "pageHeader")
        );
        assert!(ctx.cleaned_css.contains("display: none"));
        assert!(!ctx.cleaned_css.contains("running"));
        assert!(ctx.cleaned_css.contains("font-size: 12px"));
    }

    #[test]
    fn test_extract_margin_box() {
        let css = "@page { @top-center { content: element(pageHeader); } }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        let mb = &ctx.margin_boxes[0];
        assert_eq!(mb.position, MarginBoxPosition::TopCenter);
        assert_eq!(mb.page_selector, None);
        assert_eq!(
            mb.content,
            vec![ContentItem::Element {
                name: "pageHeader".to_string(),
                policy: ElementPolicy::First,
            }]
        );
        // @page block should be removed from cleaned_css
        assert!(!ctx.cleaned_css.contains("@page"));
    }

    #[test]
    fn test_extract_counter() {
        let css =
            r#"@page { @bottom-center { content: "Page " counter(page) " of " counter(pages); } }"#;
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        let mb = &ctx.margin_boxes[0];
        assert_eq!(mb.position, MarginBoxPosition::BottomCenter);
        assert_eq!(
            mb.content,
            vec![
                ContentItem::String("Page ".to_string()),
                ContentItem::Counter {
                    name: "page".into(),
                    style: CounterStyle::Decimal,
                },
                ContentItem::String(" of ".to_string()),
                ContentItem::Counter {
                    name: "pages".into(),
                    style: CounterStyle::Decimal,
                },
            ]
        );
    }

    #[test]
    fn test_mixed_css_preserves_non_gcpm() {
        let css = "body { color: red; }\n@page { @top-center { content: element(hdr); } }\np { margin: 0; }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        assert!(ctx.cleaned_css.contains("body { color: red; }"));
        assert!(ctx.cleaned_css.contains("p { margin: 0; }"));
        assert!(!ctx.cleaned_css.contains("@page"));
        // Verify no stray closing brace from @page removal
        let without_rules = ctx
            .cleaned_css
            .replace("body { color: red; }", "")
            .replace("p { margin: 0; }", "");
        assert!(
            !without_rules.contains('}'),
            "stray brace in cleaned_css: {:?}",
            ctx.cleaned_css
        );
    }

    #[test]
    fn test_page_selector() {
        let css = "@page :first { @top-center { content: element(firstHeader); } }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        let mb = &ctx.margin_boxes[0];
        assert_eq!(mb.page_selector, Some(":first".to_string()));
        assert_eq!(mb.position, MarginBoxPosition::TopCenter);
        assert_eq!(
            mb.content,
            vec![ContentItem::Element {
                name: "firstHeader".to_string(),
                policy: ElementPolicy::First,
            }]
        );
    }

    #[test]
    fn test_ignores_gcpm_in_comments() {
        let css = "/* @page { @top-center { content: element(x); } } */ body { color: red; }";
        let ctx = parse_gcpm(css);
        assert!(ctx.margin_boxes.is_empty());
        assert!(ctx.cleaned_css.contains("body { color: red; }"));
    }

    #[test]
    fn test_ignores_gcpm_in_string_literals() {
        let css = r#"body { content: "position: running(x)"; color: blue; }"#;
        let ctx = parse_gcpm(css);
        assert!(ctx.running_mappings.is_empty());
    }

    #[test]
    fn test_running_name_case_insensitive_property() {
        // POSITION: Running(name) — プロパティ名の大文字小文字
        let css = ".header { POSITION: running(pageHeader); }";
        let ctx = parse_gcpm(css);
        assert!(
            ctx.running_mappings
                .iter()
                .any(|m| m.running_name == "pageHeader")
        );
        assert!(ctx.cleaned_css.contains("display: none"));
    }

    #[test]
    fn test_multiple_running_names() {
        let css = ".h { position: running(hdr); } .f { position: running(ftr); }";
        let ctx = parse_gcpm(css);
        assert!(ctx.running_mappings.iter().any(|m| m.running_name == "hdr"));
        assert!(ctx.running_mappings.iter().any(|m| m.running_name == "ftr"));
    }

    #[test]
    fn test_running_with_other_declarations() {
        // running() 以外の宣言が cleaned_css に残ること
        let css = ".header { color: red; position: running(hdr); font-size: 14px; }";
        let ctx = parse_gcpm(css);
        assert!(ctx.running_mappings.iter().any(|m| m.running_name == "hdr"));
        assert!(ctx.cleaned_css.contains("color: red"));
        assert!(ctx.cleaned_css.contains("font-size: 14px"));
    }

    #[test]
    fn test_page_with_multiple_margin_boxes() {
        let css = "@page { @top-left { content: \"Left\"; } @top-center { content: element(hdr); } @top-right { content: counter(page); } }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 3);
    }

    #[test]
    fn test_margin_box_with_extra_declarations() {
        let css = "@page { @top-center { content: element(hdr); font-size: 10pt; color: gray; } }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        let mb = &ctx.margin_boxes[0];
        assert_eq!(
            mb.content,
            vec![ContentItem::Element {
                name: "hdr".to_string(),
                policy: ElementPolicy::First,
            }]
        );
        assert!(mb.declarations.contains("font-size"));
        assert!(mb.declarations.contains("color"));
    }

    #[test]
    fn test_page_left_right_selectors() {
        let css = r#"
        @page :left { @bottom-left { content: counter(page); } }
        @page :right { @bottom-right { content: counter(page); } }
    "#;
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 2);
        assert_eq!(ctx.margin_boxes[0].page_selector, Some(":left".to_string()));
        assert_eq!(
            ctx.margin_boxes[1].page_selector,
            Some(":right".to_string())
        );
    }

    #[test]
    fn test_class_selector_extraction() {
        let css = ".my-header { position: running(pageHeader); }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.running_mappings.len(), 1);
        assert_eq!(
            ctx.running_mappings[0].parsed,
            ParsedSelector::Class("my-header".to_string())
        );
        assert_eq!(ctx.running_mappings[0].running_name, "pageHeader");
    }

    #[test]
    fn test_id_selector_extraction() {
        let css = "#main-title { position: running(docTitle); }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.running_mappings.len(), 1);
        assert_eq!(
            ctx.running_mappings[0].parsed,
            ParsedSelector::Id("main-title".to_string())
        );
        assert_eq!(ctx.running_mappings[0].running_name, "docTitle");
    }

    #[test]
    fn test_tag_selector_extraction() {
        let css = "header { position: running(pageHeader); }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.running_mappings.len(), 1);
        assert_eq!(
            ctx.running_mappings[0].parsed,
            ParsedSelector::Tag("header".to_string())
        );
        assert_eq!(ctx.running_mappings[0].running_name, "pageHeader");
    }

    #[test]
    fn test_compound_selector_not_matched() {
        // Compound selectors like `.a .b` should not create a mapping
        let css = ".a .b { position: running(hdr); }";
        let ctx = parse_gcpm(css);
        assert!(ctx.running_mappings.is_empty());
    }

    #[test]
    fn test_group_selector_not_matched() {
        // Group selectors like `.a, .b` should not create a mapping
        let css = ".a, .b { position: running(hdr); }";
        let ctx = parse_gcpm(css);
        assert!(ctx.running_mappings.is_empty());
    }

    #[test]
    fn test_parse_string_set_content_text() {
        let css = "h1 { string-set: chapter-title content(text); }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.string_set_mappings.len(), 1);
        let m = &ctx.string_set_mappings[0];
        assert_eq!(m.parsed, ParsedSelector::Tag("h1".to_string()));
        assert_eq!(m.name, "chapter-title");
        assert_eq!(m.values, vec![StringSetValue::ContentText]);
        assert!(!ctx.cleaned_css.contains("string-set"));
    }

    #[test]
    fn test_parse_string_set_multiple_values() {
        let css = r#"h1 { string-set: title "Chapter " content(text) " - " attr(data-sub); }"#;
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.string_set_mappings.len(), 1);
        let m = &ctx.string_set_mappings[0];
        assert_eq!(m.name, "title");
        assert_eq!(
            m.values,
            vec![
                StringSetValue::Literal("Chapter ".to_string()),
                StringSetValue::ContentText,
                StringSetValue::Literal(" - ".to_string()),
                StringSetValue::Attr("data-sub".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_string_set_content_before_after() {
        let css = "h2 { string-set: sec content(before) content(text) content(after); }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.string_set_mappings.len(), 1);
        assert_eq!(
            ctx.string_set_mappings[0].values,
            vec![
                StringSetValue::ContentBefore,
                StringSetValue::ContentText,
                StringSetValue::ContentAfter,
            ]
        );
    }

    #[test]
    fn test_parse_string_function_default_policy() {
        let css = r#"@page { @top-center { content: string(chapter-title); } }"#;
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        assert_eq!(
            ctx.margin_boxes[0].content,
            vec![ContentItem::StringRef {
                name: "chapter-title".to_string(),
                policy: StringPolicy::First,
            }]
        );
    }

    #[test]
    fn test_parse_string_function_with_policy() {
        let css = r#"@page { @top-center { content: string(title, last); } }"#;
        let ctx = parse_gcpm(css);
        assert_eq!(
            ctx.margin_boxes[0].content,
            vec![ContentItem::StringRef {
                name: "title".to_string(),
                policy: StringPolicy::Last,
            }]
        );
    }

    #[test]
    fn test_parse_string_function_all_policies() {
        for (policy_str, policy) in [
            ("first", StringPolicy::First),
            ("start", StringPolicy::Start),
            ("last", StringPolicy::Last),
            ("first-except", StringPolicy::FirstExcept),
        ] {
            let css = format!(
                r#"@page {{ @top-center {{ content: string(title, {}); }} }}"#,
                policy_str
            );
            let ctx = parse_gcpm(&css);
            assert_eq!(
                ctx.margin_boxes[0].content,
                vec![ContentItem::StringRef {
                    name: "title".to_string(),
                    policy,
                }],
                "Failed for policy: {}",
                policy_str
            );
        }
    }

    #[test]
    fn test_parse_element_function_default_policy() {
        let css = "@page { @top-center { content: element(hdr); } }";
        let ctx = parse_gcpm(css);
        let rule = ctx.margin_boxes.first().unwrap();
        assert_eq!(
            rule.content,
            vec![ContentItem::Element {
                name: "hdr".into(),
                policy: ElementPolicy::First,
            }]
        );
    }

    #[test]
    fn test_parse_element_function_all_policies() {
        for (policy_str, policy) in [
            ("first", ElementPolicy::First),
            ("start", ElementPolicy::Start),
            ("last", ElementPolicy::Last),
            ("first-except", ElementPolicy::FirstExcept),
        ] {
            let css = format!(
                "@page {{ @top-center {{ content: element(hdr, {}); }} }}",
                policy_str
            );
            let ctx = parse_gcpm(&css);
            let rule = ctx.margin_boxes.first().unwrap();
            assert_eq!(
                rule.content,
                vec![ContentItem::Element {
                    name: "hdr".into(),
                    policy,
                }],
                "Failed for policy: {}",
                policy_str
            );
        }
    }

    #[test]
    fn test_parse_element_function_invalid_policy() {
        // Unknown policy identifier — the whole element() call should be dropped.
        let css = "@page { @top-center { content: element(hdr, bogus); } }";
        let ctx = parse_gcpm(css);
        let rule = ctx.margin_boxes.first().unwrap();
        assert!(rule.content.is_empty());
    }

    #[test]
    fn test_parse_string_set_with_class_selector() {
        let css = ".chapter-heading { string-set: chapter content(text); }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.string_set_mappings.len(), 1);
        assert_eq!(
            ctx.string_set_mappings[0].parsed,
            ParsedSelector::Class("chapter-heading".to_string())
        );
    }

    /// Regression: when `string-set` is the last declaration in a rule and has
    /// no trailing semicolon, the cleaned CSS must still contain the rule's
    /// closing brace. Previously the CssEdit::Remove skip-`}` logic (written
    /// for @page blocks) would eat the style rule's closing brace.
    #[test]
    fn test_string_set_last_declaration_without_semicolon() {
        let css = "h1 { color: red; string-set: title content(text) }\np { margin: 0; }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.string_set_mappings.len(), 1);
        assert!(
            ctx.cleaned_css.contains("color: red"),
            "color: red should remain in cleaned_css: {:?}",
            ctx.cleaned_css
        );
        assert!(
            ctx.cleaned_css.contains("p { margin: 0; }"),
            "following rule must be intact — the h1 closing brace was not eaten: {:?}",
            ctx.cleaned_css
        );
        assert!(
            !ctx.cleaned_css.contains("string-set"),
            "string-set declaration should be removed: {:?}",
            ctx.cleaned_css
        );
    }

    #[test]
    fn test_parse_custom_counter() {
        let css = r#"@page { @bottom-center { content: "Ch. " counter(chapter); } }"#;
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        assert_eq!(
            ctx.margin_boxes[0].content,
            vec![
                ContentItem::String("Ch. ".into()),
                ContentItem::Counter {
                    name: "chapter".into(),
                    style: CounterStyle::Decimal,
                },
            ]
        );
    }

    #[test]
    fn test_parse_counter_with_style() {
        let css = r#"@page { @bottom-center { content: counter(chapter, upper-roman); } }"#;
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        assert_eq!(
            ctx.margin_boxes[0].content,
            vec![ContentItem::Counter {
                name: "chapter".into(),
                style: CounterStyle::UpperRoman,
            }]
        );
    }

    #[test]
    fn test_parse_counter_lower_alpha_style() {
        let css = r#"@page { @bottom-center { content: counter(section, lower-alpha); } }"#;
        let ctx = parse_gcpm(css);
        assert_eq!(
            ctx.margin_boxes[0].content,
            vec![ContentItem::Counter {
                name: "section".into(),
                style: CounterStyle::LowerAlpha,
            }]
        );
    }

    #[test]
    fn test_parse_counter_reset() {
        let css = "body { counter-reset: chapter 0; }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.counter_mappings.len(), 1);
        assert_eq!(
            ctx.counter_mappings[0].parsed,
            ParsedSelector::Tag("body".into())
        );
        assert_eq!(
            ctx.counter_mappings[0].ops,
            vec![CounterOp::Reset {
                name: "chapter".into(),
                value: 0,
            }]
        );
    }

    #[test]
    fn test_parse_counter_increment() {
        let css = "h2 { counter-increment: chapter; }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.counter_mappings.len(), 1);
        assert_eq!(
            ctx.counter_mappings[0].ops,
            vec![CounterOp::Increment {
                name: "chapter".into(),
                value: 1,
            }]
        );
    }

    #[test]
    fn test_parse_counter_set() {
        let css = "h2 { counter-set: chapter 5; }";
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.counter_mappings.len(), 1);
        assert_eq!(
            ctx.counter_mappings[0].ops,
            vec![CounterOp::Set {
                name: "chapter".into(),
                value: 5,
            }]
        );
    }

    #[test]
    fn test_parse_counter_multiple_names() {
        let css = "h2 { counter-reset: chapter 0 section 0; }";
        let ctx = parse_gcpm(css);
        assert_eq!(
            ctx.counter_mappings[0].ops,
            vec![
                CounterOp::Reset {
                    name: "chapter".into(),
                    value: 0,
                },
                CounterOp::Reset {
                    name: "section".into(),
                    value: 0,
                },
            ]
        );
    }

    #[test]
    fn test_parse_counter_increment_default_value() {
        let css = "h2 { counter-increment: section; }";
        let ctx = parse_gcpm(css);
        assert_eq!(
            ctx.counter_mappings[0].ops,
            vec![CounterOp::Increment {
                name: "section".into(),
                value: 1,
            }]
        );
    }

    #[test]
    fn test_parse_pseudo_element_content_with_counter() {
        let css = r#"h2::before { content: counter(chapter) ". "; }"#;
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.content_counter_mappings.len(), 1);
        let m = &ctx.content_counter_mappings[0];
        assert_eq!(m.parsed, ParsedSelector::Tag("h2".into()));
        assert_eq!(m.pseudo, PseudoElement::Before);
        assert_eq!(
            m.content,
            vec![
                ContentItem::Counter {
                    name: "chapter".into(),
                    style: CounterStyle::Decimal,
                },
                ContentItem::String(". ".into()),
            ]
        );
    }

    #[test]
    fn test_parse_counter_declarations_stripped_from_cleaned_css() {
        let css = "body { counter-reset: chapter; color: red; }";
        let ctx = parse_gcpm(css);
        assert!(!ctx.cleaned_css.contains("counter-reset"));
        assert!(ctx.cleaned_css.contains("color: red"));
    }

    #[test]
    fn test_parse_pseudo_content_stripped_from_cleaned_css() {
        let css = r#"h2::before { content: counter(chapter) ". "; font-weight: bold; }"#;
        let ctx = parse_gcpm(css);
        assert!(!ctx.cleaned_css.contains("counter(chapter)"));
        assert!(ctx.cleaned_css.contains("font-weight: bold"));
    }
}
