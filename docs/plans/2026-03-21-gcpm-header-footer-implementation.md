# GCPM Header/Footer Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CSS Generated Content for Paged Media (GCPM) によるヘッダー/フッター機能のMVP実装

**Architecture:** `cssparser` で GCPM 構文を自前パースし、Blitz に渡す前に抽出・除去する。Running elements は Blitz DOM からシリアライズし、2パスレンダリングでマージンボックスに描画する。

**Tech Stack:** Rust, cssparser, blitz-dom/blitz-html, krilla

---

### Task 1: `config.rs` から未実装の `header_html` / `footer_html` を削除

**Files:**
- Modify: `crates/fulgur/src/config.rs:85-86,98-99,166-174`

**Step 1: `Config` 構造体から `header_html` / `footer_html` フィールドを削除**

`config.rs` の `Config` 構造体から以下を削除:
```rust
// 削除: 85-86行目
pub header_html: Option<String>,
pub footer_html: Option<String>,
```

`Default` impl から以下を削除:
```rust
// 削除: 98-99行目
header_html: None,
footer_html: None,
```

`ConfigBuilder` から以下のメソッドを削除:
```rust
// 削除: 166-174行目
pub fn header_html(mut self, html: impl Into<String>) -> Self { ... }
pub fn footer_html(mut self, html: impl Into<String>) -> Self { ... }
```

**Step 2: ビルドして他に参照がないことを確認**

Run: `cargo build -p fulgur 2>&1`
Expected: ビルド成功 (header_html/footer_html は未使用なのでエラーなし)

**Step 3: コミット**

```bash
git add crates/fulgur/src/config.rs
git commit -m "chore: remove unused header_html/footer_html from Config"
```

---

### Task 2: `gcpm/margin_box.rs` — MarginBoxPosition enum と位置計算の型定義

**Files:**
- Create: `crates/fulgur/src/gcpm/margin_box.rs`
- Test: `crates/fulgur/src/gcpm/margin_box.rs` (同ファイル内 `#[cfg(test)]`)

**Step 1: テストを書く**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_at_keyword() {
        assert_eq!(
            MarginBoxPosition::from_at_keyword("top-center"),
            Some(MarginBoxPosition::TopCenter)
        );
        assert_eq!(
            MarginBoxPosition::from_at_keyword("bottom-left-corner"),
            Some(MarginBoxPosition::BottomLeftCorner)
        );
        assert_eq!(MarginBoxPosition::from_at_keyword("invalid"), None);
    }

    #[test]
    fn test_top_center_rect_in_margins() {
        use crate::config::{Margin, PageSize};
        let page_size = PageSize::A4;
        let margin = Margin::uniform(72.0); // 1 inch
        let rect = MarginBoxPosition::TopCenter.bounding_rect(page_size, margin);
        // top-center: x = margin.left, y = 0, w = content_width, h = margin.top
        assert!((rect.x - 72.0).abs() < 0.01);
        assert!(rect.y.abs() < 0.01);
        assert!((rect.width - (595.28 - 144.0)).abs() < 0.01);
        assert!((rect.height - 72.0).abs() < 0.01);
    }

    #[test]
    fn test_bottom_center_rect_in_margins() {
        use crate::config::{Margin, PageSize};
        let page_size = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let rect = MarginBoxPosition::BottomCenter.bounding_rect(page_size, margin);
        // bottom-center: x = margin.left, y = page_height - margin.bottom, w = content_width, h = margin.bottom
        assert!((rect.x - 72.0).abs() < 0.01);
        assert!((rect.y - (841.89 - 72.0)).abs() < 0.01);
        assert!((rect.width - (595.28 - 144.0)).abs() < 0.01);
        assert!((rect.height - 72.0).abs() < 0.01);
    }
}
```

**Step 2: テストが失敗することを確認**

Run: `cargo test -p fulgur gcpm::margin_box 2>&1`
Expected: FAIL (モジュール未作成)

**Step 3: 実装**

```rust
use crate::config::{Margin, PageSize};

/// Bounding rectangle in page coordinates (points).
#[derive(Debug, Clone, Copy)]
pub struct MarginBoxRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// CSS Paged Media margin box positions (16 total).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarginBoxPosition {
    TopLeftCorner,
    TopLeft,
    TopCenter,
    TopRight,
    TopRightCorner,
    LeftTop,
    LeftMiddle,
    LeftBottom,
    RightTop,
    RightMiddle,
    RightBottom,
    BottomLeftCorner,
    BottomLeft,
    BottomCenter,
    BottomRight,
    BottomRightCorner,
}

impl MarginBoxPosition {
    /// Parse a CSS at-keyword (without the `@` prefix) into a position.
    pub fn from_at_keyword(keyword: &str) -> Option<Self> {
        match keyword {
            "top-left-corner" => Some(Self::TopLeftCorner),
            "top-left" => Some(Self::TopLeft),
            "top-center" => Some(Self::TopCenter),
            "top-right" => Some(Self::TopRight),
            "top-right-corner" => Some(Self::TopRightCorner),
            "left-top" => Some(Self::LeftTop),
            "left-middle" => Some(Self::LeftMiddle),
            "left-bottom" => Some(Self::LeftBottom),
            "right-top" => Some(Self::RightTop),
            "right-middle" => Some(Self::RightMiddle),
            "right-bottom" => Some(Self::RightBottom),
            "bottom-left-corner" => Some(Self::BottomLeftCorner),
            "bottom-left" => Some(Self::BottomLeft),
            "bottom-center" => Some(Self::BottomCenter),
            "bottom-right" => Some(Self::BottomRight),
            "bottom-right-corner" => Some(Self::BottomRightCorner),
            _ => None,
        }
    }

    /// Compute the bounding rectangle for this margin box position.
    /// MVP: only TopCenter and BottomCenter return full content-width rects.
    /// Other positions return simplified rects (to be refined in Phase 2).
    pub fn bounding_rect(&self, page_size: PageSize, margin: Margin) -> MarginBoxRect {
        let content_width = page_size.width - margin.left - margin.right;
        let content_height = page_size.height - margin.top - margin.bottom;

        match self {
            // Top edge
            Self::TopLeftCorner => MarginBoxRect {
                x: 0.0,
                y: 0.0,
                width: margin.left,
                height: margin.top,
            },
            Self::TopLeft => MarginBoxRect {
                x: margin.left,
                y: 0.0,
                width: content_width / 3.0,
                height: margin.top,
            },
            Self::TopCenter => MarginBoxRect {
                x: margin.left,
                y: 0.0,
                width: content_width,
                height: margin.top,
            },
            Self::TopRight => MarginBoxRect {
                x: margin.left + content_width * 2.0 / 3.0,
                y: 0.0,
                width: content_width / 3.0,
                height: margin.top,
            },
            Self::TopRightCorner => MarginBoxRect {
                x: page_size.width - margin.right,
                y: 0.0,
                width: margin.right,
                height: margin.top,
            },
            // Left edge
            Self::LeftTop => MarginBoxRect {
                x: 0.0,
                y: margin.top,
                width: margin.left,
                height: content_height / 3.0,
            },
            Self::LeftMiddle => MarginBoxRect {
                x: 0.0,
                y: margin.top + content_height / 3.0,
                width: margin.left,
                height: content_height / 3.0,
            },
            Self::LeftBottom => MarginBoxRect {
                x: 0.0,
                y: margin.top + content_height * 2.0 / 3.0,
                width: margin.left,
                height: content_height / 3.0,
            },
            // Right edge
            Self::RightTop => MarginBoxRect {
                x: page_size.width - margin.right,
                y: margin.top,
                width: margin.right,
                height: content_height / 3.0,
            },
            Self::RightMiddle => MarginBoxRect {
                x: page_size.width - margin.right,
                y: margin.top + content_height / 3.0,
                width: margin.right,
                height: content_height / 3.0,
            },
            Self::RightBottom => MarginBoxRect {
                x: page_size.width - margin.right,
                y: margin.top + content_height * 2.0 / 3.0,
                width: margin.right,
                height: content_height / 3.0,
            },
            // Bottom edge
            Self::BottomLeftCorner => MarginBoxRect {
                x: 0.0,
                y: page_size.height - margin.bottom,
                width: margin.left,
                height: margin.bottom,
            },
            Self::BottomLeft => MarginBoxRect {
                x: margin.left,
                y: page_size.height - margin.bottom,
                width: content_width / 3.0,
                height: margin.bottom,
            },
            Self::BottomCenter => MarginBoxRect {
                x: margin.left,
                y: page_size.height - margin.bottom,
                width: content_width,
                height: margin.bottom,
            },
            Self::BottomRight => MarginBoxRect {
                x: margin.left + content_width * 2.0 / 3.0,
                y: page_size.height - margin.bottom,
                width: content_width / 3.0,
                height: margin.bottom,
            },
            Self::BottomRightCorner => MarginBoxRect {
                x: page_size.width - margin.right,
                y: page_size.height - margin.bottom,
                width: margin.right,
                height: margin.bottom,
            },
        }
    }
}
```

**Step 4: テスト実行**

Run: `cargo test -p fulgur gcpm::margin_box 2>&1`
Expected: PASS

**Step 5: コミット**

```bash
git add crates/fulgur/src/gcpm/
git commit -m "feat(gcpm): add MarginBoxPosition enum with bounding rect calculation"
```

---

### Task 3: `gcpm/mod.rs` — GcpmContext と ContentItem 型定義

**Files:**
- Create: `crates/fulgur/src/gcpm/mod.rs`
- Modify: `crates/fulgur/src/lib.rs:1` (モジュール追加)

**Step 1: `gcpm/mod.rs` を作成**

```rust
pub mod counter;
pub mod margin_box;
pub mod parser;
pub mod running;

use margin_box::MarginBoxPosition;
use std::collections::HashSet;

/// A parsed GCPM content value item.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentItem {
    /// `element(<name>)` — reference to a running element
    Element(String),
    /// `counter(page)` or `counter(pages)`
    Counter(CounterType),
    /// Literal string, e.g., `" / "`
    String(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CounterType {
    Page,
    Pages,
}

/// A parsed @page margin box rule.
#[derive(Debug, Clone)]
pub struct MarginBoxRule {
    /// Page selector (e.g., ":first", ":left"), None for default @page
    pub page_selector: Option<String>,
    /// Which margin box position
    pub position: MarginBoxPosition,
    /// Parsed content items
    pub content: Vec<ContentItem>,
    /// Other CSS declarations (background, font-size, etc.) as raw CSS text
    pub declarations: String,
}

/// Result of GCPM CSS preprocessing.
#[derive(Debug, Clone)]
pub struct GcpmContext {
    /// Margin box rules extracted from @page
    pub margin_boxes: Vec<MarginBoxRule>,
    /// Names declared via `position: running(<name>)`
    pub running_names: HashSet<String>,
    /// CSS with GCPM constructs removed, safe for Blitz
    pub cleaned_css: String,
}

impl GcpmContext {
    /// Returns true if no GCPM features are used.
    pub fn is_empty(&self) -> bool {
        self.margin_boxes.is_empty() && self.running_names.is_empty()
    }
}
```

**Step 2: `lib.rs` にモジュール追加**

`crates/fulgur/src/lib.rs` の先頭モジュール一覧に追加:
```rust
pub mod gcpm;
```

**Step 3: スタブモジュールを作成**

`crates/fulgur/src/gcpm/parser.rs`:
```rust
// GCPM CSS parser — implemented in Task 4
```

`crates/fulgur/src/gcpm/running.rs`:
```rust
// Running elements lifecycle — implemented in Task 6
```

`crates/fulgur/src/gcpm/counter.rs`:
```rust
// Page counter resolution — implemented in Task 5
```

**Step 4: ビルド確認**

Run: `cargo build -p fulgur 2>&1`
Expected: ビルド成功

**Step 5: コミット**

```bash
git add crates/fulgur/src/gcpm/ crates/fulgur/src/lib.rs
git commit -m "feat(gcpm): add GcpmContext, ContentItem, MarginBoxRule types"
```

---

### Task 4: `gcpm/parser.rs` — cssparser による GCPM 構文抽出

**Files:**
- Modify: `crates/fulgur/Cargo.toml` (cssparser 依存追加)
- Modify: `crates/fulgur/src/gcpm/parser.rs`
- Test: 同ファイル内 `#[cfg(test)]`

**Step 1: `cssparser` を依存に追加**

`crates/fulgur/Cargo.toml` の `[dependencies]` に追加:
```toml
cssparser = "0.34"
```

Note: Cargo.lock で既に stylo 経由で cssparser が入っている。バージョンは Cargo.lock を確認して合わせること。

**Step 2: テストを書く**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcpm::{ContentItem, CounterType};
    use crate::gcpm::margin_box::MarginBoxPosition;

    #[test]
    fn test_empty_css() {
        let ctx = parse_gcpm("body { color: red; }");
        assert!(ctx.is_empty());
        assert_eq!(ctx.cleaned_css, "body { color: red; }");
    }

    #[test]
    fn test_extract_running_name() {
        let css = ".header { position: running(pageHeader); color: blue; }";
        let ctx = parse_gcpm(css);
        assert!(ctx.running_names.contains("pageHeader"));
        // position: running() は display: none に置換される
        assert!(ctx.cleaned_css.contains("display: none"));
        assert!(!ctx.cleaned_css.contains("running"));
    }

    #[test]
    fn test_extract_margin_box() {
        let css = r#"
            @page {
                @top-center {
                    content: element(pageHeader);
                }
            }
        "#;
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        assert_eq!(ctx.margin_boxes[0].position, MarginBoxPosition::TopCenter);
        assert_eq!(
            ctx.margin_boxes[0].content,
            vec![ContentItem::Element("pageHeader".to_string())]
        );
        // @page block with only margin boxes should be removed from cleaned_css
        assert!(!ctx.cleaned_css.contains("@top-center"));
    }

    #[test]
    fn test_extract_counter() {
        let css = r#"
            @page {
                @bottom-center {
                    content: "Page " counter(page) " of " counter(pages);
                }
            }
        "#;
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        assert_eq!(
            ctx.margin_boxes[0].content,
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
        let css = r#"
            body { font-size: 14px; }
            .header { position: running(hdr); }
            @page { @top-center { content: element(hdr); } }
            p { margin: 1em 0; }
        "#;
        let ctx = parse_gcpm(css);
        assert!(ctx.running_names.contains("hdr"));
        assert_eq!(ctx.margin_boxes.len(), 1);
        assert!(ctx.cleaned_css.contains("font-size: 14px"));
        assert!(ctx.cleaned_css.contains("margin: 1em 0"));
    }

    #[test]
    fn test_page_selector() {
        let css = r#"
            @page :first {
                @top-center { content: "First page header"; }
            }
        "#;
        let ctx = parse_gcpm(css);
        assert_eq!(ctx.margin_boxes.len(), 1);
        assert_eq!(
            ctx.margin_boxes[0].page_selector,
            Some(":first".to_string())
        );
    }
}
```

**Step 3: テストが失敗することを確認**

Run: `cargo test -p fulgur gcpm::parser 2>&1`
Expected: FAIL

**Step 4: `parse_gcpm` を実装**

パーサーの戦略: `cssparser` の低レベルトークナイザを使わず、文字列ベースの前処理で十分。理由:
- `position: running(name)` はプロパティ値の単純なパターンマッチ
- `@page { @top-center { ... } }` はネスト構造だが、ブレース追跡で抽出可能
- `content: element(name) counter(page) "str"` はトークン列の順次パース

```rust
use crate::gcpm::{ContentItem, CounterType, GcpmContext, MarginBoxRule};
use crate::gcpm::margin_box::MarginBoxPosition;
use std::collections::HashSet;

/// Parse CSS and extract GCPM constructs.
/// Returns a GcpmContext with extracted margin boxes, running names,
/// and cleaned CSS safe for Blitz.
pub fn parse_gcpm(css: &str) -> GcpmContext {
    let mut running_names = HashSet::new();
    let mut margin_boxes = Vec::new();
    let mut cleaned = String::with_capacity(css.len());

    let mut chars = css.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        // Detect `@page`
        if c == '@' && css[i..].starts_with("@page") {
            let (page_rule_end, page_selector, boxes) = parse_page_rule(&css[i..]);
            margin_boxes.extend(boxes);
            // Skip the @page block in cleaned CSS (non-margin-box content could be preserved,
            // but Blitz doesn't use @page anyway)
            for _ in 0..page_rule_end {
                chars.next();
            }
            continue;
        }

        // Detect `position: running(`
        if c == 'p' && css[i..].starts_with("position") {
            if let Some((name, replacement, consumed)) = try_parse_running(&css[i..]) {
                running_names.insert(name);
                cleaned.push_str(&replacement);
                for _ in 0..consumed {
                    chars.next();
                }
                continue;
            }
        }

        cleaned.push(c);
        chars.next();
    }

    GcpmContext {
        margin_boxes,
        running_names,
        cleaned_css: cleaned,
    }
}

/// Try to parse `position: running(<name>)` or `position:running(<name>)`.
/// Returns (name, replacement_css, chars_consumed) or None.
fn try_parse_running(s: &str) -> Option<(String, String, usize)> {
    // Match: position\s*:\s*running\s*\(\s*<ident>\s*\)
    let after_position = &s["position".len()..];
    let after_position = after_position.trim_start();
    if !after_position.starts_with(':') {
        return None;
    }
    let after_colon = after_position[1..].trim_start();
    if !after_colon.starts_with("running") {
        return None;
    }
    let after_running = after_colon["running".len()..].trim_start();
    if !after_running.starts_with('(') {
        return None;
    }
    let after_paren = after_running[1..].trim_start();
    // Read identifier
    let name_end = after_paren
        .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .unwrap_or(after_paren.len());
    if name_end == 0 {
        return None;
    }
    let name = after_paren[..name_end].to_string();
    let after_name = after_paren[name_end..].trim_start();
    if !after_name.starts_with(')') {
        return None;
    }
    // Find the semicolon or end of declaration
    let rest = &after_name[1..];
    let semi_offset = rest.find(';').map(|p| p + 1).unwrap_or(0);
    let total_consumed = s.len() - rest[semi_offset..].len();
    Some((name, "display: none;".to_string(), total_consumed))
}

/// Parse an `@page` rule block. Returns (chars_consumed, page_selector, margin_boxes).
fn parse_page_rule(s: &str) -> (usize, Option<String>, Vec<MarginBoxRule>) {
    let mut boxes = Vec::new();

    // Skip "@page"
    let rest = &s["@page".len()..];
    let rest = rest.trim_start();

    // Parse optional page selector (e.g., ":first", ":left", "named")
    let (page_selector, rest) = if rest.starts_with('{') {
        (None, rest)
    } else {
        let brace_pos = match rest.find('{') {
            Some(p) => p,
            None => return (s.len(), None, boxes),
        };
        let selector = rest[..brace_pos].trim().to_string();
        let selector = if selector.is_empty() {
            None
        } else {
            Some(selector)
        };
        (selector, &rest[brace_pos..])
    };

    // Find matching closing brace
    let block_content = match find_matching_brace(rest) {
        Some((content, end_pos)) => {
            let consumed = s.len() - rest.len() + end_pos;
            // Parse margin box at-rules inside the block
            parse_margin_boxes(content, &page_selector, &mut boxes);
            return (consumed, page_selector, boxes);
        }
        None => return (s.len(), page_selector, boxes),
    };
}

/// Find matching `{...}` and return (inner_content, position_after_closing_brace).
fn find_matching_brace(s: &str) -> Option<(&str, usize)> {
    if !s.starts_with('{') {
        return None;
    }
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[1..i], i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse margin box at-rules inside a @page block's content.
fn parse_margin_boxes(
    content: &str,
    page_selector: &Option<String>,
    boxes: &mut Vec<MarginBoxRule>,
) {
    let mut rest = content;
    while let Some(at_pos) = rest.find('@') {
        rest = &rest[at_pos + 1..];
        // Read at-keyword
        let kw_end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '-')
            .unwrap_or(rest.len());
        let keyword = &rest[..kw_end];
        let position = match MarginBoxPosition::from_at_keyword(keyword) {
            Some(p) => p,
            None => continue,
        };
        rest = &rest[kw_end..];
        let rest_trimmed = rest.trim_start();
        if let Some((block, end_pos)) = find_matching_brace(rest_trimmed) {
            let content_items = parse_content_property(block);
            let declarations = extract_non_content_declarations(block);
            boxes.push(MarginBoxRule {
                page_selector: page_selector.clone(),
                position,
                content: content_items,
                declarations,
            });
            let consumed = rest.len() - rest_trimmed.len() + end_pos;
            rest = &rest[consumed..];
        }
    }
}

/// Parse the `content:` property value from a declaration block.
fn parse_content_property(block: &str) -> Vec<ContentItem> {
    // Find `content:` in the block
    let content_start = match block.find("content") {
        Some(p) => p,
        None => return Vec::new(),
    };
    let after_content = &block[content_start + "content".len()..];
    let after_content = after_content.trim_start();
    if !after_content.starts_with(':') {
        return Vec::new();
    }
    let value_str = &after_content[1..].trim_start();
    // Value ends at `;` or end of block
    let value_end = value_str.find(';').unwrap_or(value_str.len());
    let value = value_str[..value_end].trim();

    parse_content_value(value)
}

/// Parse a content value string into ContentItems.
/// Handles: `element(<name>)`, `counter(page)`, `counter(pages)`, `"string"`.
fn parse_content_value(value: &str) -> Vec<ContentItem> {
    let mut items = Vec::new();
    let mut rest = value.trim();

    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }

        if rest.starts_with('"') {
            // String literal
            if let Some(end) = rest[1..].find('"') {
                items.push(ContentItem::String(rest[1..end + 1].to_string()));
                rest = &rest[end + 2..];
            } else {
                break;
            }
        } else if rest.starts_with("element(") {
            let after = &rest["element(".len()..];
            if let Some(paren_end) = after.find(')') {
                let name = after[..paren_end].trim().to_string();
                items.push(ContentItem::Element(name));
                rest = &rest["element(".len() + paren_end + 1..];
            } else {
                break;
            }
        } else if rest.starts_with("counter(") {
            let after = &rest["counter(".len()..];
            if let Some(paren_end) = after.find(')') {
                let counter_name = after[..paren_end].trim();
                match counter_name {
                    "page" => items.push(ContentItem::Counter(CounterType::Page)),
                    "pages" => items.push(ContentItem::Counter(CounterType::Pages)),
                    _ => {} // Unknown counter — skip
                }
                rest = &rest["counter(".len() + paren_end + 1..];
            } else {
                break;
            }
        } else {
            // Skip unknown token
            let next_space = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == ';')
                .unwrap_or(rest.len());
            rest = &rest[next_space..];
        }
    }

    items
}

/// Extract non-content declarations from a margin box block.
fn extract_non_content_declarations(block: &str) -> String {
    block
        .split(';')
        .filter(|decl| {
            let trimmed = decl.trim();
            !trimmed.is_empty() && !trimmed.starts_with("content")
        })
        .map(|d| d.trim())
        .collect::<Vec<_>>()
        .join("; ")
}
```

**Step 5: テスト実行**

Run: `cargo test -p fulgur gcpm::parser 2>&1`
Expected: PASS

**Step 6: コミット**

```bash
git add crates/fulgur/Cargo.toml crates/fulgur/src/gcpm/parser.rs
git commit -m "feat(gcpm): implement CSS parser for @page margin boxes and running()"
```

---

### Task 5: `gcpm/counter.rs` — ページカウンター解決

**Files:**
- Modify: `crates/fulgur/src/gcpm/counter.rs`
- Test: 同ファイル内 `#[cfg(test)]`

**Step 1: テストを書く**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcpm::{ContentItem, CounterType};

    #[test]
    fn test_resolve_counters() {
        let items = vec![
            ContentItem::String("Page ".to_string()),
            ContentItem::Counter(CounterType::Page),
            ContentItem::String(" of ".to_string()),
            ContentItem::Counter(CounterType::Pages),
        ];
        let resolved = resolve_content_to_string(&items, 3, 10);
        assert_eq!(resolved, "Page 3 of 10");
    }

    #[test]
    fn test_element_becomes_empty() {
        let items = vec![
            ContentItem::Element("hdr".to_string()),
            ContentItem::String(" - page ".to_string()),
            ContentItem::Counter(CounterType::Page),
        ];
        // element() references are not resolved to string — they need separate handling
        let resolved = resolve_content_to_string(&items, 1, 5);
        assert_eq!(resolved, " - page 1");
    }

    #[test]
    fn test_resolve_html_with_running_element() {
        let items = vec![
            ContentItem::Element("hdr".to_string()),
        ];
        let running = vec![("hdr".to_string(), "<div>Header</div>".to_string())];
        let html = resolve_content_to_html(&items, &running, 1, 5);
        assert_eq!(html, "<div>Header</div>");
    }

    #[test]
    fn test_resolve_html_mixed() {
        let items = vec![
            ContentItem::Element("hdr".to_string()),
            ContentItem::String(" | Page ".to_string()),
            ContentItem::Counter(CounterType::Page),
            ContentItem::String("/".to_string()),
            ContentItem::Counter(CounterType::Pages),
        ];
        let running = vec![("hdr".to_string(), "<span>Title</span>".to_string())];
        let html = resolve_content_to_html(&items, &running, 2, 10);
        assert_eq!(html, "<span>Title</span> | Page 2/10");
    }
}
```

**Step 2: テスト失敗確認**

Run: `cargo test -p fulgur gcpm::counter 2>&1`
Expected: FAIL

**Step 3: 実装**

```rust
use crate::gcpm::{ContentItem, CounterType};

/// Resolve content items to a plain string (element references are skipped).
pub fn resolve_content_to_string(items: &[ContentItem], page: usize, total_pages: usize) -> String {
    let mut result = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => result.push_str(s),
            ContentItem::Counter(CounterType::Page) => {
                result.push_str(&page.to_string());
            }
            ContentItem::Counter(CounterType::Pages) => {
                result.push_str(&total_pages.to_string());
            }
            ContentItem::Element(_) => {} // Skipped in plain string resolution
        }
    }
    result
}

/// Resolve content items to HTML, substituting running elements and counters.
/// `running_elements` is a list of (name, html) pairs.
pub fn resolve_content_to_html(
    items: &[ContentItem],
    running_elements: &[(String, String)],
    page: usize,
    total_pages: usize,
) -> String {
    let mut result = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => result.push_str(s),
            ContentItem::Counter(CounterType::Page) => {
                result.push_str(&page.to_string());
            }
            ContentItem::Counter(CounterType::Pages) => {
                result.push_str(&total_pages.to_string());
            }
            ContentItem::Element(name) => {
                if let Some((_, html)) = running_elements.iter().find(|(n, _)| n == name) {
                    result.push_str(html);
                }
            }
        }
    }
    result
}
```

**Step 4: テスト実行**

Run: `cargo test -p fulgur gcpm::counter 2>&1`
Expected: PASS

**Step 5: コミット**

```bash
git add crates/fulgur/src/gcpm/counter.rs
git commit -m "feat(gcpm): implement page counter resolution"
```

---

### Task 6: `gcpm/running.rs` — Running Elements 管理 + DOM シリアライザ

**Files:**
- Modify: `crates/fulgur/src/gcpm/running.rs`
- Test: 同ファイル内 `#[cfg(test)]`

**Step 1: テストを書く**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_running_element_store_and_lookup() {
        let mut store = RunningElementStore::new();
        store.register("header".to_string(), "<div>Logo</div>".to_string());
        store.register("footer".to_string(), "<span>Footer</span>".to_string());

        assert_eq!(store.get("header"), Some("<div>Logo</div>"));
        assert_eq!(store.get("footer"), Some("<span>Footer</span>"));
        assert_eq!(store.get("nonexistent"), None);
    }

    #[test]
    fn test_to_pairs() {
        let mut store = RunningElementStore::new();
        store.register("a".to_string(), "<p>A</p>".to_string());
        let pairs = store.to_pairs();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "a");
        assert_eq!(pairs[0].1, "<p>A</p>");
    }
}
```

**Step 2: テスト失敗確認**

Run: `cargo test -p fulgur gcpm::running 2>&1`
Expected: FAIL

**Step 3: 実装**

```rust
use std::collections::HashMap;

/// Storage for running elements extracted from the DOM.
#[derive(Debug, Default)]
pub struct RunningElementStore {
    elements: HashMap<String, String>,
}

impl RunningElementStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a running element by name with its serialized HTML.
    pub fn register(&mut self, name: String, html: String) {
        self.elements.insert(name, html);
    }

    /// Look up a running element's HTML by name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.elements.get(name).map(|s| s.as_str())
    }

    /// Convert to a list of (name, html) pairs for counter resolution.
    pub fn to_pairs(&self) -> Vec<(String, String)> {
        self.elements
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// Serialize a Blitz DOM node subtree back to an HTML string.
/// This is used to extract running elements for re-layout in margin boxes.
pub fn serialize_node(doc: &blitz_dom::BaseDocument, node_id: usize) -> String {
    let Some(node) = doc.get_node(node_id) else {
        return String::new();
    };

    match &node.data {
        blitz_dom::NodeData::Text(text_data) => text_data.content.to_string(),
        blitz_dom::NodeData::Element(elem) => {
            let tag = elem.name.local.as_ref();
            let mut html = format!("<{}", tag);

            // Serialize attributes
            for attr in elem.attrs() {
                html.push_str(&format!(
                    " {}=\"{}\"",
                    attr.name.local.as_ref(),
                    attr.value.as_ref()
                ));
            }

            // Serialize inline styles from computed values if available
            if let Some(styles) = node.primary_styles() {
                // Extract key properties as inline style for the re-layout pass
                let color = styles.clone_color();
                let font_size = styles.clone_font_size();
                html.push_str(&format!(
                    " style=\"color: rgba({},{},{},{}); font-size: {}px;\"",
                    (color.components.0 * 255.0) as u8,
                    (color.components.1 * 255.0) as u8,
                    (color.components.2 * 255.0) as u8,
                    color.alpha,
                    font_size.computed_size().px(),
                ));
            }

            html.push('>');

            // Serialize children
            for &child_id in &node.children {
                html.push_str(&serialize_node(doc, child_id));
            }

            html.push_str(&format!("</{}>", tag));
            html
        }
        _ => String::new(),
    }
}
```

Note: `serialize_node` は Blitz DOM の `Node` API に依存する。`primary_styles()` や `element_data()` の正確な API は既に `convert.rs` で使用されているパターンに従う。`font_size.computed_size().px()` 等の API は Stylo の computed values API に依存するため、ビルド時に調整が必要になる可能性がある。

**Step 4: テスト実行**

Run: `cargo test -p fulgur gcpm::running 2>&1`
Expected: PASS (RunningElementStore のテストのみ。serialize_node は統合テストで確認)

**Step 5: コミット**

```bash
git add crates/fulgur/src/gcpm/running.rs
git commit -m "feat(gcpm): add RunningElementStore and DOM serializer"
```

---

### Task 7: `convert.rs` — Running 要素の除外と HTML 片抽出

**Files:**
- Modify: `crates/fulgur/src/convert.rs:13,48-49,144-189`

**Step 1: `dom_to_pageable` のシグネチャ変更**

`dom_to_pageable` に `GcpmContext` と `RunningElementStore` を渡し、running 要素を除外しながら変換する。

`convert.rs` の `dom_to_pageable` を変更:

```rust
use crate::gcpm::GcpmContext;
use crate::gcpm::running::{RunningElementStore, serialize_node};

/// Convert a resolved Blitz document into a Pageable tree.
/// If gcpm is provided, running elements are excluded from the tree
/// and their HTML is stored in running_store.
pub fn dom_to_pageable(
    doc: &HtmlDocument,
    gcpm: Option<&GcpmContext>,
    running_store: &mut RunningElementStore,
) -> Box<dyn Pageable> {
    let root = doc.root_element();
    if std::env::var("FULGUR_DEBUG").is_ok() {
        debug_print_tree(doc.deref(), root.id, 0);
    }
    convert_node(doc.deref(), root.id, gcpm, running_store)
}
```

`convert_node` にも同じ引数を追加し、`collect_positioned_children` で running 要素をスキップ:

```rust
fn collect_positioned_children(
    doc: &blitz_dom::BaseDocument,
    child_ids: &[usize],
    gcpm: Option<&GcpmContext>,
    running_store: &mut RunningElementStore,
) -> Vec<PositionedChild> {
    // ... 既存ロジック ...
    for &child_id in child_ids {
        // ... 既存のスキップロジック ...

        // GCPM: check if this element has position: running()
        if let Some(ctx) = gcpm {
            if is_running_element(doc, child_id, ctx) {
                // Serialize and store, skip from pageable tree
                let html = serialize_node(doc, child_id);
                let name = get_running_name(doc, child_id, ctx);
                if let Some(name) = name {
                    running_store.register(name, html);
                }
                continue;
            }
        }

        // ... 既存の変換ロジック ...
    }
}
```

Note: `is_running_element` は CSS前処理で `position: running()` を `display: none` に置換済みなので、`display: none` の要素で かつ `running_names` に含まれるクラス名/ID を持つ要素を検出する。具体的な検出ロジックは以下:

```rust
/// Check if a node is a running element by matching against GCPM running names.
/// Since the CSS preprocessor replaced `position: running(name)` with `display: none`,
/// we identify running elements by checking if they are display:none AND their
/// class/id matches a known running name pattern.
fn is_running_element(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &GcpmContext,
) -> bool {
    if ctx.running_names.is_empty() {
        return false;
    }
    let Some(node) = doc.get_node(node_id) else {
        return false;
    };
    let Some(elem) = node.element_data() else {
        return false;
    };
    // Check if display is none (set by our CSS preprocessor)
    if let Some(styles) = node.primary_styles() {
        let display = styles.clone_display();
        if !display.is_none() {
            return false;
        }
    }
    // Match class names against running_names
    // The parser extracted `position: running(X)` from `.X { ... }` rules,
    // so we need the CSS class → running name mapping from the parser.
    // For now, check if any class matches a running name.
    for class in elem.classes() {
        if ctx.running_names.contains(class) {
            return true;
        }
    }
    false
}

fn get_running_name(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &GcpmContext,
) -> Option<String> {
    let node = doc.get_node(node_id)?;
    let elem = node.element_data()?;
    for class in elem.classes() {
        if ctx.running_names.contains(class) {
            return Some(class.to_string());
        }
    }
    None
}
```

**重要な注意:** `is_running_element` の検出はクラス名マッチングに依存している。これは「`.header { position: running(header); }`」のようにクラス名と running 名が一致する場合のみ動作する。より正確な実装では、パーサーがセレクタ → running 名のマッピングを保持する必要がある。MVP ではクラス名一致で十分とし、Phase 2 でセレクタマッピングに拡張する。

**Step 2: `engine.rs` の呼び出しを更新**

`engine.rs:68` の `dom_to_pageable` 呼び出しを更新:

```rust
// engine.rs render_html() 内
let mut running_store = crate::gcpm::running::RunningElementStore::new();
let root = crate::convert::dom_to_pageable(&doc, None, &mut running_store);
```

GCPM 統合は Task 8 で行うので、ここでは `None` を渡して既存動作を維持。

**Step 3: ビルド確認**

Run: `cargo build -p fulgur 2>&1`
Expected: ビルド成功

**Step 4: 既存テスト通過確認**

Run: `cargo test -p fulgur 2>&1`
Expected: 全テスト PASS (既存の振る舞い変更なし)

**Step 5: コミット**

```bash
git add crates/fulgur/src/convert.rs crates/fulgur/src/engine.rs
git commit -m "feat(gcpm): add running element detection and exclusion in convert.rs"
```

---

### Task 8: `engine.rs` + `render.rs` — GCPM パイプライン統合と2パスレンダリング

**Files:**
- Modify: `crates/fulgur/src/engine.rs:37-69`
- Modify: `crates/fulgur/src/render.rs`

**Step 1: `engine.rs` の `render_html` に GCPM 前処理を統合**

```rust
pub fn render_html(&self, html: &str) -> Result<Vec<u8>> {
    let combined_css = self
        .assets
        .as_ref()
        .map(|a| a.combined_css())
        .unwrap_or_default();

    // GCPM preprocessing
    let gcpm = crate::gcpm::parser::parse_gcpm(&combined_css);

    // Use cleaned CSS (GCPM constructs removed)
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
        crate::render::render_to_pdf_with_gcpm(
            root,
            &self.config,
            &gcpm,
            &running_store,
            fonts,
        )
    }
}
```

**Step 2: `render.rs` に `render_to_pdf_with_gcpm` を追加**

```rust
use crate::gcpm::GcpmContext;
use crate::gcpm::counter::resolve_content_to_html;
use crate::gcpm::running::RunningElementStore;
use std::collections::HashMap;
use std::sync::Arc;

/// Render with GCPM margin boxes (2-pass rendering).
pub fn render_to_pdf_with_gcpm(
    root: Box<dyn Pageable>,
    config: &Config,
    gcpm: &GcpmContext,
    running_store: &RunningElementStore,
    font_data: &[Arc<Vec<u8>>],
) -> Result<Vec<u8>> {
    let content_width = config.content_width();
    let content_height = config.content_height();

    // Pass 1: paginate body content
    let pages = paginate(root, content_width, content_height);
    let total_pages = pages.len();

    let page_size = if config.landscape {
        config.page_size.landscape()
    } else {
        config.page_size
    };

    let running_pairs = running_store.to_pairs();

    // Layout cache: resolved_html → pageable tree
    let mut layout_cache: HashMap<String, Box<dyn Pageable>> = HashMap::new();

    let mut document = krilla::Document::new();

    // Pass 2: render each page with margin boxes
    for (page_idx, page_content) in pages.iter().enumerate() {
        let page_num = page_idx + 1;

        let settings = krilla::page::PageSettings::from_wh(page_size.width, page_size.height)
            .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;
        let mut page = document.start_page_with(settings);
        let mut surface = page.surface();
        let mut canvas = Canvas {
            surface: &mut surface,
        };

        // Draw margin boxes
        for margin_box in &gcpm.margin_boxes {
            let resolved_html = resolve_content_to_html(
                &margin_box.content,
                &running_pairs,
                page_num,
                total_pages,
            );

            if resolved_html.is_empty() {
                continue;
            }

            let rect = margin_box
                .position
                .bounding_rect(page_size, config.margin);

            // Wrap resolved HTML in a basic document for Blitz
            let margin_html = format!(
                "<html><body style=\"margin:0;padding:0;\">{}</body></html>",
                resolved_html
            );

            // Check cache or layout
            let margin_pageable = if let Some(cached) = layout_cache.get(&resolved_html) {
                cached
            } else {
                let margin_doc = crate::blitz_adapter::parse_and_layout(
                    &margin_html,
                    rect.width,
                    rect.height,
                    font_data,
                );
                let mut store = RunningElementStore::new();
                let pageable = crate::convert::dom_to_pageable(&margin_doc, None, &mut store);
                layout_cache.insert(resolved_html.clone(), pageable);
                layout_cache.get(&resolved_html).unwrap()
            };

            // Draw margin box content at its position
            margin_pageable.draw(
                &mut canvas,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
            );
        }

        // Draw body content
        page_content.draw(
            &mut canvas,
            config.margin.left,
            config.margin.top,
            content_width,
            content_height,
        );
    }

    // Set metadata
    let mut metadata = krilla::metadata::Metadata::new();
    if let Some(ref title) = config.title {
        metadata = metadata.title(title.clone());
    }
    if let Some(ref author) = config.author {
        metadata = metadata.authors(vec![author.clone()]);
    }
    document.set_metadata(metadata);

    let pdf_bytes = document
        .finish()
        .map_err(|e| Error::PdfGeneration(format!("{e:?}")))?;
    Ok(pdf_bytes)
}
```

**Step 3: ビルド確認**

Run: `cargo build -p fulgur 2>&1`
Expected: ビルド成功

**Step 4: 既存テスト通過確認**

Run: `cargo test -p fulgur 2>&1`
Expected: 全テスト PASS

**Step 5: コミット**

```bash
git add crates/fulgur/src/engine.rs crates/fulgur/src/render.rs
git commit -m "feat(gcpm): integrate 2-pass rendering pipeline with margin box support"
```

---

### Task 9: 統合テスト — ヘッダー/フッター付き PDF 生成

**Files:**
- Create: `crates/fulgur/tests/gcpm_integration.rs`

**Step 1: 統合テストを作成**

```rust
use fulgur::asset::AssetBundle;
use fulgur::Engine;

#[test]
fn test_gcpm_header_footer_generates_pdf() {
    let mut assets = AssetBundle::new();
    assets.add_css(r#"
        .header { position: running(pageHeader); }
        .footer { position: running(pageFooter); }
        @page {
            @top-center { content: element(pageHeader); }
            @bottom-center { content: element(pageFooter) " - " counter(page) " / " counter(pages); }
        }
    "#);

    let html = r#"
        <html>
        <body>
            <div class="header">Document Title</div>
            <div class="footer">Confidential</div>
            <p>First page content.</p>
        </body>
        </html>
    "#;

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine.render_html(html).expect("should generate PDF");
    assert!(!pdf.is_empty());
    // PDF magic bytes
    assert_eq!(&pdf[..5], b"%PDF-");
}

#[test]
fn test_gcpm_no_gcpm_css_works_as_before() {
    let html = "<html><body><p>Hello world</p></body></html>";
    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("should generate PDF");
    assert!(!pdf.is_empty());
    assert_eq!(&pdf[..5], b"%PDF-");
}

#[test]
fn test_gcpm_multipage_counter() {
    let mut assets = AssetBundle::new();
    assets.add_css(r#"
        @page {
            @bottom-center { content: counter(page) " / " counter(pages); }
        }
    "#);

    // Generate enough content for multiple pages
    let mut body = String::new();
    for i in 0..100 {
        body.push_str(&format!("<p>Paragraph {} with some text content.</p>", i));
    }

    let html = format!("<html><body>{}</body></html>", body);
    let engine = Engine::builder().assets(assets).build();
    let pdf = engine.render_html(&html).expect("should generate PDF");
    assert!(!pdf.is_empty());
}
```

**Step 2: テスト実行**

Run: `cargo test -p fulgur --test gcpm_integration 2>&1`
Expected: PASS

**Step 3: 全テスト通過確認**

Run: `cargo test -p fulgur 2>&1`
Expected: 全テスト PASS

**Step 4: Lint**

Run: `cargo clippy -p fulgur 2>&1`
Expected: warning なし (または既存の warning のみ)

**Step 5: コミット**

```bash
git add crates/fulgur/tests/gcpm_integration.rs
git commit -m "test(gcpm): add integration tests for header/footer with GCPM"
```

---

### Task 10: CLI 対応 — `--header` / `--footer` を GCPM 経由に

**Files:**
- Check: `crates/fulgur-cli/src/main.rs` (CLI の引数定義を確認)

Note: CLI の `--header` / `--footer` オプションは GCPM が CSS で定義されるため不要になる可能性がある。ユーザーは CSS ファイル内に GCPM ルールを記述し、`--css` オプションで指定する。CLI に特別な変更は不要かもしれないが、CLI コードを確認して判断する。

**Step 1: CLI コードを確認**

Run: `cat crates/fulgur-cli/src/main.rs` で確認し、必要に応じて調整。

**Step 2: 必要があればコミット**

---

## 実装上の注意事項

### cssparser バージョン

Cargo.lock 内の cssparser のバージョンを確認し、`Cargo.toml` で指定するバージョンを合わせること。`cargo build` でバージョン競合が出た場合は Cargo.lock のバージョンに合わせる。

### Blitz DOM API

`serialize_node` (Task 6) は Blitz の `Node` API に依存する。`element_data()`, `primary_styles()`, `children` フィールドは既に `convert.rs` で使用されているが、`attrs()` や `classes()` メソッドの正確な API は Blitz のバージョンによる。ビルド時にコンパイルエラーが出た場合は `blitz_dom::Node` の実際の API に合わせて調整する。

### Running 要素の検出精度

Task 7 の `is_running_element` はクラス名と running 名の一致に依存する簡易実装。これは `.header { position: running(header); }` のようなケースでのみ動作する。ID セレクタ (`#header`) や複合セレクタはサポートしない。Phase 2 でパーサーがセレクタ → running 名のマッピングを提供するように拡張する。
