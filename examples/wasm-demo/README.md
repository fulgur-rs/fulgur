# fulgur WASM demo

Browser-side `HTML → PDF` rendering powered by `fulgur-wasm`. Renders entirely
in the browser; no network calls except for the local font / CSS / image assets
served alongside the demo.

## Scope (B-3c)

- `Engine` builder mirror: `new()`, `add_font(bytes)`, `add_css(text)`,
  `add_image(name, bytes)`, `configure(options)`, `render(html)`.
- The default sample HTML uses `<h1>`, a subtitle paragraph, and an `<img>`
  tag. The demo fetches three assets at startup and registers them on the
  engine before enabling the Render button:
  - `../.fonts/NotoSans-Regular.ttf` &rarr; `engine.add_font`
  - `./style.css` &rarr; `engine.add_css`
  - `../image/icon.png` &rarr; `engine.add_image("icon.png", …)`
- The render config form (page size, landscape toggle, title) is forwarded
  to `engine.configure({...})` immediately before each render. `configure`
  accepts a POJO with the following keys (all optional, partial merge,
  later calls override earlier ones):
  - `pageSize`: `"A4"` / `"Letter"` / `"A3"` (case-insensitive) or
    `{ widthMm, heightMm }`
  - `margin`: `{ mm }` (uniform mm) / `{ pt }` (uniform pt) /
    `{ topMm, rightMm, bottomMm, leftMm }`
  - `landscape`: boolean
  - `title` / `description` / `creator` / `producer` / `creationDate` /
    `lang`: string
  - `authors` / `keywords`: string array
  - `bookmarks`: boolean (PDF outline from `<h1>`–`<h6>`)
- The B-1 standalone `render_html(html)` entry point is preserved for callers
  that don't need fonts / CSS / images / config.
- Bundle-size optimisation, CJK fallback chain, and dynamic
  `<link rel=stylesheet>` resolution are out of scope here — see scope 3b
  and the bundle-size step.

## Build

Requires [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/).

```bash
wasm-pack build crates/fulgur-wasm --target web --dev \
  --out-dir ../../examples/wasm-demo/pkg
```

This populates `examples/wasm-demo/pkg/` with `fulgur_wasm.js`,
`fulgur_wasm_bg.wasm`, and TypeScript declarations.

## Run

ES modules require a real HTTP origin (file:// will not work). The demo
also fetches assets from `../.fonts/` and `../image/`, so you must serve
the **repository root** (not just `examples/wasm-demo/`) so the relative
paths resolve:

```bash
# from repo root
python3 -m http.server 8000
# then visit http://localhost:8000/examples/wasm-demo/
```

Edit the HTML in the textarea, click "Render PDF", and the browser will
download `output.pdf`.

## Notes

- `--dev` builds produce a ~37 MB `.wasm`. `wasm-pack build … --release` (no
  flag = release) shrinks it but currently still ships every fulgur dependency.
  Aggressive size reduction (`wasm-opt`, dead-code analysis) is part of the
  later B-3 / scope C work.
- The first call after page load incurs a one-time WASM compile cost on top
  of the render itself; subsequent calls reuse the instance.
- WOFF2 payloads work too — `Engine.add_font` decodes them in-process via
  `fulgur::AssetBundle::add_font_bytes`. WOFF1 is rejected.
- `<link rel="stylesheet">` and `@import` inside the input HTML are **not**
  resolved in the WASM target (no async NetProvider yet, see scope 3b).
  Always fetch CSS on the JS side and pass it via `engine.add_css(text)`.
- `<img src="…">` works only for keys that have been registered via
  `engine.add_image(name, bytes)`; arbitrary remote URLs are not fetched.

## Tracking

- `fulgur-iym` (strategic v0.7.0) — overall WASM bet
- `fulgur-id9x` (closed) — B-1: bare wasm-bindgen wrapper
- `fulgur-7js9` (closed) — B-2: font bridge via `AssetBundle::add_font_bytes`
- `fulgur-xi6c` (closed) — B-3a: CSS / image bridge via `add_css` / `add_image`
- `fulgur-ufda` (this step) — B-3c: config mirror via `Engine.configure`
- `crates/fulgur/CLAUDE.md` ※memory `project_wasm_resource_bridging.md` —
  scope 1 / 3a / 3b stage design
