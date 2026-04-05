use std::collections::BTreeMap;

use crate::pageable::{
    BlockPageable, ListItemPageable, Pageable, Pt, StringSetPageable, StringSetWrapperPageable,
    TablePageable,
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

    let mut pages = vec![];
    let mut remaining = root;

    loop {
        match remaining.split_boxed(page_width, page_height) {
            Ok((this_page, rest)) => {
                pages.push(this_page);
                remaining = rest;
                // Re-wrap the remaining content
                remaining.wrap(page_width, page_height);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pageable::{BlockPageable, PositionedChild, SpacerPageable, StringSetPageable};

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
}
