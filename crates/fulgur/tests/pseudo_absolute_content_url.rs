//! Verify whether Taffy's `final_layout.size` honours the explicit `width` /
//! `height` declarations on a `position: absolute` `::before` pseudo whose
//! `content` resolves to a `url(...)` image.
//!
//! The non-absolute pseudo path (`build_pseudo_image`) reads sizes directly
//! from computed styles because Blitz/Taffy does not propagate them to
//! `final_layout` for text-less pseudos. The absolute pseudo path now
//! re-emits the pseudo via `convert_node` -> `convert_content_url`, which
//! sizes from `final_layout.size` instead. coderabbit flagged this as a
//! potential regression: if Taffy also drops the explicit width/height for
//! the abs pseudo, the image renders at the wrong (zero) size.
//!
//! This test pins the actual behaviour with a regression net so the
//! threshold is empirical, not speculative.

use fulgur::asset::AssetBundle;
use fulgur::config::{Margin, PageSize};
use fulgur::drawables::{Drawables, ImageEntry};
use fulgur::engine::Engine;
use fulgur::pagination_layout::PaginationGeometryTable;

const MINIMAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// Find the `(node_id, &ImageEntry)` whose dimensions approximately match
/// `(want_w, want_h)` (pt). Returns `None` when no entry matches.
fn find_image_by_size(
    drawables: &Drawables,
    want_w: f32,
    want_h: f32,
) -> Option<(usize, &ImageEntry)> {
    drawables
        .images
        .iter()
        .find(|(_, img)| (img.width - want_w).abs() < 0.5 && (img.height - want_h).abs() < 0.5)
        .map(|(id, e)| (*id, e))
}

/// Find the geometry placement for `node_id`. Returns `(x, y)` in
/// CSS px (the unit `Fragment.x` / `.y` use). Asserts the node has at
/// least one fragment.
fn placement(geometry: &PaginationGeometryTable, node_id: usize) -> (f32, f32) {
    let geom = geometry
        .get(&node_id)
        .unwrap_or_else(|| panic!("expected pagination geometry for node {node_id}"));
    let frag = geom
        .fragments
        .first()
        .unwrap_or_else(|| panic!("expected at least one fragment for node {node_id}"));
    (frag.x, frag.y)
}

#[test]
fn absolute_pseudo_with_content_url_honours_explicit_size() {
    let mut assets = AssetBundle::new();
    assets.add_image("dot.png", MINIMAL_PNG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();

    // 100 CSS px -> 75 PDF pt (PX_TO_PT = 0.75). Sized only via CSS on the
    // pseudo -- the parent has no in-flow content that could push Taffy to
    // size the pseudo via fallback heuristics.
    let html = r#"<!DOCTYPE html><html><head><style>
        .marker { position: relative; width: 0; height: 0; }
        .marker::before {
            content: url(dot.png);
            position: absolute;
            width: 100px;
            height: 100px;
            left: 0;
            top: 0;
        }
    </style></head><body><div class="marker"></div></body></html>"#;

    let drawables = engine.build_drawables_for_testing_no_gcpm(html);

    assert!(
        !drawables.images.is_empty(),
        "expected at least one ImageEntry from the abs pseudo, got 0"
    );
    // We accept any image whose size matches the explicit CSS dimensions --
    // there should be exactly one (the pseudo's content url image).
    let want = (75.0_f32, 75.0_f32);
    let matched = find_image_by_size(&drawables, want.0, want.1);
    assert!(
        matched.is_some(),
        "expected an ImageEntry sized {:?} pt (100 CSS px), got {:?}",
        want,
        drawables
            .images
            .values()
            .map(|i| (i.width, i.height))
            .collect::<Vec<_>>()
    );
}

/// Regression: `right` / `bottom` insets on a textless `content: url(...)`
/// abs pseudo must be resolved against the pseudo's effective image size.
/// `pseudo.final_layout.size` is `(0, 0)` for textless pseudos (Blitz
/// limitation), so reading it directly makes `cb_w - pw - r` collapse to
/// `cb_w - r`, shifting the image off-canvas by its own width/height.
#[test]
fn absolute_pseudo_with_right_bottom_offsets_by_image_size() {
    let mut assets = AssetBundle::new();
    assets.add_image("dot.png", MINIMAL_PNG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();

    // Parent: 200x200 px = 150x150 pt.
    // Pseudo: 50x50 px = 37.5x37.5 pt at right:0; bottom:0.
    // Expected pseudo position relative to parent (in pt):
    //   x = 150 - 37.5 - 0 = 112.5
    //   y = 150 - 37.5 - 0 = 112.5
    // Bug case (pre-fix): pw/ph = 0, so x = y = 150.
    let html = r#"<!DOCTYPE html><html><head><style>
        .marker { position: relative; width: 200px; height: 200px; }
        .marker::before {
            content: url(dot.png);
            position: absolute;
            width: 50px;
            height: 50px;
            right: 0;
            bottom: 0;
        }
    </style></head><body><div class="marker"></div></body></html>"#;

    let (drawables, geometry) = engine.build_drawables_and_geometry_for_testing_no_gcpm(html);

    // Find the pseudo image (37.5 pt x 37.5 pt) and its containing
    // marker block (150 pt x 150 pt). Both live as flat entries in
    // `Drawables`; their absolute placement comes from the
    // pagination geometry table.
    let (image_id, _) = find_image_by_size(&drawables, 37.5, 37.5).unwrap_or_else(|| {
        panic!(
            "expected a 37.5pt x 37.5pt ImageEntry; got: {:?}",
            drawables
                .images
                .values()
                .map(|i| (i.width, i.height))
                .collect::<Vec<_>>()
        )
    });

    // Find the marker block (150 pt square).
    let (marker_id, _) = drawables
        .block_styles
        .iter()
        .find(|(_, b)| {
            b.layout_size
                .is_some_and(|s| (s.width - 150.0).abs() < 0.5 && (s.height - 150.0).abs() < 0.5)
        })
        .map(|(id, e)| (*id, e))
        .expect("marker block should exist with 150x150 pt layout_size");

    // Resolve placements via pagination geometry. Fragment x/y are in
    // CSS px so convert to pt for comparison against the expected
    // 112.5 pt offset.
    let (image_x_px, image_y_px) = placement(&geometry, image_id);
    let (marker_x_px, marker_y_px) = placement(&geometry, marker_id);
    let dx_pt = (image_x_px - marker_x_px) * 0.75;
    let dy_pt = (image_y_px - marker_y_px) * 0.75;

    let want = 112.5_f32;
    assert!(
        (dx_pt - want).abs() < 0.5 && (dy_pt - want).abs() < 0.5,
        "expected pseudo to land at marker + ({want:.2}, {want:.2}) pt, got delta ({dx_pt:.2}, {dy_pt:.2}); \
         bug case would be (~150, ~150)",
    );
}

/// Regression: percentage `width` / `height` on an abs `content: url(...)`
/// pseudo must resolve against the CB's padding-box in **pt** -- the
/// `build_pseudo_image` helper does `pt_to_px(parent_width)` internally.
/// Passing CSS-px dims (as `cb.padding_box_size` is documented) makes the
/// percentage 4/3x too large.
#[test]
fn absolute_pseudo_percentage_size_resolves_against_padding_box_in_pt() {
    let mut assets = AssetBundle::new();
    assets.add_image("dot.png", MINIMAL_PNG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();

    // Parent: position:relative, 400 px wide. CB padding-box width = 400 px = 300 pt.
    // Pseudo: width: 50%; -> expected = 150 pt.
    // Bug case: basis treated as pt, pt_to_px(400) = ~533, then *50%/2 ->
    // px_to_pt(266) = 200 pt. (4/3x too large.)
    //
    // We do NOT specify height -- let the image use its intrinsic 1px (=1pt)
    // height to keep the assertion focused on width.
    let html = r#"<!DOCTYPE html><html><head><style>
        .marker { position: relative; width: 400px; height: 100px; }
        .marker::before {
            content: url(dot.png);
            position: absolute;
            width: 50%;
            left: 0;
            top: 0;
        }
    </style></head><body><div class="marker"></div></body></html>"#;

    let drawables = engine.build_drawables_for_testing_no_gcpm(html);

    assert!(
        !drawables.images.is_empty(),
        "expected an ImageEntry from the abs pseudo"
    );
    let pseudo = drawables
        .images
        .values()
        .find(|img| img.width > 0.0)
        .expect("at least one non-zero image");
    let want_w = 150.0_f32;
    assert!(
        (pseudo.width - want_w).abs() < 1.0,
        "expected pseudo width {:.2} pt (50% of 300pt CB), got {:.2} pt; bug case would be ~200 pt",
        want_w,
        pseudo.width,
    );
}
