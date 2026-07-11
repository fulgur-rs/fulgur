# link-media

Demonstrates that fulgur evaluates CSS `@media` rules against the **print**
media type. `styles.css` carries both a `@media screen` block (red on yellow)
and a `@media print` block (a print-only heading accent). In fulgur's PDF
output the screen block is excluded and only the print accent applies.

Since the blitz 0.3 upgrade fulgur sets `DocumentConfig::media_type =
MediaType::print()`, so blitz evaluates `@media` natively — this replaced an
earlier CSS-text rewrite hack.

## What you'll see

- `examples/link-media/index.pdf`: the heading in the print accent color
  (`#b91c1c`) and the paragraph in the base dark green (`#064e3b`); **no**
  yellow background and **no** red strikethrough from the `@media screen` block.
- Open `examples/link-media/index.html` in a real browser and the screen block
  *does* apply (red on yellow), because a browser uses the screen media type —
  the inverse of fulgur's print rendering.

## Known limitation

The `@media` *at-rule* is gated correctly, but the `<link media="...">`
*attribute* is not yet honoured: blitz 0.3 still hardcodes an empty media list
for `<link>` stylesheets, so a `<link rel="stylesheet" media="screen">` would
still apply under the print device. Use `@media` blocks inside the stylesheet
to gate by media type until the `<link>` attribute is re-supported.

## Regenerate

```bash
FONTCONFIG_FILE=examples/.fontconfig/fonts.conf \
    cargo run --release --bin fulgur -- render \
    examples/link-media/index.html \
    -o examples/link-media/index.pdf
```
