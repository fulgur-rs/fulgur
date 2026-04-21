//! Integration tests for CSS `break-inside: avoid` (fulgur-ftp).

use fulgur::{Engine, PageSize};

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

/// avoid block がページ境界にまたがる → 次ページへ promote。
#[test]
fn avoid_block_straddling_boundary_promotes_to_next_page() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: 200pt 200pt; margin: 0; }
        body { margin: 0; }
        .spacer { height: 160pt; background: #eee; }
        .keep { height: 60pt; background: #c00; break-inside: avoid; }
    </style></head><body>
      <div class="spacer"></div>
      <div class="keep"></div>
    </body></html>"#;
    let engine = Engine::builder()
        .page_size(PageSize::custom(70.5556, 70.5556))
        .build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        page_count(&pdf) >= 2,
        "expected avoid block to promote to page 2, got {} pages",
        page_count(&pdf)
    );
}
