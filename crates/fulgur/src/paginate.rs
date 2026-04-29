use std::collections::{BTreeMap, BTreeSet};

use crate::gcpm::CounterOp;
use crate::gcpm::counter::CounterState;
use crate::pageable::{
    BlockPageable, BookmarkMarkerWrapperPageable, CounterOpMarkerPageable,
    CounterOpWrapperPageable, ListItemPageable, Pageable, RunningElementMarkerPageable,
    RunningElementWrapperPageable, StringSetPageable, StringSetWrapperPageable, TablePageable,
};

/// Per-page state for a named string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StringSetPageState {
    /// Value at start of page (carried from previous page's `last`).
    pub start: Option<String>,
    /// First value set on this page.
    pub first: Option<String>,
    /// Last value set on this page.
    pub last: Option<String>,
}

/// Walk paginated pages and collect StringSetPageable markers per page.
pub fn collect_string_set_states(
    pages: &[Box<dyn Pageable>],
) -> Vec<BTreeMap<String, StringSetPageState>> {
    let mut result: Vec<BTreeMap<String, StringSetPageState>> = Vec::with_capacity(pages.len());
    let mut carry: BTreeMap<String, String> = BTreeMap::new();

    for page in pages {
        let mut page_state: BTreeMap<String, StringSetPageState> = BTreeMap::new();

        // Initialize start values from carry
        for (name, value) in &carry {
            page_state.entry(name.clone()).or_default().start = Some(value.clone());
        }

        // Collect markers from this page
        let mut markers = Vec::new();
        collect_markers(page.as_ref(), &mut markers);

        for (name, value) in &markers {
            let state = page_state.entry(name.clone()).or_default();
            if state.first.is_none() {
                state.first = Some(value.clone());
            }
            state.last = Some(value.clone());
            carry.insert(name.clone(), value.clone());
        }

        result.push(page_state);
    }

    result
}

/// Recursively find all string-set markers in a Pageable tree.
///
/// Markers are inserted via `StringSetWrapperPageable` in `convert.rs`. The
/// wrapper also keeps markers attached to the first fragment of its child on
/// split, so the markers always travel with the content they describe.
fn collect_markers(pageable: &dyn Pageable, markers: &mut Vec<(String, String)>) {
    let any = pageable.as_any();
    if let Some(wrapper) = any.downcast_ref::<StringSetWrapperPageable>() {
        for m in &wrapper.markers {
            markers.push((m.name.clone(), m.value.clone()));
        }
        collect_markers(wrapper.child.as_ref(), markers);
    } else if let Some(wrapper) = any.downcast_ref::<RunningElementWrapperPageable>() {
        collect_markers(wrapper.child.as_ref(), markers);
    } else if let Some(wrapper) = any.downcast_ref::<CounterOpWrapperPageable>() {
        collect_markers(wrapper.child.as_ref(), markers);
    } else if let Some(wrapper) = any.downcast_ref::<BookmarkMarkerWrapperPageable>() {
        collect_markers(wrapper.child.as_ref(), markers);
    } else if let Some(marker) = any.downcast_ref::<StringSetPageable>() {
        // Used by unit tests that construct markers directly.
        markers.push((marker.name.clone(), marker.value.clone()));
    } else if let Some(block) = any.downcast_ref::<BlockPageable>() {
        for child in &block.children {
            collect_markers(child.child.as_ref(), markers);
        }
    } else if let Some(table) = any.downcast_ref::<TablePageable>() {
        for child in &table.header_cells {
            collect_markers(child.child.as_ref(), markers);
        }
        for child in &table.body_cells {
            collect_markers(child.child.as_ref(), markers);
        }
    } else if let Some(list_item) = any.downcast_ref::<ListItemPageable>() {
        collect_markers(list_item.body.as_ref(), markers);
    }
}

/// Per-page state for running element instances of a given name.
#[derive(Debug, Clone, Default)]
pub struct PageRunningState {
    /// Instance IDs of running elements whose source position falls on this
    /// page, in source order.
    pub instance_ids: Vec<usize>,
}

/// Walk paginated pages and collect `RunningElementMarkerPageable` markers
/// per page, keyed by running element name.
///
/// Each `instance_id` is adopted only on the first page where it appears.
/// This is necessary because some containers (e.g. `TablePageable`
/// `header_cells`) are replicated on every page that shows the table; without
/// deduplication, a running element declared inside a `<thead>` would be
/// counted as a fresh assignment on every subsequent page and break
/// `first` / `last` / `first-except` semantics.
///
/// Used by the render stage together with `resolve_element_policy` to
/// determine which running element instance should be shown in each
/// margin box on each page.
pub fn collect_running_element_states(
    pages: &[Box<dyn Pageable>],
) -> Vec<BTreeMap<String, PageRunningState>> {
    let mut result: Vec<BTreeMap<String, PageRunningState>> = Vec::with_capacity(pages.len());
    let mut adopted: BTreeSet<usize> = BTreeSet::new();

    for page in pages {
        let mut page_state: BTreeMap<String, PageRunningState> = BTreeMap::new();
        let mut markers = Vec::new();
        collect_running_markers(page.as_ref(), &mut markers);
        for (name, instance_id) in markers {
            if !adopted.insert(instance_id) {
                continue; // already adopted on an earlier page
            }
            page_state
                .entry(name)
                .or_default()
                .instance_ids
                .push(instance_id);
        }
        result.push(page_state);
    }

    result
}

/// Recursively find all running element markers in a Pageable tree.
///
/// Mirrors `collect_markers` (for string-set) but looks for
/// `RunningElementMarkerPageable` instances. Descends into both
/// `StringSetWrapperPageable` and `RunningElementWrapperPageable` so markers
/// wrapped by either mechanism are still discovered.
fn collect_running_markers(pageable: &dyn Pageable, markers: &mut Vec<(String, usize)>) {
    let any = pageable.as_any();
    if let Some(m) = any.downcast_ref::<RunningElementMarkerPageable>() {
        markers.push((m.name.clone(), m.instance_id));
    } else if let Some(wrapper) = any.downcast_ref::<RunningElementWrapperPageable>() {
        for m in &wrapper.markers {
            markers.push((m.name.clone(), m.instance_id));
        }
        collect_running_markers(wrapper.child.as_ref(), markers);
    } else if let Some(wrapper) = any.downcast_ref::<StringSetWrapperPageable>() {
        collect_running_markers(wrapper.child.as_ref(), markers);
    } else if let Some(wrapper) = any.downcast_ref::<CounterOpWrapperPageable>() {
        collect_running_markers(wrapper.child.as_ref(), markers);
    } else if let Some(wrapper) = any.downcast_ref::<BookmarkMarkerWrapperPageable>() {
        collect_running_markers(wrapper.child.as_ref(), markers);
    } else if let Some(block) = any.downcast_ref::<BlockPageable>() {
        for child in &block.children {
            collect_running_markers(child.child.as_ref(), markers);
        }
    } else if let Some(table) = any.downcast_ref::<TablePageable>() {
        for child in &table.header_cells {
            collect_running_markers(child.child.as_ref(), markers);
        }
        for child in &table.body_cells {
            collect_running_markers(child.child.as_ref(), markers);
        }
    } else if let Some(list_item) = any.downcast_ref::<ListItemPageable>() {
        collect_running_markers(list_item.body.as_ref(), markers);
    }
}

/// Walk paginated pages, replay counter operations in document order,
/// and return the cumulative counter state at the end of each page.
pub fn collect_counter_states(pages: &[Box<dyn Pageable>]) -> Vec<BTreeMap<String, i32>> {
    let mut state = CounterState::new();
    let mut result = Vec::with_capacity(pages.len());

    for page in pages {
        let mut ops = Vec::new();
        collect_counter_markers(page.as_ref(), &mut ops);
        for op in &ops {
            match op {
                CounterOp::Reset { name, value } => state.reset(name, *value),
                CounterOp::Increment { name, value } => state.increment(name, *value),
                CounterOp::Set { name, value } => state.set(name, *value),
            }
        }
        result.push(state.snapshot());
    }

    result
}

/// Recursively find all counter-op markers in a Pageable tree.
///
/// Mirrors `collect_running_markers` but looks for `CounterOpMarkerPageable`
/// instances. Descends into wrappers so markers inside wrapped children are
/// still discovered.
fn collect_counter_markers(pageable: &dyn Pageable, ops: &mut Vec<CounterOp>) {
    let any = pageable.as_any();
    if let Some(wrapper) = any.downcast_ref::<CounterOpWrapperPageable>() {
        ops.extend(wrapper.ops.clone());
        collect_counter_markers(wrapper.child.as_ref(), ops);
    } else if let Some(marker) = any.downcast_ref::<CounterOpMarkerPageable>() {
        ops.extend(marker.ops.clone());
    } else if let Some(block) = any.downcast_ref::<BlockPageable>() {
        for child in &block.children {
            collect_counter_markers(child.child.as_ref(), ops);
        }
    } else if let Some(wrapper) = any.downcast_ref::<StringSetWrapperPageable>() {
        collect_counter_markers(wrapper.child.as_ref(), ops);
    } else if let Some(wrapper) = any.downcast_ref::<RunningElementWrapperPageable>() {
        collect_counter_markers(wrapper.child.as_ref(), ops);
    } else if let Some(wrapper) = any.downcast_ref::<BookmarkMarkerWrapperPageable>() {
        collect_counter_markers(wrapper.child.as_ref(), ops);
    } else if let Some(table) = any.downcast_ref::<TablePageable>() {
        // Skip header_cells: they are repeated on every page the table
        // spans, so walking them would replay counter ops per page. Only
        // body_cells carry unique ops.
        for child in &table.body_cells {
            collect_counter_markers(child.child.as_ref(), ops);
        }
    } else if let Some(list_item) = any.downcast_ref::<ListItemPageable>() {
        collect_counter_markers(list_item.body.as_ref(), ops);
    }
}
