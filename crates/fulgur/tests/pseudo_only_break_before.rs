//! pseudo 要素のみを持つ要素に `break-before: page` が適用された場合、
//! ページ分割が有効になることを検証する。
//!
//! 回帰防止: `build_list_item_body` と `convert_node_inner` の
//! pseudo-only fallback で `Pagination` が落ちて、break-before:page が
//! 無視されていたバグ (PR #194, coderabbit review threads 2 & 3) への対策。
//!
//! このパスに到達するには、inline root かつテキスト無し、inline pseudo image
//! 有りという条件を満たす必要がある。そのため `::before` を `content: url(...)`
//! で画像化する (AssetBundle 経由で画像を登録)。

use fulgur::{AssetBundle, Engine, PageSize};

/// 1x1 red PNG minimal bytes.
const TEST_PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn page_count(pdf: &[u8]) -> usize {
    let prefix = b"/Type /Page";
    let mut count = 0usize;
    let mut i = 0;
    while i + prefix.len() < pdf.len() {
        if &pdf[i..i + prefix.len()] == prefix {
            let next = pdf[i + prefix.len()];
            if !next.is_ascii_alphanumeric() {
                count += 1;
            }
            i += prefix.len();
        } else {
            i += 1;
        }
    }
    count
}

fn make_bundle_with_image() -> AssetBundle {
    let mut bundle = AssetBundle::default();
    bundle.add_image("dot.png", TEST_PNG_1X1.to_vec());
    bundle
}

/// pseudo-only な `<div>` (`::before` で画像を供給) に `break-before: page`
/// が付いた場合、新しいページが生成される。
/// `convert_node_inner` 内の inline-root pseudo-only fallback を exercise する。
#[test]
fn pseudo_only_inline_root_honours_break_before_page() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body, div { margin: 0; padding: 0; }
        .first { height: 40pt; background: #eee; }
        .icon { break-before: page; }
        .icon::before { content: url("dot.png"); }
    </style></head><body>
      <div class="first">before</div>
      <div class="icon"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .assets(make_bundle_with_image())
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "pseudo-only element with break-before:page should force new page, got {} pages",
        page_count(&pdf)
    );
}

/// 空の `<div>` (子無し、pseudo 無し、style 無し) に `break-before: page`
/// が付いた場合、新しいページが生成される。
/// `convert_node_inner` 内の "Plain leaf node" SpacerPageable fallback を
/// exercise する。
#[test]
fn empty_leaf_div_honours_break_before_page() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body, div { margin: 0; padding: 0; }
        .first { height: 40pt; background: #eee; }
        .leaf { break-before: page; height: 10pt; }
    </style></head><body>
      <div class="first">before</div>
      <div class="leaf"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "empty leaf <div> with break-before:page should force new page, got {} pages",
        page_count(&pdf)
    );
}

/// 裸の `<img>` (visual block style 無し) に `break-before: page` が付いた
/// 場合、新しいページが生成される。
/// `wrap_replaced_in_block_style` の no-style branch を exercise する。
#[test]
fn bare_img_honours_break_before_page() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; padding: 0; }
        img { display: block; }
        .first { height: 40pt; }
        .marker { break-before: page; }
    </style></head><body>
      <img class="first" src="dot.png">
      <img class="marker" src="dot.png">
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .assets(make_bundle_with_image())
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "bare <img> with break-before:page should force new page, got {} pages",
        page_count(&pdf)
    );
}

/// pseudo-only な `<li>` (`::before` で画像を供給) に `break-before: page`
/// が付いた場合、新しいページが生成される。
/// `build_list_item_body` 内の pseudo-only fallback を exercise する。
#[test]
fn pseudo_only_list_item_honours_break_before_page() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body, ul, li { margin: 0; padding: 0; }
        .first { height: 40pt; }
        .icon { break-before: page; list-style: none; }
        .icon::before { content: url("dot.png"); }
    </style></head><body>
      <ul>
        <li class="first">before</li>
        <li class="icon"></li>
      </ul>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .assets(make_bundle_with_image())
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "pseudo-only <li> with break-before:page should force new page, got {} pages",
        page_count(&pdf)
    );
}
