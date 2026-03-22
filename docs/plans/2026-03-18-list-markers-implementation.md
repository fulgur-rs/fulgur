# List Marker Rendering Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Render list markers (ul/ol/li) in PDF output by adding ListItemPageable that reads Blitz's pre-computed marker data.

**Architecture:** Blitz already computes list marker strings and Parley layouts for `outside` positioned markers via `ElementData.list_item_data`. We extract the marker's shaped glyphs from the Parley Layout (same technique as `extract_paragraph`), store them in `ListItemPageable`, and draw the marker to the left of the body content.

**Tech Stack:** Rust, blitz-dom 0.2.4 (`ListItemLayout`, `Marker`, `ListItemLayoutPosition`), parley 0.6, krilla 0.6.0

---

### Task 1: Add ListItemPageable struct

**Files:**

- Modify: `crates/fulgur-core/src/pageable.rs`

**Step 1: Write the failing test**

Add at the end of the `mod tests` block in `pageable.rs`:

```rust
#[test]
fn test_list_item_delegates_to_body() {
    let body = make_spacer(100.0);
    let mut item = ListItemPageable {
        marker_lines: Vec::new(),
        marker_width: 20.0,
        body,
        style: BlockStyle::default(),
        width: 200.0,
        height: 100.0,
    };
    let size = item.wrap(200.0, 1000.0);
    assert!((size.height - 100.0).abs() < 0.01);
}

#[test]
fn test_list_item_split_keeps_marker_on_first_part() {
    let body = BlockPageable::new(vec![make_spacer(100.0), make_spacer(100.0), make_spacer(100.0)]);
    let mut body = body;
    body.wrap(200.0, 1000.0);
    let mut item = ListItemPageable {
        marker_lines: vec![],
        marker_width: 20.0,
        body: Box::new(body),
        style: BlockStyle::default(),
        width: 200.0,
        height: 300.0,
    };
    item.wrap(200.0, 1000.0);
    let result = item.split(200.0, 250.0);
    assert!(result.is_some());
    let (first, second) = result.unwrap();
    // First part keeps marker
    let first_item = first.as_any().downcast_ref::<ListItemPageable>().unwrap();
    assert!((first_item.marker_width - 20.0).abs() < 0.01);
    // Second part has no marker
    let second_item = second.as_any().downcast_ref::<ListItemPageable>().unwrap();
    assert!((second_item.marker_width - 0.0).abs() < 0.01);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p fulgur-core --lib test_list_item`
Expected: FAIL — `ListItemPageable` not defined

**Step 3: Add ListItemPageable and Pageable trait additions**

Add `as_any` method to the `Pageable` trait (needed for test downcasting):

```rust
// In trait Pageable
fn as_any(&self) -> &dyn std::any::Any;
```

Implement `as_any` on all existing Pageable types (BlockPageable, SpacerPageable, ParagraphPageable, ImagePageable) — just return `self`.

Add to `pageable.rs` after `SpacerPageable`:

```rust
// ─── ListItemPageable ───────────────────────────────────

use crate::paragraph::ShapedLine;

/// A list item with an outside-positioned marker.
#[derive(Clone)]
pub struct ListItemPageable {
    /// Shaped lines for the marker text (extracted from Blitz's Parley layout)
    pub marker_lines: Vec<ShapedLine>,
    /// Width of the marker (for positioning to the left of body)
    pub marker_width: Pt,
    /// The list item's body content
    pub body: Box<dyn Pageable>,
    /// Visual style (background, borders, padding)
    pub style: BlockStyle,
    /// Taffy-computed width
    pub width: Pt,
    /// Cached height from wrap()
    pub height: Pt,
}

impl Pageable for ListItemPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        let body_size = self.body.wrap(avail_width, avail_height);
        self.height = body_size.height;
        Size {
            width: avail_width,
            height: self.height,
        }
    }

    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        let (top_body, bottom_body) = self.body.split(avail_width, avail_height)?;
        Some((
            Box::new(ListItemPageable {
                marker_lines: self.marker_lines.clone(),
                marker_width: self.marker_width,
                body: top_body,
                style: self.style.clone(),
                width: self.width,
                height: 0.0,
            }),
            Box::new(ListItemPageable {
                marker_lines: Vec::new(),
                marker_width: 0.0,
                body: bottom_body,
                style: self.style.clone(),
                width: self.width,
                height: 0.0,
            }),
        ))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        // Draw marker to the left of the body
        if !self.marker_lines.is_empty() {
            let marker_x = x - self.marker_width;
            crate::paragraph::draw_shaped_lines(canvas, &self.marker_lines, marker_x, y);
        }
        // Draw body
        self.body.draw(canvas, x, y, avail_width, avail_height);
    }

    fn pagination(&self) -> Pagination {
        self.body.pagination()
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.height
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p fulgur-core --lib test_list_item`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/fulgur-core/src/pageable.rs
git commit -m "feat: add ListItemPageable struct with wrap/split/draw"
```

---

### Task 2: Extract draw_shaped_lines helper from ParagraphPageable

**Files:**

- Modify: `crates/fulgur-core/src/paragraph.rs`

The marker drawing reuses the same glyph rendering logic as ParagraphPageable. Extract the draw loop into a shared function.

**Step 1: Extract `draw_shaped_lines` function**

In `paragraph.rs`, extract the body of `ParagraphPageable::draw` into a standalone pub function:

```rust
/// Draw pre-shaped text lines at the given position.
pub fn draw_shaped_lines(canvas: &mut Canvas<'_, '_>, lines: &[ShapedLine], x: Pt, y: Pt) {
    let mut current_y = y;
    for line in lines {
        let baseline_y = current_y + line.baseline;
        for run in &line.glyph_runs {
            // ... (existing draw_glyphs logic, unchanged)
        }
        current_y += line.height;
    }
}
```

Update `ParagraphPageable::draw` to call `draw_shaped_lines(canvas, &self.lines, x, y)`.

**Step 2: Run all tests**

Run: `cargo test -p fulgur-core`
Expected: All existing tests PASS

**Step 3: Commit**

```bash
git add crates/fulgur-core/src/paragraph.rs
git commit -m "refactor: extract draw_shaped_lines from ParagraphPageable"
```

---

### Task 3: Extract marker glyphs from Blitz's Parley Layout

**Files:**

- Modify: `crates/fulgur-core/src/convert.rs`

**Step 1: Add `extract_marker_lines` function**

This function reads the Parley Layout from `ListItemLayoutPosition::Outside` and extracts `ShapedLine`s using the same technique as `extract_paragraph`.

```rust
use blitz_dom::node::{ListItemLayoutPosition, Marker};

/// Extract shaped lines from a list marker's Parley layout.
fn extract_marker_lines(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
) -> (Vec<ShapedLine>, f32) {
    let elem_data = match node.element_data() {
        Some(d) => d,
        None => return (Vec::new(), 0.0),
    };
    let list_item_data = match &elem_data.list_item_data {
        Some(d) => d,
        None => return (Vec::new(), 0.0),
    };
    let parley_layout = match &list_item_data.position {
        ListItemLayoutPosition::Outside(layout) => layout,
        ListItemLayoutPosition::Inside => return (Vec::new(), 0.0),
    };

    let marker_text = match &list_item_data.marker {
        Marker::Char(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).to_string()
        }
        Marker::String(s) => s.clone(),
    };

    let mut shaped_lines = Vec::new();
    let mut max_width: f32 = 0.0;

    for line in parley_layout.lines() {
        let metrics = line.metrics();
        let mut glyph_runs = Vec::new();
        let mut line_width: f32 = 0.0;

        for item in line.items() {
            if let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item {
                let run = glyph_run.run();
                let font_data = run.font();
                let font_bytes: Vec<u8> = font_data.data.data().to_vec();
                let font_index = font_data.index;
                let font_size = run.font_size();

                let brush = &glyph_run.style().brush;
                let color = get_text_color(doc, brush.id);

                let text_len = marker_text.len();
                let mut glyphs = Vec::new();
                for g in glyph_run.glyphs() {
                    line_width += g.advance;
                    glyphs.push(ShapedGlyph {
                        id: g.id,
                        x_advance: g.advance / font_size,
                        x_offset: g.x / font_size,
                        y_offset: g.y / font_size,
                        text_range: 0..text_len,
                    });
                }

                if !glyphs.is_empty() {
                    glyph_runs.push(ShapedGlyphRun {
                        font_data: Arc::new(font_bytes),
                        font_index,
                        font_size,
                        color,
                        glyphs,
                        text: marker_text.clone(),
                        x_offset: glyph_run.offset(),
                    });
                }
            }
        }

        max_width = max_width.max(line_width);
        shaped_lines.push(ShapedLine {
            height: metrics.line_height,
            baseline: metrics.baseline,
            glyph_runs,
        });
    }

    (shaped_lines, max_width)
}
```

**Step 2: Verify compilation**

Run: `cargo check -p fulgur-core`
Expected: Compiles (function is unused at this point — that's OK, we'll wire it in Task 4)

**Step 3: Commit**

```bash
git add crates/fulgur-core/src/convert.rs
git commit -m "feat: add extract_marker_lines for list marker glyph extraction"
```

---

### Task 4: Wire up ListItemPageable in convert_node

**Files:**

- Modify: `crates/fulgur-core/src/convert.rs`

**Step 1: Detect li elements and create ListItemPageable**

Modify `convert_node` to check for `list_item_data` before creating a generic BlockPageable. Add this check after the inline root check and before the generic container handling:

```rust
// In convert_node, after inline root check, before children.is_empty() check:

// Check if this is a list item with an outside marker
if let Some(elem_data) = node.element_data()
    && elem_data.list_item_data.is_some()
{
    let (marker_lines, marker_width) = extract_marker_lines(doc, node);
    let children: &[usize] = &node.children;
    let positioned_children = collect_positioned_children(doc, children);
    let style = extract_block_style(node);
    let mut body = BlockPageable::with_positioned_children(positioned_children).with_style(style.clone());
    body.wrap(width, 10000.0);

    let mut item = ListItemPageable {
        marker_lines,
        marker_width,
        body: Box::new(body),
        style: BlockStyle::default(), // li itself: style is on the body BlockPageable
        width,
        height: 0.0,
    };
    item.wrap(width, 10000.0);
    return Box::new(item);
}
```

Update the import at the top of `convert.rs`:

```rust
use crate::pageable::{BlockPageable, BlockStyle, ListItemPageable, Pageable, PositionedChild, SpacerPageable};
```

**Step 2: Run existing tests**

Run: `cargo test -p fulgur-core`
Expected: All existing tests PASS

**Step 3: Commit**

```bash
git add crates/fulgur-core/src/convert.rs
git commit -m "feat: wire ListItemPageable into DOM conversion for li elements"
```

---

### Task 5: Add integration test for list rendering

**Files:**

- Create: `crates/fulgur-core/tests/list_test.rs`

**Step 1: Write integration tests**

```rust
use fulgur_core::config::{Margin, PageSize};
use fulgur_core::engine::Engine;

fn make_engine() -> Engine {
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
}

#[test]
fn test_unordered_list_renders() {
    let engine = make_engine();
    let html = r#"
        <html><body>
            <ul>
                <li>Item one</li>
                <li>Item two</li>
                <li>Item three</li>
            </ul>
        </body></html>
    "#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 100);
}

#[test]
fn test_ordered_list_renders() {
    let engine = make_engine();
    let html = r#"
        <html><body>
            <ol>
                <li>First</li>
                <li>Second</li>
                <li>Third</li>
            </ol>
        </body></html>
    "#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 100);
}

#[test]
fn test_nested_list_renders() {
    let engine = make_engine();
    let html = r#"
        <html><body>
            <ul>
                <li>Parent item
                    <ul>
                        <li>Nested item</li>
                    </ul>
                </li>
            </ul>
        </body></html>
    "#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 100);
}

#[test]
fn test_mixed_list_styles_render() {
    let engine = make_engine();
    let html = r#"
        <html><body>
            <ol style="list-style-type: lower-alpha">
                <li>Alpha item</li>
                <li>Beta item</li>
            </ol>
            <ul style="list-style-type: square">
                <li>Square item</li>
            </ul>
        </body></html>
    "#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(pdf.len() > 100);
}
```

**Step 2: Run tests**

Run: `cargo test -p fulgur-core --test list_test`
Expected: All PASS

**Step 3: Commit**

```bash
git add crates/fulgur-core/tests/list_test.rs
git commit -m "test: add integration tests for list marker rendering"
```

---

### Task 6: Visual verification and cleanup

**Step 1: Create a visual test HTML**

Create `tmp/list_test.html`:

```html
<!DOCTYPE html>
<html>
<body style="font-family: sans-serif; font-size: 14px; line-height: 1.6;">
  <h2>Unordered List (disc)</h2>
  <ul>
    <li>First item</li>
    <li>Second item</li>
    <li>Third item with longer text to verify wrapping behavior</li>
  </ul>

  <h2>Ordered List (decimal)</h2>
  <ol>
    <li>First</li>
    <li>Second</li>
    <li>Third</li>
  </ol>

  <h2>Nested Lists</h2>
  <ul>
    <li>Parent
      <ul>
        <li>Child (circle)
          <ul>
            <li>Grandchild (square)</li>
          </ul>
        </li>
      </ul>
    </li>
  </ul>

  <h2>Ordered Variants</h2>
  <ol style="list-style-type: lower-alpha">
    <li>Alpha</li>
    <li>Beta</li>
    <li>Gamma</li>
  </ol>
  <ol style="list-style-type: upper-roman">
    <li>Roman I</li>
    <li>Roman II</li>
    <li>Roman III</li>
  </ol>
</body>
</html>
```

**Step 2: Generate PDF and visually inspect**

Run: `cargo run -p fulgur-cli -- render -o tmp/list_test.pdf tmp/list_test.html`

Open `tmp/list_test.pdf` and verify:

- Disc markers appear to the left of ul items
- Numbers appear to the left of ol items
- Nested lists have correct indentation and marker style changes
- Marker alignment looks reasonable

**Step 3: Fix any visual issues found**

If marker X position is wrong, adjust the offset calculation in `ListItemPageable::draw`.

**Step 4: Final commit**

```bash
git add -A
git commit -m "feat: complete list marker rendering (fulgur-rlf)"
```
