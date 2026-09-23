# Coordinate System and Unit Conversion Rules

Most coordinate bugs in fulgur trace back to either "forgetting to convert CSS px ↔ PDF pt"
or "misunderstanding Krilla's Y direction."
Always consult this file when writing or reviewing rendering code.

## Unit layers in the pipeline

```text
Blitz/Taffy (CSS px) ──Px::in_pt()──► Pageable tree / Krilla (PDF pt)
                     ◄──Pt::in_px()──
```

| Layer | Unit | Notes |
|-------|------|-------|
| Blitz input (viewport, width) | CSS px | convert pt-valued input via `v.as_pt().in_px()` before passing |
| Taffy `final_layout` | CSS px | extract via `layout_in_pt()` / `size_in_pt()` |
| Pageable tree internals | PDF pt | |
| Krilla Surface | PDF pt | |
| `PageSize::custom(w, h)` | **mm** | `fulgur-core` `config.rs` — converted to pt internally |
| `Margin::uniform(v)` | **pt** | |
| `Margin::uniform_mm(v)` | **mm** | |

Conversion constant: `1 CSS px = 0.75 PDF pt` (`PX_TO_PT = 0.75`, defined in
`crates/fulgur-core/src/units.rs`; applied via `units::Px::in_pt` / `units::Pt::in_px`).

## Reading the `Px` / `Pt` vocabulary in a diff

The `units::{Px, Pt}` newtype methods encode each value's unit *state* in plain
text, so a reviewer or coding agent reading a bare diff — with no IDE to peek
types — can still tell a raw external `f32` from an already-typed value. Read the
**first** conversion applied to a value:

- `x.as_px()` / `x.as_pt()` — `x` is a raw `f32` (from an external crate such as
  usvg / taffy / stylo, or a literal) being *tagged* with its unit. `F32Units`
  is implemented for `f32` **only**, so a visible `.as_px()` / `.as_pt()` is a
  compiler-enforced proof that its receiver was untyped (`Pt.as_px()` does not
  compile — no such method, no `Deref` to `f32`). The `as_` prefix
  disambiguates from Stylo's `CSSPixelLength::px()`, which extracts an `f32`
  from a computed `Length` and legitimately keeps its bare `.px()` name.
- `x.in_pt()` / `x.in_px()` — `x` is already a `Px` / `Pt` being *converted*
  (the only place `PX_TO_PT` is applied).
- `x.to_f32()` — a `Px` / `Pt` being *dropped* back to raw `f32` at an f32 sink
  (FFI boundary, or a still-unmigrated f32 consumer).

So `size.width().as_px().in_pt()` reads as "raw f32 (px) → `Px` → `Pt`": the
leading `.as_px()` is what proves `size.width()` is an untyped external `f32`. A
trailing `.in_pt()` only converts the freshly-tagged `Px` and says nothing about
the origin — always read the *first* hop.

The authoritative version of this key, with a `compile_fail` doctest proving
`.as_px()` can never be called on a `Pt`, lives in the `crate::units` module
docstring (`crates/fulgur-core/src/units.rs`).

## Blitz boundary conversion rules (most important)

Forgetting a conversion produces a **4/3× or 3/4× scale bug**.

```rust
// WRONG: pass pt value directly (a bare f32 with no unit witness)
parse_html_with_local_resources(html, config.content_width(), ...)

// CORRECT: tag as Pt at the source and convert to Px explicitly. The trailing
// `.to_f32()` is only there because the Blitz FFI still takes bare `f32`;
// keep the typed hop above it so the unit intent is visible in the diff.
parse_html_with_local_resources(
    html,
    config.content_width().as_pt().in_px().to_f32(),
    ...,
)
```

```rust
// WRONG: use Taffy layout values directly
let width = node.final_layout.size.width;

// CORRECT: go through the typed size_in_pt / layout_in_pt helpers, which
// tag Taffy's raw f32 as Px and drop to Pt in one step (Px::in_pt)
let (width, height) = size_in_pt(node.final_layout.size);
let (x, y, width, height) = layout_in_pt(&node.final_layout);
```

Helper definitions: `size_in_pt` / `layout_in_pt` in `crates/fulgur-blitz/src/convert/mod.rs`;
`units::{Px, Pt, F32Units}` and `PX_TO_PT` in `crates/fulgur-core/src/units.rs`.

## Exception: `compute_transform` arguments

`blitz_adapter::compute_transform(styles, border_box_width, border_box_height)` is the one
place that runs Stylo length resolution in **pt space, not px**. The parameters are still
bare `f32`, but the `convert/mod.rs` call site feeds them **pt-valued** dims —
`size_in_pt(node.final_layout.size)` (see the "PR 8i note" near `convert/mod.rs:605`).

Percentage-based results (`%` translates, the default `transform-origin: 50% 50%`) resolve
self-consistently against this pt-valued basis and are consumed as pt **unconverted, by
design**: `LengthPercentage::resolve` is unit-agnostic, so `percent * pt_basis` already
lands in pt. Do not "fix" this path by feeding CSS-px dims instead — that reassociates the
float arithmetic (`(size*0.75)*p` → `(size*p)*0.75`) and would re-bless unrelated VRT
goldens with sub-ULP diffs (see the Approach A vs. B rationale in `bd show fulgur-9vw5`'s
design field).

Absolute-length results (`translate(Npx)`, `matrix()` tx/ty, `transform-origin: Npx ...`)
are different: `resolve()` ignores the basis for these and returns the literal CSS px
value, so they get a real px → pt fold (`Px::in_pt()`, ×0.75) inside
`compute_transform`/`op_to_matrix` — either through the `resolve_length_component` helper
(translate x/y, transform-origin horizontal/vertical) or directly (`matrix()`'s `e`/`f`,
which are always absolute `<number>`, never a percentage, so no branch is needed there).
Fixed in fulgur-9vw5; previously this value was reinterpreted as pt with no conversion, a
latent 4/3 over-shift. `calc(px + %)` mixed expressions still take the percentage path
(`has_percentage()` is true for `Calc`), so their absolute component is not folded — a
documented, known limitation, not a regression.

The returned `Affine2D.e`/`.f` (translate components) and the `Point2` origin are typed
`units::Pt` and are already correctly folded by the time they reach
`render::draw_under_transform`. **Do not add another `.in_pt()` at that consumer** — the
values arriving there are pt, and converting again would double-convert.

## Stylo length-percentage resolution

For **layout-space** resolves (positioned insets, pseudo offsets, border-radius,
viewport-relative lengths), `LengthPercentage::resolve(basis: Length)` — `basis` must be in
**CSS px**. Passing a pt value produces a 3/4× error.

```rust
// WRONG: pt basis
inset.resolve(Length::new(cb_width_pt)).px()

// CORRECT: px basis (layout values are CSS px)
inset.resolve(Length::new(cb_width_px)).px()
```

The `transform` path is the deliberate exception to this rule: it resolves against a **pt**
basis and uses the result directly as pt — see *Exception: `compute_transform` arguments*
above.

## Krilla / Pageable coordinate system

- **Origin: top-left, Y axis: downward (Y-down)**
- PDF spec (ISO 32000) uses bottom-left origin with Y up, but Krilla flips internally
- All fulgur code assumes top-left origin with Y growing down

`Quadrilateral` vertex order (`pageable.rs:297`):
bottom-left → bottom-right → top-right → top-left (in Y-down coordinates)

## CSS transform matrix composition (`Affine2D`)

```rust
// A.mul(&B) returns the matrix product A × B (point p transforms as A * B * p)
// CSS transform lists apply right-to-left (first operation is innermost)
let composed = t_origin.mul(&m).mul(&t_neg_origin);
// = T(ox, oy) · M · T(-ox, -oy)
```

`Affine2D`'s `(a, b, c, d, e, f)` maps to `krilla::geom::Transform::from_row(a, b, c, d, e, f)`.

## PDF text coordinates in inspect.rs

`Td`/`TD` operands are offsets in text coordinate space, not page space.
Apply the linear part of the text matrix `(a, b, c, d)` to convert to user-space displacement.

```rust
// WRONG: add offset directly to page coordinates
tx += dx; ty += dy;

// CORRECT: transform through text matrix linear part
tlm_e += dx * tm_a + dy * tm_c;
tlm_f += dx * tm_b + dy * tm_d;
```

`BT` resets both the text matrix and text line matrix to identity (PDF §9.4.1).
Track the CTM stack (`q`/`Q`) to obtain final page coordinates.

## References

- `crates/fulgur-blitz/src/convert/mod.rs` — `size_in_pt` / `layout_in_pt` helper definitions
- `crates/fulgur-core/src/units.rs` — `PX_TO_PT` and the `Px::in_pt` / `Pt::in_px` conversions
- `crates/fulgur-core/src/config.rs` — `PageSize::custom` mm definition
- `docs/plans/2026-04-17-viewport-pt-to-css-px.md` — deep-dive on the px/pt boundary bug
- PR #90 (superseded) / beads fulgur-9ul — history of the viewport pt/px misidentification fix
