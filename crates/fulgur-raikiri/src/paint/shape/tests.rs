use super::*;
use raikiri_html::computed::ComputedLengthPercentage;

fn radius(px: f32) -> ComputedBorderRadius {
    ComputedBorderRadius::all(raikiri_html::computed::ComputedLength(px))
}

#[test]
fn overlapping_radii_scale_by_the_smallest_side_ratio() {
    // 100 x 40 with 30px corners: the vertical sides need 60px but have
    // 40, so f = 40 / 60 and every radius becomes 20px.
    let shape = RoundedRect::border_box(PaintRect::new(0.0, 0.0, 100.0, 40.0), &radius(30.0));
    for corner in shape.radii {
        assert!((corner[0] - 20.0).abs() < 1e-4, "{corner:?}");
        assert!((corner[1] - 20.0).abs() < 1e-4, "{corner:?}");
    }
}

#[test]
fn percentage_radius_refers_to_each_axis_of_the_border_box() {
    let all = ComputedLengthPercentage::Percent(50.0);
    let shape = RoundedRect::border_box(
        PaintRect::new(0.0, 0.0, 80.0, 40.0),
        &ComputedBorderRadius::corners(all, all, all, all),
    );
    assert_eq!(shape.radii[0], [40.0, 20.0]);
}

#[test]
fn inner_radius_subtracts_the_adjacent_border_width() {
    let outer = RoundedRect::border_box(PaintRect::new(0.0, 0.0, 100.0, 100.0), &radius(10.0));
    let inner = outer.inset(Edges {
        top: 4.0,
        right: 12.0,
        bottom: 4.0,
        left: 2.0,
    });
    assert_eq!(
        (inner.x, inner.y, inner.width, inner.height),
        (2.0, 4.0, 86.0, 92.0)
    );
    assert_eq!(inner.radii[0], [8.0, 6.0]);
    // The right border (12px) exceeds the 10px radius: one inner radius
    // is zero, so the corner is square (§4.1), not [0, 6].
    assert_eq!(inner.radii[1], [0.0, 0.0]);
}

#[test]
fn insets_larger_than_the_box_collapse_it_in_proportion() {
    let outer = RoundedRect::rect(0.0, 0.0, 10.0, 6.0);
    // 15px of horizontal and 12px of vertical inset in a 10 x 6 box:
    // each side keeps its share, and the inner box is empty.
    let inner = outer.inset(Edges {
        top: 4.0,
        right: 10.0,
        bottom: 8.0,
        left: 5.0,
    });
    assert!((inner.x - 10.0 / 3.0).abs() < 1e-5, "{inner:?}");
    assert_eq!((inner.y, inner.width, inner.height), (2.0, 0.0, 0.0));
    assert!(inner.is_empty());
    assert!(inner.path().is_none());
    // The ring of an empty inner box is the whole outer box.
    assert!(ring(&outer, &inner).is_some());
}

#[test]
fn paired_axes_survive_asymmetric_insets_and_fragment_slices() {
    use ComputedLengthPercentage::Px;
    let radius = ComputedBorderRadius::elliptical([Px(30.0); 4], [Px(15.0); 4]);
    let outer = RoundedRect::border_box(PaintRect::new(0.0, 0.0, 100.0, 60.0), &radius);
    assert_eq!(outer.radii, [[30.0, 15.0]; 4]);
    let inner = outer.inset(Edges {
        top: 4.0,
        right: 12.0,
        bottom: 8.0,
        left: 2.0,
    });
    assert_eq!(
        inner.radii,
        [[28.0, 11.0], [18.0, 11.0], [18.0, 7.0], [28.0, 7.0]]
    );
    let first = inner.sliced(Slice {
        top: false,
        bottom: true,
    });
    assert_eq!(
        first.radii,
        [[28.0, 11.0], [18.0, 11.0], [0.0, 0.0], [0.0, 0.0]]
    );
    let last = outer
        .sliced(Slice {
            top: true,
            bottom: false,
        })
        .inset(Edges {
            top: 4.0,
            ..Edges::default()
        });
    assert_eq!(
        last.radii,
        [[0.0, 0.0], [0.0, 0.0], [30.0, 15.0], [30.0, 15.0]]
    );
}

#[test]
fn unequal_percentages_use_their_own_box_axes() {
    use ComputedLengthPercentage::Percent;
    let radius = ComputedBorderRadius::elliptical([Percent(50.0); 4], [Percent(25.0); 4]);
    let outer = RoundedRect::border_box(PaintRect::new(0.0, 0.0, 80.0, 40.0), &radius);
    assert_eq!(outer.radii, [[40.0, 10.0]; 4]);
}
