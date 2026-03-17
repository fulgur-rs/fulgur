/// Point unit (1/72 inch)
pub type Pt = f32;

#[derive(Debug, Clone, Copy)]
pub struct Size {
    pub width: Pt,
    pub height: Pt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakBefore {
    Auto,
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakAfter {
    Auto,
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakInside {
    Auto,
    Avoid,
}

#[derive(Debug, Clone, Copy)]
pub struct Pagination {
    pub break_before: BreakBefore,
    pub break_after: BreakAfter,
    pub break_inside: BreakInside,
    pub orphans: usize,
    pub widows: usize,
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            break_before: BreakBefore::Auto,
            break_after: BreakAfter::Auto,
            break_inside: BreakInside::Auto,
            orphans: 2,
            widows: 2,
        }
    }
}

/// Wrapper around Krilla Surface for drawing commands.
/// This decouples Pageable types from Krilla's concrete Surface type.
pub struct Canvas<'a, 'b> {
    pub surface: &'a mut krilla::surface::Surface<'b>,
}

/// Core pagination-aware layout trait.
pub trait Pageable: Send + Sync {
    /// Measure size within available area.
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size;

    /// Split at page boundary. Returns None if element fits entirely
    /// or cannot be split.
    fn split(&self, avail_width: Pt, avail_height: Pt)
        -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)>;

    /// Emit drawing commands.
    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt);

    /// CSS pagination properties for this element.
    fn pagination(&self) -> Pagination {
        Pagination::default()
    }

    /// Clone this pageable into a boxed trait object.
    fn clone_box(&self) -> Box<dyn Pageable>;

    /// Measured height from last wrap() call.
    fn height(&self) -> Pt;
}

impl Clone for Box<dyn Pageable> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

// ─── BlockStyle ──────────────────────────────────────────

/// Visual style for a block element.
#[derive(Clone, Debug, Default)]
pub struct BlockStyle {
    /// Background color as RGBA
    pub background_color: Option<[u8; 4]>,
    /// Border color as RGBA
    pub border_color: [u8; 4],
    /// Border widths: top, right, bottom, left
    pub border_widths: [f32; 4],
    /// Padding: top, right, bottom, left
    pub padding: [f32; 4],
}

// ─── PositionedChild ─────────────────────────────────────

/// A child element with its Taffy-computed position.
#[derive(Clone)]
pub struct PositionedChild {
    pub child: Box<dyn Pageable>,
    pub x: Pt,
    pub y: Pt,
}

// ─── BlockPageable ───────────────────────────────────────

/// A block container that positions children using Taffy layout coordinates.
/// Handles margin/border/padding/background and page splitting.
#[derive(Clone)]
pub struct BlockPageable {
    pub children: Vec<PositionedChild>,
    pub pagination: Pagination,
    pub cached_size: Option<Size>,
    pub style: BlockStyle,
}

impl BlockPageable {
    pub fn new(children: Vec<Box<dyn Pageable>>) -> Self {
        // Legacy constructor: stack children vertically
        let mut y = 0.0;
        let positioned: Vec<PositionedChild> = children.into_iter().map(|child| {
            let child_y = y;
            y += child.height();
            PositionedChild { child, x: 0.0, y: child_y }
        }).collect();
        Self {
            children: positioned,
            pagination: Pagination::default(),
            cached_size: None,
            style: BlockStyle::default(),
        }
    }

    pub fn with_positioned_children(children: Vec<PositionedChild>) -> Self {
        Self {
            children,
            pagination: Pagination::default(),
            cached_size: None,
            style: BlockStyle::default(),
        }
    }

    pub fn with_pagination(mut self, pagination: Pagination) -> Self {
        self.pagination = pagination;
        self
    }

    pub fn with_style(mut self, style: BlockStyle) -> Self {
        self.style = style;
        self
    }
}

impl Pageable for BlockPageable {
    fn wrap(&mut self, avail_width: Pt, _avail_height: Pt) -> Size {
        // Use max of children's (y + height) for total height
        let total_height = self.children.iter_mut().fold(0.0f32, |max_h, pc| {
            let child_h = pc.child.height();
            max_h.max(pc.y + child_h)
        });
        let size = Size { width: avail_width, height: total_height };
        self.cached_size = Some(size);
        size
    }

    fn split(&self, _avail_width: Pt, avail_height: Pt)
        -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)>
    {
        if self.pagination.break_inside == BreakInside::Avoid {
            return None;
        }

        let has_forced_break = self.children.iter().enumerate().any(|(i, pc)| {
            (pc.child.pagination().break_before == BreakBefore::Page && i > 0)
                || (pc.child.pagination().break_after == BreakAfter::Page && i < self.children.len() - 1)
        });

        let total_height = self.cached_size.map(|s| s.height).unwrap_or(0.0);
        if total_height <= avail_height && !has_forced_break {
            return None;
        }

        // Split based on children's y positions
        let mut split_index = self.children.len();
        let mut overflow_child_index: Option<usize> = None;

        for (i, pc) in self.children.iter().enumerate() {
            if pc.child.pagination().break_before == BreakBefore::Page && i > 0 && pc.y > 0.0 {
                split_index = i;
                break;
            }

            if pc.y + pc.child.height() > avail_height {
                if i == 0 && self.children.len() == 1 {
                    // Only child overflows — try to split it recursively
                    overflow_child_index = Some(i);
                } else {
                    split_index = i.max(1);
                }
                break;
            }

            if pc.child.pagination().break_after == BreakAfter::Page {
                split_index = i + 1;
                break;
            }
        }

        // Handle the case where a single child overflows: split it recursively
        if let Some(idx) = overflow_child_index {
            let pc = &self.children[idx];
            let child_avail = avail_height - pc.y;
            if child_avail > 0.0 {
                if let Some((first_part, second_part)) = pc.child.split(0.0, child_avail) {
                    let first = vec![PositionedChild { child: first_part, x: pc.x, y: pc.y }];
                    let second = vec![PositionedChild { child: second_part, x: pc.x, y: 0.0 }];
                    return Some((
                        Box::new(BlockPageable::with_positioned_children(first).with_pagination(self.pagination).with_style(self.style.clone())),
                        Box::new(BlockPageable::with_positioned_children(second).with_pagination(self.pagination).with_style(self.style.clone())),
                    ));
                }
            }
            return None;
        }

        if split_index == 0 || split_index >= self.children.len() {
            return None;
        }

        let split_y = self.children[split_index].y;

        let first: Vec<PositionedChild> = self.children[..split_index].iter()
            .map(|pc| PositionedChild { child: pc.child.clone_box(), x: pc.x, y: pc.y })
            .collect();
        let second: Vec<PositionedChild> = self.children[split_index..].iter()
            .map(|pc| PositionedChild { child: pc.child.clone_box(), x: pc.x, y: pc.y - split_y })
            .collect();

        Some((
            Box::new(BlockPageable::with_positioned_children(first).with_pagination(self.pagination).with_style(self.style.clone())),
            Box::new(BlockPageable::with_positioned_children(second).with_pagination(self.pagination).with_style(self.style.clone())),
        ))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        let total_height = self.cached_size.map(|s| s.height).unwrap_or(avail_height);

        // Draw background
        if let Some(bg) = &self.style.background_color {
            if let Some(rect) = krilla::geom::Rect::from_xywh(x, y, avail_width, total_height) {
                let mut pb = krilla::geom::PathBuilder::new();
                pb.push_rect(rect);
                if let Some(path) = pb.finish() {
                    canvas.surface.set_fill(Some(krilla::paint::Fill {
                        paint: krilla::color::rgb::Color::new(bg[0], bg[1], bg[2]).into(),
                        opacity: krilla::num::NormalizedF32::new(bg[3] as f32 / 255.0)
                            .unwrap_or(krilla::num::NormalizedF32::ONE),
                        rule: Default::default(),
                    }));
                    canvas.surface.set_stroke(None);
                    canvas.surface.draw_path(&path);
                }
            }
        }

        // Draw borders
        let [bt, br, bb, bl] = self.style.border_widths;
        if bt > 0.0 || br > 0.0 || bb > 0.0 || bl > 0.0 {
            let bc = &self.style.border_color;
            let stroke = krilla::paint::Stroke {
                paint: krilla::color::rgb::Color::new(bc[0], bc[1], bc[2]).into(),
                opacity: krilla::num::NormalizedF32::new(bc[3] as f32 / 255.0)
                    .unwrap_or(krilla::num::NormalizedF32::ONE),
                ..Default::default()
            };

            canvas.surface.set_fill(None);

            if bt > 0.0 {
                canvas.surface.set_stroke(Some(krilla::paint::Stroke { width: bt, ..stroke.clone() }));
                let mut pb = krilla::geom::PathBuilder::new();
                pb.move_to(x, y + bt / 2.0);
                pb.line_to(x + avail_width, y + bt / 2.0);
                if let Some(path) = pb.finish() { canvas.surface.draw_path(&path); }
            }
            if bb > 0.0 {
                canvas.surface.set_stroke(Some(krilla::paint::Stroke { width: bb, ..stroke.clone() }));
                let mut pb = krilla::geom::PathBuilder::new();
                pb.move_to(x, y + total_height - bb / 2.0);
                pb.line_to(x + avail_width, y + total_height - bb / 2.0);
                if let Some(path) = pb.finish() { canvas.surface.draw_path(&path); }
            }
            if bl > 0.0 {
                canvas.surface.set_stroke(Some(krilla::paint::Stroke { width: bl, ..stroke.clone() }));
                let mut pb = krilla::geom::PathBuilder::new();
                pb.move_to(x + bl / 2.0, y);
                pb.line_to(x + bl / 2.0, y + total_height);
                if let Some(path) = pb.finish() { canvas.surface.draw_path(&path); }
            }
            if br > 0.0 {
                canvas.surface.set_stroke(Some(krilla::paint::Stroke { width: br, ..stroke }));
                let mut pb = krilla::geom::PathBuilder::new();
                pb.move_to(x + avail_width - br / 2.0, y);
                pb.line_to(x + avail_width - br / 2.0, y + total_height);
                if let Some(path) = pb.finish() { canvas.surface.draw_path(&path); }
            }
            canvas.surface.set_stroke(None);
        }

        // Draw children at their Taffy-computed positions
        for pc in &self.children {
            pc.child.draw(canvas, x + pc.x, y + pc.y, avail_width, pc.child.height());
        }
    }

    fn pagination(&self) -> Pagination {
        self.pagination
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.cached_size.map(|s| s.height).unwrap_or(0.0)
    }
}

// ─── SpacerPageable ──────────────────────────────────────

/// Fixed-height vertical space. Cannot be split.
#[derive(Clone)]
pub struct SpacerPageable {
    pub height: Pt,
}

impl SpacerPageable {
    pub fn new(height: Pt) -> Self {
        Self { height }
    }
}

impl Pageable for SpacerPageable {
    fn wrap(&mut self, avail_width: Pt, _avail_height: Pt) -> Size {
        Size { width: avail_width, height: self.height }
    }

    fn split(&self, _avail_width: Pt, _avail_height: Pt)
        -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)>
    {
        None
    }

    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {
        // Spacers are invisible
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spacer(h: Pt) -> Box<dyn Pageable> {
        let mut s = SpacerPageable::new(h);
        s.wrap(100.0, 1000.0);
        Box::new(s)
    }

    #[test]
    fn test_block_fits_on_one_page() {
        let mut block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        block.wrap(200.0, 300.0);
        assert!(block.split(200.0, 300.0).is_none());
    }

    #[test]
    fn test_block_splits_across_pages() {
        let mut block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        block.wrap(200.0, 1000.0);
        let result = block.split(200.0, 250.0);
        assert!(result.is_some());
        let (first, second) = result.unwrap();
        let mut first = first;
        let mut second = second;
        let s1 = first.wrap(200.0, 250.0);
        let s2 = second.wrap(200.0, 1000.0);
        assert!((s1.height - 200.0).abs() < 0.01);
        assert!((s2.height - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_break_before_page() {
        let breaking = BlockPageable::new(vec![make_spacer(50.0)])
            .with_pagination(Pagination {
                break_before: BreakBefore::Page,
                ..Pagination::default()
            });
        let mut breaking = breaking;
        breaking.wrap(200.0, 1000.0);

        let mut block = BlockPageable::new(vec![
            make_spacer(50.0),
            make_spacer(50.0),
            Box::new(breaking),
        ]);
        block.wrap(200.0, 1000.0);

        // Even though everything fits in 1000pt, break-before should force split
        let result = block.split(200.0, 1000.0);
        assert!(result.is_some());
    }

    #[test]
    fn test_break_inside_avoid() {
        let block = BlockPageable::new(vec![make_spacer(200.0)])
            .with_pagination(Pagination {
                break_inside: BreakInside::Avoid,
                ..Pagination::default()
            });
        let mut block = block;
        block.wrap(200.0, 1000.0);
        // Even if it doesn't fit, split returns None
        assert!(block.split(200.0, 100.0).is_none());
    }
}
