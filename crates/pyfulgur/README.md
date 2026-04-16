# pyfulgur

Python bindings for [fulgur](https://github.com/mitsuru/fulgur) — an offline, deterministic HTML/CSS to PDF conversion library written in Rust.

## Status

**This package is a name reservation.** The implementation is under active development.

## Planned API

> **Not available in v0.0.1.** The current release is a placeholder for name reservation.

```python
from pyfulgur import Engine, AssetBundle

bundle = AssetBundle()
bundle.add_css("body { font-family: sans-serif; }")
bundle.add_font_file("fonts/NotoSans-Regular.ttf")

engine = Engine.builder().page_size("A4").assets(bundle).build()
pdf_bytes = engine.render_html("<h1>Hello, world!</h1>")

with open("output.pdf", "wb") as f:
    f.write(pdf_bytes)
```

## Links

- [fulgur on GitHub](https://github.com/mitsuru/fulgur)
- [fulgur on crates.io](https://crates.io/crates/fulgur)

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
