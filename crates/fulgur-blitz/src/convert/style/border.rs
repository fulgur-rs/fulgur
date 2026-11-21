//! border-color, border-radius, border-style extraction.
//!
//! border_radii basis is CSS px (Stylo length-percentage operates in CSS px),
//! converted to pt via `.as_px().in_pt()` before storage. See coordinate-system.md.

use super::{StyleContext, absolute_to_rgba};
use crate::blitz_adapter::{Node, NodeLayoutExt};
use crate::draw_primitives::{BlockStyle, BorderStyleValue, CollapsedBorder};
use crate::units::F32Units;
use blitz_dom::CopyStyleGetters;

pub(super) fn apply_to(style: &mut BlockStyle, ctx: &StyleContext<'_>) {
    // Border colors. A single color covers the common case; sides that
    // differ (a lone `border-bottom` keeps the initial `currentcolor` on the
    // others) are drawn one by one.
    let colors = [
        ctx.styles.slow_clone_border_top_color(),
        ctx.styles.slow_clone_border_right_color(),
        ctx.styles.slow_clone_border_bottom_color(),
        ctx.styles.slow_clone_border_left_color(),
    ]
    .map(|c| absolute_to_rgba(c.resolve_to_absolute(ctx.current_color)));
    style.border_color = colors[0];
    style.side_colors = colors.iter().any(|c| *c != colors[0]).then_some(colors);

    // Border radii. Stylo evaluates length-percentage values in CSS px
    // space, so we feed it the CSS-px border-box basis and convert the
    // returned radius to pt. border_radii is consumed downstream alongside
    // pt-space widths/heights (see `compute_padding_box_inner_radii`).
    let width = ctx.layout.size.width;
    let height = ctx.layout.size.height;

    let tl = ctx.styles.slow_clone_border_top_left_radius();
    let tr = ctx.styles.slow_clone_border_top_right_radius();
    let br = ctx.styles.slow_clone_border_bottom_right_radius();
    let bl = ctx.styles.slow_clone_border_bottom_left_radius();

    style.border_radii = [
        [
            resolve_radius(&tl.0.width, width),
            resolve_radius(&tl.0.height, height),
        ],
        [
            resolve_radius(&tr.0.width, width),
            resolve_radius(&tr.0.height, height),
        ],
        [
            resolve_radius(&br.0.width, width),
            resolve_radius(&br.0.height, height),
        ],
        [
            resolve_radius(&bl.0.width, width),
            resolve_radius(&bl.0.height, height),
        ],
    ];

    style.border_styles = [
        map_border_style(ctx.styles.slow_clone_border_top_style()),
        map_border_style(ctx.styles.slow_clone_border_right_style()),
        map_border_style(ctx.styles.slow_clone_border_bottom_style()),
        map_border_style(ctx.styles.slow_clone_border_left_style()),
    ];
}

/// The border of a table or table cell in the collapsing border model
/// (CSS 2.1 §17.6.2).
///
/// Blitz lays a `border-collapse: collapse` table out as a grid with uniform
/// gutters between the cells, as wide as the widest cell border, and a table
/// border around them at least that wide, and gives the cells no border of
/// their own. Here each cell draws its
/// own computed border and the table its own, centred on the gutter (the
/// edge gutter for the table), so cells with different borders, or none,
/// keep them. At the table's edge a table border at least as wide as the
/// cell's wins; between two cells both draw. The other conflict rules
/// (`hidden` suppressing, style precedence) are not applied, nor are borders
/// set on rows, row groups and columns.
pub(super) fn apply_collapsed(style: &mut BlockStyle, node: &Node, ctx: &StyleContext<'_>) {
    use ::style::computed_values::border_collapse::T as BorderCollapse;
    use ::style::values::specified::box_::DisplayInside;
    use blitz_dom::node::SpecialElementData;

    let is_cell = match ctx.styles.clone_display().inside() {
        DisplayInside::TableCell => true,
        DisplayInside::Table => false,
        _ => return,
    };
    // Blitz makes the cells layout children of the table itself.
    let table = if is_cell {
        match node.layout_parent.get() {
            Some(id) => node.with(id),
            None => return,
        }
    } else {
        node
    };
    let Some(table_styles) = table.primary_styles() else {
        return;
    };
    if table_styles.clone_border_collapse() != BorderCollapse::Collapse {
        return;
    }
    let Some(SpecialElementData::TableRoot(context)) =
        table.element_data().map(|e| &e.special_data)
    else {
        return;
    };
    let gap_x = context.style.gap.width.into_raw().value();
    let gap_y = context.style.gap.height.into_raw().value();
    // The gutters between cells are the grid's gaps; the gutters around them
    // are the table's layout border, which Blitz makes at least as wide as
    // the gaps (wider when the table's own border is).
    let border = &context.style.border;
    let edges = [
        border.top.into_raw().value().max(gap_y),
        border.right.into_raw().value().max(gap_x),
        border.bottom.into_raw().value().max(gap_y),
        border.left.into_raw().value().max(gap_x),
    ];
    let mut gutters = edges;

    let own = side_borders(ctx.styles, ctx.current_color);
    // A cell side on the table's edge gives way to a table border at least as
    // wide.
    let mut beaten = [false; 4];
    if is_cell {
        const EPS: f32 = 0.5;
        let cell = node.final_layout_or_zero();
        let outer = table.final_layout_or_zero().size;
        let on_edge = [
            cell.location.y <= edges[0] + EPS,
            cell.location.x + cell.size.width >= outer.width - edges[1] - EPS,
            cell.location.y + cell.size.height >= outer.height - edges[2] - EPS,
            cell.location.x <= edges[3] + EPS,
        ];
        let inner = [gap_y, gap_x, gap_y, gap_x];
        for i in 0..4 {
            if !on_edge[i] {
                gutters[i] = inner[i];
            }
        }
        let table_current = table_styles.clone_color();
        let table_sides = side_borders(&table_styles, &table_current);
        for i in 0..4 {
            beaten[i] = on_edge[i] && table_sides[i].0 > 0.0 && table_sides[i].0 >= own[i].0;
        }
    }

    let mut collapsed = CollapsedBorder::default();
    for (i, &(width, side_style, color)) in own.iter().enumerate() {
        let width = if beaten[i] { 0.0 } else { width };
        collapsed.widths[i] = width.as_px().in_pt();
        collapsed.styles[i] = side_style;
        collapsed.colors[i] = color;
        let outset = if is_cell {
            gutters[i] / 2.0 + width / 2.0
        } else {
            width / 2.0 - gutters[i] / 2.0
        };
        collapsed.outsets[i] = outset.as_px().in_pt();
    }
    style.collapsed_border = Some(collapsed);
}

/// Each side's used width in CSS px (0 for `none` / `hidden`), style and
/// color: top, right, bottom, left. In the collapsing model `inset` draws as
/// `ridge` and `outset` as `groove` (CSS 2.1 §17.6.2.1).
fn side_borders(
    styles: &::style::properties::ComputedValues,
    current_color: &::style::color::AbsoluteColor,
) -> [(f32, BorderStyleValue, [u8; 4]); 4] {
    let border = styles.get_border();
    let sides = [
        (
            border.border_top_width.0,
            border.border_top_style,
            &border.border_top_color,
        ),
        (
            border.border_right_width.0,
            border.border_right_style,
            &border.border_right_color,
        ),
        (
            border.border_bottom_width.0,
            border.border_bottom_style,
            &border.border_bottom_color,
        ),
        (
            border.border_left_width.0,
            border.border_left_style,
            &border.border_left_color,
        ),
    ];
    sides.map(|(width, side_style, color)| {
        let width = if side_style.none_or_hidden() {
            0.0
        } else {
            width.to_f32_px()
        };
        let side_style = match map_border_style(side_style) {
            BorderStyleValue::Inset => BorderStyleValue::Ridge,
            BorderStyleValue::Outset => BorderStyleValue::Groove,
            other => other,
        };
        let color = absolute_to_rgba(color.resolve_to_absolute(current_color));
        (width, side_style, color)
    })
}

/// Resolve one border-radius corner component to PDF pt. Stylo resolves the
/// length-percentage in CSS px (`Length::px()` -> f32 CSS px); `F32Units`
/// (`f32 -> Px`) then `.in_pt()` converts to `Pt`. Byte-neutral.
fn resolve_radius(
    r: &style::values::computed::length_percentage::NonNegativeLengthPercentage,
    basis: f32,
) -> crate::units::Pt {
    let radius_css_px: f32 =
        r.0.resolve(style::values::computed::Length::new(basis))
            .px();
    radius_css_px.as_px().in_pt()
}

fn map_border_style(bs: style::values::specified::BorderStyle) -> BorderStyleValue {
    use style::values::specified::BorderStyle as BS;
    match bs {
        BS::None | BS::Hidden => BorderStyleValue::None,
        BS::Dashed => BorderStyleValue::Dashed,
        BS::Dotted => BorderStyleValue::Dotted,
        BS::Double => BorderStyleValue::Double,
        BS::Groove => BorderStyleValue::Groove,
        BS::Ridge => BorderStyleValue::Ridge,
        BS::Inset => BorderStyleValue::Inset,
        BS::Outset => BorderStyleValue::Outset,
        BS::Solid => BorderStyleValue::Solid,
    }
}

#[cfg(test)]
mod tests {
    use super::{map_border_style, resolve_radius};
    use crate::draw_primitives::BorderStyleValue;
    use style::values::computed::{Length, LengthPercentage, Percentage};
    use style::values::generics::NonNegative;
    use style::values::specified::BorderStyle as BS;

    #[test]
    fn resolve_radius_absolute_via_in_pt() {
        // An absolute 8 CSS px radius converts to 8 * 0.75 = 6 pt, and the
        // basis is irrelevant for an absolute length (pass a nonzero basis to
        // prove it is ignored).
        let r = NonNegative(LengthPercentage::new_length(Length::new(8.0)));
        assert_eq!(resolve_radius(&r, 200.0).to_f32(), 6.0);
    }

    #[test]
    fn resolve_radius_percentage_of_basis_to_pt() {
        // 50% of a 100 CSS px basis is 50 CSS px, which converts to
        // 50 * 0.75 = 37.5 pt.
        let r = NonNegative(LengthPercentage::new_percent(Percentage(0.5)));
        assert_eq!(resolve_radius(&r, 100.0).to_f32(), 37.5);
    }

    #[test]
    fn none_and_hidden_collapse_to_none() {
        assert_eq!(map_border_style(BS::None), BorderStyleValue::None);
        assert_eq!(map_border_style(BS::Hidden), BorderStyleValue::None);
    }

    #[test]
    fn dashed_dotted_double() {
        assert_eq!(map_border_style(BS::Dashed), BorderStyleValue::Dashed);
        assert_eq!(map_border_style(BS::Dotted), BorderStyleValue::Dotted);
        assert_eq!(map_border_style(BS::Double), BorderStyleValue::Double);
    }

    #[test]
    fn groove_ridge_inset_outset() {
        assert_eq!(map_border_style(BS::Groove), BorderStyleValue::Groove);
        assert_eq!(map_border_style(BS::Ridge), BorderStyleValue::Ridge);
        assert_eq!(map_border_style(BS::Inset), BorderStyleValue::Inset);
        assert_eq!(map_border_style(BS::Outset), BorderStyleValue::Outset);
    }

    #[test]
    fn solid() {
        assert_eq!(map_border_style(BS::Solid), BorderStyleValue::Solid);
    }
}
