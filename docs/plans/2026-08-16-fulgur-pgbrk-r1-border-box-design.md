# fulgur-pgbrk R1: inline-root fragments must describe the border box

**Goal:** Make an inline root's recorded fragments cover its whole border box —
leading border/padding included — so the push-whole decision stops
under-measuring, split paragraphs stop painting a page's worth of lines into
the bottom margin, continuation pages stop re-applying `padding-top`, and the
R3 overflow guard can see all of it.

**Parent review:** [2026-08-16-fulgur-pgbrk-page-fragmentation-review.md](./2026-08-16-fulgur-pgbrk-page-fragmentation-review.md)
(item R1).

**Predecessor:** [2026-08-16-fulgur-pgbrk-r3-overflow-detection-design.md](./2026-08-16-fulgur-pgbrk-r3-overflow-detection-design.md).

**Spec baseline:** [CSS Fragmentation Module Level 3](https://www.w3.org/TR/css-break-3/),
§5.4 (`box-decoration-break`).

**Tech stack:** Rust, `crates/fulgur/src/pagination_layout.rs`,
`crates/fulgur/src/render.rs`.

---

## Measured behaviour on the current tree

All four observations below are from the debug CLI at commit `28926fe4`, page
`600px × 500px`, `margin: 50px` — content strip `37.5 … 337.5pt`, paper edge
`375pt`.

**1. R1 reproduces.** A `<p style="padding:150px 0;line-height:20px">` holding
120 probe words, nested two `<div>`s deep after a `100px` spacer, renders **110
of 120 words** with exit code `0` on a single page. Lines run down to
`yMax=372.375pt`: three of them paint into the bottom margin, and the tail
clears the paper edge and is discarded.

**2. `padding-top` on an inline root is honoured in paint.** Padded vs unpadded
first line: `226.875pt` vs `114.375pt`, a delta of exactly `150px × 0.75`. The
CLAUDE.md gotcha about inline-root `padding-top` being ignored does not hold for
this shape.

**3. The recorded fragment is anchored and measured from different edges.**
`fragment_inline_root` writes `y = cursor` — the border-box top — but
`height = last_bottom_local - frag_top_local`, which covers line boxes only.
Parley lays an inline root out in **content-box** coordinates, so neither edge
of the box's decoration appears in its metrics. Probed directly on a `<p>` with
`border-top: 7px; padding: 150px 0 90px`:

```text
final_layout: border.top=7  padding.top=150  padding.bottom=90  size.height=287
line_metrics: [(0.0, 20.0), (20.0, 40.0)]        <- first min_coord is 0.0
recorded:     Fragment { y: 0, height: 40 }      <- 287px box, 40px fragment
```

The review's measurement — a `460px` box recorded as `y=100, height=160` — is
the same shape.

**4. Continuation pages re-apply `padding-top`.** A padded paragraph forced to
split by lines paints its first line at `yMin=84.375pt` on page 1 **and on page
2**. That is `box-decoration-break: clone` where CSS defaults to `slice` (§5.4).
The same render puts page 1's last line at `yMax=364.875pt` — `27.375pt` past
the strip — so the split path overshoots even when it works.

Symptoms 1 and 2 are one root cause: the fragment height is short by
`lead_in + lead_out`. Symptom 4 is a second, independent site —
`render.rs:3149` adds `block.style.content_inset()` to the inner content's
origin on **every** fragment, so a continuation page re-applies `padding-top`.
That inset is also where the paint offset in symptom 2 comes from: the
fragmenter chooses a split in line space, and the painter then shifts the lines
down by `lead_in`, which is why the split overshoots by exactly that much.

### Why the R3 guard is blind to this

`find_overflowing_fragments` tests `f.y + f.height > page_height_px + 0.5`
against the fragmenter's own numbers. Those numbers under-report the box by
`lead_in + lead_out`, so the guard reads a fragment sitting comfortably inside
the strip while Parley paints lines off the paper. The one defect that
under-measures the quantity the guard measures is the one defect the guard
cannot report. Fixing R1 restores its sight as a consequence, with no change to
the guard itself.

---

## The model

The fragmenter's numbers become border-box. Per fragment, following
`box-decoration-break: slice` (§5.4, the CSS initial value):

| fragment | height |
| --- | --- |
| only (no split) | `lead_in + lines + lead_out` |
| first of N | `lead_in + lines` |
| middle of N | `lines` |
| last of N | `lines + lead_out` |

The reverse reconciliation — moving the painter into line space — was rejected:
backgrounds, borders and shadows genuinely need the border box, so it relocates
the discrepancy rather than removing it.

Both edges come from Taffy — `final_layout.border` / `final_layout.padding`.
Parley contributes only `lines_h`.

> **Resolved during implementation.** An earlier revision of this document had
> `lead_in` coming from `line_metrics[0].0`, on the theory that Parley's
> coordinates were border-box relative. The probe above disproves it:
> `line_metrics[0].0` is `0.0` under a `150px` `padding-top`. Both edges are
> read from Taffy instead; nothing else in the design changed.

### Where the two values live

On `PaginationGeometry`, not on `Fragment`:

```rust
pub struct PaginationGeometry {
    pub fragments: Vec<Fragment>,
    pub is_repeat: bool,
    /// border-top + padding-top, carried by the FIRST fragment only.
    pub content_lead_in: crate::units::Px,
    /// padding-bottom + border-bottom, carried by the LAST fragment only.
    pub content_lead_out: crate::units::Px,
}
```

They describe the box, not a slice — "the first fragment carries `lead_in`" is
already implied by position in the vector, so per-fragment storage would
duplicate a fact the vector encodes. The practical weight agrees: `Fragment {`
is constructed at 45 sites across five files, `PaginationGeometry {` at 6, and
every other producer goes through `entry().or_default()`. Both fields are
`Px::ZERO` for every non-inline-root node, which is every existing caller.

---

## Changes

### `pagination_layout.rs`

A helper beside `collect_inline_line_metrics`:

```rust
/// Border-box metrics for an inline root: the decoration above the first
/// line box, the line-box extent, and the decoration below the last.
fn inline_root_box_metrics(
    node: &blitz_dom::Node,
    line_metrics: &[(f32, f32)],
) -> (f32, f32, f32); // (lead_in, lines_h, lead_out)
```

It replaces the two hand-duplicated blocks at `:889` (body-direct) and `:2209`
(nested). Those two have to agree and currently agree only by inspection —
Risk 1 in the parent review — so unifying them here costs nothing extra.

Both sites then compute `box_total_h = lead_in + lines_h + lead_out` and use it
for:

- `avoid_is_fulfillable` — "can this box ever fit a fragmentainer?"
- the push-whole test (`cursor_y + box_total_h > page_height_px`)

`fragment_inline_root` takes `lead_in` and `lead_out`, and changes in three
places: the split test adds `lead_in` while `fragment_start_idx == 0`; emitted
heights follow the table above; the returned `cursor_y` advances past
`lead_out`. It records both values on the geometry entry.

### `render.rs`

`paragraph_lines_for_page` (`:3425`) partitions lines by summing fragment
heights and comparing against `ShapedLine.height`, which is pure line-box
space. Once fragment heights carry decoration, that walk needs the decoration
subtracted back out before it partitions: `lead_in` off fragment 0, `lead_out`
off the last. Its parameter changes from `fragments: &[Fragment]` to
`&PaginationGeometry` rather than threading two more `f32`s through its six
call sites.

Symptom 4 needs a second render edit: a `fragment_is_continuation(geom, frag)`
predicate, applied at all four sites that add `content_inset()` to inner
content (`draw_block_with_inner_content`, `draw_list_item_with_block`, and the
transform and overflow-clip variants). A continuation fragment gets no vertical
inset. `is_split()` is the right gate — it is `false` for `is_repeat`
geometry, where each fragment is a full redraw and does carry its own leading
edge.

Background/border painting needs no change: it already reads `frag.height` only
when `is_split()` (`:1821`, `:2249`), falling back to `layout_size.height`
otherwise, so single-fragment paragraphs are unaffected by the height change.

---

## Testing

Per CLAUDE.md's coverage rule, the fragmenter logic is lib-level and belongs in
`#[cfg(test)] mod tests` in `pagination_layout.rs`.

- `inline_root_box_metrics`: lead-in and lead-out both from Taffy,
  zero-padding case, single-line case, empty-metrics case.
- `fragment_inline_root`: each row of the height table — unsplit, first,
  middle, last — plus `cursor_y` advancing past `lead_out`.
- The push-whole decision on a padded paragraph that fits only once its
  decoration is counted.
- `paragraph_lines_for_page`: a three-page padded paragraph partitions to the
  same line sets as its unpadded twin.
- The R1 repro as a lib-level assertion via `find_overflowing_fragments` — no
  `pdftotext` needed, unlike the existing `render_smoke` pagination test.
- A padded variant in `render_smoke.rs` alongside
  `leading_child_that_must_break_does_not_lose_content`.

VRT was expected to move for any fixture containing a padded or bordered
paragraph that splits across pages. It did not: the stash / re-run / diff
protocol from the R3 design reports the same 29 of 64 fixtures with the same
byte sizes, with and without this change. As with the R3 fixes, **no VRT
fixture exercises this shape**, so the lib and `render_smoke` tests are the
only regression barrier. Do not regenerate goldens on macOS, where those 29
differ for unrelated environment reasons.

## Outcome

| Check | Before | After |
| --- | --- | --- |
| 120-word padded repro (downstream shape) | 110 / 120 words, 1 page, exit 0 | 120 / 120 words, 2 pages |
| padded split paragraph, page 1 last line | `yMax=364.875pt` (strip ends `337.5`) | `yMax=334.875pt` |
| padded split paragraph, page 2 first line | `yMin=84.375pt` (= page 1, `clone`) | `yMin=39.375pt` (`slice`) |
| `cargo test -p fulgur --lib` | 2029 passed / 15 ignored | 2034 passed / 15 ignored |
| `cargo test -p fulgur --test render_smoke` | 214 passed | 215 passed |
| `cargo test -p fulgur` (all targets) | — | 2481 passed / 0 failed / 17 ignored |
| `cargo test -p fulgur-vrt` | 29 of 64 differ | 29 of 64 differ, byte-identical |

The `39.375pt` figure is the predicted one: `84.375 - 45`, where `45pt` is the
fixture's `60px` `padding-top`.

`padded_leading_child_that_must_break_does_not_lose_content` was confirmed to
fail without the fix (13 of 30 probe words absent at `filler=1300px`) before
being accepted. A first draft of it passed both with and without the fix — it
reused the 300-word probe from its unpadded sibling, which is page-tall and
therefore takes the line-splitting path instead of the under-measured
push-whole path. The probe has to be **short**: its line boxes must fit the
remaining strip while its border box does not.

---

## Out of scope

**`fulgur-cli` installs no logger.** `crates/fulgur-cli/Cargo.toml` depended on
`fulgur`, `clap`, `serde_json` and `which` — no `log` implementation, so every
`log::warn!` in the library was dropped. That included R3's overflow warning and
the pre-existing warnings in `asset.rs`, `blitz_adapter.rs` and `column_css.rs`.
The R3 design assumed otherwise ("`fulgur-cli` already installs one"), which made
the production half of R3 inert for CLI users — the exact audience that filed the
original bug report. Fixed in a separate commit on this branch with a ~30-line
stderr logger honouring `RUST_LOG` (default `warn`), rather than pulling in
`env_logger`.

**R2 / R6** (widow relaxation, author `orphans` / `widows` values) follow R1 and
share `fragment_inline_root`'s signature. Landing R1 first means that signature
is edited once for decoration and once for constraints, rather than three times.

---

## Verification commands

```bash
cargo test -p fulgur --lib
cargo test -p fulgur --lib fragment_inline_root
cargo test -p fulgur --lib -- --ignored        # open gaps: expected to FAIL
cargo test -p fulgur --test render_smoke
cargo clippy -p fulgur && cargo fmt --check
npx markdownlint-cli2 'docs/plans/2026-08-16-fulgur-pgbrk-r1-border-box-design.md'
```
