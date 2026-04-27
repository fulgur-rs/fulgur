# Bundled fonts for examples

These font files are bundled so that `examples/` can be regenerated with
byte-identical PDF output across different environments (local dev,
GitHub Actions CI, contributor machines).

See [../../docs/architecture.md](../../docs/architecture.md) or the
project README's *Determinism and fonts* section for the rationale. The
short version: `blitz-dom` loads system fonts via `fontdb::Database::load_system_fonts`
with no hook for callers to override, so SVG `<text>` elements — and to
a lesser extent HTML text — will pick different fonts depending on which
`*.ttf`/`*.otf` files happen to be installed on the host. Shipping a
fixed font set + a pinned `fontconfig` config (`../.fontconfig/fonts.conf`)
sidesteps the issue for examples without patching upstream Blitz.

## Files

| File | License | Copyright | Source |
|---|---|---|---|
| `NotoSans-Regular.ttf` | SIL OFL 1.1 | © 2022 The Noto Project Authors | [notofonts/latin-greek-cyrillic NotoSans-v2.015](https://github.com/notofonts/latin-greek-cyrillic/releases/tag/NotoSans-v2.015) — `unhinted/ttf/NotoSans-Regular.ttf` |
| `NotoSans-Bold.ttf` | SIL OFL 1.1 | © 2022 The Noto Project Authors | same release — `unhinted/ttf/NotoSans-Bold.ttf` |
| `NotoSans-Italic.ttf` | SIL OFL 1.1 | © 2022 The Noto Project Authors | same release — `unhinted/ttf/NotoSans-Italic.ttf` |
| `NotoSans-BoldItalic.ttf` | SIL OFL 1.1 | © 2022 The Noto Project Authors | same release — `unhinted/ttf/NotoSans-BoldItalic.ttf` |
| `NotoSansMono-Regular.ttf` | SIL OFL 1.1 | © 2022 The Noto Project Authors | [notofonts/latin-greek-cyrillic NotoSansMono-v2.014](https://github.com/notofonts/latin-greek-cyrillic/releases/tag/NotoSansMono-v2.014) — `unhinted/ttf/NotoSansMono-Regular.ttf` |
| `NotoSansJP-Regular.otf` | SIL OFL 1.1 | © 2014–2021 Adobe (Noto is a trademark of Google Inc.) | [notofonts/noto-cjk Sans2.004](https://github.com/notofonts/noto-cjk/releases/tag/Sans2.004) — `16_NotoSansJP.zip` |
| `NotoSansJP-Bold.otf` | SIL OFL 1.1 | © 2014–2021 Adobe (Noto is a trademark of Google Inc.) | same release |

The full SIL OFL 1.1 license text is in [`OFL.txt`](./OFL.txt).

## Why static files instead of variable fonts?

The variable-font versions published on `google/fonts` (e.g.
`NotoSans[wdth,wght].ttf`) are single 2–10 MB files that encode every
weight. Fontique picks the first instance listed in the font's `fvar`
table when resolving "Regular" / "Bold" matches, which on Noto Sans JP
happens to be the *Thin* instance. This caused `<h1>`/`<h2>` to render
at Thin weight instead of Bold, which is a silent visual regression.

Shipping static Regular + Bold files avoids the variable-instance
selection problem entirely. Total bundle size is ~10 MB.

## Regenerating / updating the bundle

```bash
# Latin + Mono (TTF, unhinted)
curl -sL -o /tmp/noto-latin.zip \
  https://github.com/notofonts/latin-greek-cyrillic/releases/download/NotoSans-v2.015/NotoSans-v2.015.zip
curl -sL -o /tmp/noto-mono.zip \
  https://github.com/notofonts/latin-greek-cyrillic/releases/download/NotoSansMono-v2.014/NotoSansMono-v2.014.zip

unzip -o /tmp/noto-latin.zip -d /tmp/
unzip -o /tmp/noto-mono.zip -d /tmp/

cp /tmp/NotoSans/unhinted/ttf/NotoSans-Regular.ttf     examples/.fonts/
cp /tmp/NotoSans/unhinted/ttf/NotoSans-Bold.ttf        examples/.fonts/
cp /tmp/NotoSans/unhinted/ttf/NotoSans-Italic.ttf      examples/.fonts/
cp /tmp/NotoSans/unhinted/ttf/NotoSans-BoldItalic.ttf  examples/.fonts/
cp /tmp/NotoSansMono/unhinted/ttf/NotoSansMono-Regular.ttf examples/.fonts/

# Japanese (OTF static, region-specific subset)
curl -sL -o /tmp/noto-jp.zip \
  https://github.com/notofonts/noto-cjk/releases/download/Sans2.004/16_NotoSansJP.zip
unzip -o /tmp/noto-jp.zip -d /tmp/noto-jp/
cp /tmp/noto-jp/NotoSansJP-Regular.otf examples/.fonts/
cp /tmp/noto-jp/NotoSansJP-Bold.otf    examples/.fonts/
```

When updating, remember to:

1. Re-run `mise run update-examples` so `examples/*/index.pdf` is regenerated against the new fonts.
2. Run `cargo test -p fulgur-cli --test examples_determinism` to confirm the regenerated PDFs are still byte-stable (two-run determinism) and that `examples/svg/index.pdf` matches the freshly rendered output (staleness check).
3. Bump the source tags in the table above.
