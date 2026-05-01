# Phase 4 PR 8g: inline-box draw migration to Drawables — design

**Status**: design draft
**Author**: claude (with Mitsuru)
**Created**: 2026-05-01
**Phase 4 epic**: fulgur-9t3z
**Depends on**: PR 8c–8f merged into pageable-replacement (in-flight as PR #324)

## Goal

Delete `Pageable::draw` and `Pageable::is_visible` trait methods. After this PR, the `Pageable` trait holds only `clone_box`, `as_any`, `node_id` — pure trait-object machinery, no rendering or visibility logic. PR 8h (direct-convert) follows up to remove the trait entirely.

## Background

PR 8a-8f progressively stripped the `Pageable` trait of methods that v2 dispatch did not need:

- 8a: `render::render_to_pdf*` public surface
- 8b: `slice_for_page`, `pagination`
- 8c: `Pagination` struct (write-only fields)
- 8d: `wrap`
- 8e: `collect_ids`
- 8f: `height`

The remaining methods all sit on the `paragraph::draw_shaped_lines` inline-box draw path:

```rust
// paragraph.rs:802 (current)
LineItem::InlineBox(ib) => {
    if !ib.visible { continue; }
    let ox = x + ib.x_offset;
    let oy = line_top_abs + ib.computed_y;
    crate::pageable::draw_with_opacity(canvas, ib.opacity, |canvas| {
        ib.content.draw(canvas, ox, oy, ib.width, ib.height); // ← Pageable::draw
    });
    // ... link rect ...
}
```

`ib.content` is `Box<dyn Pageable>` (the inline-box's CSS content tree). Its only consumer is this draw call.

## PR 8e first attempt (failed)

PR 8e tried to recurse `extract_drawables_from_pageable` into inline-box content and let the v2 dispatcher render it. Result: visual diff in 6 VRT goldens (`layout/inline-block-basic.html` etc.) showed the inline-block content rendered at the **wrong position** — Taffy-recorded geometry vs Parley inline-flow position.

**Root cause** (verified via `convert/inline_root.rs:485-505`): fulgur applies a CSS 2.1 §10.8.1 baseline correction at convert time:

```rust
let baseline_shift = inline_box_baseline_offset(content)
    .map(|inner_baseline| height_pt - inner_baseline)
    .unwrap_or(0.0);
let computed_y = px_to_pt(positioned.y) - accumulated_line_top + baseline_shift;
```

Taffy stores Parley's `positioned.y` (bottom-aligned to surrounding text baseline). fulgur's render shifts by `baseline_shift` to align the *inner* last-line baseline with the surrounding baseline (CSS 2.1). The fragmenter records Taffy's stored value (no shift) at body-relative coordinates; paragraph render computes `oy` with shift applied. The two positions differ by `baseline_shift`.

PR 8e migration assumed the geometry-recorded position equalled the inline-flow position, which is not true when `inline_box_baseline_offset` is non-zero.

## Approach: offset transform

The paragraph render path already knows the corrected `(ox, oy)`. Wrap the v2 dispatch in a translate transform equal to `(ox - geometry_recorded_x, oy - geometry_recorded_y)`. The standard dispatcher then renders the inline-box content (and its descendants) at the corrected position.

```text
push_transform(translate(off_x, off_y))
    dispatch_fragment(content_node_id)
    for desc in descendants: dispatch_fragment(desc)
pop_transform
```

This kills double-rendering by gating the v2 dispatcher's main loop on a per-NodeId skip set populated at extract time.

## Drawables additions

```rust
pub struct Drawables {
    // ... existing fields ...

    /// PR 8g: NodeIds the v2 dispatcher's main loop must skip because
    /// they belong to inline-box content (or its descendants) dispatched
    /// explicitly by `paragraph::draw_shaped_lines` under an offset
    /// transform. Membership in this set means: "do not dispatch at the
    /// geometry-recorded body-relative position; the paragraph render
    /// path owns this NodeId and will translate it to inline-flow
    /// position before invoking the standard dispatcher."
    pub inline_box_subtree_skip: BTreeSet<NodeId>,

    /// PR 8g: per-inline-box-content descendant list. Keyed by the
    /// inline-box content's root NodeId; value is the strict descendant
    /// NodeIds the paragraph render path dispatches under the same
    /// offset transform. Both the key and values appear in
    /// `inline_box_subtree_skip`. `BTreeMap` keeps iteration
    /// deterministic for PDF byte-equality.
    pub inline_box_subtree_descendants: BTreeMap<NodeId, Vec<NodeId>>,
}
```

## Pageable::is_visible deletion

The only production caller is `convert/inline_root.rs:505`:

```rust
let visible = content.is_visible(); // ← reads through wrappers to inner Block/Paragraph/etc.
```

CSS `visibility: hidden` resolves on the inline-box's own DOM node. Wrapper Pageables (Transform, BookmarkMarker, CounterOp, StringSet, RunningElement) do not change visibility — they delegate to the inner. Therefore `box_node`'s computed `visibility` is equivalent to `content.is_visible()`.

```rust
// New (no trait dispatch needed)
let (_, visible) = extract_opacity_visible(box_node);
items.push(LineItem::InlineBox(InlineBoxItem { content, /* ... */, visible }));
```

Delete `Pageable::is_visible` (default impl + 7 concrete + 6 wrapper delegate impls).

## ImagePageable / SvgPageable type elimination

Currently `ImagePageable` and `SvgPageable` exist as Pageable trait implementors AND as data containers used by `ListItemMarker::Image { marker: ImageMarker::Raster(ImagePageable) }`. After PR 8g their fields are identical to `ImageEntry` / `SvgEntry`. Migrate:

```rust
// drawables.rs
pub struct ImageEntry {
    pub image_data: Arc<Vec<u8>>,
    pub format: ImageFormat,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub visible: bool,
    pub node_id: Option<NodeId>, // PR 8g: added
}
impl Pageable for ImageEntry {
    fn clone_box(&self) -> Box<dyn Pageable> { Box::new(self.clone()) }
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn node_id(&self) -> Option<usize> { self.node_id }
}
// SvgEntry is parallel.
```

Construction sites in `convert/replaced.rs`, `convert/list_marker.rs` swap `ImagePageable::new(data, format, w, h)` for direct `ImageEntry { ... }` literals. `extract_drawables_from_pageable` matches `downcast_ref::<ImageEntry>()` instead of `<ImagePageable>()` and inserts a clone into `out.images`. `ListItemMarker::ImageMarker` variants hold `ImageEntry` / `SvgEntry`.

## Pageable::draw deletion

The trait method is unreachable once `paragraph::draw_shaped_lines` stops calling `ib.content.draw(...)`. Internal recursive `pc.child.draw(...)` calls inside `BlockPageable::draw` / `TablePageable::draw` / wrapper delegates die together with their containing impls (PR 8d/8e/8f cascade pattern).

`render.rs:2311` (ListItemMarker render) calls `img.draw(canvas, ...)` and `svg.draw(canvas, ...)` directly on `ImageEntry` / `SvgEntry`. Replace with free functions in `image.rs` / `svg.rs`:

```rust
// image.rs
pub(crate) fn draw_image_entry(
    canvas: &mut Canvas<'_, '_>,
    entry: &ImageEntry,
    x: Pt,
    y: Pt,
    width: Pt,
    height: Pt,
) {
    // Body of existing `impl Pageable for ImagePageable { fn draw }`.
}
```

After removal: `Pageable` trait holds only `clone_box`, `as_any`, `node_id`. `BlockPageable::draw`, `ParagraphPageable::draw`, `TablePageable::draw`, `ListItemPageable::draw`, `MulticolRulePageable::draw`, `BookmarkMarkerPageable::draw`, 6 wrapper delegate `draw`s — all gone.

## extract_drawables_from_pageable changes

```rust
if let Some(para) = any.downcast_ref::<ParagraphPageable>() {
    // existing: register ParagraphEntry
    if let Some(node_id) = para.node_id { /* ... */ }

    // PR 8g: recurse into inline-box content + populate skip set.
    for line in &para.lines {
        for item in &line.items {
            if let LineItem::InlineBox(ib) = item {
                let before = collect_known_node_ids(out);
                extract_drawables_from_pageable(ib.content.as_ref(), out);
                let after = collect_known_node_ids(out);
                if let Some(content_id) = ib.content.node_id() {
                    let descendants: Vec<NodeId> = after
                        .difference(&before)
                        .copied()
                        .filter(|id| *id != content_id)
                        .collect();
                    out.inline_box_subtree_skip.insert(content_id);
                    out.inline_box_subtree_skip.extend(&descendants);
                    out.inline_box_subtree_descendants.insert(content_id, descendants);
                }
            }
        }
    }
    return;
}
```

`collect_known_node_ids` is a helper returning `block_styles ∪ paragraphs ∪ images ∪ svgs ∪ tables ∪ list_items` keys.

Before/after snapshot pattern matches existing `BlockEntry::clip_descendants` / `opacity_descendants` populate (Phase 4 PR 5/6).

## v2 dispatcher main loop changes

```rust
// render.rs (existing main loop, simplified)
for (&node_id, geom) in geometry {
    if drawables.inline_box_subtree_skip.contains(&node_id) {
        continue; // PR 8g: dispatched by paragraph render under offset transform
    }
    // ... bookmark anchor / transformed_descendants / clipped_descendants /
    // opacity_wrapped_descendants skip checks (existing) ...
    // ... dispatch_fragment(node_id, ...) ...
}
```

Single-line addition. Skip ordering: place after bookmark anchor record (anchor pre-skip is already separate) and parallel with the other descendant-skip checks.

## paragraph::draw_shaped_lines changes

Function gains `drawables: &Drawables` and `geometry: &PaginationGeometryTable` parameters (call site in `render::render_v2` already has both).

```rust
LineItem::InlineBox(ib) => {
    if !ib.visible { continue; }
    let ox = x + ib.x_offset;
    let oy = line_top_abs + ib.computed_y;

    // PR 8g: dispatch via v2 path under offset transform.
    if let Some(content_id) = ib.content.node_id()
        && let Some(geom) = geometry.get(&content_id)
        && let Some(frag) = geom.fragments.first()
    {
        let geo_x_pt = px_to_pt(frag.x);
        let geo_y_pt = px_to_pt(frag.y) + body_offset_pt.1; // body-relative y → absolute
        let off_x = ox - geo_x_pt;
        let off_y = oy - geo_y_pt;
        let transform = krilla::geom::Transform::from_translate(off_x, off_y);
        crate::pageable::draw_with_opacity(canvas, ib.opacity, |canvas| {
            canvas.surface.push_transform(&transform);
            dispatch_fragment_at_geometry(canvas, content_id, drawables, geometry, /* ... */);
            if let Some(descendants) = drawables.inline_box_subtree_descendants.get(&content_id) {
                for &desc_id in descendants {
                    dispatch_fragment_at_geometry(canvas, desc_id, drawables, geometry, /* ... */);
                }
            }
            canvas.surface.pop();
        });
    }

    // Link rect (existing, unchanged)
    if let Some(link_span) = ib.link.as_ref() { /* ... */ }
}
```

`dispatch_fragment_at_geometry` is the existing `dispatch_fragment` from `render.rs`, exposed at `pub(crate)` so paragraph.rs can call it.

## Multi-page paragraph note

`geom.fragments.first()` simplifies the lookup. Inline-box content spans only the lines in its containing paragraph fragment, so a paragraph fragment that covers the line containing the inline-box also contains the inline-box's geometry fragment. Multi-page paragraphs that split across the inline-box are not specifically handled in PR 8g — matches existing behaviour where `paragraph_lines_for_page` slices lines per page; the inline-box only appears on the page hosting its line.

## Future optimization

Multiple `push_transform` / `pop_transform` pairs per paragraph (one per inline-box) could be batched at the krilla content-stream level — measure on paragraph-heavy pages first to confirm the cost is real. Out of scope for PR 8g.

## Test plan

### lib tests (codecov target)

1. `extract_drawables_from_pageable` populates `inline_box_subtree_skip` / `inline_box_subtree_descendants` correctly for an inline-block paragraph.
2. `extract_opacity_visible(box_node)` returns the same `visible` value that `Pageable::is_visible` previously did, including through wrapper chains.
3. `draw_image_entry` / `draw_svg_entry` smoke: render a list with `list-style-image` and assert non-empty PDF (existing test reused).
4. `impl Pageable for ImageEntry`: `clone_box` produces an equivalent entry, `as_any` downcast roundtrips, `node_id` returns the stored value.

### integration tests (`render_smoke.rs`)

1. Inline `<svg>` inside paragraph (`<p>text <svg>...</svg> text</p>`) renders with baseline_shift applied.
2. Nested inline-block (3 levels: `<span style="display:inline-block"><span style="display:inline-block">...</span></span>`) — recursive subtree skip / dispatch.
3. `<span style="display:inline-block; visibility:hidden">` skips render (`ib.visible == false`).
4. Inline-block with `overflow:hidden` (forces `inline_box_baseline_offset → None` fallback to zero shift).

### VRT byte-equality

1. The 6 fixtures that diverged in PR 8e first attempt — `layout/inline-block-basic.html`, `layout/inline-block-nested.html`, `layout/inline-flex-smoke.html`, `layout/inline-grid-smoke.html`, `layout/review_card_inline_block.html`, `svg/shapes.html` — now byte-match their goldens.
2. ListItemMarker::Image fixtures (`bullet.png` / svg-marker variants) byte-match (ImageEntry migration is field-equivalent).

## Risk areas

- **Multi-page paragraph + inline-box**: `geom.fragments.first()` assumes single-fragment for the inline-box content. Paragraphs may split across pages but inline-box content stays line-bound, so its geometry fragment lives on the line's page. Validate via test #5 with a tall paragraph spanning two pages.
- **Nested inline-box subtree-skip recursion**: handled by the recursive `extract_drawables_from_pageable` call. Test #6 confirms.
- **`Pageable::clone_box` for `ImageEntry`**: trait machinery added in this PR; verify the test #4 roundtrip.
- **`extract_opacity_visible` parity**: check that all wrapper-walked test fixtures (Transform / BookmarkMarker / CounterOp inside an inline-block) keep the same visible flag. Test #2 covers the canonical wrapper chain.

## Follow-up: PR 8h (direct convert)

After PR 8g, `Pageable` trait holds only `clone_box`, `as_any`, `node_id` (3 trait-machinery methods). `Box<dyn Pageable>` is still used as the convert intermediate, walked once by `extract_drawables_from_pageable` to populate `Drawables`.

PR 8h rewrites `convert::dom_to_pageable` → `convert::dom_to_drawables` to populate `Drawables` directly during the DOM walk. The `Pageable` trait, `BlockPageable` / `ParagraphPageable` / `ListItemPageable` / `TablePageable` / `MulticolRulePageable` types, all wrapper types, `PositionedChild`, and `extract_drawables_from_pageable` go away in one PR. Estimated 2-3K LOC. Outside PR 8g's scope.

Stack ordering preserves single-PR review-ability and lets PR 8g land independently while PR 8h's convert rewrite gets dedicated review.
