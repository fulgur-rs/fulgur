# Fulgur Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build an HTML/CSS-to-PDF library combining Blitz (HTML/CSS engine) and Krilla (PDF generator) via a Pageable pagination abstraction.

**Architecture:** HTML is parsed and laid out by Blitz (blitz-html + blitz-dom + Stylo + Taffy + Parley). The laid-out DOM is converted to a Pageable tree that carries pre-computed sizes. Pageables split themselves across page boundaries, then draw to Krilla's Surface API to produce PDF bytes.

**Tech Stack:** Rust, blitz-html 0.2.0, blitz-dom 0.2.4, krilla 0.6.0, clap 4.x

---

### Task 1: Project Scaffold — Cargo Workspace

**Files:**

- Create: `Cargo.toml` (workspace root)
- Create: `crates/fulgur-core/Cargo.toml`
- Create: `crates/fulgur-core/src/lib.rs`
- Create: `crates/fulgur-cli/Cargo.toml`
- Create: `crates/fulgur-cli/src/main.rs`

**Step 1: Create workspace Cargo.toml**

```toml
# Cargo.toml (root)
[workspace]
resolver = "2"
members = ["crates/fulgur-core", "crates/fulgur-cli"]
```

**Step 2: Create fulgur-core Cargo.toml**

```toml
# crates/fulgur-core/Cargo.toml
[package]
name = "fulgur-core"
version = "0.1.0"
edition = "2024"

[dependencies]
krilla = "0.6.0"
thiserror = "2"
```

Note: We start with krilla only. blitz-html and blitz-dom will be added in Task 4 when we integrate Blitz. This lets us build and test the Pageable abstraction independently first.

**Step 3: Create fulgur-core/src/lib.rs stub**

```rust
// crates/fulgur-core/src/lib.rs
pub mod config;
pub mod error;

pub use config::Config;
pub use error::Error;
```

**Step 4: Create fulgur-cli Cargo.toml**

```toml
# crates/fulgur-cli/Cargo.toml
[package]
name = "fulgur-cli"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "fulgur"
path = "src/main.rs"

[dependencies]
fulgur-core = { path = "../fulgur-core" }
clap = { version = "4", features = ["derive"] }
```

**Step 5: Create fulgur-cli/src/main.rs stub**

```rust
// crates/fulgur-cli/src/main.rs
fn main() {
    println!("fulgur - HTML to PDF converter");
}
```

**Step 6: Verify workspace builds**

Run: `cargo build`
Expected: Compiles successfully with no errors.

**Step 7: Commit**

```bash
git init
git add Cargo.toml crates/ mise.toml docs/
git commit -m "feat: scaffold cargo workspace with fulgur-core and fulgur-cli"
```

---

### Task 2: Config and Error Types

**Files:**

- Create: `crates/fulgur-core/src/config.rs`
- Create: `crates/fulgur-core/src/error.rs`
- Test: `crates/fulgur-core/src/config.rs` (unit tests in module)

**Step 1: Write the failing test for Config**

Add to `crates/fulgur-core/src/config.rs`:

```rust
// crates/fulgur-core/src/config.rs

/// Page size in points (1 point = 1/72 inch)
#[derive(Debug, Clone, Copy)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

impl PageSize {
    pub const A4: Self = Self { width: 595.28, height: 841.89 };
    pub const LETTER: Self = Self { width: 612.0, height: 792.0 };
    pub const A3: Self = Self { width: 841.89, height: 1190.55 };

    pub fn custom(width_mm: f32, height_mm: f32) -> Self {
        Self {
            width: width_mm * 72.0 / 25.4,
            height: height_mm * 72.0 / 25.4,
        }
    }

    pub fn landscape(self) -> Self {
        Self {
            width: self.height,
            height: self.width,
        }
    }
}

/// Margin in points
#[derive(Debug, Clone, Copy)]
pub struct Margin {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Margin {
    pub fn uniform(pt: f32) -> Self {
        Self { top: pt, right: pt, bottom: pt, left: pt }
    }

    pub fn symmetric(vertical: f32, horizontal: f32) -> Self {
        Self { top: vertical, right: horizontal, bottom: vertical, left: horizontal }
    }

    pub fn uniform_mm(mm: f32) -> Self {
        Self::uniform(mm * 72.0 / 25.4)
    }
}

impl Default for Margin {
    fn default() -> Self {
        Self::uniform_mm(20.0)
    }
}

/// PDF generation configuration
#[derive(Debug, Clone)]
pub struct Config {
    pub page_size: PageSize,
    pub margin: Margin,
    pub landscape: bool,
    pub title: Option<String>,
    pub author: Option<String>,
    pub lang: Option<String>,
    pub header_html: Option<String>,
    pub footer_html: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            page_size: PageSize::A4,
            margin: Margin::default(),
            landscape: false,
            title: None,
            author: None,
            lang: None,
            header_html: None,
            footer_html: None,
        }
    }
}

impl Config {
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::default()
    }

    /// Content area width (page width minus left and right margins)
    pub fn content_width(&self) -> f32 {
        let ps = if self.landscape { self.page_size.landscape() } else { self.page_size };
        ps.width - self.margin.left - self.margin.right
    }

    /// Content area height (page height minus top and bottom margins)
    pub fn content_height(&self) -> f32 {
        let ps = if self.landscape { self.page_size.landscape() } else { self.page_size };
        ps.height - self.margin.top - self.margin.bottom
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConfigBuilder {
    config: Config,
}

impl ConfigBuilder {
    pub fn page_size(mut self, size: PageSize) -> Self {
        self.config.page_size = size;
        self
    }

    pub fn margin(mut self, margin: Margin) -> Self {
        self.config.margin = margin;
        self
    }

    pub fn landscape(mut self, landscape: bool) -> Self {
        self.config.landscape = landscape;
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.config.title = Some(title.into());
        self
    }

    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.config.author = Some(author.into());
        self
    }

    pub fn lang(mut self, lang: impl Into<String>) -> Self {
        self.config.lang = Some(lang.into());
        self
    }

    pub fn header_html(mut self, html: impl Into<String>) -> Self {
        self.config.header_html = Some(html.into());
        self
    }

    pub fn footer_html(mut self, html: impl Into<String>) -> Self {
        self.config.footer_html = Some(html.into());
        self
    }

    pub fn build(self) -> Config {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a4_dimensions() {
        let size = PageSize::A4;
        assert!((size.width - 595.28).abs() < 0.01);
        assert!((size.height - 841.89).abs() < 0.01);
    }

    #[test]
    fn test_landscape() {
        let size = PageSize::A4.landscape();
        assert!((size.width - 841.89).abs() < 0.01);
        assert!((size.height - 595.28).abs() < 0.01);
    }

    #[test]
    fn test_content_area() {
        let config = Config::builder()
            .page_size(PageSize::A4)
            .margin(Margin::uniform(72.0)) // 1 inch
            .build();
        assert!((config.content_width() - (595.28 - 144.0)).abs() < 0.01);
        assert!((config.content_height() - (841.89 - 144.0)).abs() < 0.01);
    }

    #[test]
    fn test_content_area_landscape() {
        let config = Config::builder()
            .page_size(PageSize::A4)
            .margin(Margin::uniform(72.0))
            .landscape(true)
            .build();
        // landscape: width=841.89, height=595.28
        assert!((config.content_width() - (841.89 - 144.0)).abs() < 0.01);
        assert!((config.content_height() - (595.28 - 144.0)).abs() < 0.01);
    }

    #[test]
    fn test_custom_mm_size() {
        let size = PageSize::custom(210.0, 297.0); // A4 in mm
        assert!((size.width - 595.28).abs() < 0.2);
        assert!((size.height - 841.89).abs() < 0.2);
    }
}
```

**Step 2: Create error.rs**

```rust
// crates/fulgur-core/src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTML parse error: {0}")]
    HtmlParse(String),

    #[error("Layout error: {0}")]
    Layout(String),

    #[error("PDF generation error: {0}")]
    PdfGeneration(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Asset error: {0}")]
    Asset(String),
}

pub type Result<T> = std::result::Result<T, Error>;
```

**Step 3: Run tests**

Run: `cargo test -p fulgur-core`
Expected: All 5 tests pass.

**Step 4: Commit**

```bash
git add crates/fulgur-core/src/
git commit -m "feat: add Config, PageSize, Margin, and Error types"
```

---

### Task 3: Pageable Trait and BlockPageable

**Files:**

- Create: `crates/fulgur-core/src/pageable.rs`
- Create: `crates/fulgur-core/src/paginate.rs`
- Modify: `crates/fulgur-core/src/lib.rs`

**Step 1: Create the Pageable trait and Pagination types**

```rust
// crates/fulgur-core/src/pageable.rs

/// Point unit (1/72 inch)
pub type Pt = f32;

#[derive(Debug, Clone, Copy)]
pub struct Size {
    pub width: Pt,
    pub height: Pt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakBefore {
    Auto,
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakAfter {
    Auto,
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakInside {
    Auto,
    Avoid,
}

#[derive(Debug, Clone, Copy)]
pub struct Pagination {
    pub break_before: BreakBefore,
    pub break_after: BreakAfter,
    pub break_inside: BreakInside,
    pub orphans: usize,
    pub widows: usize,
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            break_before: BreakBefore::Auto,
            break_after: BreakAfter::Auto,
            break_inside: BreakInside::Auto,
            orphans: 2,
            widows: 2,
        }
    }
}

/// Wrapper around Krilla Surface for drawing commands.
/// This decouples Pageable types from Krilla's concrete Surface type.
pub struct Canvas<'a> {
    pub surface: &'a mut krilla::page::Surface<'a>,
}

/// Core pagination-aware layout trait.
pub trait Pageable: Send + Sync {
    /// Measure size within available area.
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size;

    /// Split at page boundary. Returns None if element fits entirely
    /// or cannot be split.
    fn split(&self, avail_width: Pt, avail_height: Pt)
        -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)>;

    /// Emit drawing commands.
    fn draw(&self, canvas: &mut Canvas, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt);

    /// CSS pagination properties for this element.
    fn pagination(&self) -> Pagination {
        Pagination::default()
    }

    /// Clone this pageable into a boxed trait object.
    fn clone_box(&self) -> Box<dyn Pageable>;

    /// Measured height from last wrap() call.
    fn height(&self) -> Pt;
}

impl Clone for Box<dyn Pageable> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

// ─── BlockPageable ───────────────────────────────────────

/// A block container that stacks children vertically.
/// Handles margin/border/padding/background and page splitting.
#[derive(Clone)]
pub struct BlockPageable {
    pub children: Vec<Box<dyn Pageable>>,
    pub pagination: Pagination,
    pub cached_size: Option<Size>,
}

impl BlockPageable {
    pub fn new(children: Vec<Box<dyn Pageable>>) -> Self {
        Self {
            children,
            pagination: Pagination::default(),
            cached_size: None,
        }
    }

    pub fn with_pagination(mut self, pagination: Pagination) -> Self {
        self.pagination = pagination;
        self
    }
}

impl Pageable for BlockPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        let mut total_height: Pt = 0.0;
        for child in &mut self.children {
            let child_size = child.wrap(avail_width, avail_height - total_height);
            total_height += child_size.height;
        }
        let size = Size { width: avail_width, height: total_height };
        self.cached_size = Some(size);
        size
    }

    fn split(&self, avail_width: Pt, avail_height: Pt)
        -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)>
    {
        if self.pagination.break_inside == BreakInside::Avoid {
            return None;
        }

        let total_height = self.cached_size.map(|s| s.height).unwrap_or(0.0);
        if total_height <= avail_height {
            return None; // Fits entirely
        }

        let mut consumed: Pt = 0.0;
        let mut split_index = self.children.len();

        for (i, child) in self.children.iter().enumerate() {
            let child_h = child.height();

            // Check break-before
            if child.pagination().break_before == BreakBefore::Page && i > 0 && consumed > 0.0 {
                split_index = i;
                break;
            }

            if consumed + child_h > avail_height {
                // Try to split the child itself
                if let Some((first_part, second_part)) = child.split(avail_width, avail_height - consumed) {
                    let mut first_children: Vec<Box<dyn Pageable>> = self.children[..i].iter().map(|c| c.clone_box()).collect();
                    first_children.push(first_part);

                    let mut second_children = vec![second_part];
                    for c in &self.children[i + 1..] {
                        second_children.push(c.clone_box());
                    }

                    return Some((
                        Box::new(BlockPageable::new(first_children).with_pagination(self.pagination)),
                        Box::new(BlockPageable::new(second_children).with_pagination(self.pagination)),
                    ));
                }
                // Can't split child; put it on the next page
                split_index = i;
                break;
            }

            consumed += child_h;

            // Check break-after
            if child.pagination().break_after == BreakAfter::Page {
                split_index = i + 1;
                break;
            }
        }

        if split_index == 0 || split_index == self.children.len() {
            return None; // Can't split meaningfully
        }

        let first_children: Vec<Box<dyn Pageable>> = self.children[..split_index].iter().map(|c| c.clone_box()).collect();
        let second_children: Vec<Box<dyn Pageable>> = self.children[split_index..].iter().map(|c| c.clone_box()).collect();

        Some((
            Box::new(BlockPageable::new(first_children).with_pagination(self.pagination)),
            Box::new(BlockPageable::new(second_children).with_pagination(self.pagination)),
        ))
    }

    fn draw(&self, canvas: &mut Canvas, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        let _ = avail_height;
        let mut current_y = y;
        for child in &self.children {
            child.draw(canvas, x, current_y, avail_width, child.height());
            current_y += child.height();
        }
    }

    fn pagination(&self) -> Pagination {
        self.pagination
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.cached_size.map(|s| s.height).unwrap_or(0.0)
    }
}

// ─── SpacerPageable ──────────────────────────────────────

/// Fixed-height vertical space. Cannot be split.
#[derive(Clone)]
pub struct SpacerPageable {
    pub height: Pt,
}

impl SpacerPageable {
    pub fn new(height: Pt) -> Self {
        Self { height }
    }
}

impl Pageable for SpacerPageable {
    fn wrap(&mut self, avail_width: Pt, _avail_height: Pt) -> Size {
        Size { width: avail_width, height: self.height }
    }

    fn split(&self, _avail_width: Pt, _avail_height: Pt)
        -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)>
    {
        None
    }

    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {
        // Spacers are invisible
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spacer(h: Pt) -> Box<dyn Pageable> {
        let mut s = SpacerPageable::new(h);
        s.wrap(100.0, 1000.0);
        Box::new(s)
    }

    #[test]
    fn test_block_fits_on_one_page() {
        let mut block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        block.wrap(200.0, 300.0);
        assert!(block.split(200.0, 300.0).is_none());
    }

    #[test]
    fn test_block_splits_across_pages() {
        let mut block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        block.wrap(200.0, 1000.0);
        let result = block.split(200.0, 250.0);
        assert!(result.is_some());
        let (first, second) = result.unwrap();
        // First page: 2 spacers (200pt), second page: 1 spacer (100pt)
        let mut first = first;
        let mut second = second;
        let s1 = first.wrap(200.0, 250.0);
        let s2 = second.wrap(200.0, 1000.0);
        assert!((s1.height - 200.0).abs() < 0.01);
        assert!((s2.height - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_break_before_page() {
        let mut s1 = SpacerPageable::new(50.0);
        s1.wrap(200.0, 1000.0);

        let mut s2 = SpacerPageable::new(50.0);
        s2.wrap(200.0, 1000.0);

        // Third child has break-before: page
        let mut block = BlockPageable::new(vec![
            Box::new(s1),
            Box::new(s2.clone()),
            Box::new(SpacerPageable::new(50.0)),
        ]);

        // Manually set break_before on last child - we need a wrapper
        // For this test, use BlockPageable with break_before
        let breaking = BlockPageable::new(vec![make_spacer(50.0)])
            .with_pagination(Pagination {
                break_before: BreakBefore::Page,
                ..Pagination::default()
            });
        let mut breaking = breaking;
        breaking.wrap(200.0, 1000.0);

        let mut block = BlockPageable::new(vec![
            make_spacer(50.0),
            make_spacer(50.0),
            Box::new(breaking),
        ]);
        block.wrap(200.0, 1000.0);

        // Even though everything fits in 1000pt, break-before should force split
        let result = block.split(200.0, 1000.0);
        assert!(result.is_some());
    }

    #[test]
    fn test_break_inside_avoid() {
        let block = BlockPageable::new(vec![make_spacer(200.0)])
            .with_pagination(Pagination {
                break_inside: BreakInside::Avoid,
                ..Pagination::default()
            });
        let mut block = block;
        block.wrap(200.0, 1000.0);
        // Even if it doesn't fit, split returns None
        assert!(block.split(200.0, 100.0).is_none());
    }
}
```

**Step 2: Create paginate.rs**

```rust
// crates/fulgur-core/src/paginate.rs
use crate::pageable::{Pageable, Pt, Size};

/// Split a Pageable tree into per-page fragments.
pub fn paginate(
    mut root: Box<dyn Pageable>,
    page_width: Pt,
    page_height: Pt,
) -> Vec<Box<dyn Pageable>> {
    root.wrap(page_width, page_height);

    let mut pages = vec![];
    let mut remaining = root;

    loop {
        match remaining.split(page_width, page_height) {
            Some((this_page, rest)) => {
                pages.push(this_page);
                remaining = rest;
                // Re-wrap the remaining content
                remaining.wrap(page_width, page_height);
            }
            None => {
                pages.push(remaining);
                break;
            }
        }
    }

    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pageable::{BlockPageable, SpacerPageable};

    fn make_spacer(h: Pt) -> Box<dyn Pageable> {
        let mut s = SpacerPageable::new(h);
        s.wrap(100.0, 1000.0);
        Box::new(s)
    }

    #[test]
    fn test_paginate_single_page() {
        let block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        let pages = paginate(Box::new(block), 200.0, 300.0);
        assert_eq!(pages.len(), 1);
    }

    #[test]
    fn test_paginate_two_pages() {
        let block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        let pages = paginate(Box::new(block), 200.0, 250.0);
        assert_eq!(pages.len(), 2);
    }

    #[test]
    fn test_paginate_three_pages() {
        let block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        // 500pt total, 200pt per page => 3 pages (200, 200, 100)
        let pages = paginate(Box::new(block), 200.0, 200.0);
        assert_eq!(pages.len(), 3);
    }
}
```

**Step 3: Update lib.rs**

```rust
// crates/fulgur-core/src/lib.rs
pub mod config;
pub mod error;
pub mod pageable;
pub mod paginate;

pub use config::{Config, ConfigBuilder, Margin, PageSize};
pub use error::{Error, Result};
```

**Step 4: Run tests**

Run: `cargo test -p fulgur-core`
Expected: All tests pass (config tests + pageable tests + paginate tests).

**Step 5: Commit**

```bash
git add crates/fulgur-core/src/
git commit -m "feat: add Pageable trait, BlockPageable, SpacerPageable, and pagination algorithm"
```

---

### Task 4: Krilla Rendering — Pageable to PDF

**Files:**

- Create: `crates/fulgur-core/src/render.rs`
- Modify: `crates/fulgur-core/src/lib.rs`
- Test: Integration test — produce an actual PDF with colored rectangles

**Step 1: Create render.rs — PDF generation from Pageable pages**

```rust
// crates/fulgur-core/src/render.rs
use crate::config::Config;
use crate::error::{Error, Result};
use crate::pageable::{Canvas, Pageable, Pt};
use crate::paginate::paginate;

/// Render a Pageable tree to PDF bytes.
pub fn render_to_pdf(
    root: Box<dyn Pageable>,
    config: &Config,
) -> Result<Vec<u8>> {
    let content_width = config.content_width();
    let content_height = config.content_height();

    // Paginate
    let pages = paginate(root, content_width, content_height);
    let total_pages = pages.len();

    // Create PDF document
    let mut document = krilla::Document::new();

    let page_size = if config.landscape {
        config.page_size.landscape()
    } else {
        config.page_size
    };

    for (page_num, page_content) in pages.iter().enumerate() {
        let settings = krilla::page::PageSettings::from_wh(page_size.width, page_size.height)
            .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;

        let mut page = document.start_page_with(settings);
        let mut surface = page.surface();

        // Translate to content area (apply margins)
        let transform = krilla::geom::Transform::from_translate(
            config.margin.left,
            config.margin.top,
        );
        surface.push_transform(transform);

        let mut canvas = Canvas { surface: &mut surface };
        page_content.draw(&mut canvas, 0.0, 0.0, content_width, content_height);

        surface.pop();
        surface.finish();
        page.finish();
    }

    // Set metadata
    let mut metadata = krilla::metadata::Metadata::default();
    if let Some(ref title) = config.title {
        metadata.title = Some(title.clone());
    }
    if let Some(ref author) = config.author {
        metadata.author = Some(author.clone());
    }

    document.set_metadata(metadata);

    let pdf_bytes = document.finish().map_err(|e| Error::PdfGeneration(format!("{e:?}")))?;
    Ok(pdf_bytes)
}
```

Note: The exact Krilla API for metadata, transforms, and Surface lifetimes may need adjustment when compiling. The implementor should check `krilla` 0.6.0 docs and adjust types accordingly. The key pattern is: Document -> Page -> Surface -> draw -> finish chain.

**Step 2: Write integration test**

Create `crates/fulgur-core/tests/render_test.rs`:

```rust
// crates/fulgur-core/tests/render_test.rs
use fulgur_core::config::{Config, PageSize, Margin};
use fulgur_core::pageable::{BlockPageable, SpacerPageable};
use fulgur_core::render::render_to_pdf;

#[test]
fn test_render_empty_pdf() {
    let config = Config::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build();

    let root = BlockPageable::new(vec![]);
    let pdf = render_to_pdf(Box::new(root), &config).unwrap();

    // PDF should start with %PDF header
    assert!(pdf.starts_with(b"%PDF"));
    // Should be non-trivially sized
    assert!(pdf.len() > 100);
}

#[test]
fn test_render_multipage_pdf() {
    let config = Config::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build();

    let content_height = config.content_height();

    // Create content taller than one page
    let mut spacers: Vec<Box<dyn fulgur_core::pageable::Pageable>> = Vec::new();
    let spacer_height = content_height / 3.0;
    for _ in 0..7 {
        let mut s = SpacerPageable::new(spacer_height);
        s.wrap(100.0, 1000.0);
        spacers.push(Box::new(s));
    }

    let root = BlockPageable::new(spacers);
    let pdf = render_to_pdf(Box::new(root), &config).unwrap();

    assert!(pdf.starts_with(b"%PDF"));

    // Write to file for manual inspection (optional)
    // std::fs::write("/tmp/fulgur_test_multipage.pdf", &pdf).unwrap();
}
```

**Step 3: Update lib.rs to export render module**

Add `pub mod render;` to lib.rs.

**Step 4: Run tests**

Run: `cargo test -p fulgur-core`
Expected: All tests pass including integration tests. PDF bytes start with `%PDF`.

Note: If Krilla API doesn't exactly match the code above, the implementor should fix compilation errors by checking `krilla` 0.6.0 API. Common adjustments:

- `Surface` lifetime may require different borrowing patterns
- `Transform::from_translate` might be `Transform::translate`
- `Metadata` struct fields may differ
- `Document::set_metadata` may be `Document::with_metadata` or similar

**Step 5: Commit**

```bash
git add crates/fulgur-core/src/render.rs crates/fulgur-core/tests/
git commit -m "feat: add PDF rendering via Krilla - Pageable tree to PDF bytes"
```

---

### Task 5: Minimal Public API (Engine)

**Files:**

- Create: `crates/fulgur-core/src/engine.rs`
- Modify: `crates/fulgur-core/src/lib.rs`
- Test: `crates/fulgur-core/tests/engine_test.rs`

**Step 1: Create engine.rs**

```rust
// crates/fulgur-core/src/engine.rs
use crate::config::{Config, ConfigBuilder, Margin, PageSize};
use crate::error::Result;
use crate::pageable::{BlockPageable, Pageable};
use crate::render::render_to_pdf;
use std::path::Path;

/// Reusable PDF generation engine.
pub struct Engine {
    config: Config,
}

impl Engine {
    pub fn builder() -> EngineBuilder {
        EngineBuilder {
            config_builder: Config::builder(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Render a Pageable tree to PDF bytes.
    pub fn render_pageable(&self, root: Box<dyn Pageable>) -> Result<Vec<u8>> {
        render_to_pdf(root, &self.config)
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

    pub fn build(self) -> Engine {
        Engine {
            config: self.config_builder.build(),
        }
    }
}
```

Note: `render_html` and `render_pdf` (taking HTML strings) will be added in Task 7 after Blitz integration. For now, Engine works with Pageable trees directly.

**Step 2: Write test**

```rust
// crates/fulgur-core/tests/engine_test.rs
use fulgur_core::config::{PageSize, Margin};
use fulgur_core::engine::Engine;
use fulgur_core::pageable::{BlockPageable, SpacerPageable};

#[test]
fn test_engine_render_pageable() {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .title("Test Document")
        .build();

    let mut s = SpacerPageable::new(100.0);
    s.wrap(100.0, 1000.0);
    let root = BlockPageable::new(vec![Box::new(s)]);

    let pdf = engine.render_pageable(Box::new(root)).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
```

**Step 3: Update lib.rs**

```rust
// crates/fulgur-core/src/lib.rs
pub mod config;
pub mod engine;
pub mod error;
pub mod pageable;
pub mod paginate;
pub mod render;

pub use config::{Config, ConfigBuilder, Margin, PageSize};
pub use engine::{Engine, EngineBuilder};
pub use error::{Error, Result};
```

**Step 4: Run tests**

Run: `cargo test -p fulgur-core`
Expected: All tests pass.

**Step 5: Commit**

```bash
git add crates/fulgur-core/
git commit -m "feat: add Engine public API for PDF generation"
```

---

### Task 6: CLI — Basic `render` Subcommand

**Files:**

- Modify: `crates/fulgur-cli/src/main.rs`
- Modify: `crates/fulgur-cli/Cargo.toml`

**Step 1: Implement CLI with clap**

```rust
// crates/fulgur-cli/src/main.rs
use clap::{Parser, Subcommand};
use fulgur_core::config::{Margin, PageSize};
use fulgur_core::engine::Engine;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "fulgur", version, about = "HTML to PDF converter")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Render HTML to PDF (currently: test mode with placeholder content)
    Render {
        /// Output PDF file path
        #[arg(short, long)]
        output: PathBuf,

        /// Page size (A4, Letter, A3)
        #[arg(short, long, default_value = "A4")]
        size: String,

        /// Landscape orientation
        #[arg(short, long, default_value_t = false)]
        landscape: bool,

        /// PDF title
        #[arg(long)]
        title: Option<String>,
    },
}

fn parse_page_size(s: &str) -> PageSize {
    match s.to_uppercase().as_str() {
        "A4" => PageSize::A4,
        "A3" => PageSize::A3,
        "LETTER" => PageSize::LETTER,
        _ => {
            eprintln!("Unknown page size '{}', defaulting to A4", s);
            PageSize::A4
        }
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Render { output, size, landscape, title } => {
            let mut builder = Engine::builder()
                .page_size(parse_page_size(&size))
                .margin(Margin::uniform_mm(20.0))
                .landscape(landscape);

            if let Some(title) = title {
                builder = builder.title(title);
            }

            let engine = builder.build();

            // For now, render a placeholder PDF (no HTML parsing yet)
            let mut spacer = fulgur_core::pageable::SpacerPageable::new(100.0);
            spacer.wrap(100.0, 1000.0);
            let root = fulgur_core::pageable::BlockPageable::new(vec![Box::new(spacer)]);

            match engine.render_pageable_to_file(Box::new(root), &output) {
                Ok(()) => println!("PDF written to {}", output.display()),
                Err(e) => eprintln!("Error: {e}"),
            }
        }
    }
}
```

**Step 2: Test CLI**

Run: `cargo run --bin fulgur -- render -o /tmp/test.pdf --title "Test"`
Expected: "PDF written to /tmp/test.pdf" and the file exists and is a valid PDF.

**Step 3: Commit**

```bash
git add crates/fulgur-cli/
git commit -m "feat: add CLI with render subcommand (placeholder content)"
```

---

### Task 7: Blitz Integration — HTML Parse + Style Resolution + Layout

This is the most complex task. It adds blitz-html and blitz-dom to parse HTML, resolve styles, run Taffy layout, and convert the result into a Pageable tree.

**Files:**

- Modify: `crates/fulgur-core/Cargo.toml` (add blitz dependencies)
- Create: `crates/fulgur-core/src/blitz_adapter.rs`
- Create: `crates/fulgur-core/src/convert.rs`
- Modify: `crates/fulgur-core/src/engine.rs` (add render_html method)
- Modify: `crates/fulgur-core/src/lib.rs`
- Test: `crates/fulgur-core/tests/html_test.rs`

**Step 1: Add blitz dependencies to Cargo.toml**

Add to `crates/fulgur-core/Cargo.toml`:

```toml
[dependencies]
krilla = "0.6.0"
blitz-html = "0.2"
blitz-dom = "0.2"
thiserror = "2"
```

Note: blitz-dom may pull in heavy dependencies (Stylo, Taffy, Parley). Expect long first compile.

**Step 2: Create blitz_adapter.rs — thin wrapper over Blitz APIs**

```rust
// crates/fulgur-core/src/blitz_adapter.rs
//!
//! Thin adapter over Blitz APIs. All Blitz-specific code is isolated here
//! so that upstream API changes only require changes in this module.

use blitz_dom::{BaseDocument, DocumentConfig};
use blitz_html::HtmlDocument;

/// Parse HTML and return a fully resolved document (styles + layout computed).
pub fn parse_and_layout(
    html: &str,
    viewport_width: f32,
    viewport_height: f32,
) -> HtmlDocument {
    let config = DocumentConfig::default();
    let mut doc = HtmlDocument::from_html(html, config);

    // Set viewport size for layout
    doc.set_viewport_size(viewport_width, viewport_height);

    // Resolve styles (Stylo) and layout (Taffy)
    doc.resolve(0.0); // 0.0 = animation time

    doc
}
```

Note: The exact API may differ. The implementor should check:

- `HtmlDocument::from_html` signature
- How to set viewport dimensions
- How `resolve()` is called (it may be on `BaseDocument` rather than `HtmlDocument`)
- Whether `HtmlDocument` derefs to `BaseDocument`

Adjust the adapter accordingly. The key goal: this module is the ONLY place that imports `blitz_*` types.

**Step 3: Create convert.rs — DOM tree to Pageable tree conversion**

```rust
// crates/fulgur-core/src/convert.rs
//!
//! Convert a Blitz DOM (after style resolution + layout) into a Pageable tree.

use crate::pageable::{BlockPageable, SpacerPageable, Pageable, Pagination, BreakBefore, BreakAfter, BreakInside};
use blitz_html::HtmlDocument;

/// Convert a resolved Blitz document into a Pageable tree.
///
/// This is the initial, minimal implementation that treats every block-level
/// element as a BlockPageable and every leaf node as a SpacerPageable with
/// the height from Taffy's layout.
///
/// Later tasks will add ParagraphPageable (with text rendering),
/// ImagePageable, TablePageable, etc.
pub fn dom_to_pageable(doc: &HtmlDocument) -> Box<dyn Pageable> {
    // Get root element node ID
    // Walk the DOM tree, converting each node to a Pageable

    // For initial implementation: walk the Taffy layout tree and
    // create BlockPageable/SpacerPageable based on computed sizes.
    // This won't render text yet, but will produce correctly-sized
    // and correctly-paginated block structure.

    let root_id = doc.root_element().id;
    convert_node(doc, root_id)
}

fn convert_node(doc: &HtmlDocument, node_id: usize) -> Box<dyn Pageable> {
    let node = doc.get_node(node_id).unwrap();
    let layout = node.final_layout;
    let height = layout.size.height;

    // Check for children
    let children: Vec<usize> = node.children.clone();

    if children.is_empty() {
        // Leaf node — create a spacer with the computed height
        let mut spacer = SpacerPageable::new(height);
        spacer.wrap(layout.size.width, height);
        return Box::new(spacer);
    }

    // Container node — recurse into children
    let child_pageables: Vec<Box<dyn Pageable>> = children
        .iter()
        .map(|&child_id| convert_node(doc, child_id))
        .collect();

    let mut block = BlockPageable::new(child_pageables);
    // TODO: Extract pagination CSS properties from computed styles
    // let styles = node.primary_styles();
    // Extract break-before, break-after, break-inside from styles
    block.wrap(layout.size.width, 10000.0);
    Box::new(block)
}
```

Note: The exact node access API will need adjustment:

- `doc.root_element()` may return different types
- `node.children` may be accessed differently
- `node.final_layout` field names may differ
- Node IDs may be a different type than `usize`

The implementor should explore the blitz-dom API and adjust. The pattern is: walk DOM tree → read Taffy layout sizes → create corresponding Pageable nodes.

**Step 4: Add render_html to Engine**

Add to `crates/fulgur-core/src/engine.rs`:

```rust
    /// Render HTML string to PDF bytes.
    pub fn render_html(&self, html: &str) -> Result<Vec<u8>> {
        let doc = crate::blitz_adapter::parse_and_layout(
            html,
            self.config.content_width(),
            self.config.content_height(),
        );
        let root = crate::convert::dom_to_pageable(&doc);
        self.render_pageable(root)
    }

    /// Render HTML string to a PDF file.
    pub fn render_html_to_file(
        &self,
        html: &str,
        path: impl AsRef<Path>,
    ) -> Result<()> {
        let pdf = self.render_html(html)?;
        std::fs::write(path, pdf)?;
        Ok(())
    }
```

**Step 5: Add top-level convenience function to lib.rs**

```rust
/// Convert HTML to PDF with default settings.
pub fn convert_html(html: &str) -> Result<Vec<u8>> {
    let engine = Engine::builder().build();
    engine.render_html(html)
}
```

**Step 6: Write integration test**

```rust
// crates/fulgur-core/tests/html_test.rs
use fulgur_core::config::{PageSize, Margin};
use fulgur_core::engine::Engine;

#[test]
fn test_render_simple_html() {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build();

    let html = "<html><body><h1>Hello World</h1><p>This is a test.</p></body></html>";
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 100);
}

#[test]
fn test_convert_html_convenience() {
    let pdf = fulgur_core::convert_html("<h1>Test</h1>").unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
```

**Step 7: Update lib.rs**

```rust
pub mod blitz_adapter;
pub mod config;
pub mod convert;
pub mod engine;
pub mod error;
pub mod pageable;
pub mod paginate;
pub mod render;

pub use config::{Config, ConfigBuilder, Margin, PageSize};
pub use engine::{Engine, EngineBuilder};
pub use error::{Error, Result};

/// Convert HTML to PDF with default settings.
pub fn convert_html(html: &str) -> Result<Vec<u8>> {
    let engine = Engine::builder().build();
    engine.render_html(html)
}
```

**Step 8: Run tests**

Run: `cargo test -p fulgur-core`
Expected: All tests pass. The HTML tests produce valid PDF files (block structure only, no visible text yet).

**Step 9: Commit**

```bash
git add crates/fulgur-core/
git commit -m "feat: integrate Blitz for HTML parsing and layout - DOM to Pageable conversion"
```

---

### Task 8: Update CLI to Accept HTML Input

**Files:**

- Modify: `crates/fulgur-cli/src/main.rs`

**Step 1: Update CLI to read HTML files**

```rust
// Update the Render command to accept an HTML input file
Commands::Render {
    /// Input HTML file (omit for --stdin)
    #[arg()]
    input: Option<PathBuf>,

    /// Read HTML from stdin
    #[arg(long)]
    stdin: bool,

    // ... existing fields ...
}
```

Add HTML reading logic:

```rust
let html = if stdin {
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
        .expect("Failed to read stdin");
    buf
} else if let Some(input) = input {
    std::fs::read_to_string(&input)
        .unwrap_or_else(|e| { eprintln!("Error reading {}: {e}", input.display()); std::process::exit(1); })
} else {
    eprintln!("Error: provide an input HTML file or use --stdin");
    std::process::exit(1);
};

match engine.render_html_to_file(&html, &output) {
    Ok(()) => println!("PDF written to {}", output.display()),
    Err(e) => eprintln!("Error: {e}"),
}
```

**Step 2: Test CLI with HTML file**

Run: `echo '<h1>Hello</h1><p>World</p>' > /tmp/test.html && cargo run --bin fulgur -- render /tmp/test.html -o /tmp/test.pdf`
Expected: "PDF written to /tmp/test.pdf"

**Step 3: Commit**

```bash
git add crates/fulgur-cli/
git commit -m "feat: CLI accepts HTML file input and stdin"
```

---

### Task 9: Text Rendering — ParagraphPageable with Krilla draw_glyphs

This task adds actual text rendering. It reads Parley's text shaping results from Blitz and renders them via Krilla's `draw_glyphs`.

**Files:**

- Create: `crates/fulgur-core/src/pageable/paragraph.rs`
- Modify: `crates/fulgur-core/src/pageable.rs` (refactor to mod directory)
- Modify: `crates/fulgur-core/src/convert.rs`
- Modify: `crates/fulgur-core/Cargo.toml` (may need parley dependency)
- Test: `crates/fulgur-core/tests/text_test.rs`

This task is intentionally less prescriptive because the exact API bridge between Parley's glyph runs and Krilla's `draw_glyphs` requires careful type mapping that depends on the specific versions of both libraries. The implementor should:

**Step 1: Study Parley's glyph run types**

In blitz-dom, after layout, inline root nodes contain `inline_layout_data` with a Parley `Layout`. Iterate:

- `layout.lines()` → each line
- `line.items()` → `PositionedLayoutItem::GlyphRun(run)`
- `run.run().font()` → font info
- `run.run().glyphs()` → glyph IDs and advances

**Step 2: Study Krilla's draw_glyphs types**

`surface.draw_glyphs(start_point, &glyphs, font, text, font_size, outlined)`

- `KrillaGlyph::new(glyph_id, x_advance, y_advance, x_offset, y_offset, text_range, cluster)`
- `Font::new(data: Arc<Vec<u8>>, index: u32)`

**Step 3: Build the font bridge**

Create a mapping layer that:

1. Takes Parley's font reference → extracts the font file data
2. Creates a Krilla `Font` from the same data (shared via `Arc`)
3. Caches font mappings to avoid re-creating Krilla fonts

**Step 4: Implement ParagraphPageable**

```rust
pub struct ParagraphPageable {
    /// Pre-shaped lines from Parley
    lines: Vec<ShapedLine>,
    pagination: Pagination,
    cached_height: f32,
}

struct ShapedLine {
    height: f32,
    baseline: f32,
    glyph_runs: Vec<ShapedGlyphRun>,
}

struct ShapedGlyphRun {
    font_data: Arc<Vec<u8>>,
    font_index: u32,
    font_size: f32,
    color: [u8; 4], // RGBA
    glyphs: Vec<GlyphInfo>,
    text: String,
}

struct GlyphInfo {
    id: u16,
    x_advance: f32,
    x_offset: f32,
    y_offset: f32,
    text_range: std::ops::Range<usize>,
}
```

**Step 5: Implement split() with orphans/widows**

Split at line boundaries. Respect `orphans` (min lines on first page) and `widows` (min lines on second page).

**Step 6: Implement draw() — emit Krilla draw_glyphs**

For each line, for each glyph run:

1. Create Krilla `Font` from cached font data
2. Convert `GlyphInfo` → `KrillaGlyph`
3. Call `surface.draw_glyphs(point, &glyphs, font, text, size, false)`

**Step 7: Update convert.rs to detect text nodes**

When a node is an inline root (`node.flags.is_inline_root()`), read `inline_layout_data` and create `ParagraphPageable` instead of `SpacerPageable`.

**Step 8: Test**

```rust
#[test]
fn test_render_html_with_text() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = "<html><body><h1>Hello World</h1><p>This is fulgur.</p></body></html>";
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    // PDF should be larger than empty doc due to font embedding
    assert!(pdf.len() > 1000);

    // Optional: write for manual inspection
    // std::fs::write("/tmp/fulgur_text_test.pdf", &pdf).unwrap();
}
```

**Step 9: Commit**

```bash
git add crates/fulgur-core/
git commit -m "feat: add ParagraphPageable with text rendering via Parley->Krilla bridge"
```

---

### Task 10: Background and Border Rendering in BlockPageable

**Files:**

- Modify: `crates/fulgur-core/src/pageable.rs` (or `pageable/block.rs`)
- Modify: `crates/fulgur-core/src/convert.rs`

**Step 1: Add style fields to BlockPageable**

```rust
pub struct BlockStyle {
    pub background_color: Option<[u8; 4]>,  // RGBA
    pub border_color: [u8; 4],
    pub border_widths: [f32; 4],            // top, right, bottom, left
    pub border_radius: [f32; 4],            // top-left, top-right, bottom-right, bottom-left
    pub padding: [f32; 4],                  // top, right, bottom, left
}
```

**Step 2: Update draw() to render backgrounds and borders**

In `BlockPageable::draw()`, before drawing children:

1. Draw background rectangle (if background_color is set) using `surface.set_fill()` + `surface.draw_path()`
2. Draw border lines using `surface.set_stroke()` + `surface.draw_path()`

**Step 3: Update convert.rs to extract styles from Stylo**

Read `node.primary_styles()` and extract:

- `background_color` → `style.clone_background_color()`
- `border-*` → `style.clone_border_*_width()`, `style.clone_border_*_color()`
- `padding-*` → `style.clone_padding_*().to_px()`

**Step 4: Test**

```rust
#[test]
fn test_render_styled_html() {
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let html = r#"<html><body>
        <div style="background-color: lightblue; padding: 20px; border: 2px solid navy;">
            <h1>Styled Content</h1>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
```

**Step 5: Commit**

```bash
git add crates/fulgur-core/
git commit -m "feat: render background colors and borders in BlockPageable"
```

---

### Task 11: Image Rendering — ImagePageable

**Files:**

- Create: `crates/fulgur-core/src/pageable/image.rs` (or add to pageable.rs)
- Modify: `crates/fulgur-core/src/convert.rs`

**Step 1: Implement ImagePageable**

```rust
pub struct ImagePageable {
    image_data: Arc<Vec<u8>>,
    width: f32,
    height: f32,
    // image format detected from data
}
```

**Step 2: draw() — use Krilla's Image API**

```rust
fn draw(&self, canvas: &mut Canvas, x: f32, y: f32, ...) {
    let image = krilla::graphics::image::Image::from_png(self.image_data.clone(), true);
    // or from_jpeg depending on format
    canvas.surface.push_transform(Transform::from_translate(x, y));
    canvas.surface.draw_image(image, Size::from_wh(self.width, self.height).unwrap());
    canvas.surface.pop();
}
```

**Step 3: Update convert.rs to handle `<img>` elements**

Detect `<img>` tags, read the `src` attribute, load from AssetBundle or file path, create ImagePageable.

**Step 4: Commit**

```bash
git add crates/fulgur-core/
git commit -m "feat: add ImagePageable for rendering images in PDF"
```

---

### Task 12: AssetBundle

**Files:**

- Create: `crates/fulgur-core/src/asset.rs`
- Modify: `crates/fulgur-core/src/engine.rs`
- Modify: `crates/fulgur-core/src/lib.rs`

**Step 1: Implement AssetBundle**

```rust
// crates/fulgur-core/src/asset.rs
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub struct AssetBundle {
    pub css: Vec<String>,
    pub fonts: Vec<Arc<Vec<u8>>>,
    pub images: HashMap<String, Arc<Vec<u8>>>,
}

impl AssetBundle {
    pub fn new() -> Self {
        Self {
            css: Vec::new(),
            fonts: Vec::new(),
            images: HashMap::new(),
        }
    }

    pub fn add_css(&mut self, css: impl Into<String>) {
        self.css.push(css.into());
    }

    pub fn add_css_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let css = std::fs::read_to_string(path)?;
        self.css.push(css);
        Ok(())
    }

    pub fn add_font_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let data = std::fs::read(path)?;
        self.fonts.push(Arc::new(data));
        Ok(())
    }

    pub fn add_image_file(&mut self, name: impl Into<String>, path: impl AsRef<Path>) -> Result<()> {
        let data = std::fs::read(path)?;
        self.images.insert(name.into(), Arc::new(data));
        Ok(())
    }
}
```

**Step 2: Wire into Engine**

Add `assets: Option<AssetBundle>` to `Engine` and pass CSS/fonts to Blitz during parsing.

**Step 3: Test and commit**

```bash
git add crates/fulgur-core/
git commit -m "feat: add AssetBundle for CSS, fonts, and images"
```

---

## Summary

| Task | What it delivers | Key files |
|---|---|---|
| 1 | Cargo workspace scaffold | `Cargo.toml`, both crate stubs |
| 2 | Config + Error types | `config.rs`, `error.rs` |
| 3 | Pageable trait + BlockPageable + pagination | `pageable.rs`, `paginate.rs` |
| 4 | Krilla PDF rendering | `render.rs` |
| 5 | Engine public API | `engine.rs` |
| 6 | CLI render subcommand | `main.rs` |
| 7 | Blitz integration (HTML → Pageable) | `blitz_adapter.rs`, `convert.rs` |
| 8 | CLI accepts HTML input | `main.rs` |
| 9 | Text rendering (Parley → Krilla) | `paragraph.rs` |
| 10 | Background/border rendering | `pageable.rs`, `convert.rs` |
| 11 | Image rendering | `image.rs` |
| 12 | AssetBundle | `asset.rs` |

After Task 8, you have a working end-to-end pipeline: `fulgur render input.html -o output.pdf` (block structure only). After Task 9, text is visible. After Task 10-12, the MVP is functionally complete with styled blocks, images, and asset management.
