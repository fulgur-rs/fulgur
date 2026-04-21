//! Integration: inline-block with overflow:hidden produces a BlockPageable
//! with has_overflow_clip() set, reachable via a ParagraphPageable's
//! LineItem::InlineBox. This is the structural prerequisite for fulgur-i5a's
//! `inline_block_with_overflow_hidden_becomes_clipped_block` ignored test.

use fulgur::engine::Engine;
use fulgur::pageable::{BlockPageable, Pageable, PositionedChild};
use fulgur::paragraph::{LineItem, ParagraphPageable};

fn build_tree(html: &str) -> Box<dyn Pageable> {
    Engine::builder()
        .build()
        .build_pageable_for_testing_no_gcpm(html)
}

fn walk_paragraphs<'a>(root: &'a dyn Pageable, out: &mut Vec<&'a ParagraphPageable>) {
    if let Some(p) = root.as_any().downcast_ref::<ParagraphPageable>() {
        out.push(p);
        return;
    }
    if let Some(block) = root.as_any().downcast_ref::<BlockPageable>() {
        for PositionedChild { child, .. } in &block.children {
            walk_paragraphs(child.as_ref(), out);
        }
    }
}

#[test]
fn inline_block_with_overflow_hidden_is_reachable_as_clipped_block() {
    let html = r#"<!DOCTYPE html><html><head><style>
        .ib {
            display: inline-block;
            width: 100px;
            height: 50px;
            overflow: hidden;
            background: #eee;
        }
    </style></head><body><div><span class="ib"><span style="display:inline-block;width:200px;height:200px;background:red"></span></span></div></body></html>"#;
    let tree = build_tree(html);
    let mut paras = Vec::new();
    walk_paragraphs(tree.as_ref(), &mut paras);

    let clipped = paras
        .iter()
        .flat_map(|p| p.lines.iter())
        .flat_map(|l| l.items.iter())
        .find_map(|it| match it {
            LineItem::InlineBox(ib) => ib
                .content
                .as_any()
                .downcast_ref::<BlockPageable>()
                .filter(|b| b.style.has_overflow_clip()),
            _ => None,
        });
    assert!(
        clipped.is_some(),
        "expected an inline-block with overflow:hidden to be reachable via LineItem::InlineBox with has_overflow_clip()"
    );
}
