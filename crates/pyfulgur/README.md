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

## Links

- [fulgur on GitHub](https://github.com/mitsuru/fulgur)
- [fulgur on crates.io](https://crates.io/crates/fulgur)

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
