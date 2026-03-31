# Fulgur

[![Coverage](https://raw.githubusercontent.com/mitsuru/octocovs/main/badges/mitsuru/fulgur/coverage.svg)](https://github.com/mitsuru/octocovs)
[![Code to Test Ratio](https://raw.githubusercontent.com/mitsuru/octocovs/main/badges/mitsuru/fulgur/ratio.svg)](https://github.com/mitsuru/octocovs)
[![Test Execution Time](https://raw.githubusercontent.com/mitsuru/octocovs/main/badges/mitsuru/fulgur/time.svg)](https://github.com/mitsuru/octocovs)

A modern, lightweight alternative to wkhtmltopdf. Converts HTML/CSS to PDF without a browser engine.

Built in Rust for server-side workloads where memory footprint and startup time matter.

## Why Fulgur?

- **No browser required** — No Chromium, no WebKit, no headless browser. Single binary, instant cold start.
- **Low memory footprint** — Designed for server-side batch processing without blowing up your container's memory limits.
- **Deterministic output** — Same input always produces the same PDF, byte for byte. Safe for CI/CD and automated pipelines.
- **Template + JSON data** — Feed an HTML template and a JSON file to generate PDFs at scale. Built-in [MiniJinja](https://github.com/mitsuhiko/minijinja) engine.
- **Offline by design** — No network access. All assets (fonts, images, CSS) are explicitly bundled.

## Features

- HTML/CSS to PDF conversion with automatic page splitting
- CSS pagination control (`break-before`, `break-after`, `break-inside`, orphans/widows)
- CSS Generated Content for Paged Media (page counters, running headers/footers, margin boxes)
- Template engine with JSON data binding (MiniJinja)
- Image embedding (PNG / JPEG / GIF)
- Custom font bundling with subsetting
- External CSS injection
- Page sizes (A4 / Letter / A3) with landscape support
- PDF metadata (title, author, keywords, language)
- PDF bookmarks from heading structure

## Installation

```bash
cargo install --path crates/fulgur-cli
```

## CLI Usage

```bash
# Basic conversion
fulgur render -o output.pdf input.html

# Read from stdin
cat input.html | fulgur render --stdin -o output.pdf

# Page options
fulgur render -o output.pdf -s Letter -l --margin "20 30" input.html

# Custom fonts and CSS
fulgur render -o output.pdf -f fonts/NotoSansJP.ttf --css style.css input.html

# Images
fulgur render -o output.pdf -i logo.png=assets/logo.png input.html

# Template + JSON data
fulgur render -o invoice.pdf -d data.json template.html
```

### Template Example

`template.html`:

```html
<h1>Invoice #{{ invoice_number }}</h1>
<p>{{ customer_name }}</p>
<table>
  {% for item in items %}
  <tr><td>{{ item.name }}</td><td>{{ item.price }}</td></tr>
  {% endfor %}
</table>
```

`data.json`:

```json
{
  "invoice_number": "2026-001",
  "customer_name": "Acme Corp",
  "items": [
    { "name": "Widget", "price": "$10.00" },
    { "name": "Gadget", "price": "$25.00" }
  ]
}
```

### Options

| Option | Description | Default |
|---|---|---|
| `-o, --output` | Output PDF file path (required, use `-` for stdout) | — |
| `-s, --size` | Page size (A4, Letter, A3) | A4 |
| `-l, --landscape` | Landscape orientation | false |
| `--margin` | Page margins in mm (CSS shorthand: `"20"`, `"20 30"`, `"10 20 30 40"`) | — |
| `--title` | PDF title metadata | — |
| `-f, --font` | Font files to bundle (repeatable) | — |
| `--css` | CSS files to include (repeatable) | — |
| `-i, --image` | Image files to bundle as name=path (repeatable) | — |
| `-d, --data` | JSON data file for template mode (use `-` for stdin) | — |
| `--stdin` | Read HTML from stdin | false |

## Library Usage

```rust
use fulgur::engine::Engine;
use fulgur::config::{PageSize, Margin};

// Basic conversion
let engine = Engine::builder().build();
let pdf = engine.render_html("<h1>Hello</h1>")?;

// With page options
let engine = Engine::builder()
    .page_size(PageSize::A4)
    .margin(Margin::uniform_mm(20.0))
    .title("My Document")
    .build();

let pdf = engine.render_html(html)?;
engine.render_html_to_file(html, "output.pdf")?;

// Template + JSON
let engine = Engine::builder()
    .template("invoice.html", template_str)
    .build();

let data = serde_json::json!({
    "invoice_number": "2026-001",
    "customer_name": "Acme Corp"
});
let pdf = engine.render_template(data)?;
```

## Architecture

Fulgur integrates [Blitz](https://github.com/nickelpack/blitz) (HTML parsing, CSS style resolution, layout) with [Krilla](https://github.com/LaurenzV/krilla) (PDF generation) through a pagination-aware layout abstraction.

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
