# link-media

Demonstrates that `<link rel="stylesheet" media="print">` is honoured:
a browser that views `index.html` sees black text (screen stylesheet),
while the PDF rendered by fulgur (print media) shows dark green
(`#064e3b`).

## Regenerate

```bash
FONTCONFIG_FILE=examples/.fontconfig/fonts.conf \
    cargo run --release --bin fulgur -- render \
    examples/link-media/index.html \
    -o examples/link-media/index.pdf
```
