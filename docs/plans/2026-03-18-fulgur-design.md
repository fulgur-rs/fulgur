# Fulgur: HTML/CSS to PDF Conversion Library — Design Document

## Overview

Fulgur is a Rust library and CLI tool for converting HTML/CSS to PDF. It combines Blitz (HTML/CSS rendering engine) with Krilla (PDF generation library) through a pagination-aware layout abstraction called "Pageable."

**Target use cases:**

- Server-side report/invoice generation from HTML templates
- Document conversion (Markdown/HTML to book/manual-style PDF)

**Design principles:**

- Offline-first: no network access, all assets explicitly registered
- Deterministic: same input always produces the same output
- Engine reuse: configure once, render many

## Architecture

### Processing Pipeline

```text
HTML + CSS (string)
  -> blitz-html (parse -> DOM)
  -> blitz-dom + Stylo (style resolution -> computed styles)
  -> Taffy + Parley (layout computation -> sizes & positions)
  -> Pageable tree conversion (carry Taffy's computed layout)
     - wrap()  -> return Taffy's pre-computed sizes
     - split() -> page break decisions based on known child sizes
     - draw()  -> emit drawing commands to Krilla
  -> Krilla Document -> PDF bytes
```

### Project Structure

```text
fulgur/
├── crates/
│   ├── fulgur-core/
│   │   ├── src/
│   │   │   ├── lib.rs            # Public API (Engine, Config, AssetBundle)
│   │   │   ├── config.rs         # PDF generation settings
│   │   │   ├── pageable.rs       # Pageable trait and concrete types
│   │   │   ├── paginate.rs       # Page splitting algorithm
│   │   │   ├── compose.rs        # PageComposer (header/footer/content)
│   │   │   ├── convert.rs        # DOM -> Pageable tree conversion
│   │   │   ├── render.rs         # Pageable -> Krilla Surface rendering
│   │   │   └── blitz_adapter.rs  # Thin adapter over Blitz APIs
│   │   └── Cargo.toml
│   └── fulgur-cli/
│       ├── src/main.rs
│       └── Cargo.toml
└── Cargo.toml                    # Workspace
```

### Dependencies

```text
fulgur-core
├── blitz-html          # HTML parsing
├── blitz-dom           # DOM + Stylo style resolution + Taffy layout
├── parley              # Text shaping (via blitz-dom)
├── krilla              # PDF generation
└── taffy               # (indirect via blitz-dom)

fulgur-cli
├── fulgur-core
└── clap                # CLI argument parsing
```

## Public API (Rust Library)

```rust
// Minimal usage
let pdf_bytes = fulgur::convert_html("<h1>Hello</h1>")?;
std::fs::write("output.pdf", pdf_bytes)?;

// Engine with configuration (reusable)
let engine = fulgur::Engine::builder()
    .page_size("A4")
    .margin("20mm")
    .build()?;

let pdf_bytes = engine.render_pdf(html)?;

// Full configuration with asset bundle
let mut assets = fulgur::AssetBundle::new();
assets.add_css_file("styles/base.css")?;
assets.add_font_file("fonts/NotoSansJP-Regular.ttf")?;
assets.add_image_file("logo.png")?;

let engine = fulgur::Engine::builder()
    .page_size("210mm x 297mm")
    .margin("15mm 20mm")          // CSS-style shorthand
    .assets(assets)
    .header_html("<div>Header</div>")
    .footer_html("<div>Page {{page}} / {{pages}}</div>")
    .build()?;

// Single render
let pdf = engine.render_pdf(html)?;
engine.render_pdf_to_file(html, "output.pdf")?;

// Batch: process multiple documents efficiently with same config
let results: Vec<Vec<u8>> = engine.render_batch(&[html1, html2, html3])?;
```

### Engine::builder() Configuration

| Category | Setting | Example |
|---|---|---|
| Page | `page_size`, `margin`, `landscape` | `"A4"`, `"20mm"`, `true` |
| Metadata | `title`, `author`, `lang` | `"Invoice"`, `"John"`, `"ja"` |
| Header/Footer | `header_html`, `footer_html` | Template vars: `{{page}}`, `{{pages}}` |
| Assets | `AssetBundle` | `.add_font_file()`, `.add_css_file()` |

## Pageable Abstraction

The core layout/pagination abstraction. Each element knows how to measure, split across pages, and draw itself.

```rust
pub trait Pageable: Send + Sync {
    /// Measure size within available area
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size;

    /// Split at page boundary. None = cannot split or fits entirely
    fn split(&self, avail_width: Pt, avail_height: Pt)
        -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)>;

    /// Emit drawing commands
    fn draw(&self, canvas: &mut KrillaCanvas, x: Pt, y: Pt,
            avail_width: Pt, avail_height: Pt);

    /// CSS pagination properties
    fn pagination(&self) -> Pagination;

    fn clone_box(&self) -> Box<dyn Pageable>;
}
```

### Concrete Pageable Types (Phase 1)

| Type | Source HTML | Role |
|---|---|---|
| `BlockPageable` | `<div>`, `<section>`, etc. | Block box. margin/border/padding/background. Vertical stacking of children |
| `ParagraphPageable` | `<p>`, text nodes | Text line wrapping & drawing. orphans/widows support |
| `ImagePageable` | `<img>` | Image drawing |
| `TablePageable` | `<table>` | Table layout with header repetition across pages |
| `ListItemPageable` | `<li>` | Marker + body |
| `SpacerPageable` | `<br>`, margin | Vertical space |

### Hybrid Layout Strategy (Taffy + Pageable)

Taffy handles the actual layout computation (block, flexbox, etc.). Pageable carries the pre-computed results and handles pagination on top:

- **wrap()**: Returns Taffy's pre-computed size as-is
- **split()**: Uses known child sizes to decide page break points. No re-layout needed
- **draw()**: Uses known positions to render to Krilla

**Re-layout after split:**

- **Block containers**: Child sizes are known; just decide "which children fit on this page"
- **Paragraph text**: Keep Parley's line layout results (line heights); split at line boundaries
- **Tables**: Row heights are known; split at row boundaries; add header repetition drawing

## Page Splitting Algorithm

```rust
fn paginate(
    root: Box<dyn Pageable>,
    page_width: Pt,
    page_height: Pt,  // content area height (after margin)
) -> Vec<Box<dyn Pageable>> {
    let mut pages = vec![];
    let mut remaining = root;

    loop {
        remaining.wrap(page_width, page_height);
        match remaining.split(page_width, page_height) {
            Some((this_page, rest)) => {
                pages.push(this_page);
                remaining = rest;
            }
            None => {
                pages.push(remaining);
                break;
            }
        }
    }
    pages
}
```

### Pagination CSS Properties

```rust
pub struct Pagination {
    pub break_before: BreakBefore,  // Auto | Page
    pub break_after: BreakAfter,    // Auto | Page
    pub break_inside: BreakInside,  // Auto | Avoid
    pub orphans: usize,             // default 2, min 1
    pub widows: usize,              // default 2, min 1
}
```

### Header/Footer Composition

```rust
struct PageComposer {
    config: Config,
    header: Option<Box<dyn Pageable>>,
    footer: Option<Box<dyn Pageable>>,
}
```

Two-pass rendering: first paginate all content to determine total page count, then draw (to resolve `{{pages}}`).

## CSS Paged Media Roadmap

### Phase 1 — MVP

| Feature | CSS Property | Description |
|---|---|---|
| Page size & margin | `@page { size; margin }` | A4, Letter, custom sizes |
| Break control | `break-before`, `break-after`, `break-inside` | `page`, `avoid`, `auto` + legacy `page-break-*` |
| Text split quality | `orphans`, `widows` | Minimum line count control |
| Print media | `@media print` | Print-only style application |
| Background printing | `print-color-adjust: exact` | Preserve background colors/images |
| Page counter | `counter(page)`, `counter(pages)` | Page numbering |
| Header/footer | `{{page}}`, `{{pages}}` template | API-configured (pre-CSS Paged Media) |

### Phase 2 — Page Margin Boxes & Named Pages

| Feature | CSS Property | Description |
|---|---|---|
| Margin boxes | `@top-left`, `@bottom-center`, etc. (16 slots) | CSS-based headers/footers |
| Page pseudo-classes | `@page :first`, `:left`, `:right` | Spread & cover styling |
| Named pages | `page` property | Mix different page sizes/layouts |
| Page orientation | `page-orientation` | Per-page rotation |
| Box decoration break | `box-decoration-break` | `slice` / `clone` |

### Phase 3 — GCPM (Advanced Typesetting)

| Feature | CSS Property | Description |
|---|---|---|
| Dynamic headers | `string-set`, `string()` | Auto-capture chapter titles for headers |
| Running elements | `running()`, `element()` | Place elements into headers/footers |
| PDF bookmarks | `bookmark-level`, `bookmark-label`, `bookmark-state` | Table of contents navigation |
| TOC page numbers | `target-counter()` | Auto-insert link target's page number |
| Dot leaders | `leader()` | TOC dot leaders |

### Phase 4 — Commercial Print & Accessibility

| Feature | CSS Property | Description |
|---|---|---|
| Crop marks & bleed | `marks`, `bleed` | For commercial printing |
| Footnotes | `float: footnote`, etc. | Academic/book use |
| Tagged PDF | Krilla tag API | PDF/UA accessibility |
| PDF/A | Krilla settings | Long-term archival |

## CLI Design

```text
fulgur render input.html -o output.pdf
fulgur render input.html --size A4 --margin "20mm" -o output.pdf
fulgur render input.html --landscape --css styles.css -o output.pdf
fulgur render --stdin -o output.pdf
fulgur render input.html -o -

fulgur batch *.html --outdir ./pdfs/
fulgur batch manifest.json

fulgur --version
fulgur info output.pdf
```

### `render` Subcommand Flags

| Flag | Short | Description |
|---|---|---|
| `--output` | `-o` | Output destination (file or `-` for stdout) |
| `--size` | `-s` | Page size (`A4`, `Letter`, `210mm x 297mm`) |
| `--margin` | `-m` | Margins (CSS-style shorthand) |
| `--landscape` | `-l` | Landscape orientation |
| `--css` | `-c` | External CSS file(s) |
| `--font-dir` | | Font directory |
| `--header` | | Header HTML string |
| `--footer` | | Footer HTML string |
| `--title` | | PDF title |
| `--author` | | PDF author |

### Batch Manifest Example

```json
{
  "defaults": { "size": "A4", "css": ["base.css"] },
  "documents": [
    { "input": "invoice_001.html", "output": "invoice_001.pdf" },
    { "input": "invoice_002.html", "output": "invoice_002.pdf", "css": ["invoice.css"] }
  ]
}
```

## Technical Risks & Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Blitz internal API instability | Blitz is under active development; DOM/style access APIs may change | Thin adapter layer (`blitz_adapter.rs`) to isolate Blitz API dependency |
| Stylo to Pageable type conversion complexity | Stylo computed styles have massive property sets | Extract only needed properties into `ResolvedStyle` intermediate type |
| Text shaping handoff | Need to pass Parley glyph results to Krilla's `draw_glyphs` | Font ID and glyph ID mapping layer; Parley `Font` -> Krilla `Font` conversion |
| Font double-loading | Parley (for shaping) and Krilla (for embedding) read the same fonts | Share font binary via `Arc<Vec<u8>>` |

## Explicit Non-Goals

- JavaScript execution
- Automatic network resource fetching
- Interactive PDF (forms, etc.)
