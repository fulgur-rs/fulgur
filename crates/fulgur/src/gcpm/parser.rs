use std::collections::HashSet;

use super::margin_box::MarginBoxPosition;
use super::{ContentItem, CounterType, GcpmContext, MarginBoxRule};

/// Parse a CSS string, extracting GCPM constructs and returning a `GcpmContext`.
///
/// - `position: running(<name>)` is replaced with `display: none` in cleaned_css
/// - `@page { @<position> { content: ...; } }` blocks are extracted as margin box rules
/// - All other CSS is preserved verbatim in `cleaned_css`
pub fn parse_gcpm(css: &str) -> GcpmContext {
    let mut running_names = HashSet::new();
    let mut margin_boxes = Vec::new();
    let mut cleaned_css = String::with_capacity(css.len());

    let len = css.len();
    let mut i = 0;

    while i < len {
        let remaining = &css[i..];
        let ch = remaining.chars().next().unwrap();
        let ch_len = ch.len_utf8();

        // Skip CSS comments /* ... */
        if remaining.starts_with("/*") {
            if let Some(end) = css[i + 2..].find("*/") {
                cleaned_css.push_str(&css[i..i + 2 + end + 2]);
                i += 2 + end + 2;
            } else {
                // Unterminated comment — copy the rest
                cleaned_css.push_str(remaining);
                break;
            }
            continue;
        }

        // Skip string literals "..." and '...'
        if ch == '"' || ch == '\'' {
            let quote = ch;
            let mut j = i + 1;
            while j < len {
                let c = css[j..].chars().next().unwrap();
                if c == '\\' {
                    j += c.len_utf8();
                    if j < len {
                        j += css[j..].chars().next().unwrap().len_utf8();
                    }
                } else if c == quote {
                    j += 1;
                    break;
                } else {
                    j += c.len_utf8();
                }
            }
            cleaned_css.push_str(&css[i..j]);
            i = j;
            continue;
        }

        // Check for @page rule
        if ch == '@' {
            if let Some(rest) = css.get(i + 1..) {
                if rest.starts_with("page") {
                    let after_page = i + 5; // after "@page"
                    let after_page_ch = css[after_page..].chars().next();
                    // Make sure it's not @page-something (must be followed by whitespace, {, or :)
                    if after_page >= len
                        || matches!(after_page_ch, Some(c) if c.is_ascii_whitespace() || c == '{' || c == ':')
                    {
                        if let Some((consumed, page_sel, boxes)) = parse_page_rule(&css[i..]) {
                            margin_boxes.extend(boxes);
                            // Skip the entire @page block (don't add to cleaned_css)
                            // but preserve a newline to avoid merging adjacent rules
                            if !cleaned_css.is_empty()
                                && !cleaned_css.ends_with('\n')
                                && !cleaned_css.ends_with(' ')
                            {
                                cleaned_css.push('\n');
                            }
                            let _ = page_sel; // used inside parse_page_rule
                            i += consumed;
                            continue;
                        }
                    }
                }
            }
        }

        // Check for position: running(...)
        if ch == 'p' || ch == 'P' {
            if let Some(result) = try_parse_running(&css[i..]) {
                let (name, replacement, consumed) = result;
                running_names.insert(name);
                cleaned_css.push_str(&replacement);
                i += consumed;
                continue;
            }
        }

        cleaned_css.push_str(&css[i..i + ch_len]);
        i += ch_len;
    }

    GcpmContext {
        margin_boxes,
        running_names,
        cleaned_css,
    }
}

/// Try to parse `position\s*:\s*running\s*(\s*<name>\s*)` at the current position.
/// Returns (name, replacement_text, chars_consumed) or None.
fn try_parse_running(s: &str) -> Option<(String, String, usize)> {
    // Match "position" case-insensitively
    let lower = s.get(..8)?;
    if !lower.eq_ignore_ascii_case("position") {
        return None;
    }

    let rest = &s[8..];
    let mut idx = 0;

    // Skip whitespace
    while idx < rest.len() && rest.as_bytes()[idx].is_ascii_whitespace() {
        idx += 1;
    }

    // Expect ':'
    if idx >= rest.len() || rest.as_bytes()[idx] != b':' {
        return None;
    }
    idx += 1;

    // Skip whitespace
    while idx < rest.len() && rest.as_bytes()[idx].is_ascii_whitespace() {
        idx += 1;
    }

    // Expect "running"
    let after_colon = &rest[idx..];
    if !after_colon.starts_with("running") {
        return None;
    }
    idx += 7;

    // Skip whitespace
    while idx < rest.len() && rest.as_bytes()[idx].is_ascii_whitespace() {
        idx += 1;
    }

    // Expect '('
    if idx >= rest.len() || rest.as_bytes()[idx] != b'(' {
        return None;
    }
    idx += 1;

    // Skip whitespace
    while idx < rest.len() && rest.as_bytes()[idx].is_ascii_whitespace() {
        idx += 1;
    }

    // Read name (alphanumeric, hyphen, underscore)
    let name_start = idx;
    while idx < rest.len() {
        let c = rest.as_bytes()[idx];
        if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' {
            idx += 1;
        } else {
            break;
        }
    }
    let name = rest[name_start..idx].to_string();
    if name.is_empty() {
        return None;
    }

    // Skip whitespace
    while idx < rest.len() && rest.as_bytes()[idx].is_ascii_whitespace() {
        idx += 1;
    }

    // Expect ')'
    if idx >= rest.len() || rest.as_bytes()[idx] != b')' {
        return None;
    }
    idx += 1;

    let total_consumed = 8 + idx; // "position" + rest consumed
    Some((name, "display: none".to_string(), total_consumed))
}

/// Parse an `@page` rule starting at `s`.
/// Returns (chars_consumed, page_selector, Vec<MarginBoxRule>) or None.
fn parse_page_rule(s: &str) -> Option<(usize, Option<String>, Vec<MarginBoxRule>)> {
    // s starts with "@page"
    let mut idx = 5; // skip "@page"

    // Skip whitespace
    while idx < s.len() && s.as_bytes()[idx].is_ascii_whitespace() {
        idx += 1;
    }

    // Check for optional page selector (e.g. ":first", ":left", ":right")
    let mut page_selector = None;
    if idx < s.len() && s.as_bytes()[idx] == b':' {
        let sel_start = idx;
        idx += 1;
        // Read selector name
        while idx < s.len() && s.as_bytes()[idx].is_ascii_alphanumeric() {
            idx += 1;
        }
        page_selector = Some(s[sel_start..idx].to_string());

        // Skip whitespace after selector
        while idx < s.len() && s.as_bytes()[idx].is_ascii_whitespace() {
            idx += 1;
        }
    }

    // Expect '{'
    if idx >= s.len() || s.as_bytes()[idx] != b'{' {
        return None;
    }

    // Find matching brace
    let (inner, end_pos) = find_matching_brace(&s[idx..])?;
    let total_consumed = idx + end_pos + 1; // +1 for closing '}'

    let mut boxes = Vec::new();
    parse_margin_boxes(&inner, &page_selector, &mut boxes);

    Some((total_consumed, page_selector, boxes))
}

/// Given a string starting with '{', find the matching '}' and return
/// (inner_content_without_braces, position_of_closing_brace_relative_to_input).
fn find_matching_brace(s: &str) -> Option<(String, usize)> {
    if s.as_bytes().first()? != &b'{' {
        return None;
    }

    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((s[1..i].to_string(), i));
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse the inner content of an `@page { ... }` block for margin box at-rules.
fn parse_margin_boxes(
    content: &str,
    page_selector: &Option<String>,
    boxes: &mut Vec<MarginBoxRule>,
) {
    let bytes = content.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Look for @<keyword>
        if bytes[i] == b'@' {
            i += 1;
            // Skip whitespace
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Read keyword
            let kw_start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
                i += 1;
            }
            let keyword = &content[kw_start..i];

            // Skip whitespace
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }

            // Expect '{'
            if i < bytes.len() && bytes[i] == b'{' {
                if let Some(position) = MarginBoxPosition::from_at_keyword(keyword) {
                    if let Some((inner, end_pos)) = find_matching_brace(&content[i..]) {
                        let (content_items, declarations) = parse_content_property(&inner);
                        boxes.push(MarginBoxRule {
                            page_selector: page_selector.clone(),
                            position,
                            content: content_items,
                            declarations,
                        });
                        i += end_pos + 1;
                        continue;
                    }
                }
                // Unknown at-rule or parse failure — skip the block
                if let Some((_, end_pos)) = find_matching_brace(&content[i..]) {
                    i += end_pos + 1;
                    continue;
                }
            }
        }

        i += 1;
    }
}

/// Parse the declarations block of a margin box, extracting the `content` property
/// and returning (content_items, remaining_declarations).
fn parse_content_property(block: &str) -> (Vec<ContentItem>, String) {
    let mut content_items = Vec::new();
    let mut declarations = String::new();

    // Split by semicolons (simple approach — works for non-nested values)
    for decl in block.split(';') {
        let trimmed = decl.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Split on first ':'
        if let Some(colon_pos) = trimmed.find(':') {
            let prop = trimmed[..colon_pos].trim();
            let value = trimmed[colon_pos + 1..].trim();

            if prop.eq_ignore_ascii_case("content") {
                content_items = parse_content_value(value);
            } else {
                if !declarations.is_empty() {
                    declarations.push_str("; ");
                }
                declarations.push_str(trimmed);
            }
        }
    }

    (content_items, declarations)
}

/// Parse a `content` property value into a list of ContentItems.
/// Handles: `element(<name>)`, `counter(page)`, `counter(pages)`, `"string"`.
fn parse_content_value(value: &str) -> Vec<ContentItem> {
    let mut items = Vec::new();
    let bytes = value.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Skip whitespace
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // String literal
        if bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1; // skip escaped char
                }
                i += 1;
            }
            items.push(ContentItem::String(value[start..i].to_string()));
            if i < bytes.len() {
                i += 1; // skip closing quote
            }
            continue;
        }

        // Single-quoted string
        if bytes[i] == b'\'' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != b'\'' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            items.push(ContentItem::String(value[start..i].to_string()));
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }

        // Function-like: element(...) or counter(...)
        if bytes[i].is_ascii_alphabetic() {
            let fn_start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
            {
                i += 1;
            }
            let fn_name = &value[fn_start..i];

            // Skip whitespace
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }

            if i < bytes.len() && bytes[i] == b'(' {
                i += 1;
                // Skip whitespace
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                let arg_start = i;
                while i < bytes.len() && bytes[i] != b')' && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                let arg = value[arg_start..i].trim();
                // Skip to closing paren
                while i < bytes.len() && bytes[i] != b')' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1; // skip ')'
                }

                match fn_name {
                    "element" => {
                        items.push(ContentItem::Element(arg.to_string()));
                    }
                    "counter" => match arg {
                        "page" => items.push(ContentItem::Counter(CounterType::Page)),
                        "pages" => items.push(ContentItem::Counter(CounterType::Pages)),
                        _ => {} // unknown counter
                    },
                    _ => {} // unknown function
                }
            }
            continue;
        }

        i += 1;
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_css() {
        let css = "body { color: red; }\np { margin: 0; }";
        let ctx = parse_gcpm(css);
        assert!(ctx.running_names.is_empty());
        assert!(ctx.margin_boxes.is_empty());
        assert_eq!(ctx.cleaned_css, css);
    }

    #[test]
    fn test_extract_running_name() {
        let css = ".header { position: running(pageHeader); font-size: 12px; }";
        let ctx = parse_gcpm(css);
        assert!(ctx.running_names.contains("pageHeader"));
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
        assert!(ctx.running_names.is_empty());
    }
}
