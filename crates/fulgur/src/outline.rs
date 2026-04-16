//! Build a krilla Outline (PDF bookmark tree) from flat BookmarkEntry records.

use krilla::destination::XyzDestination;
use krilla::geom::Point;
use krilla::outline::{Outline, OutlineNode};

use crate::pageable::{BookmarkEntry, Pt};

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
    pub y_pt: Pt,
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
    let dest = XyzDestination::new(node.page_idx, Point::from_xy(0.0, node.y_pt));
    let mut o = OutlineNode::new(node.label, dest);
    for child in node.children {
        o.push_child(to_krilla_node(child));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(page: usize, y: Pt, level: u8, label: &str) -> BookmarkEntry {
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
            entry(0, 10.0, 1, "Chapter 1"),
            entry(0, 50.0, 2, "Section 1.1"),
            entry(1, 10.0, 2, "Section 1.2"),
            entry(2, 10.0, 1, "Chapter 2"),
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
        let entries = vec![entry(0, 10.0, 3, "Stray")];
        let tree = build_tree(&entries);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].label, "Stray");
        assert_eq!(tree[0].level, 3);
        assert!(tree[0].children.is_empty());
    }

    #[test]
    fn skipped_level_nests_under_nearest_shallower() {
        let entries = vec![entry(0, 10.0, 1, "A"), entry(0, 50.0, 3, "A.x")];
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
}
