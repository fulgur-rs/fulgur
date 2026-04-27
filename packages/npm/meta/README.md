# fulgur

[![npm version](https://img.shields.io/npm/v/%40fulgur-rs%2Fcli.svg)](https://www.npmjs.com/package/@fulgur-rs/cli)
[![License](https://img.shields.io/npm/l/%40fulgur-rs%2Fcli.svg)](https://github.com/fulgur-rs/fulgur/blob/main/LICENSE-MIT)
[![CI](https://github.com/fulgur-rs/fulgur/actions/workflows/ci.yml/badge.svg)](https://github.com/fulgur-rs/fulgur/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/fulgur-rs/fulgur/graph/badge.svg)](https://codecov.io/gh/fulgur-rs/fulgur)

A modern, lightweight successor to wkhtmltopdf. Converts HTML/CSS to PDF without
a browser engine. Single binary, instant cold start, deterministic output.

## Why fulgur?

- **No browser required.** No Chromium, no WebKit. Just a single native binary.
- **Low memory footprint.** Designed for server-side batch generation.
- **Deterministic output.** Identical input produces byte-identical PDFs — safe
  for CI/CD and reproducible pipelines.
- **Template + JSON data.** Feed an HTML template and a JSON file to generate
  PDFs at scale. Built-in [MiniJinja](https://github.com/mitsuhiko/minijinja)
  engine.
- **Offline by design.** No network access. Fonts, images, and CSS must be
  explicitly bundled.

## Install

```bash
# Global install
npm i -g @fulgur-rs/cli

# Or run ad-hoc without installing
npx @fulgur-rs/cli render input.html -o output.pdf
```

## Quick start

```bash
fulgur render input.html -o output.pdf
```

## Commands

- [`fulgur render`](#fulgur-render) — convert HTML/CSS to PDF
- [`fulgur template schema`](#fulgur-template-schema) — extract JSON Schema from
  a template (for AI-driven data generation)

### `fulgur render`

Render an HTML document (optionally a MiniJinja template with `--data`) to PDF.

```bash
# Basic
fulgur render input.html -o output.pdf

# Read HTML from stdin, write PDF to stdout
cat input.html | fulgur render --stdin -o -

# Page size and margins
fulgur render input.html -o output.pdf --size A4 --landscape --margin "20 30"

# Bundle fonts, CSS, and images
fulgur render input.html -o output.pdf \
  -f fonts/NotoSansJP.ttf \
  --css style.css \
  -i logo.png=assets/logo.png

# PDF metadata and bookmarks
fulgur render input.html -o output.pdf \
  --title "Quarterly Report" \
  --author "Acme Corp" \
  --language en \
  --bookmarks

# Template + JSON data
fulgur render template.html -o invoice.pdf -d data.json
```

| Option | Description |
|---|---|
| `-o, --output <PATH>` | Output PDF path (required; use `-` for stdout) |
| `--stdin` | Read HTML from stdin |
| `-s, --size <SIZE>` | Page size: `A4`, `Letter`, `A3` (default: `A4`) |
| `-l, --landscape` | Landscape orientation |
| `--margin <SPEC>` | Margins in mm (CSS shorthand: `"20"`, `"20 30"`, `"10 20 30"`, `"10 20 30 40"`) |
| `-f, --font <FILE>` | Font file to bundle (repeatable; TTF / OTF / TTC / WOFF2) |
| `--css <FILE>` | External CSS file (repeatable) |
| `-i, --image <NAME=PATH>` | Image file to bundle (repeatable) |
| `-d, --data <FILE>` | JSON data for template rendering (use `-` for stdin) |
| `--bookmarks` | Generate PDF bookmarks from `h1`–`h6` |
| `--title`, `--author`, `--description`, `--keyword`, `--language`, `--creator`, `--producer`, `--creation-date` | PDF metadata |

### `fulgur template schema`

Extract a JSON Schema from a MiniJinja HTML template. The CLI analyzes template
syntax to infer variable names and types; with `--data`, it uses a sample JSON
file for more precise inference.

```bash
# Infer schema from template syntax alone
fulgur template schema template.html -o schema.json

# Sharpen inference with a sample data file
fulgur template schema template.html -d sample.json -o schema.json
```

This is primarily intended for AI agents and tooling that need to know the
shape of the JSON data a template expects — see [Template engine](#template-engine)
below.

## Template engine

fulgur ships with [MiniJinja](https://github.com/mitsuhiko/minijinja). Supply an
HTML template and a JSON data file to `fulgur render -d`, and the template is
expanded before rendering.

`template.html`:

```html
<h1>Invoice #{{ invoice_number }}</h1>
<p>Bill to: {{ customer.name }}</p>
<table>
  <thead><tr><th>Item</th><th>Price</th></tr></thead>
  <tbody>
    {% for item in items %}
    <tr><td>{{ item.name }}</td><td>{{ item.price }}</td></tr>
    {% endfor %}
  </tbody>
</table>
<p>Total: {{ items | map(attribute="price") | sum }}</p>
```

`data.json`:

```json
{
  "invoice_number": "2026-001",
  "customer": { "name": "Acme Corp" },
  "items": [
    { "name": "Widget", "price": 10 },
    { "name": "Gadget", "price": 25 }
  ]
}
```

```bash
fulgur render template.html -o invoice.pdf -d data.json
```

### Designer + agent workflow

The template / data split is a deliberate separation of concerns that works
well for AI-agent-driven document generation:

- **Humans (or designers) author the template.** They control layout,
  typography, and branding in familiar HTML/CSS.
- **AI agents produce the data.** Given a JSON Schema — which
  `fulgur template schema` can extract directly from the template — an agent
  can populate the structured payload without touching the presentation layer.

The result: agents never produce malformed HTML, and designers never need to
review generated markup. Each side stays on its own contract.

## Supported platforms

| OS | Arch | Package |
|---|---|---|
| Linux (glibc) | x64 | `@fulgur-rs/cli-linux-x64` |
| Linux (musl / Alpine) | x64 | `@fulgur-rs/cli-linux-x64-musl` |
| Linux | arm64 | `@fulgur-rs/cli-linux-arm64` |
| macOS | arm64 (Apple Silicon) | `@fulgur-rs/cli-darwin-arm64` |
| macOS | x64 (Intel) | `@fulgur-rs/cli-darwin-x64` |
| Windows | x64 | `@fulgur-rs/cli-win32-x64` |

## How it works (distribution)

`@fulgur-rs/cli` is a thin meta package. The actual native binary lives in one
of the `@fulgur-rs/cli-<platform>` packages above, all declared as
[`optionalDependencies`](https://docs.npmjs.com/cli/v10/configuring-npm/package-json#optionaldependencies).
npm only installs the one that matches the current `os` / `cpu` / libc, so
there is no per-platform bloat and no cross-compilation at install time.

The `bin/fulgur` entry is a small JavaScript shim that, at run time, resolves
the platform package via `require.resolve` and execs its native binary. There
is no `postinstall` step, so `npx @fulgur-rs/cli` works on the very first run.
If you install with `--ignore-optional` or on an unsupported platform, the
shim exits with a clear error message.

## Links

- [GitHub repository](https://github.com/fulgur-rs/fulgur)
- [Issue tracker](https://github.com/fulgur-rs/fulgur/issues)
- [Full documentation (README)](https://github.com/fulgur-rs/fulgur#readme)
- [CSS property support reference](https://github.com/fulgur-rs/fulgur/blob/main/docs/css-support.md)

## License

Dual-licensed under either of [MIT](https://github.com/fulgur-rs/fulgur/blob/main/LICENSE-MIT)
or [Apache-2.0](https://github.com/fulgur-rs/fulgur/blob/main/LICENSE-APACHE)
at your option.
