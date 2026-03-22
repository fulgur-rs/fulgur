# Fulgur

An HTML/CSS to PDF converter written in Rust.

Integrates [Blitz](https://github.com/nickelpack/blitz) (HTML parsing, CSS style resolution, layout) with [Krilla](https://github.com/LaurenzV/krilla) (PDF generation) through a pagination-aware layout abstraction.

## Features

- HTML/CSS to PDF conversion
- Automatic page splitting with CSS pagination control (`break-before`, `break-after`, `break-inside`, orphans/widows)
- Text shaping via Parley
- Background colors, borders, and padding
- Deterministic output (same input always produces same PDF)
- Image embedding (PNG / JPEG / GIF)
- Custom font bundling
- External CSS file injection
- Page sizes (A4 / Letter / A3) with landscape support
- PDF metadata (title, author)

## Installation

```bash
cargo install --path crates/fulgur-cli
```

## CLI Usage

```bash
# Convert from file
fulgur render -o output.pdf input.html

# Read from stdin
cat input.html | fulgur render --stdin -o output.pdf

# With options
fulgur render -o output.pdf -s Letter -l --title "My Document" input.html

# Custom fonts and CSS
fulgur render -o output.pdf -f fonts/NotoSansJP.ttf --css style.css input.html

# With images
fulgur render -o output.pdf -i logo.png=assets/logo.png -i photo.png=assets/photo.png input.html
```

### Options

| Option | Description | Default |
|---|---|---|
| `-o, --output` | Output PDF file path (required) | — |
| `-s, --size` | Page size (A4, Letter, A3) | A4 |
| `-l, --landscape` | Landscape orientation | false |
| `--title` | PDF title metadata | — |
| `-f, --font` | Bundle font files (repeatable) | — |
| `--css` | External CSS files (repeatable) | — |
| `-i, --image` | Bundle image files (name=path or path, repeatable) | — |
| `--stdin` | Read HTML from stdin | false |

## Library Usage

```rust
use fulgur::engine::Engine;
use fulgur::asset::AssetBundle;
use fulgur::config::{PageSize, Margin};

// Convert with default settings
let engine = Engine::builder().build();
let pdf = engine.render_html("<h1>Hello</h1>")?;

// Custom configuration
let engine = Engine::builder()
    .page_size(PageSize::A4)
    .margin(Margin::uniform_mm(20.0))
    .title("My Document")
    .build();

let pdf = engine.render_html(html)?;
engine.render_html_to_file(html, "output.pdf")?;

// With images
let mut assets = AssetBundle::new();
assets.add_image_file("logo.png", "assets/logo.png")?;

let engine = Engine::builder()
    .page_size(PageSize::A4)
    .assets(assets)
    .build();

let html = r#"<img src="logo.png" style="display:block;width:100px;height:100px">"#;
let pdf = engine.render_html(html)?;
```

## Architecture

```text
HTML/CSS input
  ↓
Blitz (HTML parse → DOM → style resolution → Taffy layout)
  ↓
DOM → Pageable conversion (BlockPageable / ParagraphPageable / ImagePageable)
  ↓
Pagination (split Pageable tree at page boundaries)
  ↓
Krilla rendering (Pageable.draw() per page → PDF Surface)
  ↓
PDF bytes
```

## Project Structure

```text
crates/
├── fulgur/        # Core library (conversion, layout, rendering)
└── fulgur-cli/    # CLI tool
```

## Development

```bash
# Build
cargo build

# Test
cargo test

# Run CLI directly
cargo run -p fulgur-cli -- render -o output.pdf input.html
```
