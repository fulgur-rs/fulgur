use super::*;

#[test]
fn an_item_is_clipped_by_the_fragment_it_lies_in() {
    // Two columns of one box on a page.
    let columns = [
        PaintRect::new(20.0, 20.0, 120.0, 160.0),
        PaintRect::new(160.0, 20.0, 120.0, 160.0),
    ];
    assert_eq!(nearest(&columns, PaintRect::new(30.0, 40.0, 50.0, 20.0)), 0);
    assert_eq!(
        nearest(&columns, PaintRect::new(170.0, 40.0, 50.0, 20.0)),
        1
    );
    // Overflowing below the second column, it still belongs to it.
    assert_eq!(
        nearest(&columns, PaintRect::new(170.0, 190.0, 50.0, 20.0)),
        1
    );
}
