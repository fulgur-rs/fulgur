# Fulgur

[![codecov](https://codecov.io/gh/fulgur-rs/fulgur/graph/badge.svg)](https://codecov.io/gh/fulgur-rs/fulgur)
[![Coverage](https://raw.githubusercontent.com/mitsuru/octocovs/main/badges/fulgur-rs/fulgur/coverage.svg)](https://github.com/mitsuru/octocovs)
[![Code to Test Ratio](https://raw.githubusercontent.com/mitsuru/octocovs/main/badges/fulgur-rs/fulgur/ratio.svg)](https://github.com/mitsuru/octocovs)
[![Test Execution Time](https://raw.githubusercontent.com/mitsuru/octocovs/main/badges/fulgur-rs/fulgur/time.svg)](https://github.com/mitsuru/octocovs)

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
- Custom font bundling with subsetting (TTF / OTF / TTC / WOFF2)
- External CSS injection
- Page sizes (A4 / Letter / A3) with landscape support
- PDF metadata (title, author, keywords, language)
- PDF bookmarks from heading structure
- [CSS property support reference](docs/css-support.md)

## Installation

Run directly with `npx` (no install needed):

```bash
npx @fulgur-rs/cli render -o output.pdf input.html
```

Or install globally via npm:

```bash
npm install -g @fulgur-rs/cli
fulgur render -o output.pdf input.html
```

Or via Cargo:

```bash
cargo install fulgur-cli
```

From source:

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
| `--margin` | Page margins in mm (CSS shorthand: `"20"`, `"20 30"`, `"10 20 30"`, `"10 20 30 40"`) | — |
| `--title` | PDF title metadata | — |
| `-f, --font` | Font files to bundle (repeatable) | — |
| `--css` | CSS files to include (repeatable) | — |
| `-i, --image` | Image files to bundle as name=path (repeatable) | — |
| `-d, --data` | JSON data file for template mode (use `-` for stdin) | — |
| `--bookmarks` | Generate PDF bookmarks (outline) from `h1`-`h6` headings | false |
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
    .data(serde_json::json!({
        "invoice_number": "2026-001",
        "customer_name": "Acme Corp",
    }))
    .build();

let pdf = engine.render()?;
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

## Versioning

Fulgur follows the [ZeroVer](https://0ver.org) convention while still in
the `0.x` line. The number after the leading zero — the **minor** — is
what we treat as a release boundary; the patch field exists only to ship
hotfixes between minors.

- **Each normal release bumps the minor** (`0.5.x` → `0.6` → `0.7`). We
  refer to it as "Fulgur 0.6" externally; internal artefacts (Cargo.toml,
  PyPI, RubyGems, npm, git tag) carry the full semver string `0.6.0`
  because those registries require it.
- **Patch numbers are reserved for hotfixes** off a previous minor
  (e.g. `0.6.1` to fix a regression in `0.6.0`). They are not used for
  routine releases.
- **`fulgur`, `fulgur-cli`, `fulgur-wasm`, `fulgur-ruby`, `pyfulgur` share
  the same version**. Independent binding versioning is tracked as future
  work.
- **API stability is not guaranteed in `0.x`**. Each minor may introduce
  breaking changes. The 1.0 line will be cut once the public surface
  stabilises (criteria TBD).

If you depend on Fulgur from another Rust crate or a binding, pin the
exact minor (`fulgur = "0.6"` resolves to `>=0.6.0, <0.7.0` under Cargo's
default caret behaviour) until you are ready to absorb a breaking change.

## Release

See [docs/RELEASE_SETUP.md](docs/RELEASE_SETUP.md) for PyPI / RubyGems
Trusted Publisher setup and release steps.

## Determinism and fonts

Fulgur aims for byte-identical PDF output from identical input. The core pipeline
(Blitz → Taffy → Parley → Krilla) is deterministic, but there is **one known
environment dependency** you should be aware of:

- [Blitz `blitz-dom` 0.2.4](https://docs.rs/blitz-dom) uses a process-global
  `fontdb::Database::load_system_fonts()` call for parsing inline `<svg>`
  elements. Fulgur cannot currently override it, so the font chosen for
  `<text>` elements inside SVG — and the default fallback for HTML text when
  no bundled fonts are supplied — depends on which `.ttf`/`.otf` files are
  installed on the host. The same HTML can therefore produce *different*
  PDFs on two machines if their system font sets differ.

To get reproducible output today, pin the font environment via `fontconfig`:

```bash
# Point fontconfig at a controlled set of font files.
export FONTCONFIG_FILE="$PWD/my-fonts.conf"
fulgur render -o output.pdf input.html
```

The repository ships a pinned Noto Sans bundle under `examples/.fonts/`
together with a matching `examples/.fontconfig/fonts.conf`, which is what
`mise run update-examples` and the GitHub Actions regen workflows use to
keep `examples/*/index.pdf` byte-identical across environments. See
`examples/.fonts/README.md` for the exact font list and re-fetch
instructions. Making this configurable at the library API level (so
`fulgur::Engine` callers get determinism without touching fontconfig) is
tracked as a follow-up — once landed, library callers will be able to
supply their own font database directly.

## Security

If you plan to accept untrusted HTML templates or JSON data (e.g. in a
multi-tenant SaaS), see the
[threat model](docs/security/threat-model.md)
([日本語](docs/security/threat-model.ja.md)) for the full analysis of
attack vectors and mitigations.

## Contributing

Contributions are welcome! Please read the [Contributing Guide](CONTRIBUTING.md)
before opening a pull request. All contributors must sign the
[Contributor License Agreement](CLA.md) — a bot will guide you through the
one-time sign-off on your first PR.

## License

Fulgur is dual-licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
