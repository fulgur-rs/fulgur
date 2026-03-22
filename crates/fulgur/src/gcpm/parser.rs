use cssparser::{
    AtRuleParser, BasicParseErrorKind, CowRcStr, DeclarationParser, ParseError, Parser,
    ParserInput, QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser, Token,
};

use super::margin_box::MarginBoxPosition;
use super::{ContentItem, CounterType, GcpmContext, MarginBoxRule, ParsedSelector, RunningMapping};

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
        parse_page_block(input, &page_selector, &mut boxes);

        // Record the full @page rule span for removal.
        let start_offset = start.position().byte_index();
        let end_offset = input.position().byte_index();
        self.edits.push(CssEdit::Remove {
            start: start_offset,
            end: end_offset,
        });

        self.margin_boxes.extend(boxes);
        Ok(TopLevelItem::PageRule)
    }
}

impl<'i, 'a> QualifiedRuleParser<'i> for GcpmSheetParser<'a> {
    type Prelude = Option<ParsedSelector>;
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
        // Reject compound/group selectors — only simple selectors are supported.
        // If any non-whitespace tokens remain, this is not a simple selector.
        while let Ok(tok) = input.next_including_whitespace() {
            match tok {
                Token::WhiteSpace(_) => {}
                _ => return Ok(None),
            }
        }
        Ok(Some(selector))
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::QualifiedRule, ParseError<'i, ()>> {
        // Only scan for `position: running(...)` if the selector is supported.
        // Otherwise, skip the block to avoid replacing declarations with
        // `display: none` for elements that won't be registered as running.
        let Some(selector) = prelude else {
            while input.next().is_ok() {}
            return Ok(TopLevelItem::StyleRule);
        };

        let mut running_name: Option<String> = None;

        let mut parser = StyleRuleParser {
            edits: self.edits,
            running_name: &mut running_name,
        };
        let iter = RuleBodyParser::new(input, &mut parser);
        for item in iter {
            let _ = item;
        }

        if let Some(running_name) = running_name {
            self.running_mappings.push(RunningMapping {
                parsed: selector,
                running_name,
            });
        }

        Ok(TopLevelItem::StyleRule)
    }
}

// ---------------------------------------------------------------------------
// 2. @page block parser (PageRuleParser) — uses RuleBodyParser
// ---------------------------------------------------------------------------

fn parse_page_block(
    input: &mut Parser<'_, '_>,
    page_selector: &Option<String>,
    boxes: &mut Vec<MarginBoxRule>,
) {
    let mut parser = PageRuleParser {
        page_selector,
        boxes,
    };
    let iter = RuleBodyParser::new(input, &mut parser);
    for item in iter {
        let _ = item;
    }
}

struct PageRuleParser<'a> {
    page_selector: &'a Option<String>,
    boxes: &'a mut Vec<MarginBoxRule>,
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
        // Skip declarations directly inside @page (not inside margin boxes)
        let _ = name;
        while input.next().is_ok() {}
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
        if !name.eq_ignore_ascii_case("position") {
            // Skip non-position declarations
            while input.next().is_ok() {}
            return Ok(());
        }

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

            // Record the edit: replace `position: running(...)` with `display: none`
            // The decl_start points to just before the property name (the ident token).
            let decl_start_byte = decl_start.position().byte_index();
            let end_byte = input.position().byte_index();
            self.edits.push(CssEdit::Replace {
                start: decl_start_byte,
                end: end_byte,
                replacement: "display: none".to_string(),
            });
        } else {
            // Not running(...), skip the rest
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
// 5. Content value parser
// ---------------------------------------------------------------------------

/// Parse a `content` property value into a list of `ContentItem`s using cssparser.
/// Handles: `element(<name>)`, `counter(page)`, `counter(pages)`, `"string"`.
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
                            items.push(ContentItem::Element(arg.to_string()));
                        } else if fn_name.eq_ignore_ascii_case("counter") {
                            match &*arg {
                                "page" => items.push(ContentItem::Counter(CounterType::Page)),
                                "pages" => items.push(ContentItem::Counter(CounterType::Pages)),
                                _ => {} // unknown counter
                            }
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
    let mut edits: Vec<CssEdit> = Vec::new();

    // Run the cssparser-based parse to collect GCPM data and edit spans.
    {
        let mut input = ParserInput::new(css);
        let mut input = Parser::new(&mut input);

        let mut parser = GcpmSheetParser {
            edits: &mut edits,
            margin_boxes: &mut margin_boxes,
            running_mappings: &mut running_mappings,
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
            vec![ContentItem::Element("pageHeader".to_string())]
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
                ContentItem::Counter(CounterType::Page),
                ContentItem::String(" of ".to_string()),
                ContentItem::Counter(CounterType::Pages),
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
            vec![ContentItem::Element("firstHeader".to_string())]
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
        assert_eq!(mb.content, vec![ContentItem::Element("hdr".to_string())]);
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
}
