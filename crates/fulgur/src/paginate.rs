use std::collections::{BTreeMap, BTreeSet};

use crate::gcpm::CounterOp;
use crate::gcpm::counter::CounterState;
use crate::pageable::{
    BlockPageable, BookmarkMarkerWrapperPageable, CounterOpMarkerPageable,
    CounterOpWrapperPageable, ListItemPageable, Pageable, Pt, RunningElementMarkerPageable,
    RunningElementWrapperPageable, StringSetPageable, StringSetWrapperPageable, TablePageable,
};

/// Per-page state for a named string.
#[derive(Debug, Clone, Default)]
pub struct StringSetPageState {
    /// Value at start of page (carried from previous page's `last`).
    pub start: Option<String>,
    /// First value set on this page.
    pub first: Option<String>,
    /// Last value set on this page.
    pub last: Option<String>,
}

/// Split a Pageable tree into per-page fragments.
pub fn paginate(
    mut root: Box<dyn Pageable>,
    page_width: Pt,
    page_height: Pt,
) -> Vec<Box<dyn Pageable>> {
    root.wrap(page_width, page_height);
    // Propagate the true page budget so `break-inside: avoid` can detect
    // "content taller than any possible page" and fall back to a normal
    // split. wrap() can't carry this itself because BlockPageable::wrap
    // probes children with avail_height=10000.0.
    root.propagate_page_height(page_height);

    let mut pages = vec![];
    let mut remaining = root;

    loop {
        match remaining.split_boxed(page_width, page_height) {
            Ok((this_page, rest)) => {
                pages.push(this_page);
                remaining = rest;
                // Re-wrap the remaining content
                remaining.wrap(page_width, page_height);
                remaining.propagate_page_height(page_height);
            }
            Err(unsplit) => {
                pages.push(unsplit);
                break;
            }
        }
    }

    pages
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
        // Skip header_cells: they are cloned into every page fragment by
        // TablePageable::split(), so walking them would replay counter ops
        // on every page the table spans.  Only body_cells carry unique ops.
        for child in &table.body_cells {
            collect_counter_markers(child.child.as_ref(), ops);
        }
    } else if let Some(list_item) = any.downcast_ref::<ListItemPageable>() {
        collect_counter_markers(list_item.body.as_ref(), ops);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pageable::{
        BlockPageable, BookmarkMarkerPageable, BookmarkMarkerWrapperPageable, PositionedChild,
        SpacerPageable, StringSetPageable, StringSetWrapperPageable,
    };

    fn make_spacer(h: Pt) -> Box<dyn Pageable> {
        let mut s = SpacerPageable::new(h);
        s.wrap(100.0, 1000.0);
        Box::new(s)
    }

    #[test]
    fn test_paginate_single_page() {
        let block = BlockPageable::new(vec![make_spacer(100.0), make_spacer(100.0)]);
        let pages = paginate(Box::new(block), 200.0, 300.0);
        assert_eq!(pages.len(), 1);
    }

    #[test]
    fn test_paginate_two_pages() {
        let block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        let pages = paginate(Box::new(block), 200.0, 250.0);
        assert_eq!(pages.len(), 2);
    }

    #[test]
    fn test_paginate_three_pages() {
        let block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        // 500pt total, 200pt per page => 3 pages (200, 200, 100)
        let pages = paginate(Box::new(block), 200.0, 200.0);
        assert_eq!(pages.len(), 3);
    }

    // ─── String set collection tests ────────────────────────

    fn make_marker(name: &str, value: &str) -> Box<dyn Pageable> {
        Box::new(StringSetPageable::new(name.to_string(), value.to_string()))
    }

    fn pos(child: Box<dyn Pageable>) -> PositionedChild {
        PositionedChild {
            child,
            x: 0.0,
            y: 0.0,
        }
    }

    #[test]
    fn test_collect_string_sets_single_page() {
        let block = BlockPageable::with_positioned_children(vec![
            pos(make_marker("title", "Ch1")),
            pos(make_spacer(50.0)),
        ]);
        let pages = paginate(Box::new(block), 100.0, 200.0);
        let states = collect_string_set_states(&pages);
        assert_eq!(states.len(), 1);
        let page_state = &states[0]["title"];
        assert_eq!(page_state.start, None);
        assert_eq!(page_state.first, Some("Ch1".to_string()));
        assert_eq!(page_state.last, Some("Ch1".to_string()));
    }

    #[test]
    fn test_collect_string_sets_across_pages() {
        // Create content that spans 2+ pages (page height = 100)
        let block = BlockPageable::with_positioned_children(vec![
            pos(make_marker("title", "Ch1")),
            pos(make_spacer(150.0)),
            pos(make_marker("title", "Ch2")),
            pos(make_spacer(50.0)),
        ]);
        let pages = paginate(Box::new(block), 100.0, 100.0);
        let states = collect_string_set_states(&pages);
        assert!(states.len() >= 2);
        // Page 2 should have start = "Ch1"
        let p2 = &states[1]["title"];
        assert_eq!(p2.start, Some("Ch1".to_string()));
    }

    #[test]
    fn test_collect_string_sets_inside_bookmark_wrapper() {
        // Regression: `BookmarkMarkerWrapperPageable` is the outermost wrapper
        // on every h1-h6 (emitted by convert_node). If the walkers miss this
        // type, all string-set / counter / running markers inside the heading
        // are silently dropped. See PR #84 discussion.
        let heading_body = BlockPageable::with_positioned_children(vec![pos(make_spacer(10.0))]);
        let with_string_set = StringSetWrapperPageable::new(
            vec![StringSetPageable::new("title".into(), "Ch1".into())],
            Box::new(heading_body),
        );
        let with_bookmark_marker = BookmarkMarkerWrapperPageable::new(
            BookmarkMarkerPageable::new(1, "Ch1".into()),
            Box::new(with_string_set),
        );
        let root =
            BlockPageable::with_positioned_children(vec![pos(Box::new(with_bookmark_marker))]);

        let pages = paginate(Box::new(root), 100.0, 200.0);
        let states = collect_string_set_states(&pages);
        assert_eq!(states.len(), 1);
        assert_eq!(
            states[0].get("title").and_then(|s| s.first.clone()),
            Some("Ch1".to_string()),
            "string-set marker under BookmarkMarkerWrapperPageable must be visible to walker"
        );
    }

    #[test]
    fn test_collect_string_sets_no_markers() {
        let block = BlockPageable::with_positioned_children(vec![pos(make_spacer(50.0))]);
        let pages = paginate(Box::new(block), 100.0, 200.0);
        let states = collect_string_set_states(&pages);
        assert_eq!(states.len(), 1);
        assert!(states[0].is_empty());
    }

    #[test]
    fn test_collect_string_sets_multiple_names() {
        let block = BlockPageable::with_positioned_children(vec![
            pos(make_marker("chapter", "Ch1")),
            pos(make_marker("section", "Sec1")),
            pos(make_spacer(50.0)),
        ]);
        let pages = paginate(Box::new(block), 100.0, 200.0);
        let states = collect_string_set_states(&pages);
        assert_eq!(states[0].len(), 2);
        assert_eq!(states[0]["chapter"].first, Some("Ch1".to_string()));
        assert_eq!(states[0]["section"].first, Some("Sec1".to_string()));
    }

    /// Regression: when an unsplittable child with a string-set marker is
    /// pushed to the next page (because it cannot fit on the current one),
    /// the marker must travel with it and NOT orphan on the previous page.
    #[test]
    fn test_string_set_wrapper_keeps_markers_with_unsplittable_child() {
        use crate::pageable::StringSetWrapperPageable;

        // Page height is 100pt.
        // Page 1: 80pt filler + (wrapped 60pt spacer can't fit) -> wrapper moves to page 2.
        let mut filler = SpacerPageable::new(80.0);
        filler.wrap(100.0, 1000.0);

        let mut content = SpacerPageable::new(60.0);
        content.wrap(100.0, 1000.0);

        let markers = vec![StringSetPageable::new("title".into(), "Ch2".into())];
        let wrapper = StringSetWrapperPageable::new(markers, Box::new(content));

        let mut block = BlockPageable::with_positioned_children(vec![
            pos(Box::new(filler)),
            PositionedChild {
                child: Box::new(wrapper),
                x: 0.0,
                y: 80.0,
            },
        ]);
        block.wrap(100.0, 1000.0);

        let pages = paginate(Box::new(block), 100.0, 100.0);
        let states = collect_string_set_states(&pages);

        assert_eq!(pages.len(), 2, "content should span two pages");
        assert!(
            !states[0].contains_key("title"),
            "marker must NOT be on page 1 (content was pushed to page 2)"
        );
        assert_eq!(
            states[1].get("title").and_then(|s| s.first.clone()),
            Some("Ch2".to_string()),
            "marker must be on page 2 with its content"
        );
    }

    // ─── Running element collection tests ────────────────────

    #[test]
    fn test_collect_running_element_states_single_page() {
        use crate::pageable::RunningElementMarkerPageable;

        let marker_a: Box<dyn Pageable> =
            Box::new(RunningElementMarkerPageable::new("hdr".into(), 0));
        let marker_b: Box<dyn Pageable> =
            Box::new(RunningElementMarkerPageable::new("hdr".into(), 1));
        let block = BlockPageable::with_positioned_children(vec![
            pos(marker_a),
            pos(make_spacer(50.0)),
            pos(marker_b),
            pos(make_spacer(50.0)),
        ]);
        let pages = paginate(Box::new(block), 200.0, 500.0);
        assert_eq!(pages.len(), 1);
        let states = collect_running_element_states(&pages);
        assert_eq!(states[0].get("hdr").unwrap().instance_ids, vec![0, 1]);
    }

    #[test]
    fn test_collect_running_element_states_splits_across_pages() {
        use crate::pageable::RunningElementMarkerPageable;

        // Need total_height > page_height for paginate() to split.
        // Use positioned y coordinates so children stack vertically rather
        // than overlapping at y=0 (which defeats wrap()'s max(y+h) total).
        let marker_a: Box<dyn Pageable> =
            Box::new(RunningElementMarkerPageable::new("hdr".into(), 0));
        let marker_b: Box<dyn Pageable> =
            Box::new(RunningElementMarkerPageable::new("hdr".into(), 1));
        let block = BlockPageable::with_positioned_children(vec![
            PositionedChild {
                child: marker_a,
                x: 0.0,
                y: 0.0,
            },
            PositionedChild {
                child: make_spacer(80.0),
                x: 0.0,
                y: 0.0,
            },
            PositionedChild {
                child: marker_b,
                x: 0.0,
                y: 120.0,
            },
            PositionedChild {
                child: make_spacer(80.0),
                x: 0.0,
                y: 120.0,
            },
        ]);
        let pages = paginate(Box::new(block), 200.0, 100.0);
        assert!(pages.len() >= 2);
        let states = collect_running_element_states(&pages);
        // marker_a sits before the first spacer on page 1.
        assert_eq!(states[0].get("hdr").unwrap().instance_ids, vec![0]);
        // marker_b sits with the second spacer, which overflows to page 2.
        assert_eq!(states[1].get("hdr").unwrap().instance_ids, vec![1]);
    }

    #[test]
    fn test_collect_running_element_states_empty() {
        // No markers — result is Vec of empty BTreeMaps.
        let block = BlockPageable::with_positioned_children(vec![pos(make_spacer(100.0))]);
        let pages = paginate(Box::new(block), 200.0, 500.0);
        let states = collect_running_element_states(&pages);
        assert_eq!(states.len(), 1);
        assert!(states[0].is_empty());
    }

    #[test]
    fn test_collect_running_element_states_deduplicates_instance_ids() {
        // Simulates a marker that appears on multiple pages — e.g. inside a
        // repeated table header. The same instance_id must only count once,
        // adopted on the first page it appears.
        use crate::pageable::RunningElementMarkerPageable;

        let marker = || -> Box<dyn Pageable> {
            Box::new(RunningElementMarkerPageable::new("hdr".into(), 7))
        };

        let page1 = BlockPageable::with_positioned_children(vec![pos(marker())]);
        let page2 = BlockPageable::with_positioned_children(vec![pos(marker())]);
        let pages: Vec<Box<dyn Pageable>> = vec![Box::new(page1), Box::new(page2)];

        let states = collect_running_element_states(&pages);
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].get("hdr").unwrap().instance_ids, vec![7]);
        assert!(
            !states[1].contains_key("hdr"),
            "instance_id 7 must not be re-counted on the second page"
        );
    }

    #[test]
    fn test_collect_running_markers_descends_into_wrapper() {
        // Markers attached via RunningElementWrapperPageable must still be
        // discovered by collect_running_element_states.
        use crate::pageable::{RunningElementMarkerPageable, RunningElementWrapperPageable};

        let marker = RunningElementMarkerPageable::new("hdr".into(), 3);
        let child: Box<dyn Pageable> = make_spacer(50.0);
        let wrapper = RunningElementWrapperPageable::new(vec![marker], child);
        let block = BlockPageable::with_positioned_children(vec![pos(Box::new(wrapper))]);
        let pages = paginate(Box::new(block), 200.0, 500.0);

        let states = collect_running_element_states(&pages);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].get("hdr").unwrap().instance_ids, vec![3]);
    }

    // ─── Counter state tests ──────────────────────────────

    use crate::gcpm::CounterOp;
    use crate::pageable::CounterOpMarkerPageable;

    #[test]
    fn test_collect_counter_states_single_page() {
        let marker = CounterOpMarkerPageable::new(vec![
            CounterOp::Reset {
                name: "chapter".into(),
                value: 0,
            },
            CounterOp::Increment {
                name: "chapter".into(),
                value: 1,
            },
        ]);
        let block = BlockPageable::with_positioned_children(vec![
            pos(Box::new(marker)),
            pos(make_spacer(50.0)),
        ]);
        let pages = paginate(Box::new(block), 100.0, 200.0);
        let states = collect_counter_states(&pages);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].get("chapter"), Some(&1));
    }

    #[test]
    fn test_collect_counter_states_across_pages() {
        let m1 = CounterOpMarkerPageable::new(vec![
            CounterOp::Reset {
                name: "chapter".into(),
                value: 0,
            },
            CounterOp::Increment {
                name: "chapter".into(),
                value: 1,
            },
        ]);
        let m2 = CounterOpMarkerPageable::new(vec![CounterOp::Increment {
            name: "chapter".into(),
            value: 1,
        }]);
        let block = BlockPageable::with_positioned_children(vec![
            pos(Box::new(m1)),
            PositionedChild {
                child: make_spacer(80.0),
                x: 0.0,
                y: 0.0,
            },
            PositionedChild {
                child: Box::new(m2),
                x: 0.0,
                y: 120.0,
            },
            PositionedChild {
                child: make_spacer(80.0),
                x: 0.0,
                y: 120.0,
            },
        ]);
        let pages = paginate(Box::new(block), 200.0, 100.0);
        let states = collect_counter_states(&pages);
        assert!(states.len() >= 2);
        assert_eq!(states[0].get("chapter"), Some(&1));
        assert_eq!(states[1].get("chapter"), Some(&2));
    }

    #[test]
    fn test_collect_counter_states_empty() {
        let block = BlockPageable::with_positioned_children(vec![pos(make_spacer(50.0))]);
        let pages = paginate(Box::new(block), 100.0, 200.0);
        let states = collect_counter_states(&pages);
        assert_eq!(states.len(), 1);
        assert!(states[0].is_empty());
    }
}
