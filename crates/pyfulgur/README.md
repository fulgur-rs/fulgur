# pyfulgur

Python bindings for [fulgur](https://github.com/mitsuru/fulgur) — an offline, deterministic HTML/CSS to PDF conversion library written in Rust.

## Status

**Alpha (v0.0.2).** Core `Engine` / `AssetBundle` / `PageSize` / `Margin` / `render_html` API is available. Batch rendering, sandboxing, and template engine wiring are planned for later releases.

## Install

> **Note:** v0.0.2 is an early alpha. Pre-built wheels are not yet published to PyPI; install from source for now.

```bash
# From a checkout of the fulgur repository
pip install maturin
maturin develop --release -m crates/pyfulgur/Cargo.toml
```

Pre-built wheels for manylinux / macOS / Windows will be published in a later release.

## Quick start

```python
from pyfulgur import AssetBundle, Engine, PageSize

bundle = AssetBundle()
bundle.add_css("body { font-family: sans-serif; }")

engine = Engine(page_size=PageSize.A4, assets=bundle)
pdf_bytes = engine.render_html("<h1>Hello, world!</h1>")

with open("output.pdf", "wb") as f:
    f.write(pdf_bytes)
```

Builder style:

```python
engine = (
    Engine.builder()
    .page_size(PageSize.A4)
    .landscape(False)
    .title("My doc")
    .assets(bundle)
    .build()
)
engine.render_html_to_file("<h1>Hi</h1>", "out.pdf")
```

## API surface

- `Engine(**kwargs)` / `Engine.builder()` → `EngineBuilder`
- `Engine.render_html(html: str) -> bytes` — render to PDF bytes (releases the GIL)
- `Engine.render_html_to_file(html: str, path: str | os.PathLike) -> None` — render to a file
- `AssetBundle`: `add_css`, `add_css_file`, `add_font_file`, `add_image`, `add_image_file`
- `PageSize`: `A4`, `LETTER`, `A3`, `custom(w_mm, h_mm)`, `.landscape()`
- `Margin`: `Margin(top, right, bottom, left)`, `Margin.uniform(pt)`, `Margin.symmetric(v, h)`, `Margin.uniform_mm(mm)`
- Exceptions: `FileNotFoundError`, `ValueError`, `pyfulgur.RenderError`

## Known limitation: blitz parse-error noise on stdout

The underlying `blitz-dom` parser writes non-fatal html5ever parse errors directly to the process's stdout via `println!`. These fire for browser-tolerated but technically invalid HTML — documents without a `<!DOCTYPE>`, missing `<html>`/`<body>` wrappers, or structural quirks that browsers silently auto-correct — and show up as `ERROR: ...` noise in Jupyter notebooks or any environment that captures stdout. The PDF bytes returned by `render_html` are **not** affected; only the caller's terminal is polluted.

pyfulgur intentionally does **not** redirect fd 1 from inside the binding: process-wide fd manipulation in a multi-threaded library context races with concurrent `render_html` calls from other threads (mixed suppress / non-suppress callers would silently lose stdout during a suppressed window). Correctness and parallelism take priority over cosmetic stdout cleanliness.

If you need clean stdout:

- **Redirect at the caller side** with a Python `contextlib.redirect_stdout` for Python-level writes, or `os.dup2` for fd-level writes. This keeps the fd manipulation scoped to your own call site where you can guarantee single-threaded use.
- **Run renders in a subprocess** via `multiprocessing` (each worker has its own fd 1).
- **Use the `fulgur` CLI** (which handles stdout isolation internally) invoked via `subprocess.run` when cold-start cost is acceptable.

A wrapper-style Python package that shells out to the `fulgur` CLI (clean stdout, parallel via process isolation, no native build required) is on the roadmap as a complement to this native binding.

## Links

- [fulgur on GitHub](https://github.com/mitsuru/fulgur)
- [fulgur on crates.io](https://crates.io/crates/fulgur)

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
