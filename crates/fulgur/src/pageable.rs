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

/// Run a draw closure wrapped in opacity guards.
/// Skips drawing entirely if fully transparent (opacity == 0).
/// Wraps in a Krilla transparency group if partially transparent.
///
/// **Does NOT check visibility.** CSS `visibility: hidden` only hides
/// the element's own content (background, border, text) but children
/// with `visibility: visible` must still render. Container draw()
/// methods handle visibility themselves.
pub fn draw_with_opacity(
    canvas: &mut Canvas<'_, '_>,
    opacity: f32,
    f: impl FnOnce(&mut Canvas<'_, '_>),
) {
    if opacity == 0.0 {
        return;
    }
    let needs_opacity = opacity < 1.0;
    if needs_opacity {
        let nf =
            krilla::num::NormalizedF32::new(opacity).unwrap_or(krilla::num::NormalizedF32::ONE);
        canvas.surface.push_opacity(nf);
    }
    f(canvas);
    if needs_opacity {
        canvas.surface.pop();
    }
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

    /// Split consuming the boxed value (avoids cloning children).
    /// Returns `Ok((first, second))` on success, or `Err(self)` when no split is
    /// possible, giving the caller back ownership.
    fn split_boxed(self: Box<Self>, avail_width: Pt, avail_height: Pt) -> SplitResult {
        match self.split(avail_width, avail_height) {
            Some(pair) => Ok(pair),
            None => Err(self.clone_box()),
        }
    }

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

pub type SplitPair = (Box<dyn Pageable>, Box<dyn Pageable>);
pub type SplitResult = Result<SplitPair, Box<dyn Pageable>>;

/// Result of scanning a `BlockPageable`'s children to decide where to split.
enum SplitDecision {
    /// Element fits or cannot be split.
    NoSplit,
    /// Split between children; all children before `usize` go to the first
    /// fragment, the rest go to the second.
    AtIndex(usize),
    /// Split *within* the child at `usize`; the contained pair is the child's
    /// first and second fragments.
    WithinChild(usize, SplitPair),
}

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
    pub opacity: f32,
    pub visible: bool,
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
            opacity: 1.0,
            visible: true,
        }
    }

    pub fn with_positioned_children(children: Vec<PositionedChild>) -> Self {
        Self {
            children,
            pagination: Pagination::default(),
            cached_size: None,
            layout_size: None,
            style: BlockStyle::default(),
            opacity: 1.0,
            visible: true,
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

    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Determine where (if at all) this block should be split at the given
    /// available height.  Both `split()` and `split_boxed()` call this to
    /// avoid duplicating the scanning logic.
    fn find_split_point(&self, avail_height: Pt) -> SplitDecision {
        if self.pagination.break_inside == BreakInside::Avoid {
            return SplitDecision::NoSplit;
        }

        let has_forced_break = self.children.iter().enumerate().any(|(i, pc)| {
            (pc.child.pagination().break_before == BreakBefore::Page && i > 0)
                || (pc.child.pagination().break_after == BreakAfter::Page
                    && i < self.children.len() - 1)
        });

        let total_height = self.cached_size.map(|s| s.height).unwrap_or(0.0);
        if total_height <= avail_height && !has_forced_break {
            return SplitDecision::NoSplit;
        }

        for (i, pc) in self.children.iter().enumerate() {
            if pc.child.pagination().break_before == BreakBefore::Page && i > 0 && pc.y > 0.0 {
                return SplitDecision::AtIndex(i);
            }

            if pc.y + pc.child.height() > avail_height {
                let child_avail = avail_height - pc.y;
                // TODO: Use split_boxed for child sub-split once the child can be extracted from Vec before iteration
                if let Some(parts) = (child_avail > 0.0)
                    .then(|| pc.child.split(0.0, child_avail))
                    .flatten()
                {
                    return SplitDecision::WithinChild(i, parts);
                } else if i == 0 && self.children.len() == 1 {
                    return SplitDecision::NoSplit;
                } else {
                    return SplitDecision::AtIndex(i.max(1));
                }
            }

            if pc.child.pagination().break_after == BreakAfter::Page {
                return SplitDecision::AtIndex(i + 1);
            }
        }

        SplitDecision::NoSplit
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
/// `is_top_or_left` determines the light/dark color assignment for 3D styles.
/// `outward_sign` is +1.0 if the computed normal (-dy,dx) points outward, -1.0 if inward.
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
    outward_sign: f32,
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
            let inward_sign = -outward_sign;
            stroke_line(
                canvas,
                x1 + nx * outward_sign,
                y1 + ny * outward_sign,
                x2 + nx * outward_sign,
                y2 + ny * outward_sign,
                colored_stroke(&outer_color, half_w, opacity),
            );
            stroke_line(
                canvas,
                x1 + nx * inward_sign,
                y1 + ny * inward_sign,
                x2 + nx * inward_sign,
                y2 + ny * inward_sign,
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

        // top: normal=(0,+half) points down=inward, so outward_sign=-1
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
            -1.0,
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
            1.0, // bottom: normal=(0,+half) points down=outward
        );
        // left: normal=(-half,0) points left=outward, so outward_sign=+1
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
            1.0,
        );
        // right: normal=(-half,0) points left=inward, so outward_sign=-1
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
            -1.0, // right: outward_sign=-1
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
        match self.find_split_point(avail_height) {
            SplitDecision::NoSplit => None,

            SplitDecision::WithinChild(idx, (first_part, second_part)) => {
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

                Some((
                    Box::new(
                        BlockPageable::with_positioned_children(first)
                            .with_pagination(self.pagination)
                            .with_style(self.style.clone())
                            .with_opacity(self.opacity)
                            .with_visible(self.visible),
                    ),
                    Box::new(
                        BlockPageable::with_positioned_children(second)
                            .with_pagination(self.pagination)
                            .with_style(self.style.clone())
                            .with_opacity(self.opacity)
                            .with_visible(self.visible),
                    ),
                ))
            }

            SplitDecision::AtIndex(split_index) => {
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
                            .with_style(self.style.clone())
                            .with_opacity(self.opacity)
                            .with_visible(self.visible),
                    ),
                    Box::new(
                        BlockPageable::with_positioned_children(second)
                            .with_pagination(self.pagination)
                            .with_style(self.style.clone())
                            .with_opacity(self.opacity)
                            .with_visible(self.visible),
                    ),
                ))
            }
        }
    }

    fn split_boxed(self: Box<Self>, _avail_width: Pt, avail_height: Pt) -> SplitResult {
        match self.find_split_point(avail_height) {
            SplitDecision::NoSplit => Err(self),

            SplitDecision::WithinChild(idx, (first_part, second_part)) => {
                let mut me = *self;
                let split_child_x = me.children[idx].x;
                let split_child_y = me.children[idx].y;

                let mut tail = me.children.split_off(idx + 1);
                let _split_child = me.children.pop().unwrap(); // remove the split child
                let mut first = me.children; // children[..idx]

                first.push(PositionedChild {
                    child: first_part,
                    x: split_child_x,
                    y: split_child_y,
                });

                // Shift tail y coordinates
                for pc in &mut tail {
                    pc.y -= split_child_y;
                }

                let mut second = vec![PositionedChild {
                    child: second_part,
                    x: split_child_x,
                    y: 0.0,
                }];
                second.append(&mut tail);

                Ok((
                    Box::new(
                        BlockPageable::with_positioned_children(first)
                            .with_pagination(me.pagination)
                            .with_style(me.style.clone())
                            .with_opacity(me.opacity)
                            .with_visible(me.visible),
                    ),
                    Box::new(
                        BlockPageable::with_positioned_children(second)
                            .with_pagination(me.pagination)
                            .with_style(me.style)
                            .with_opacity(me.opacity)
                            .with_visible(me.visible),
                    ),
                ))
            }

            SplitDecision::AtIndex(split_index) => {
                let mut me = *self;

                if split_index == 0 || split_index >= me.children.len() {
                    return Err(Box::new(me));
                }

                let split_y = me.children[split_index].y;
                let mut second_children = me.children.split_off(split_index);
                let first_children = me.children;

                // Shift y coordinates on second fragment
                for pc in &mut second_children {
                    pc.y -= split_y;
                }

                Ok((
                    Box::new(
                        BlockPageable::with_positioned_children(first_children)
                            .with_pagination(me.pagination)
                            .with_style(me.style.clone())
                            .with_opacity(me.opacity)
                            .with_visible(me.visible),
                    ),
                    Box::new(
                        BlockPageable::with_positioned_children(second_children)
                            .with_pagination(me.pagination)
                            .with_style(me.style)
                            .with_opacity(me.opacity)
                            .with_visible(me.visible),
                    ),
                ))
            }
        }
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        draw_with_opacity(canvas, self.opacity, |canvas| {
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

            // visibility: hidden skips own rendering but children still draw
            if self.visible {
                draw_block_background(canvas, &self.style, x, y, total_width, total_height);
                draw_block_border(canvas, &self.style, x, y, total_width, total_height);
            }

            for pc in &self.children {
                pc.child
                    .draw(canvas, x + pc.x, y + pc.y, avail_width, pc.child.height());
            }
        });
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
    /// CSS opacity (0.0–1.0), applied to both marker and body
    pub opacity: f32,
    /// CSS visibility (false = hidden)
    pub visible: bool,
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
                opacity: self.opacity,
                visible: self.visible,
            }),
            Box::new(ListItemPageable {
                marker_lines: Vec::new(),
                marker_width: 0.0,
                body: bottom_body,
                style: self.style.clone(),
                width: self.width,
                height: 0.0,
                opacity: self.opacity,
                visible: self.visible,
            }),
        ))
    }

    fn split_boxed(self: Box<Self>, avail_width: Pt, avail_height: Pt) -> SplitResult {
        let me = *self;
        let (top_body, bottom_body) = match me.body.split_boxed(avail_width, avail_height) {
            Ok(pair) => pair,
            Err(body) => {
                return Err(Box::new(ListItemPageable { body, ..me }));
            }
        };
        Ok((
            Box::new(ListItemPageable {
                marker_lines: me.marker_lines,
                marker_width: me.marker_width,
                body: top_body,
                style: me.style.clone(),
                width: me.width,
                height: 0.0,
                opacity: me.opacity,
                visible: me.visible,
            }),
            Box::new(ListItemPageable {
                marker_lines: Vec::new(),
                marker_width: 0.0,
                body: bottom_body,
                style: me.style,
                width: me.width,
                height: 0.0,
                opacity: me.opacity,
                visible: me.visible,
            }),
        ))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        draw_with_opacity(canvas, self.opacity, |canvas| {
            // visibility: hidden skips marker but body still draws (children have own visibility)
            if self.visible && !self.marker_lines.is_empty() {
                let marker_x = x - self.marker_width;
                crate::paragraph::draw_shaped_lines(canvas, &self.marker_lines, marker_x, y);
            }
            self.body.draw(canvas, x, y, avail_width, avail_height);
        });
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
    pub opacity: f32,
    pub visible: bool,
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
                opacity: self.opacity,
                visible: self.visible,
            }),
            Box::new(TablePageable {
                header_cells: second_header,
                body_cells: second_body,
                header_height: self.header_height,
                style: self.style.clone(),
                layout_size: None,
                width: self.width,
                cached_height: 0.0,
                opacity: self.opacity,
                visible: self.visible,
            }),
        ))
    }

    fn split_boxed(self: Box<Self>, _avail_width: Pt, avail_height: Pt) -> SplitResult {
        // Find the first body cell that overflows the available height
        let overflow_index = self
            .body_cells
            .iter()
            .position(|pc| pc.y + pc.child.height() > avail_height);

        let overflow_index = match overflow_index {
            Some(0) | None => return Err(self),
            Some(i) => i,
        };

        // Snap to the start of the row containing the overflow cell.
        let overflow_y = self.body_cells[overflow_index].y;
        let split_index = self.body_cells[..overflow_index]
            .iter()
            .rposition(|pc| pc.y < overflow_y)
            .map(|i| i + 1)
            .unwrap_or(0);

        if split_index == 0 {
            return Err(self);
        }

        let split_y = self.body_cells[split_index].y;
        let mut me = *self;

        let mut second_body = me.body_cells.split_off(split_index);
        let first_body = me.body_cells;

        // Shift y coordinates on second fragment body cells
        let header_height = me.header_height;
        for pc in &mut second_body {
            pc.y = header_height + (pc.y - split_y);
        }

        // Headers: clone for first, move for second
        let first_header = clone_children(&me.header_cells, 0.0);

        Ok((
            Box::new(TablePageable {
                header_cells: first_header,
                body_cells: first_body,
                header_height,
                style: me.style.clone(),
                layout_size: None,
                width: me.width,
                cached_height: 0.0,
                opacity: me.opacity,
                visible: me.visible,
            }),
            Box::new(TablePageable {
                header_cells: me.header_cells,
                body_cells: second_body,
                header_height,
                style: me.style,
                layout_size: None,
                width: me.width,
                cached_height: 0.0,
                opacity: me.opacity,
                visible: me.visible,
            }),
        ))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, _avail_width: Pt, _avail_height: Pt) {
        draw_with_opacity(canvas, self.opacity, |canvas| {
            let total_width = self.width;
            let total_height = self
                .layout_size
                .map(|s| s.height)
                .unwrap_or(self.cached_height);

            if self.visible {
                draw_block_background(canvas, &self.style, x, y, total_width, total_height);
                draw_block_border(canvas, &self.style, x, y, total_width, total_height);
            }

            for pc in self.header_cells.iter().chain(self.body_cells.iter()) {
                pc.child
                    .draw(canvas, x + pc.x, y + pc.y, total_width, pc.child.height());
            }
        });
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
            opacity: 1.0,
            visible: true,
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
            opacity: 1.0,
            visible: true,
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

    #[test]
    fn test_table_split_boxed_repeats_headers_and_rebases_y() {
        // Header row at y=0, height=30
        let header = vec![PositionedChild {
            child: make_spacer(30.0),
            x: 0.0,
            y: 0.0,
        }];

        // Three body rows at y=30, y=80, y=130 (each 50pt tall)
        let body = vec![
            PositionedChild {
                child: make_spacer(50.0),
                x: 0.0,
                y: 30.0,
            },
            PositionedChild {
                child: make_spacer(50.0),
                x: 0.0,
                y: 80.0,
            },
            PositionedChild {
                child: make_spacer(50.0),
                x: 0.0,
                y: 130.0,
            },
        ];

        let mut table = TablePageable {
            header_cells: header,
            body_cells: body,
            header_height: 30.0,
            style: BlockStyle::default(),
            layout_size: None,
            width: 200.0,
            cached_height: 0.0,
            opacity: 1.0,
            visible: true,
        };
        table.wrap(200.0, 1000.0);

        // Available height = 120pt → header(30) + first body row(50) fits,
        // second body row at y=80 with height 50 overflows (80+50=130 > 120).
        let concrete: Box<TablePageable> = Box::new(table);

        let result = concrete.split_boxed(200.0, 120.0);
        assert!(result.is_ok(), "split_boxed should return Ok");

        let (first, second) = match result {
            Ok(pair) => pair,
            Err(_) => panic!("split_boxed returned Err"),
        };
        let first_table = first.as_any().downcast_ref::<TablePageable>().unwrap();
        let second_table = second.as_any().downcast_ref::<TablePageable>().unwrap();

        // Both fragments have headers
        assert_eq!(first_table.header_cells.len(), 1);
        assert_eq!(second_table.header_cells.len(), 1);

        // First fragment: 1 body row
        assert_eq!(first_table.body_cells.len(), 1);
        assert!((first_table.body_cells[0].y - 30.0).abs() < 0.01);

        // Second fragment: 2 body rows, y rebased to header_height
        assert_eq!(second_table.body_cells.len(), 2);
        // First body cell: header_height + (80 - 80) = 30
        assert!(
            (second_table.body_cells[0].y - 30.0).abs() < 0.01,
            "expected y=30.0, got {}",
            second_table.body_cells[0].y
        );
        // Second body cell: header_height + (130 - 80) = 80
        assert!(
            (second_table.body_cells[1].y - 80.0).abs() < 0.01,
            "expected y=80.0, got {}",
            second_table.body_cells[1].y
        );
    }
}
