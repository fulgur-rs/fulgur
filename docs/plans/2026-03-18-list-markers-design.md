# List Marker Rendering — Design Document

## Goal

Render list markers (ul/ol/li) in PDF output. Currently, li elements are converted to plain BlockPageable and markers are lost.

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Scope | All `list-style-type` values | Full CSS support |
| `list-style-position` | `outside` only | Most common; `inside` deferred |
| Indentation | Blitz/Taffy computed layout | Reuse pre-computed `padding-inline-start` |
| Marker rendering | Text glyphs (unified) | Simpler than shape primitives; better baseline accessibility without tagged PDF |
| Marker string source | `ElementData.list_item_data.marker` | Blitz already resolves counter values, `start`/`value` attrs, and nested `list-style-type` defaults |
| Counter management | Blitz-managed | No custom counter logic needed in fulgur |
| ul/ol container | Existing `BlockPageable` | No special type needed; padding/margin handled by Taffy |

## Marker Characters

Blitz generates marker strings via `Marker::Char` or `Marker::String`:

| list-style-type | Marker example |
|-----------------|---------------|
| disc | U+2022 (●) |
| circle | U+25CB (○) |
| square | U+25A0 (■) |
| decimal | "1.", "2.", ... |
| lower-alpha | "a.", "b.", ... |
| upper-alpha | "A.", "B.", ... |
| lower-roman | "i.", "ii.", ... |
| upper-roman | "I.", "II.", ... |
| (others) | As resolved by Blitz/Stylo |

## Data Model

```rust
pub struct ListItemPageable {
    pub marker_text: String,
    pub marker_glyphs: Vec<ShapedGlyph>,
    pub marker_width: f32,
    pub body: Box<dyn Pageable>,
    pub style: BlockStyle,
    pub width: f32,
    pub height: f32,
}
```

## Conversion (convert.rs)

1. Detect li node during DOM traversal
2. Read `element.list_item_data` → extract marker string
3. Shape marker text with Parley (using li's computed font size and color)
4. Convert li's children as BlockPageable → assign to `body`
5. ul/ol nodes convert to BlockPageable as usual

Nested lists work naturally:

```html
<ul>       → BlockPageable
  <li>     → ListItemPageable (marker + body)
    <ul>   → BlockPageable inside body
      <li> → ListItemPageable
```

## Drawing

- Marker drawn at `(x - marker_width, y)` — outside the body's content box
- Marker Y-coordinate aligned to first line baseline of body
- Body drawn via `body.draw()` delegation

## Pagination

- `wrap()`: delegates to body; marker is outside so doesn't affect size
- `split()`: delegates to body; top half keeps marker, bottom half gets empty marker
- `pagination()`: delegates to body's pagination properties

```rust
fn split(&self, w: Pt, h: Pt) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
    let (top_body, bottom_body) = self.body.split(w, h)?;
    Some((
        ListItemPageable { marker_text: self.marker_text.clone(), body: top_body, .. },
        ListItemPageable { marker_text: String::new(), body: bottom_body, .. },
    ))
}
```

## Future Work

- `list-style-position: inside` — render marker inline within text flow
- Shape primitive markers (fulgur-t6p) — disc/circle/square as vector shapes with `ActualText` for PDF/UA
- Tagged PDF list structure: `L > LI > Lbl + LBody` (Phase 4)

## Files to Modify

- `crates/fulgur-core/src/pageable.rs` — add `ListItemPageable`
- `crates/fulgur-core/src/convert.rs` — detect li, read `list_item_data`, generate `ListItemPageable`
