//! Integration: inline-block with overflow:hidden produces a `BlockEntry`
//! in `Drawables.block_styles` with `has_overflow_clip()` set, registered as
//! inline-box content via `Drawables.inline_box_subtree_skip`. This is the
//! structural prerequisite for fulgur-i5a's
//! `inline_block_with_overflow_hidden_becomes_clipped_block` ignored test.

use fulgur::engine::Engine;

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

    let drawables = Engine::builder()
        .build()
        .build_drawables_for_testing_no_gcpm(html);

    // Find a block whose style requests overflow clipping AND that is
    // registered as inline-box content (`inline_box_subtree_skip`
    // membership). The outer `.ib` span matches both predicates: it is
    // an inline-box root (skip set), and `overflow: hidden` flips
    // `has_overflow_clip()` on its `BlockEntry`.
    let clipped_inline_box = drawables.block_styles.iter().find(|(node_id, entry)| {
        entry.style.has_overflow_clip() && drawables.inline_box_subtree_skip.contains(node_id)
    });

    assert!(
        clipped_inline_box.is_some(),
        "expected an inline-block with overflow:hidden registered as a clipped \
         inline-box-content block. block_styles entries with overflow_clip: {:?}; \
         inline_box_subtree_skip: {:?}",
        drawables
            .block_styles
            .iter()
            .filter(|(_, e)| e.style.has_overflow_clip())
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        drawables.inline_box_subtree_skip,
    );
}
