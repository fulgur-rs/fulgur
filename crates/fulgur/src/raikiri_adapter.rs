//! Adapter from Raikiri's neutral page snapshots to Fulgur pagination geometry.
//!
//! This module is an optional migration bridge. It imports only the neutral
//! `raikiri_traits` page vocabulary; Taffy, Parley, style, scene, drawable,
//! and PDF types do not cross this boundary.

use crate::pagination_layout::{Fragment, PaginationGeometry, PaginationGeometryTable};
use crate::units::F32Units;
use raikiri_traits::{PageFragment, PageFragmentGeometryTable, PageFragmentItem};
use std::collections::{BTreeMap, btree_map::Entry};
use thiserror::Error;

/// Failure while converting a neutral page snapshot into Fulgur geometry.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AdapterError {
    /// The neutral 64-bit node identifier cannot be represented by Fulgur's
    /// platform-sized pagination table key.
    #[error("neutral NodeId {node_id} does not fit Fulgur's usize node ID")]
    NodeIdOverflow {
        /// Identifier that could not be converted.
        node_id: u64,
    },
    /// One source node was marked as both split and repeated in the input.
    #[error("node {node_id} mixes split and repeated placements")]
    ConflictingRepeatSemantics {
        /// Fulgur node identifier with conflicting records.
        node_id: usize,
    },
}

/// Convert neutral page snapshots into Fulgur's deterministic geometry table.
///
/// Each neutral placement becomes one [`Fragment`]. Page metadata remains in
/// the neutral snapshots; this function maps only the node placements needed
/// by Fulgur's existing pagination/render inputs. `is_repeat` is preserved in
/// [`PaginationGeometry`], and its [`PaginationGeometry::is_split`] result
/// therefore retains split versus repeated semantics.
pub fn adapt_page_fragments(
    pages: &[PageFragment],
) -> Result<PaginationGeometryTable, AdapterError> {
    let mut pending = PendingGeometryTable::new();
    for page in pages {
        for item in &page.items {
            add_item(&mut pending, page.page_index, item)?;
        }
    }
    Ok(finish_geometry(pending))
}

/// Convert a neutral node-centric geometry table into Fulgur geometry.
///
/// This is the equivalent path for consumers that already grouped snapshots
/// with `raikiri_dom::page_fragment_geometry_table`.
pub fn adapt_page_fragment_geometry(
    source: &PageFragmentGeometryTable,
) -> Result<PaginationGeometryTable, AdapterError> {
    let mut pending = PendingGeometryTable::new();
    for (node_id, node_geometry) in source {
        let node_id = node_id_to_usize(node_id.0)?;
        for item in &node_geometry.fragments {
            if item.is_repeat != node_geometry.is_repeat {
                return Err(AdapterError::ConflictingRepeatSemantics { node_id });
            }
            add_pending_fragment(
                &mut pending,
                node_id,
                node_geometry.is_repeat,
                item.fragment_index,
                fragment_from_item(item.page_index, item),
            )?;
        }
        // Preserve an explicitly empty geometry record as well. Consumers can
        // use its repeat flag even when no visible placement was produced.
        if node_geometry.fragments.is_empty() {
            add_empty_geometry(&mut pending, node_id, node_geometry.is_repeat)?;
        }
    }
    Ok(finish_geometry(pending))
}

type PendingGeometryTable = BTreeMap<usize, PendingGeometry>;

struct PendingGeometry {
    is_repeat: bool,
    fragments: Vec<(u32, Fragment)>,
}

/// Add one page-snapshot item to the temporary table while retaining its
/// neutral fragment ordinal until final conversion.
fn add_item(
    pending: &mut PendingGeometryTable,
    page_index: u32,
    item: &PageFragmentItem,
) -> Result<(), AdapterError> {
    let node_id = node_id_to_usize(item.node_id.0)?;
    add_pending_fragment(
        pending,
        node_id,
        item.is_repeat,
        item.fragment_index,
        fragment_from_item(page_index, item),
    )
}

/// Add one converted placement and reject a node that mixes split/repeat
/// semantics across its records.
fn add_pending_fragment(
    pending: &mut PendingGeometryTable,
    node_id: usize,
    is_repeat: bool,
    fragment_index: u32,
    fragment: Fragment,
) -> Result<(), AdapterError> {
    let entry = match pending.entry(node_id) {
        Entry::Vacant(vacant) => vacant.insert(PendingGeometry {
            is_repeat,
            fragments: Vec::new(),
        }),
        Entry::Occupied(occupied) => {
            let entry = occupied.into_mut();
            if entry.is_repeat != is_repeat {
                return Err(AdapterError::ConflictingRepeatSemantics { node_id });
            }
            entry
        }
    };
    entry.fragments.push((fragment_index, fragment));
    Ok(())
}

/// Retain a node-centric geometry record that has no visible placements.
fn add_empty_geometry(
    pending: &mut PendingGeometryTable,
    node_id: usize,
    is_repeat: bool,
) -> Result<(), AdapterError> {
    match pending.entry(node_id) {
        Entry::Vacant(vacant) => {
            vacant.insert(PendingGeometry {
                is_repeat,
                fragments: Vec::new(),
            });
            Ok(())
        }
        Entry::Occupied(occupied) if occupied.get().is_repeat == is_repeat => Ok(()),
        Entry::Occupied(_) => Err(AdapterError::ConflictingRepeatSemantics { node_id }),
    }
}

/// Convert a neutral node identifier to the key type used by Fulgur's arena.
fn node_id_to_usize(node_id: u64) -> Result<usize, AdapterError> {
    usize::try_from(node_id).map_err(|_| AdapterError::NodeIdOverflow { node_id })
}

/// Convert one neutral CSS-pixel rectangle to Fulgur's typed pixel rectangle.
fn fragment_from_item(page_index: u32, item: &PageFragmentItem) -> Fragment {
    Fragment {
        page_index,
        x: item.rect.x.as_px(),
        y: item.rect.y.as_px(),
        width: item.rect.width.as_px(),
        height: item.rect.height.as_px(),
    }
}

/// Finalize temporary records in page/fragment order while dropping the
/// neutral-only ordinal that Fulgur's existing `Fragment` does not store.
fn finish_geometry(pending: PendingGeometryTable) -> PaginationGeometryTable {
    pending
        .into_iter()
        .map(|(node_id, mut pending)| {
            pending.fragments.sort_by(|left, right| {
                left.1
                    .page_index
                    .cmp(&right.1.page_index)
                    .then_with(|| left.0.cmp(&right.0))
                    .then_with(|| left.1.y.to_f32().total_cmp(&right.1.y.to_f32()))
                    .then_with(|| left.1.x.to_f32().total_cmp(&right.1.x.to_f32()))
                    .then_with(|| left.1.width.to_f32().total_cmp(&right.1.width.to_f32()))
                    .then_with(|| left.1.height.to_f32().total_cmp(&right.1.height.to_f32()))
            });
            (
                node_id,
                PaginationGeometry {
                    fragments: pending
                        .fragments
                        .into_iter()
                        .map(|(_, fragment)| fragment)
                        .collect(),
                    is_repeat: pending.is_repeat,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use raikiri_traits::{
        NodeId, PageFragment, PageFragmentGeometry, PageFragmentGeometryTable, PageFragmentKind,
        PageFragmentRect,
    };

    /// Construct a neutral item with a caller-controlled ordinal and geometry.
    fn item(node_id: u64, page_index: u32, fragment_index: u32, y: f32) -> PageFragmentItem {
        PageFragmentItem::new(
            NodeId::new(node_id),
            PageFragmentRect::new(0.0, y, 10.0, 10.0),
            PageFragmentKind::Box,
            fragment_index,
            2,
            false,
        )
        .with_page_index(page_index)
    }

    /// Verify that both public adapter paths retain source fragment ordering
    /// even when coordinates would produce a different order.
    #[test]
    fn fragment_index_order_is_preserved_within_page() {
        let mut page = PageFragment::new();
        page.page_index = 0;
        page.items = vec![item(7, 0, 1, 1.0), item(7, 0, 0, 20.0)];
        let from_pages = adapt_page_fragments(&[page]).expect("page snapshots should adapt");
        assert_eq!(from_pages[&7].fragments[0].y.to_f32(), 20.0);
        assert_eq!(from_pages[&7].fragments[1].y.to_f32(), 1.0);

        let node_id = NodeId::new(7);
        let mut node = PageFragmentGeometry::new(node_id, false);
        node.fragments = vec![item(7, 0, 1, 1.0), item(7, 0, 0, 20.0)];
        let mut source = PageFragmentGeometryTable::new();
        source.insert(node_id, node);
        let from_geometry =
            adapt_page_fragment_geometry(&source).expect("geometry table should adapt");
        assert_eq!(from_geometry[&7].fragments[0].y.to_f32(), 20.0);
        assert_eq!(from_geometry[&7].fragments[1].y.to_f32(), 1.0);
    }

    /// Verify that conflicting source metadata is rejected before conversion.
    #[test]
    fn mixed_repeat_semantics_are_rejected() {
        let node_id = NodeId::new(13);
        let mut node = PageFragmentGeometry::new(node_id, false);
        node.fragments.push(PageFragmentItem::new(
            node_id,
            PageFragmentRect::new(0.0, 0.0, 1.0, 1.0),
            PageFragmentKind::Box,
            0,
            2,
            true,
        ));
        let mut source = PageFragmentGeometryTable::new();
        source.insert(node_id, node);
        assert_eq!(
            adapt_page_fragment_geometry(&source).expect_err("mixed metadata must fail"),
            AdapterError::ConflictingRepeatSemantics { node_id: 13 }
        );
    }

    /// Verify the checked conversion reports overflow on 32-bit targets.
    #[cfg(target_pointer_width = "32")]
    #[test]
    fn node_id_overflow_is_reported() {
        let mut page = PageFragment::new();
        page.items.push(item(u64::from(u32::MAX) + 1, 0, 0, 0.0));
        assert_eq!(
            adapt_page_fragments(&[page]).expect_err("32-bit node ID must overflow"),
            AdapterError::NodeIdOverflow {
                node_id: u64::from(u32::MAX) + 1
            }
        );
    }
}
