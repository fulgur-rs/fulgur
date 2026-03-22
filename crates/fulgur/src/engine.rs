use crate::asset::AssetBundle;
use crate::config::{Config, ConfigBuilder, Margin, PageSize};
use crate::error::Result;
use crate::pageable::Pageable;
use crate::render::render_to_pdf;
use std::borrow::Cow;
use std::path::Path;

/// Rewrite `<caption>` inside `<table>` into a Taffy-layoutable wrapper structure.
///
/// Blitz/Taffy returns 0×0 for `<caption>`, so we rewrite it into an inline-block
/// wrapper `<div>` with the caption as a regular `<div>` sibling of the table.
/// Handles nested tables correctly and supports `caption-side: bottom`.
fn rewrite_table_captions(html: &str) -> Cow<'_, str> {
    if !html.contains("<caption") {
        return Cow::Borrowed(html);
    }

    let mut result = String::with_capacity(html.len());
    let mut remaining = html;

    while let Some(table_start) = remaining.find("<table") {
        let after_table_tag = &remaining[table_start..];
        let Some(tag_end) = after_table_tag.find('>') else {
            break;
        };
        let table_tag = &remaining[table_start..table_start + tag_end + 1];
        let table_content_start = table_start + tag_end + 1;

        // Find matching </table>, accounting for nested tables
        let Some(table_close_abs) = find_matching_close_table(remaining, table_content_start)
        else {
            break;
        };
        let table_inner = &remaining[table_content_start..table_close_abs];

        if let Some(cap) = extract_caption(table_inner) {
            result.push_str(&remaining[..table_start]);

            result.push_str("<div style=\"display: inline-block\">");
            if cap.side == CaptionSideHint::Top {
                result.push_str("<div>");
                result.push_str(&table_inner[cap.content_start..cap.content_end]);
                result.push_str("</div>");
                result.push_str(table_tag);
                result.push_str(&table_inner[..cap.tag_start]);
                result.push_str(&table_inner[cap.tag_end..]);
                result.push_str("</table>");
            } else {
                result.push_str(table_tag);
                result.push_str(&table_inner[..cap.tag_start]);
                result.push_str(&table_inner[cap.tag_end..]);
                result.push_str("</table>");
                result.push_str("<div>");
                result.push_str(&table_inner[cap.content_start..cap.content_end]);
                result.push_str("</div>");
            }
            result.push_str("</div>");

            remaining = &remaining[table_close_abs + "</table>".len()..];
        } else {
            result.push_str(&remaining[..table_close_abs + "</table>".len()]);
            remaining = &remaining[table_close_abs + "</table>".len()..];
        }
    }

    result.push_str(remaining);
    Cow::Owned(result)
}

/// Find the position of the matching `</table>` for a table starting at `content_start`,
/// correctly skipping over nested `<table>...</table>` pairs.
fn find_matching_close_table(html: &str, content_start: usize) -> Option<usize> {
    let mut depth = 1u32;
    let mut pos = content_start;
    while pos < html.len() {
        if html[pos..].starts_with("<table") {
            depth += 1;
            pos += 6;
        } else if html[pos..].starts_with("</table>") {
            depth -= 1;
            if depth == 0 {
                return Some(pos);
            }
            pos += 8;
        } else {
            pos += 1;
        }
    }
    None
}

#[derive(Debug, PartialEq)]
enum CaptionSideHint {
    Top,
    Bottom,
}

struct CaptionInfo {
    /// Byte offset of `<caption` within table inner HTML
    tag_start: usize,
    /// Byte offset after `</caption>` within table inner HTML
    tag_end: usize,
    /// Byte offsets of caption inner content within table inner HTML
    content_start: usize,
    content_end: usize,
    side: CaptionSideHint,
}

/// Extract the first `<caption>` that belongs to this table (not a nested table's).
fn extract_caption(table_inner: &str) -> Option<CaptionInfo> {
    let cap_start = table_inner.find("<caption")?;

    // Ensure this caption appears before any nested <table> tag
    if let Some(nested_table) = table_inner.find("<table") {
        if cap_start > nested_table {
            return None;
        }
    }

    let after_cap = &table_inner[cap_start..];
    let tag_end = after_cap.find('>')?;
    let cap_tag = &after_cap[..tag_end];

    // Parse caption-side from the style attribute value specifically
    let side = detect_caption_side(cap_tag);

    let content_start = cap_start + tag_end + 1;
    let close_pos = table_inner[content_start..].find("</caption>")?;
    let content_end = content_start + close_pos;
    let tag_end_abs = content_end + "</caption>".len();

    Some(CaptionInfo {
        tag_start: cap_start,
        tag_end: tag_end_abs,
        content_start,
        content_end,
        side,
    })
}

/// Detect `caption-side: bottom` from the opening tag's style attribute.
fn detect_caption_side(cap_tag: &str) -> CaptionSideHint {
    // Extract the style attribute value to avoid matching class names or data attributes
    let Some(style_start) = cap_tag.find("style=\"") else {
        return CaptionSideHint::Top;
    };
    let style_val = &cap_tag[style_start + 7..];
    let Some(style_end) = style_val.find('"') else {
        return CaptionSideHint::Top;
    };
    let style = &style_val[..style_end];
    if style.contains("caption-side") && style.contains("bottom") {
        CaptionSideHint::Bottom
    } else {
        CaptionSideHint::Top
    }
}

/// Reusable PDF generation engine.
pub struct Engine {
    config: Config,
    assets: Option<AssetBundle>,
}

impl Engine {
    pub fn builder() -> EngineBuilder {
        EngineBuilder {
            config_builder: Config::builder(),
            assets: None,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Render a Pageable tree to PDF bytes.
    pub fn render_pageable(&self, root: Box<dyn Pageable>) -> Result<Vec<u8>> {
        render_to_pdf(root, &self.config)
    }

    pub fn assets(&self) -> Option<&AssetBundle> {
        self.assets.as_ref()
    }

    /// Render HTML string to PDF bytes.
    /// If an AssetBundle is set, its CSS will be injected as a <style> block.
    /// When GCPM constructs (margin boxes, running elements) are detected in the CSS,
    /// a 2-pass rendering pipeline is used: pass 1 paginates body content, pass 2
    /// renders each page with resolved margin boxes.
    pub fn render_html(&self, html: &str) -> Result<Vec<u8>> {
        let html = rewrite_table_captions(html);
        let html: &str = &html;

        let combined_css = self
            .assets
            .as_ref()
            .map(|a| a.combined_css())
            .unwrap_or_default();

        let gcpm = crate::gcpm::parser::parse_gcpm(&combined_css);
        let css_to_inject = &gcpm.cleaned_css;

        let final_html = if css_to_inject.is_empty() {
            html.to_string()
        } else {
            let style_block = format!("<style>{}</style>", css_to_inject);
            if let Some(pos) = html.find("</head>") {
                format!("{}{}{}", &html[..pos], style_block, &html[pos..])
            } else if let Some(pos) = html.find("<body") {
                format!("{}{}{}", &html[..pos], style_block, &html[pos..])
            } else {
                format!("{}{}", style_block, html)
            }
        };

        let fonts = self
            .assets
            .as_ref()
            .map(|a| a.fonts.as_slice())
            .unwrap_or(&[]);
        let doc = crate::blitz_adapter::parse_and_layout(
            &final_html,
            self.config.content_width(),
            self.config.content_height(),
            fonts,
        );

        let gcpm_opt = if gcpm.is_empty() { None } else { Some(&gcpm) };
        let mut running_store = crate::gcpm::running::RunningElementStore::new();
        let root = crate::convert::dom_to_pageable(&doc, gcpm_opt, &mut running_store);

        if gcpm.is_empty() {
            self.render_pageable(root)
        } else {
            crate::render::render_to_pdf_with_gcpm(root, &self.config, &gcpm, &running_store, fonts)
        }
    }

    /// Render HTML string to a PDF file.
    pub fn render_html_to_file(&self, html: &str, path: impl AsRef<Path>) -> Result<()> {
        let pdf = self.render_html(html)?;
        std::fs::write(path, pdf)?;
        Ok(())
    }

    /// Render a Pageable tree to a PDF file.
    pub fn render_pageable_to_file(
        &self,
        root: Box<dyn Pageable>,
        path: impl AsRef<Path>,
    ) -> Result<()> {
        let pdf = self.render_pageable(root)?;
        std::fs::write(path, pdf)?;
        Ok(())
    }
}

pub struct EngineBuilder {
    config_builder: ConfigBuilder,
    assets: Option<AssetBundle>,
}

impl EngineBuilder {
    pub fn page_size(mut self, size: PageSize) -> Self {
        self.config_builder = self.config_builder.page_size(size);
        self
    }

    pub fn margin(mut self, margin: Margin) -> Self {
        self.config_builder = self.config_builder.margin(margin);
        self
    }

    pub fn landscape(mut self, landscape: bool) -> Self {
        self.config_builder = self.config_builder.landscape(landscape);
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.title(title);
        self
    }

    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.author(author);
        self
    }

    pub fn assets(mut self, assets: AssetBundle) -> Self {
        self.assets = Some(assets);
        self
    }

    pub fn build(self) -> Engine {
        Engine {
            config: self.config_builder.build(),
            assets: self.assets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_caption_top() {
        let html = r#"<table border="1"><caption>Title</caption><tr><td>A</td></tr></table>"#;
        let result = rewrite_table_captions(html);
        assert!(result.contains(r#"<div style="display: inline-block">"#));
        assert!(result.contains("<div>Title</div>"));
        assert!(!result.contains("<caption"));
        // Caption div comes before table
        let cap_pos = result.find("<div>Title</div>").unwrap();
        let table_pos = result.find("<table").unwrap();
        assert!(cap_pos < table_pos);
    }

    #[test]
    fn rewrite_caption_bottom() {
        let html = r#"<table><caption style="caption-side: bottom">Footer</caption><tr><td>A</td></tr></table>"#;
        let result = rewrite_table_captions(html);
        // Caption div comes after table
        let cap_pos = result.find("<div>Footer</div>").unwrap();
        let table_close = result.find("</table>").unwrap();
        assert!(cap_pos > table_close);
    }

    #[test]
    fn rewrite_no_caption() {
        let html = r#"<table border="1"><tr><td>A</td></tr></table>"#;
        let result = rewrite_table_captions(html);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(&*result, html);
    }

    #[test]
    fn rewrite_no_table() {
        let html = "<p>No tables here</p>";
        let result = rewrite_table_captions(html);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn rewrite_preserves_surrounding_html() {
        let html =
            r#"<p>Before</p><table><caption>T</caption><tr><td>A</td></tr></table><p>After</p>"#;
        let result = rewrite_table_captions(html);
        assert!(result.starts_with("<p>Before</p>"));
        assert!(result.ends_with("<p>After</p>"));
    }

    #[test]
    fn rewrite_nested_table() {
        let html = r#"<table><caption>Outer</caption><tr><td><table><tr><td>inner</td></tr></table></td></tr></table>"#;
        let result = rewrite_table_captions(html);
        assert!(result.contains("<div>Outer</div>"));
        // Inner table should be preserved intact
        assert!(result.contains("<table><tr><td>inner</td></tr></table>"));
    }

    #[test]
    fn rewrite_inner_table_caption_ignored() {
        // Caption belongs to inner table, outer has none
        let html = r#"<table><tr><td><table><caption>Inner</caption><tr><td>x</td></tr></table></td></tr></table>"#;
        let result = rewrite_table_captions(html);
        // Outer table has no caption so no wrapper
        assert!(!result.contains(r#"display: inline-block"#));
    }

    #[test]
    fn rewrite_caption_side_in_class_not_matched() {
        // "bottom" in class name should not trigger caption-side: bottom
        let html =
            r#"<table><caption class="bottom-style">Title</caption><tr><td>A</td></tr></table>"#;
        let result = rewrite_table_captions(html);
        let cap_pos = result.find("<div>Title</div>").unwrap();
        let table_pos = result.find("<table").unwrap();
        assert!(cap_pos < table_pos, "should be top (default)");
    }
}
