# link-media

Demonstrates that `<link rel="stylesheet" media="print">` is honoured
by fulgur: rules from `print-only.css` must **not** appear in the
rendered PDF, because fulgur's stylo device reports `media="screen"`.

Without the fulgur-2ai fix, blitz's `CssHandler` hardcoded `MediaList::empty()`
and the media attribute was silently ignored — the PDF would have shown
red-on-yellow text. With the fix, `<link media="print">` is rewritten
to `<style>@import url(...) print;</style>` at DOM level, stylo
propagates the print media list into the `ImportRule`, and the screen
device drops it.

## What you'll see

- `examples/link-media/index.pdf`: dark green text in the "base"
  color (`#064e3b`), no yellow background, no red strikethrough.
- Open `examples/link-media/index.html` in a real browser and the
  output is identical, because a browser's screen media also excludes
  `print-only.css`.

The follow-up question "should fulgur render as `print` media instead
of `screen`?" is tracked separately (`bd show fulgur-801`).

## Regenerate

```bash
FONTCONFIG_FILE=examples/.fontconfig/fonts.conf \
    cargo run --release --bin fulgur -- render \
    examples/link-media/index.html \
    -o examples/link-media/index.pdf
```
