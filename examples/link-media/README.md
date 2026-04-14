# link-media

Demonstrates that `<link rel="stylesheet" media="print">` is honoured
by fulgur. fulgur's stylo device now reports `media="print"`, so rules
from `print-only.css` **do** apply to the rendered PDF while they stay
suppressed in a browser's screen view.

Without the fulgur-2ai fix, blitz's `CssHandler` hardcoded `MediaList::empty()`
and the media attribute was silently ignored — the media filter
short-circuited regardless of the device. With the fix, `<link media="print">`
is rewritten to `<style>@import url(...) print;</style>` at DOM level,
stylo propagates the print media list into the `ImportRule`, and the
configured device (now `print`) decides whether to keep it.

## What you'll see

- `examples/link-media/index.pdf`: red body text on a yellow background,
  each paragraph underlined with a dotted-red line — the aggressive
  overrides from `print-only.css` are live.
- Open `examples/link-media/index.html` in a real browser and the
  output reverts to the plain dark-green "base" color, because the
  browser's screen media drops `print-only.css`.

## Regenerate

```bash
FONTCONFIG_FILE=examples/.fontconfig/fonts.conf \
    cargo run --release --bin fulgur -- render \
    examples/link-media/index.html \
    -o examples/link-media/index.pdf
```
