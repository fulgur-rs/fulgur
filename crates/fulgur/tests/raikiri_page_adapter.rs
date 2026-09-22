#![cfg(feature = "raikiri-page-fragments")]

use fulgur::pagination_layout::PaginationGeometryTable;
use fulgur::raikiri_adapter::{AdapterError, adapt_page_fragment_geometry, adapt_page_fragments};
use raikiri_traits::{
    NodeId, PageFragment, PageFragmentGeometry, PageFragmentGeometryTable, PageFragmentItem,
    PageFragmentKind, PageFragmentRect,
};
use std::collections::BTreeMap;

/// Construct one neutral placement for the adapter fixtures.
fn placement(
    page_index: u32,
    node_id: u64,
    fragment_index: u32,
    fragment_count: u32,
    is_repeat: bool,
) -> PageFragmentItem {
    PageFragmentItem::new(
        NodeId::new(node_id),
        PageFragmentRect::new(4.0, 8.0 + page_index as f32, 20.0, 10.0),
        PageFragmentKind::Box,
        fragment_index,
        fragment_count,
        is_repeat,
    )
    .with_page_index(page_index)
}

/// Build two page snapshots representing a split item, a repeated item,
/// and a forced page boundary between their placements.
fn pages_with_basic_and_forced_break_records() -> Vec<PageFragment> {
    let mut first = PageFragment::new();
    first.page_index = 0;
    first.items = vec![placement(0, 7, 0, 2, false), placement(0, 9, 0, 2, true)];
    let mut second = PageFragment::new();
    second.page_index = 1;
    second.items = vec![placement(1, 7, 1, 2, false), placement(1, 9, 1, 2, true)];
    vec![first, second]
}

/// Verify basic and forced-break page records retain split/repeat semantics.
#[test]
fn neutral_pages_map_basic_forced_break_split_and_repeat_records() {
    let geometry = adapt_page_fragments(&pages_with_basic_and_forced_break_records())
        .expect("neutral page snapshots should adapt");

    assert_geometry_contract(&geometry);
    assert_eq!(geometry[&7].fragments[0].page_index, 0);
    assert_eq!(geometry[&7].fragments[1].page_index, 1);
    assert_eq!(geometry[&9].fragments.len(), 2);
    assert!(geometry[&9].is_repeat);
    assert!(!geometry[&9].is_split());
}

/// Verify node-centric snapshots retain deterministic node order and repeat flags.
#[test]
fn neutral_geometry_table_adapter_preserves_node_order_and_repeat_flag() {
    let pages = pages_with_basic_and_forced_break_records();
    let mut source = PageFragmentGeometryTable::new();
    for page in &pages {
        for item in &page.items {
            let entry = source
                .entry(item.node_id)
                .or_insert_with(|| PageFragmentGeometry::new(item.node_id, item.is_repeat));
            entry.fragments.push(item.clone());
        }
    }

    let geometry = adapt_page_fragment_geometry(&source).expect("geometry table should adapt");
    assert_geometry_contract(&geometry);
    let keys: Vec<_> = geometry.keys().copied().collect();
    assert_eq!(keys, vec![7, 9]);
}

/// Verify invalid mixed repeat metadata is rejected by the public adapter.
#[test]
fn neutral_geometry_table_rejects_mixed_repeat_semantics() {
    let node_id = NodeId::new(13);
    let mut node = PageFragmentGeometry::new(node_id, false);
    node.fragments.push(placement(0, 13, 0, 2, true));
    let mut source = BTreeMap::new();
    source.insert(node_id, node);

    let error = adapt_page_fragment_geometry(&source).expect_err("mixed semantics must fail");
    assert_eq!(
        error,
        AdapterError::ConflictingRepeatSemantics { node_id: 13 }
    );
}

/// Assert the split/repeat mapping shared by the adapter integration tests.
fn assert_geometry_contract(geometry: &PaginationGeometryTable) {
    assert_eq!(geometry.len(), 2);
    assert!(geometry[&7].is_split());
    assert!(!geometry[&7].is_repeat);
    assert_eq!(geometry[&7].fragments.len(), 2);
    assert!(geometry[&9].is_repeat);
    assert!(!geometry[&9].is_split());
}
