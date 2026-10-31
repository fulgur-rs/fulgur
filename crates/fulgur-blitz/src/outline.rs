//! Build a krilla Outline (PDF bookmark tree) from flat BookmarkEntry records.

use krilla::destination::XyzDestination;
use krilla::geom::Point;
use krilla::outline::{Outline, OutlineNode};

use crate::draw_primitives::BookmarkEntry;

/// Intermediate, testable tree node. Converted to `OutlineNode` by
/// `to_krilla_node` before being attached to an `Outline`.
///
/// `level` is retained on the intermediate node so the nesting logic can be
/// verified by tests; once the krilla `OutlineNode` is produced, the level is
/// implicit in the tree shape, so this field is read only in `#[cfg(test)]`.
#[derive(Debug)]
pub(crate) struct TreeNode {
    pub label: String,
    #[allow(dead_code)]
    pub level: u8,
    pub page_idx: usize,
    pub y_pt: crate::units::Pt,
    pub children: Vec<TreeNode>,
}

/// Build the nested tree using a stack of currently-open ancestors.
///
/// Bookmarks are nested according to their level: a bookmark with level L becomes
/// a child of the most recent bookmark whose level < L. Orphan levels (e.g. a
/// level-3 bookmark with no preceding level-1/2) nest under the deepest currently-open
/// shallower ancestor; if the stack is empty they become top-level.
pub(crate) fn build_tree(entries: &[BookmarkEntry]) -> Vec<TreeNode> {
    let mut roots: Vec<TreeNode> = Vec::new();
    // Stack of (level, path-from-roots) for currently-open ancestors.
    let mut open: Vec<(u8, Vec<usize>)> = Vec::new();

    for e in entries {
        // Close any ancestors at or deeper than this level.
        while open.last().is_some_and(|(lvl, _)| *lvl >= e.level) {
            open.pop();
        }

        let new_node = TreeNode {
            label: e.label.clone(),
            level: e.level,
            page_idx: e.page_idx,
            y_pt: e.y_pt,
            children: vec![],
        };

        if let Some((_, path)) = open.last() {
            let parent_path = path.clone();
            let parent = walk_mut(&mut roots, &parent_path);
            parent.children.push(new_node);
            let mut new_path = parent_path.clone();
            new_path.push(parent.children.len() - 1);
            open.push((e.level, new_path));
        } else {
            roots.push(new_node);
            open.push((e.level, vec![roots.len() - 1]));
        }
    }

    roots
}

fn walk_mut<'a>(roots: &'a mut [TreeNode], path: &[usize]) -> &'a mut TreeNode {
    let (&first, rest) = path.split_first().expect("non-empty path");
    let mut node = &mut roots[first];
    for &i in rest {
        node = &mut node.children[i];
    }
    node
}

/// Build a krilla `Outline` from a flat, source-ordered list of bookmark
/// entries. Bookmarks are nested according to their level: a bookmark with
/// level L becomes a child of the most recent bookmark whose level < L.
///
/// Orphan levels (e.g. a level-3 bookmark with no preceding level-1/2) are
/// promoted to the outermost open level; if the stack is empty they become
/// top-level.
pub fn build_outline(entries: &[BookmarkEntry]) -> Outline {
    let tree = build_tree(entries);
    let mut outline = Outline::new();
    for node in tree {
        outline.push_child(to_krilla_node(node));
    }
    outline
}

fn to_krilla_node(node: TreeNode) -> OutlineNode {
    let dest = XyzDestination::new(node.page_idx, Point::from_xy(0.0, node.y_pt.to_f32()));
    let mut o = OutlineNode::new(node.label, dest);
    for child in node.children {
        o.push_child(to_krilla_node(child));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::F32Units;

    fn entry(page: usize, y: crate::units::Pt, level: u8, label: &str) -> BookmarkEntry {
        BookmarkEntry {
            page_idx: page,
            y_pt: y,
            level,
            label: label.to_string(),
        }
    }

    #[derive(Debug, PartialEq)]
    struct DebugNode {
        label: String,
        level: u8,
        page: usize,
        children: Vec<DebugNode>,
    }

    fn to_debug(n: &TreeNode) -> DebugNode {
        DebugNode {
            label: n.label.clone(),
            level: n.level,
            page: n.page_idx,
            children: n.children.iter().map(to_debug).collect(),
        }
    }

    #[test]
    fn simple_hierarchy() {
        let entries = vec![
            entry(0, 10.0.as_pt(), 1, "Chapter 1"),
            entry(0, 50.0.as_pt(), 2, "Section 1.1"),
            entry(1, 10.0.as_pt(), 2, "Section 1.2"),
            entry(2, 10.0.as_pt(), 1, "Chapter 2"),
        ];
        let tree = build_tree(&entries);
        let debug: Vec<_> = tree.iter().map(to_debug).collect();
        assert_eq!(
            debug,
            vec![
                DebugNode {
                    label: "Chapter 1".into(),
                    level: 1,
                    page: 0,
                    children: vec![
                        DebugNode {
                            label: "Section 1.1".into(),
                            level: 2,
                            page: 0,
                            children: vec![],
                        },
                        DebugNode {
                            label: "Section 1.2".into(),
                            level: 2,
                            page: 1,
                            children: vec![],
                        },
                    ],
                },
                DebugNode {
                    label: "Chapter 2".into(),
                    level: 1,
                    page: 2,
                    children: vec![],
                },
            ]
        );
    }

    #[test]
    fn orphan_h3_becomes_top_level_when_stack_empty() {
        let entries = vec![entry(0, 10.0.as_pt(), 3, "Stray")];
        let tree = build_tree(&entries);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].label, "Stray");
        assert_eq!(tree[0].level, 3);
        assert!(tree[0].children.is_empty());
    }

    #[test]
    fn skipped_level_nests_under_nearest_shallower() {
        let entries = vec![
            entry(0, 10.0.as_pt(), 1, "A"),
            entry(0, 50.0.as_pt(), 3, "A.x"),
        ];
        let tree = build_tree(&entries);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].label, "A.x");
    }

    #[test]
    fn empty_entries_produce_empty_outline() {
        let tree = build_tree(&[]);
        assert!(tree.is_empty());
    }

    // Three-level nesting (H1 > H2 > H3): when H3 is added, `walk_mut` is
    // called with a 2-element path [0, 0], causing its for-loop body to execute.
    // The existing tests only use 2-level nesting, where `walk_mut` always
    // receives a 1-element path and the loop body is never entered.
    #[test]
    fn three_level_nesting_exercises_walk_mut_deep_path() {
        let entries = vec![
            entry(0, 10.0.as_pt(), 1, "Chapter"),
            entry(0, 50.0.as_pt(), 2, "Section"),
            entry(0, 90.0.as_pt(), 3, "Subsection"),
        ];
        let tree = build_tree(&entries);
        let debug: Vec<_> = tree.iter().map(to_debug).collect();
        assert_eq!(
            debug,
            vec![DebugNode {
                label: "Chapter".into(),
                level: 1,
                page: 0,
                children: vec![DebugNode {
                    label: "Section".into(),
                    level: 2,
                    page: 0,
                    children: vec![DebugNode {
                        label: "Subsection".into(),
                        level: 3,
                        page: 0,
                        children: vec![],
                    }],
                }],
            }]
        );
    }

    // Two H3 siblings under the same H2: verifies the second grandchild also
    // finds the correct parent via walk_mut after the first grandchild was added.
    #[test]
    fn two_siblings_at_depth_three() {
        let entries = vec![
            entry(0, 10.0.as_pt(), 1, "Chapter"),
            entry(0, 50.0.as_pt(), 2, "Section"),
            entry(0, 90.0.as_pt(), 3, "Sub A"),
            entry(0, 130.0.as_pt(), 3, "Sub B"),
        ];
        let tree = build_tree(&entries);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].children.len(), 1);
        let section = &tree[0].children[0];
        assert_eq!(section.children.len(), 2);
        assert_eq!(section.children[0].label, "Sub A");
        assert_eq!(section.children[1].label, "Sub B");
    }

    // `build_outline` (not just `build_tree`) must complete without panicking for
    // a 3-level hierarchy, exercising the recursive `to_krilla_node` conversion.
    #[test]
    fn build_outline_three_levels_does_not_panic() {
        let entries = vec![
            entry(0, 10.0.as_pt(), 1, "Chapter"),
            entry(0, 50.0.as_pt(), 2, "Section"),
            entry(0, 90.0.as_pt(), 3, "Subsection"),
        ];
        let _outline = build_outline(&entries);
    }
}
