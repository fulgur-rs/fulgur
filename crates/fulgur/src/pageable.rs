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
    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)>;

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

    /// Downcast support for tests.
    fn as_any(&self) -> &dyn std::any::Any;
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
    /// Border radii: [top-left, top-right, bottom-right, bottom-left] × [rx, ry]
    pub border_radii: [[f32; 2]; 4],
    /// Border styles: top, right, bottom, left
    pub border_styles: [BorderStyleValue; 4],
}

/// CSS border-style values supported by fulgur.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorderStyleValue {
    /// No border drawn
    None,
    /// Solid line (default when border-width > 0)
    #[default]
    Solid,
    /// Dashed line
    Dashed,
    /// Dotted line
    Dotted,
    /// Two parallel lines
    Double,
    /// 3D grooved effect
    Groove,
    /// 3D ridged effect
    Ridge,
    /// 3D inset effect
    Inset,
    /// 3D outset effect
    Outset,
}

impl BlockStyle {
    /// Whether any border radius is non-zero.
    pub fn has_radius(&self) -> bool {
        self.border_radii.iter().any(|r| r[0] > 0.0 || r[1] > 0.0)
    }

    /// Whether this style has any visual properties (background, border, or padding).
    pub fn has_visual_style(&self) -> bool {
        self.background_color.is_some()
            || self.border_widths.iter().any(|&w| w > 0.0)
            || self.padding.iter().any(|&p| p > 0.0)
    }

    /// Returns (left_inset, top_inset) for content positioning inside border+padding.
    pub fn content_inset(&self) -> (f32, f32) {
        (
            self.border_widths[3] + self.padding[3],
            self.border_widths[0] + self.padding[0],
        )
    }
}

// ─── PositionedChild ─────────────────────────────────────

/// A child element with its Taffy-computed position.
#[derive(Clone)]
pub struct PositionedChild {
    pub child: Box<dyn Pageable>,
    pub x: Pt,
    pub y: Pt,
}

type SplitPair = (Box<dyn Pageable>, Box<dyn Pageable>);

// ─── BlockPageable ───────────────────────────────────────

/// A block container that positions children using Taffy layout coordinates.
/// Handles margin/border/padding/background and page splitting.
#[derive(Clone)]
pub struct BlockPageable {
    pub children: Vec<PositionedChild>,
    pub pagination: Pagination,
    pub cached_size: Option<Size>,
    /// Taffy-computed layout size (preserved across wrap() calls for drawing).
    pub layout_size: Option<Size>,
    pub style: BlockStyle,
}

impl BlockPageable {
    pub fn new(children: Vec<Box<dyn Pageable>>) -> Self {
        // Legacy constructor: stack children vertically
        let mut y = 0.0;
        let positioned: Vec<PositionedChild> = children
            .into_iter()
            .map(|child| {
                let child_y = y;
                y += child.height();
                PositionedChild {
                    child,
                    x: 0.0,
                    y: child_y,
                }
            })
            .collect();
        Self {
            children: positioned,
            pagination: Pagination::default(),
            cached_size: None,
            layout_size: None,
            style: BlockStyle::default(),
        }
    }

    pub fn with_positioned_children(children: Vec<PositionedChild>) -> Self {
        Self {
            children,
            pagination: Pagination::default(),
            cached_size: None,
            layout_size: None,
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

/// Build a rounded rectangle path using cubic Bézier curves for corners.
/// radii: [top-left, top-right, bottom-right, bottom-left] × [rx, ry]
fn build_rounded_rect_path(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radii: &[[f32; 2]; 4],
) -> Option<krilla::geom::Path> {
    // Bézier approximation constant for quarter circle
    const KAPPA: f32 = 0.552_284_8;

    // CSS spec: if adjacent radii sum exceeds an edge, scale all radii proportionally.
    // Compute the minimum scale factor across all four edges.
    let scale = |a: f32, b: f32, edge: f32| -> f32 {
        let sum = a + b;
        if sum > edge && sum > 0.0 {
            edge / sum
        } else {
            1.0
        }
    };
    let f = scale(radii[0][0], radii[1][0], w) // top edge (rx)
        .min(scale(radii[1][1], radii[2][1], h)) // right edge (ry)
        .min(scale(radii[2][0], radii[3][0], w)) // bottom edge (rx)
        .min(scale(radii[3][1], radii[0][1], h)); // left edge (ry)

    let r: [[f32; 2]; 4] = [
        [radii[0][0] * f, radii[0][1] * f],
        [radii[1][0] * f, radii[1][1] * f],
        [radii[2][0] * f, radii[2][1] * f],
        [radii[3][0] * f, radii[3][1] * f],
    ];

    let mut pb = krilla::geom::PathBuilder::new();

    // Start at top-left corner (after radius)
    pb.move_to(x + r[0][0], y);

    // Top edge → top-right corner
    pb.line_to(x + w - r[1][0], y);
    if r[1][0] > 0.0 || r[1][1] > 0.0 {
        pb.cubic_to(
            x + w - r[1][0] * (1.0 - KAPPA),
            y,
            x + w,
            y + r[1][1] * (1.0 - KAPPA),
            x + w,
            y + r[1][1],
        );
    }

    // Right edge → bottom-right corner
    pb.line_to(x + w, y + h - r[2][1]);
    if r[2][0] > 0.0 || r[2][1] > 0.0 {
        pb.cubic_to(
            x + w,
            y + h - r[2][1] * (1.0 - KAPPA),
            x + w - r[2][0] * (1.0 - KAPPA),
            y + h,
            x + w - r[2][0],
            y + h,
        );
    }

    // Bottom edge → bottom-left corner
    pb.line_to(x + r[3][0], y + h);
    if r[3][0] > 0.0 || r[3][1] > 0.0 {
        pb.cubic_to(
            x + r[3][0] * (1.0 - KAPPA),
            y + h,
            x,
            y + h - r[3][1] * (1.0 - KAPPA),
            x,
            y + h - r[3][1],
        );
    }

    // Left edge → top-left corner
    pb.line_to(x, y + r[0][1]);
    if r[0][0] > 0.0 || r[0][1] > 0.0 {
        pb.cubic_to(
            x,
            y + r[0][1] * (1.0 - KAPPA),
            x + r[0][0] * (1.0 - KAPPA),
            y,
            x + r[0][0],
            y,
        );
    }

    pb.close();
    pb.finish()
}

/// Clone a slice of PositionedChild, optionally shifting y coordinates.
/// When `y_offset` is 0.0, children are cloned as-is.
/// A negative `y_offset` shifts children upward (subtracts from y).
fn clone_children(children: &[PositionedChild], y_offset: f32) -> Vec<PositionedChild> {
    children
        .iter()
        .map(|pc| PositionedChild {
            child: pc.child.clone_box(),
            x: pc.x,
            y: pc.y - y_offset,
        })
        .collect()
}

/// Draw the background fill for a block or table element.
fn draw_block_background(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let Some(bg) = &style.background_color else {
        return;
    };
    let path = if style.has_radius() {
        build_rounded_rect_path(x, y, w, h, &style.border_radii)
    } else if let Some(rect) = krilla::geom::Rect::from_xywh(x, y, w, h) {
        let mut pb = krilla::geom::PathBuilder::new();
        pb.push_rect(rect);
        pb.finish()
    } else {
        None
    };

    if let Some(path) = path {
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

/// Lighten an RGBA color by a factor (0.0–1.0). Higher factor = lighter.
fn lighten_color(c: &[u8; 4], factor: f32) -> [u8; 4] {
    [
        (c[0] as f32 + (255.0 - c[0] as f32) * factor) as u8,
        (c[1] as f32 + (255.0 - c[1] as f32) * factor) as u8,
        (c[2] as f32 + (255.0 - c[2] as f32) * factor) as u8,
        c[3],
    ]
}

/// Darken an RGBA color by a factor (0.0–1.0). Higher factor = darker.
fn darken_color(c: &[u8; 4], factor: f32) -> [u8; 4] {
    [
        (c[0] as f32 * (1.0 - factor)) as u8,
        (c[1] as f32 * (1.0 - factor)) as u8,
        (c[2] as f32 * (1.0 - factor)) as u8,
        c[3],
    ]
}

/// For 3D border styles, determine the light and dark colors for a given side.
/// Returns (outer_color, inner_color) for groove/ridge, or just the single color for inset/outset.
/// `is_top_or_left`: true for top/left sides, false for bottom/right sides.
fn border_3d_colors(
    base: &[u8; 4],
    style: BorderStyleValue,
    is_top_or_left: bool,
) -> ([u8; 4], Option<[u8; 4]>) {
    let light = lighten_color(base, 0.5);
    let dark = darken_color(base, 0.5);
    match style {
        BorderStyleValue::Groove => {
            if is_top_or_left {
                (dark, Some(light))
            } else {
                (light, Some(dark))
            }
        }
        BorderStyleValue::Ridge => {
            if is_top_or_left {
                (light, Some(dark))
            } else {
                (dark, Some(light))
            }
        }
        BorderStyleValue::Inset => {
            if is_top_or_left {
                (dark, None)
            } else {
                (light, None)
            }
        }
        BorderStyleValue::Outset => {
            if is_top_or_left {
                (light, None)
            } else {
                (dark, None)
            }
        }
        _ => (*base, None),
    }
}

/// Apply border-style dash settings to a stroke.
fn apply_border_style(
    stroke: krilla::paint::Stroke,
    style: BorderStyleValue,
    width: f32,
) -> Option<krilla::paint::Stroke> {
    match style {
        BorderStyleValue::None => None,
        BorderStyleValue::Solid => Some(stroke),
        BorderStyleValue::Dashed => {
            let dash_len = width * 3.0;
            Some(krilla::paint::Stroke {
                dash: Some(krilla::paint::StrokeDash {
                    array: vec![dash_len, dash_len],
                    offset: 0.0,
                }),
                ..stroke
            })
        }
        BorderStyleValue::Dotted => Some(krilla::paint::Stroke {
            line_cap: krilla::paint::LineCap::Round,
            dash: Some(krilla::paint::StrokeDash {
                array: vec![0.0, width * 2.0],
                offset: 0.0,
            }),
            ..stroke
        }),
        BorderStyleValue::Double
        | BorderStyleValue::Groove
        | BorderStyleValue::Ridge
        | BorderStyleValue::Inset
        | BorderStyleValue::Outset => Some(stroke), // handled specially at call site
    }
}

/// Helper to draw a simple line segment with a given stroke.
fn stroke_line(
    canvas: &mut Canvas<'_, '_>,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    stroke: krilla::paint::Stroke,
) {
    canvas.surface.set_stroke(Some(stroke));
    let mut pb = krilla::geom::PathBuilder::new();
    pb.move_to(x1, y1);
    pb.line_to(x2, y2);
    if let Some(path) = pb.finish() {
        canvas.surface.draw_path(&path);
    }
}

/// Create a stroke with a specific color and width, inheriting opacity from base.
fn colored_stroke(
    color: &[u8; 4],
    width: f32,
    opacity: krilla::num::NormalizedF32,
) -> krilla::paint::Stroke {
    krilla::paint::Stroke {
        paint: krilla::color::rgb::Color::new(color[0], color[1], color[2]).into(),
        width,
        opacity,
        ..Default::default()
    }
}

/// Draw a single border line with style, handling double and 3D effects.
/// `base_color` is the original RGBA border color (needed for 3D color computation).
/// `is_top_or_left` determines the light/dark side for 3D styles.
#[allow(clippy::too_many_arguments)]
fn draw_border_line(
    canvas: &mut Canvas<'_, '_>,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    width: f32,
    style: BorderStyleValue,
    base_color: &[u8; 4],
    opacity: krilla::num::NormalizedF32,
    is_top_or_left: bool,
) {
    if width <= 0.0 || style == BorderStyleValue::None {
        return;
    }

    match style {
        BorderStyleValue::Double => {
            let gap = width / 3.0;
            let dx = x2 - x1;
            let dy = y2 - y1;
            let len = (dx * dx + dy * dy).sqrt();
            if len == 0.0 {
                return;
            }
            let nx = -dy / len * gap;
            let ny = dx / len * gap;
            let thin = colored_stroke(base_color, width / 3.0, opacity);
            stroke_line(canvas, x1 + nx, y1 + ny, x2 + nx, y2 + ny, thin.clone());
            stroke_line(canvas, x1 - nx, y1 - ny, x2 - nx, y2 - ny, thin);
        }
        BorderStyleValue::Groove | BorderStyleValue::Ridge => {
            let (outer_color, inner_color) = border_3d_colors(base_color, style, is_top_or_left);
            let inner_color = inner_color.unwrap_or(outer_color);
            let dx = x2 - x1;
            let dy = y2 - y1;
            let len = (dx * dx + dy * dy).sqrt();
            if len == 0.0 {
                return;
            }
            let half = width / 4.0;
            let nx = -dy / len * half;
            let ny = dx / len * half;
            let half_w = width / 2.0;
            // +normal points outward for top/left, inward for bottom/right.
            // Swap direction for bottom/right so outer_color is always on the outside.
            let (out_sign, in_sign) = if is_top_or_left {
                (1.0, -1.0)
            } else {
                (-1.0, 1.0)
            };
            stroke_line(
                canvas,
                x1 + nx * out_sign,
                y1 + ny * out_sign,
                x2 + nx * out_sign,
                y2 + ny * out_sign,
                colored_stroke(&outer_color, half_w, opacity),
            );
            stroke_line(
                canvas,
                x1 + nx * in_sign,
                y1 + ny * in_sign,
                x2 + nx * in_sign,
                y2 + ny * in_sign,
                colored_stroke(&inner_color, half_w, opacity),
            );
        }
        BorderStyleValue::Inset | BorderStyleValue::Outset => {
            let (color, _) = border_3d_colors(base_color, style, is_top_or_left);
            stroke_line(
                canvas,
                x1,
                y1,
                x2,
                y2,
                colored_stroke(&color, width, opacity),
            );
        }
        _ => {
            let base = colored_stroke(base_color, width, opacity);
            if let Some(styled) = apply_border_style(base, style, width) {
                stroke_line(canvas, x1, y1, x2, y2, styled);
            }
        }
    }
}

fn draw_block_border(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let [bt, br, bb, bl] = style.border_widths;
    let [st, sr, sb, sl] = style.border_styles;
    if !(bt > 0.0 || br > 0.0 || bb > 0.0 || bl > 0.0) {
        return;
    }
    let bc = &style.border_color;

    let uniform_width = bt == br && br == bb && bb == bl;
    let uniform_style = st == sr && sr == sb && sb == sl;
    if style.has_radius() && uniform_width && uniform_style && st != BorderStyleValue::None {
        let inset = bt / 2.0;
        let inset_radii = style
            .border_radii
            .map(|[rx, ry]| [(rx - inset).max(0.0), (ry - inset).max(0.0)]);
        if let Some(path) = build_rounded_rect_path(
            x + inset,
            y + inset,
            w - inset * 2.0,
            h - inset * 2.0,
            &inset_radii,
        ) {
            let base = krilla::paint::Stroke {
                paint: krilla::color::rgb::Color::new(bc[0], bc[1], bc[2]).into(),
                width: bt,
                opacity: krilla::num::NormalizedF32::new(bc[3] as f32 / 255.0)
                    .unwrap_or(krilla::num::NormalizedF32::ONE),
                ..Default::default()
            };
            if let Some(styled) = apply_border_style(base, st, bt) {
                canvas.surface.set_fill(None);
                canvas.surface.set_stroke(Some(styled));
                canvas.surface.draw_path(&path);
                canvas.surface.set_stroke(None);
            }
        }
    } else {
        let opacity = krilla::num::NormalizedF32::new(bc[3] as f32 / 255.0)
            .unwrap_or(krilla::num::NormalizedF32::ONE);
        canvas.surface.set_fill(None);

        // top (top_or_left = true)
        draw_border_line(
            canvas,
            x,
            y + bt / 2.0,
            x + w,
            y + bt / 2.0,
            bt,
            st,
            bc,
            opacity,
            true,
        );
        // bottom (top_or_left = false)
        draw_border_line(
            canvas,
            x,
            y + h - bb / 2.0,
            x + w,
            y + h - bb / 2.0,
            bb,
            sb,
            bc,
            opacity,
            false,
        );
        // left (top_or_left = true)
        draw_border_line(
            canvas,
            x + bl / 2.0,
            y,
            x + bl / 2.0,
            y + h,
            bl,
            sl,
            bc,
            opacity,
            true,
        );
        // right (top_or_left = false)
        draw_border_line(
            canvas,
            x + w - br / 2.0,
            y,
            x + w - br / 2.0,
            y + h,
            br,
            sr,
            bc,
            opacity,
            false,
        );

        canvas.surface.set_stroke(None);
    }
}

impl Pageable for BlockPageable {
    fn wrap(&mut self, avail_width: Pt, _avail_height: Pt) -> Size {
        // Ensure children have been wrapped (split() creates unwrapped children)
        for pc in &mut self.children {
            if pc.child.height() == 0.0 {
                pc.child.wrap(avail_width, 10000.0);
            }
        }
        // Use max of children's (y + height) for total height
        let total_height = self.children.iter_mut().fold(0.0f32, |max_h, pc| {
            let child_h = pc.child.height();
            max_h.max(pc.y + child_h)
        });
        let size = Size {
            width: avail_width,
            height: total_height,
        };
        self.cached_size = Some(size);
        size
    }

    fn split(
        &self,
        _avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        if self.pagination.break_inside == BreakInside::Avoid {
            return None;
        }

        let has_forced_break = self.children.iter().enumerate().any(|(i, pc)| {
            (pc.child.pagination().break_before == BreakBefore::Page && i > 0)
                || (pc.child.pagination().break_after == BreakAfter::Page
                    && i < self.children.len() - 1)
        });

        let total_height = self.cached_size.map(|s| s.height).unwrap_or(0.0);
        if total_height <= avail_height && !has_forced_break {
            return None;
        }

        let mut split_index = self.children.len();

        let mut split_result: Option<(usize, SplitPair)> = None;

        for (i, pc) in self.children.iter().enumerate() {
            if pc.child.pagination().break_before == BreakBefore::Page && i > 0 && pc.y > 0.0 {
                split_index = i;
                break;
            }

            if pc.y + pc.child.height() > avail_height {
                let child_avail = avail_height - pc.y;
                if let Some(parts) = (child_avail > 0.0)
                    .then(|| pc.child.split(0.0, child_avail))
                    .flatten()
                {
                    split_result = Some((i, parts));
                } else if i == 0 && self.children.len() == 1 {
                    return None;
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

        if let Some((idx, (first_part, second_part))) = split_result {
            let pc = &self.children[idx];
            let mut first = clone_children(&self.children[..idx], 0.0);
            first.push(PositionedChild {
                child: first_part,
                x: pc.x,
                y: pc.y,
            });

            let mut second = vec![PositionedChild {
                child: second_part,
                x: pc.x,
                y: 0.0,
            }];
            second.extend(clone_children(&self.children[idx + 1..], pc.y));

            return Some((
                Box::new(
                    BlockPageable::with_positioned_children(first)
                        .with_pagination(self.pagination)
                        .with_style(self.style.clone()),
                ),
                Box::new(
                    BlockPageable::with_positioned_children(second)
                        .with_pagination(self.pagination)
                        .with_style(self.style.clone()),
                ),
            ));
        }

        if split_index == 0 || split_index >= self.children.len() {
            return None;
        }

        let split_y = self.children[split_index].y;

        let first = clone_children(&self.children[..split_index], 0.0);
        let second = clone_children(&self.children[split_index..], split_y);

        Some((
            Box::new(
                BlockPageable::with_positioned_children(first)
                    .with_pagination(self.pagination)
                    .with_style(self.style.clone()),
            ),
            Box::new(
                BlockPageable::with_positioned_children(second)
                    .with_pagination(self.pagination)
                    .with_style(self.style.clone()),
            ),
        ))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        // Prefer layout_size (Taffy-computed, stable) over cached_size (may be children-only)
        let total_width = self
            .layout_size
            .or(self.cached_size)
            .map(|s| s.width)
            .unwrap_or(avail_width);
        let total_height = self
            .layout_size
            .or(self.cached_size)
            .map(|s| s.height)
            .unwrap_or(avail_height);

        draw_block_background(canvas, &self.style, x, y, total_width, total_height);
        draw_block_border(canvas, &self.style, x, y, total_width, total_height);

        for pc in &self.children {
            pc.child
                .draw(canvas, x + pc.x, y + pc.y, avail_width, pc.child.height());
        }
    }

    fn pagination(&self) -> Pagination {
        self.pagination
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.layout_size
            .or(self.cached_size)
            .map(|s| s.height)
            .unwrap_or(0.0)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
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
        Size {
            width: avail_width,
            height: self.height,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

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

// ─── TablePageable ─────────────────────────────────────

/// A table with repeating header on page breaks.
#[derive(Clone)]
pub struct TablePageable {
    /// Cells belonging to thead (repeated on each page)
    pub header_cells: Vec<PositionedChild>,
    /// Cells belonging to tbody (split across pages)
    pub body_cells: Vec<PositionedChild>,
    /// Height of the header row(s)
    pub header_height: Pt,
    /// Visual style (background, borders, border-radii)
    pub style: BlockStyle,
    /// Taffy-computed layout size
    pub layout_size: Option<Size>,
    /// Table width (preserved across splits)
    pub width: Pt,
    /// Cached height from wrap()
    pub cached_height: Pt,
}

impl Pageable for TablePageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        if let Some(ls) = self.layout_size {
            self.cached_height = ls.height;
            return ls;
        }
        let max_h = self
            .header_cells
            .iter()
            .chain(self.body_cells.iter())
            .fold(0.0f32, |acc, pc| acc.max(pc.y + pc.child.height()));
        self.cached_height = max_h;
        Size {
            width: self.width,
            height: max_h,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        // Find the first body cell that overflows the available height
        let overflow_index = self
            .body_cells
            .iter()
            .position(|pc| pc.y + pc.child.height() > avail_height);

        let overflow_index = match overflow_index {
            Some(0) | None => return None,
            Some(i) => i,
        };

        // Snap to the start of the row containing the overflow cell.
        // Cells in the same row share the same y coordinate.
        let overflow_y = self.body_cells[overflow_index].y;
        let split_index = self.body_cells[..overflow_index]
            .iter()
            .rposition(|pc| pc.y < overflow_y)
            .map(|i| i + 1)
            .unwrap_or(0);

        if split_index == 0 {
            return None;
        }

        let split_y = self.body_cells[split_index].y;

        let first_header = clone_children(&self.header_cells, 0.0);
        let first_body = clone_children(&self.body_cells[..split_index], 0.0);

        let second_header = clone_children(&self.header_cells, 0.0);
        let second_body: Vec<PositionedChild> = self.body_cells[split_index..]
            .iter()
            .map(|pc| PositionedChild {
                child: pc.child.clone_box(),
                x: pc.x,
                y: self.header_height + (pc.y - split_y),
            })
            .collect();

        Some((
            Box::new(TablePageable {
                header_cells: first_header,
                body_cells: first_body,
                header_height: self.header_height,
                style: self.style.clone(),
                layout_size: None,
                width: self.width,
                cached_height: 0.0,
            }),
            Box::new(TablePageable {
                header_cells: second_header,
                body_cells: second_body,
                header_height: self.header_height,
                style: self.style.clone(),
                layout_size: None,
                width: self.width,
                cached_height: 0.0,
            }),
        ))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, _avail_width: Pt, _avail_height: Pt) {
        let total_width = self.width;
        let total_height = self
            .layout_size
            .map(|s| s.height)
            .unwrap_or(self.cached_height);

        draw_block_background(canvas, &self.style, x, y, total_width, total_height);
        draw_block_border(canvas, &self.style, x, y, total_width, total_height);

        for pc in self.header_cells.iter().chain(self.body_cells.iter()) {
            pc.child
                .draw(canvas, x + pc.x, y + pc.y, total_width, pc.child.height());
        }
    }

    fn pagination(&self) -> Pagination {
        Pagination::default()
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.layout_size
            .map(|s| s.height)
            .unwrap_or(self.cached_height)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
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
        let mut block = BlockPageable::new(vec![make_spacer(100.0), make_spacer(100.0)]);
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
        let breaking = BlockPageable::new(vec![make_spacer(50.0)]).with_pagination(Pagination {
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
        let block = BlockPageable::new(vec![make_spacer(200.0)]).with_pagination(Pagination {
            break_inside: BreakInside::Avoid,
            ..Pagination::default()
        });
        let mut block = block;
        block.wrap(200.0, 1000.0);
        // Even if it doesn't fit, split returns None
        assert!(block.split(200.0, 100.0).is_none());
    }

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
        let mut body = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
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
}
