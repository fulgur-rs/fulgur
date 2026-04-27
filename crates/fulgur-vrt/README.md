# fulgur-vrt

Visual regression testing harness for [`fulgur`](../fulgur).

This crate is `publish = false` — it exists only to exercise fulgur's
rendering output via `cargo test`. It is never released to crates.io.

## How it works

1. Each fixture in `fixtures/` is a self-contained, font-free HTML snippet
   (rectangles, gradients, SVG shapes).
2. `cargo test -p fulgur-vrt` renders each fixture through fulgur and
   compares the resulting PDF byte-wise against a committed
   `goldens/fulgur/<path>.pdf`. fulgur's PDF output is deterministic
   (verified by `crates/fulgur-cli/tests/examples_determinism.rs`), so the
   comparison is exact — no tolerance, no normalization.
3. On failure, both PDFs are rasterized via `pdftocairo` at the fixture's
   DPI and a diff image is written to `target/vrt-diff/<path>.diff.png`
   (differing pixels in red, reference dimmed in grayscale, size-mismatch
   regions yellow). The actual PDF is also saved to
   `target/vrt-diff/<path>.actual.pdf` so a CI-rendered output can be
   copied verbatim into `goldens/` if local and CI rendering ever diverge.

The pass path does not invoke `pdftocairo`, so poppler-utils is only
needed when a fixture fails and you want to inspect the diff image.

## Running

```bash
# poppler-utils is only required when a fixture fails (for diff PNG generation)
sudo apt-get install -y poppler-utils

# Normal run: compare against committed fulgur goldens
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt

# Update every fulgur golden (after an intentional rendering change)
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt

# Update only the fulgur goldens that currently differ
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    FULGUR_VRT_UPDATE=failing cargo test -p fulgur-vrt

# Regenerate Chromium goldens (heavy, manual)
cargo test -p fulgur-vrt --features chrome-golden -- --ignored
```

`FONTCONFIG_FILE` pins the bundled Noto Sans set so font selection is
stable across hosts — required for byte-identical PDF output between
local dev and CI.

## Adding a fixture

1. Create `fixtures/<category>/<name>.html`. Keep it self-contained
   (inline styles, no external assets) and avoid host-dependent text
   styling — see the *Cross-environment determinism* section below.
2. Add a `[[fixture]]` entry to `manifest.toml`.
3. Run `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt`
   to seed the golden.
4. Inspect `goldens/fulgur/<category>/<name>.pdf` and commit both the
   fixture and the golden in the same change.

## Directory layout

```text
fixtures/          HTML inputs grouped by category
goldens/fulgur/    fulgur PDF references (committed, byte-wise compared)
goldens/chrome/    Chromium screenshot references (manual, follow-up work)
manifest.toml      fixture list plus chrome tolerance defaults
src/
  manifest.rs      TOML parser
  diff.rs          PDF byte equality plus failure-path PNG diff writer
  pdf_render.rs    fulgur PDF generation plus pdftocairo rasterizer (failure-only)
  chrome.rs        chromiumoxide adapter (feature "chrome-golden")
  runner.rs        manifest → byte compare → update orchestration
tests/vrt_test.rs  single entrypoint run by `cargo test`
```

## Cross-environment determinism

fulgur produces byte-identical PDFs for the same input (the regression
harness lives at `crates/fulgur-cli/tests/examples_determinism.rs`).
Two caveats apply when authoring fixtures:

- **`FONTCONFIG_FILE` is required.** Without the pinned config the host
  fontconfig may resolve generic families (`sans-serif`, `serif`) to
  different concrete files, changing the embedded font subset.
- **Avoid italic / bold-italic spans for now.** fulgur library callers
  go through Parley's system font database in addition to fontconfig,
  so italic variants can resolve to host-dependent fonts (e.g.
  `DejaVuSans-Oblique` on some Ubuntu images, synthesized italic on
  others) even when a bundled italic exists. Tracked separately as
  follow-up work; until that is resolved, italic in fixtures will
  break cross-environment determinism.

## Chrome tolerance (optional, future work)

The Chromium-comparison path under the `chrome-golden` feature still
uses pixel tolerance because Chromium and fulgur are different
renderers. The defaults live in `manifest.toml`:

```toml
[defaults]
tolerance_chrome = { max_channel_diff = 16, max_diff_pixels_ratio = 0.02 }
```

The chromiumoxide integration itself is not wired up end-to-end yet —
the feature exists so the workflow and `goldens/chrome/` directory have
a fixed home before the real implementation lands.
