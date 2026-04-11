# fulgur-vrt

Visual regression testing harness for [`fulgur`](../fulgur).

This crate is `publish = false` — it exists only to exercise fulgur's
rendering output via `cargo test`. It is never released to crates.io.

## How it works

1. Each fixture in `fixtures/` is a self-contained, font-free HTML snippet
   (rectangles, gradients, SVG shapes).
2. `cargo test -p fulgur-vrt` renders each fixture through fulgur, converts
   the resulting PDF to PNG via `pdftocairo` at 150 DPI, and compares the
   output to a committed `goldens/fulgur/<path>.png` using a maximum
   channel diff plus diff-ratio tolerance (see `manifest.toml`).
3. On failure, a diff image is written to
   `target/vrt-diff/<path>.diff.png` — differing pixels are highlighted in
   red against a brightened grayscale copy of the reference image. Out of
   range areas (when image sizes disagree) are painted yellow.

## Running

```bash
# Install once
sudo apt-get install -y poppler-utils

# Normal run: compare against committed fulgur goldens
cargo test -p fulgur-vrt

# Update every fulgur golden (after an intentional rendering change)
FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt

# Update only the fulgur goldens that currently differ
FULGUR_VRT_UPDATE=failing cargo test -p fulgur-vrt

# Regenerate Chromium goldens (heavy, manual)
cargo test -p fulgur-vrt --features chrome-golden -- --ignored
```

The Chromium step is not wired up end-to-end yet — the
`chrome-golden` feature currently provides a stub (`todo!()`) so the
workflow and `goldens/chrome/` directory have a fixed home before the
real chromiumoxide integration lands.

## Adding a fixture

1. Create `fixtures/<category>/<name>.html`. Keep it font-free (no text),
   use inline styles, and prefer shapes and solid colors that diff
   cleanly.
2. Add a `[[fixture]]` entry to `manifest.toml`.
3. Run `FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt` to seed the golden.
4. Inspect `goldens/fulgur/<category>/<name>.png` and commit both the
   fixture and the golden in the same change.

## Directory layout

```text
fixtures/          HTML inputs grouped by category
goldens/fulgur/    fulgur PDF → PNG references (committed)
goldens/chrome/    Chromium screenshot references (manual, follow-up work)
manifest.toml      fixture list plus tolerance defaults
src/
  manifest.rs      TOML parser
  diff.rs          pixel diff plus diff image writer
  pdf_render.rs    fulgur → pdftocairo bridge
  chrome.rs        chromiumoxide adapter (feature "chrome-golden")
  runner.rs        manifest → diff → update orchestration
tests/vrt_test.rs  single entrypoint run by `cargo test`
```

## Tolerance model

The comparator in `src/diff.rs` uses two knobs per fixture:

- `max_channel_diff` — maximum allowed absolute difference on any of R,
  G, or B for a pixel to be considered "unchanged" (alpha is ignored).
- `max_diff_pixels_ratio` — fraction of pixels allowed to exceed
  `max_channel_diff` and still count as a pass.

Defaults (from `manifest.toml`):

```toml
[defaults]
tolerance_fulgur = { max_channel_diff = 2, max_diff_pixels_ratio = 0.001 }
tolerance_chrome = { max_channel_diff = 16, max_diff_pixels_ratio = 0.02 }
```

The fulgur tolerance is tight because fulgur-vs-fulgur comparisons run
the same renderer twice and should be pixel-identical barring a genuine
regression. The chrome tolerance is loose because Chromium and fulgur
are different renderers — the chrome golden exists as an external
sanity reference, not as a strict assertion.
