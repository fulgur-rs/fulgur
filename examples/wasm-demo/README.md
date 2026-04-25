# fulgur WASM demo

Browser-side `HTML → PDF` rendering powered by `fulgur-wasm`. Renders entirely
in the browser; no network calls except for the local Noto Sans font asset.

## Scope (B-2)

- `Engine` builder mirror: `new()`, `add_font(bytes)`, `render(html)`.
- Default sample HTML is `<h1>Hello World</h1>` with a `font-family: 'Noto Sans'`
  rule. The demo fetches `../.fonts/NotoSans-Regular.ttf` at startup and
  registers it via `engine.add_font(bytes)` before enabling the Render button.
- The B-1 standalone `render_html(html)` entry point is preserved for callers
  that don't need fonts.
- CSS resources, images, page-size / metadata options, CJK fallback, and
  bundle-size optimisation are out of scope here — see B-3.

## Build

Requires [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/).

```bash
wasm-pack build crates/fulgur-wasm --target web --dev \
  --out-dir ../../examples/wasm-demo/pkg
```

This populates `examples/wasm-demo/pkg/` with `fulgur_wasm.js`,
`fulgur_wasm_bg.wasm`, and TypeScript declarations.

## Run

ES modules require a real HTTP origin (file:// will not work). The demo also
fetches Noto Sans from `../.fonts/`, so you must serve the **repository root**
(not just `examples/wasm-demo/`) so the relative path resolves:

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
- The demo fetches `../.fonts/NotoSans-Regular.ttf` over the static server at
  startup. If you serve the demo directory in isolation, copy the TTF next to
  `index.html` or adjust the `fetch` URL.
- WOFF2 payloads work too — `Engine.add_font` decodes them in-process via
  `fulgur::AssetBundle::add_font_bytes`. WOFF1 is rejected.

## Tracking

- `fulgur-iym` (strategic v0.7.0) — overall WASM bet
- `fulgur-id9x` (closed) — B-1: bare wasm-bindgen wrapper
- `fulgur-7js9` (this step) — B-2: font bridge via `AssetBundle::add_font_bytes`
- `crates/fulgur/CLAUDE.md` ※memory `project_wasm_resource_bridging.md` —
  scope 1 / 3a / 3b stage design
