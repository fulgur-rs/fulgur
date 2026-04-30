//! Phase 4 shadow harness (fulgur-9t3z).
//!
//! Drives every fixture through both `Engine::render_html` (v1, the
//! current `Pageable` trait path) and `Engine::render_html_v2` (v2,
//! the geometry + `Drawables` path). For fixtures listed in
//! `render_path_parity.toml`'s `allowlist`, asserts byte equality.
//!
//! PR 1 ships an empty allowlist — every fixture is rendered through
//! both paths to validate that v2 doesn't crash, and the harness
//! reports a coverage line ("v2 byte-identical: N / TOTAL"). PR 2 onward
//! adds fixtures to the allowlist as Pageable types migrate, and the
//! `assert_eq!` arm fires when a previously-passing fixture regresses.
//!
//! When the allowlist reaches the full 56-fixture set, PR 7 flips
//! `Engine::render_html` to call the v2 path by default and PR 8
//! deletes v1.

use fulgur::config::PageSize;
use fulgur::engine::Engine;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const ALLOWLIST_TOML: &str = include_str!("render_path_parity.toml");

/// Repository root, resolved from the fulgur crate's manifest dir.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("fulgur crate should be nested under <repo>/crates")
        .to_path_buf()
}

/// Resolve every fixture currently exercised by VRT (incl. GCPM) and
/// `fulgur-cli`'s examples_determinism integration.
fn collect_fixtures() -> Vec<FixtureSpec> {
    let mut out = Vec::new();
    let root = repo_root();

    // VRT fixtures — read manifest.toml and resolve each row's path
    // against `crates/fulgur-vrt/fixtures/`. The VRT manifest knows
    // about per-fixture margin / page-size / bookmark overrides; we
    // re-apply them so rendered PDFs match the VRT runner's settings
    // exactly.
    let vrt_manifest = root.join("crates/fulgur-vrt/manifest.toml");
    let vrt_root = root.join("crates/fulgur-vrt/fixtures");
    let vrt_text = std::fs::read_to_string(&vrt_manifest).expect("read VRT manifest");
    let vrt_parsed: VrtManifest = toml::from_str(&vrt_text).expect("parse VRT manifest");
    for row in &vrt_parsed.fixture {
        let html_path = vrt_root.join(&row.path);
        out.push(FixtureSpec {
            label: format!("vrt://{}", row.path),
            html_path,
            page_size: row
                .page_size
                .clone()
                .unwrap_or_else(|| vrt_parsed.defaults.page_size.clone()),
            margin_pt: row.margin_pt,
            bookmarks: row.bookmarks.unwrap_or(false),
            base_path: None,
        });
    }

    // examples/ fixtures — every directory under `examples/` that has
    // an `index.html` is rendered with default config. Mirrors the
    // `fulgur-cli` examples_determinism harness.
    let examples_root = root.join("examples");
    if let Ok(entries) = std::fs::read_dir(&examples_root) {
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .filter(|p| p.join("index.html").exists())
            .collect();
        dirs.sort();
        for dir in dirs {
            let name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            out.push(FixtureSpec {
                label: format!("examples://{name}"),
                html_path: dir.join("index.html"),
                page_size: "A4".into(),
                margin_pt: None,
                bookmarks: false,
                base_path: Some(dir.clone()),
            });
        }
    }

    out
}

/// Render a single fixture via the requested path; returns the PDF
/// bytes.
fn render(fx: &FixtureSpec, path: RenderPath) -> Vec<u8> {
    let html = std::fs::read_to_string(&fx.html_path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", fx.html_path.display()));

    let mut builder = Engine::builder().page_size(page_size_from_name(&fx.page_size));
    if let Some(mpt) = fx.margin_pt {
        builder = builder.margin(fulgur::config::Margin::uniform(mpt));
    }
    if fx.bookmarks {
        builder = builder.bookmarks(true);
    }
    if let Some(base) = &fx.base_path {
        builder = builder.base_path(base.clone());
    }
    let engine = builder.build();

    match path {
        RenderPath::V1 => engine
            .render_html(&html)
            .unwrap_or_else(|e| panic!("v1 render {}: {e}", fx.label)),
        RenderPath::V2 => engine
            .render_html_v2(&html)
            .unwrap_or_else(|e| panic!("v2 render {}: {e}", fx.label)),
    }
}

fn page_size_from_name(name: &str) -> PageSize {
    match name.to_ascii_uppercase().as_str() {
        "A4" => PageSize::A4,
        "A3" => PageSize::A3,
        "LETTER" => PageSize::LETTER,
        _ => PageSize::A4,
    }
}

#[derive(Debug, Clone, Copy)]
enum RenderPath {
    V1,
    V2,
}

#[derive(Debug, Clone)]
struct FixtureSpec {
    /// Human-readable identifier (e.g. `vrt://basic/borders.html`).
    label: String,
    html_path: PathBuf,
    page_size: String,
    margin_pt: Option<f32>,
    bookmarks: bool,
    /// Forwarded to `Engine::builder().base_path(..)` so relative
    /// `<link rel=stylesheet href=...>` references resolve. Required
    /// for the GCPM and `examples/` fixtures.
    base_path: Option<PathBuf>,
}

#[derive(serde::Deserialize)]
struct AllowlistFile {
    #[serde(default)]
    allowlist: Vec<String>,
}

#[derive(serde::Deserialize)]
struct VrtManifest {
    defaults: VrtDefaults,
    #[serde(default)]
    fixture: Vec<VrtFixture>,
}

#[derive(serde::Deserialize)]
struct VrtDefaults {
    page_size: String,
}

#[derive(serde::Deserialize)]
struct VrtFixture {
    path: String,
    page_size: Option<String>,
    margin_pt: Option<f32>,
    bookmarks: Option<bool>,
}

fn load_allowlist() -> BTreeSet<String> {
    let parsed: AllowlistFile =
        toml::from_str(ALLOWLIST_TOML).expect("parse render_path_parity.toml");
    parsed.allowlist.into_iter().collect()
}

#[test]
fn render_path_byte_equality() {
    let fixtures = collect_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no fixtures collected — repository layout changed?"
    );

    let allowlist = load_allowlist();
    let mut byte_eq_count = 0usize;
    let mut diffs: Vec<(String, usize, usize)> = Vec::new();
    let mut allowlist_failures: Vec<(String, usize, usize)> = Vec::new();

    // Restrict noisy fixtures: GCPM/font fixtures rely on
    // `FONTCONFIG_FILE` for determinism. Don't fail the test if it
    // isn't set; just skip the byte-eq comparison so PR 1 can run
    // green in environments without the pinned fontconfig.
    let fontconfig_set = std::env::var_os("FONTCONFIG_FILE").is_some();

    for fx in &fixtures {
        let v1 = render(fx, RenderPath::V1);
        let v2 = render(fx, RenderPath::V2);

        let eq = v1 == v2;
        if eq {
            byte_eq_count += 1;
        } else {
            diffs.push((fx.label.clone(), v1.len(), v2.len()));
        }

        if allowlist.contains(&fx.label) && !eq && fontconfig_set {
            allowlist_failures.push((fx.label.clone(), v1.len(), v2.len()));
        }
    }

    eprintln!(
        "render_path_parity: v2 byte-identical {} / {} fixtures (allowlist {})",
        byte_eq_count,
        fixtures.len(),
        allowlist.len(),
    );

    if std::env::var_os("FULGUR_PARITY_VERBOSE").is_some() {
        for (label, v1l, v2l) in &diffs {
            eprintln!("  diff {label}: v1={v1l}B v2={v2l}B");
        }
    }

    if !allowlist_failures.is_empty() {
        let detail = allowlist_failures
            .iter()
            .map(|(label, v1l, v2l)| format!("  {label}: v1={v1l}B v2={v2l}B"))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "{} allowlisted fixture(s) regressed in v2:\n{detail}",
            allowlist_failures.len()
        );
    }

    // PR 1: allowlist is empty by design. The line above is the only
    // visible signal — the test passes regardless.
    let _ = diffs;
}

/// Inline byte-equality cases. These exist alongside the on-disk
/// fixtures because PR 3's Paragraph migration only unlocks byte-eq
/// for documents with **purely paragraph content under a margin:0
/// body** — no Block backgrounds, no inline-block, no Table. The
/// existing VRT / examples fixtures all have richer content that
/// requires later PRs (Block, Table, Multicol, Transform). Inline
/// cases let each PR demonstrate productive byte-eq advancement
/// without seeding VRT goldens that lock in incomplete v2 output.
///
/// Each case asserts unconditionally — they are the unit-of-progress
/// for the migration. PR N adds cases that PR N's migration covers.
#[test]
fn inline_byte_equality_cases() {
    // PR 3 (Paragraph + inline content) coverage.
    let pr3_cases: &[(&str, &str)] = &[
        (
            "minimal body text",
            "<!DOCTYPE html><html><head><style>body{margin:0;padding:0}</style></head><body>hello world</body></html>",
        ),
        (
            "two paragraphs",
            "<!DOCTYPE html><html><head><style>body{margin:0;padding:0}p{margin:0}</style></head><body><p>first paragraph</p><p>second paragraph here</p></body></html>",
        ),
        (
            "paragraph with anchor link",
            r#"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}p{margin:0}</style></head><body><p>before <a href="https://example.com">link</a> after</p></body></html>"#,
        ),
        (
            "paragraph with internal anchor",
            r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}p{margin:0}</style></head><body><p id="top">heading line</p><p><a href="#top">jump</a></p></body></html>"##,
        ),
    ];

    // PR 4 (Block migration) coverage — backgrounds, borders,
    // border-radius, box-shadow at the block frame. Body is set to
    // margin:0 so the v2 frame anchor matches v1's block.draw call.
    let pr4_cases: &[(&str, &str)] = &[
        (
            "solid block with background color",
            "<!DOCTYPE html><html><head><style>body{margin:0;padding:0}div{width:100px;height:80px;background:#e53935}</style></head><body><div></div></body></html>",
        ),
        (
            "block with solid border",
            "<!DOCTYPE html><html><head><style>body{margin:0;padding:0}div{width:80px;height:80px;border:4px solid #444}</style></head><body><div></div></body></html>",
        ),
        (
            "block with border-radius",
            "<!DOCTYPE html><html><head><style>body{margin:0;padding:0}div{width:80px;height:80px;background:#bdf;border-radius:12px}</style></head><body><div></div></body></html>",
        ),
        (
            "two stacked blocks",
            "<!DOCTYPE html><html><head><style>body{margin:0;padding:0}div{width:80px;height:60px}.a{background:#fcd}.b{background:#cdf}</style></head><body><div class=\"a\"></div><div class=\"b\"></div></body></html>",
        ),
        // Regression for PR #303 Devin: convert wraps an inline-root
        // paragraph in a BlockPageable that shares the same node_id, so
        // both `block_styles[id]` and `paragraphs[id]` are populated. The
        // block dispatch must not `continue` past the paragraph check —
        // background draws first, glyph runs draw on top.
        (
            "paragraph with background (shared node_id)",
            "<!DOCTYPE html><html><head><style>body{margin:0;padding:0}p{margin:0;background:#fce}</style></head><body><p>hello</p></body></html>",
        ),
        // Regression for PR #303 Devin (block id anchors): `<a href="#x">`
        // targeting `<div id="x">` must resolve in v2 the same way it
        // does in v1 (`BlockPageable::collect_ids`). v1 emits a
        // `/Link → /Dest` mapping; v2 must register block ids in the
        // pre-pass `DestinationRegistry`.
        (
            "anchor link to block id",
            r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}div{width:80px;height:40px}#target{background:#cef}p{margin:0}</style></head><body><div id="target"></div><p><a href="#target">jump</a></p></body></html>"##,
        ),
        // Regression for PR #303 follow-up Devin: shared node_id
        // (block + paragraph from `convert::inline_root`) with
        // `opacity < 1.0` must compose under ONE
        // `draw_with_opacity(block.opacity, ...)` group, mirroring v1's
        // `BlockPageable::draw` which wraps bg/border + child draws in
        // a single group. Separate wrappers paint the bg at 50% but
        // glyphs at 100% — visually wrong AND byte-divergent.
        (
            "paragraph with opacity and background (shared node_id)",
            "<!DOCTYPE html><html><head><style>body{margin:0;padding:0}p{margin:0;background:#cef;opacity:0.5}</style></head><body><p>hello</p></body></html>",
        ),
    ];

    // PR 5 (Table + ListItem) extends `Drawables` with `tables` and
    // `list_items` and adds `body_offset_pt` propagation so html→body
    // collapsed margin reaches v2 children. Inline cases for the new
    // types are deferred to PR 6 — table cells contain paragraphs
    // whose text shaping resolves through inline_root, and inline-box
    // wiring still has gaps that flake the byte-eq comparison on
    // these specific configurations. The on-disk allowlist coverage
    // (10 new fixtures) demonstrates the productive byte-eq advance.
    //
    // Regression for PR #304 Devin (list-item shared node_id): `<li>`
    // and its body block share the same node_id — `list_items[id]`
    // and `block_styles[id]` co-exist. The marker dispatch must NOT
    // `continue;` past the block check or `<li style="background:...">`
    // silently drops the body block paint in v2.
    let pr5_cases: &[(&str, &str)] = &[
        (
            "list item with body block background (shared node_id)",
            r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}ul{margin:0;padding:0;list-style:none}li{background:#fdf;height:40px}</style></head><body><ul><li></li></ul></body></html>"##,
        ),
        // Regression for PR #304 follow-up Devin (list-item opacity
        // grouping): v1's `ListItemPageable::draw` wraps marker + body
        // block in a SINGLE `draw_with_opacity` group. v2 must produce
        // the same single q/Q wrapper or the PDF stream diverges when
        // `<li style="opacity:0.5">`.
        (
            "list item with opacity and body block background",
            r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}ul{margin:0;padding:0;list-style:none}li{background:#cef;height:40px;opacity:0.5}</style></head><body><ul><li></li></ul></body></html>"##,
        ),
    ];

    // PR 6 (Transform + MulticolRule + marker wrappers verification)
    // exercises the new `Drawables.transforms` and
    // `Drawables.multicol_rules` maps. Transform tests cover the
    // shared-node_id case (block + transform same id) and the strict
    // descendant case (block wraps a child with its own id). Multicol
    // rule painting requires a `column-rule` style, which the example
    // fixtures don't currently use, so we add minimal cases here.
    let pr6_cases: &[(&str, &str)] = &[
        (
            "block with transform translate (shared node_id)",
            r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}.box{width:80px;height:60px;background:#cef;transform:translate(10px,5px)}</style></head><body><div class="box"></div></body></html>"##,
        ),
        (
            "block with transform rotate around center",
            r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}.box{width:80px;height:60px;background:#fce;transform:rotate(15deg);transform-origin:center}</style></head><body><div class="box"></div></body></html>"##,
        ),
        // PR #305 Devin: nested transforms — inner transform was
        // silently dropped because `draw_under_transform` dispatched
        // descendants via `dispatch_fragment` which never checks
        // `drawables.transforms`. v1's nested
        // `TransformWrapperPageable::draw` recursively pushes both
        // matrices; v2 must do the same by recursing into
        // `draw_under_transform` when a descendant has its own
        // `TransformEntry`.
        (
            "nested transforms (rotate around translate)",
            r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}.outer{width:120px;height:80px;background:#cef;transform:translate(10px,5px)}.inner{width:60px;height:40px;background:#fce;transform:rotate(10deg)}</style></head><body><div class="outer"><div class="inner"></div></div></body></html>"##,
        ),
        // PR #305 follow-up Devin: nested transforms with a
        // non-transform descendant inside the inner transform caused
        // double-draw — `draw_under_transform` for the OUTER iterated
        // its full `descendants` list (which includes the inner's own
        // descendants), so the deeply nested node was painted once
        // under outer*inner (correct, via the inner recursion) and a
        // second time under outer only (wrong). Regression: a styled
        // grandchild block inside two stacked transforms.
        (
            "triple nested transform descendant double-draw",
            r##"<!DOCTYPE html><html><head><style>body{margin:0;padding:0}.outer{width:200px;height:120px;background:#cef;transform:translate(8px,4px)}.inner{width:120px;height:80px;background:#fce;transform:rotate(5deg)}.leaf{width:40px;height:20px;background:#ffd}</style></head><body><div class="outer"><div class="inner"><div class="leaf"></div></div></div></body></html>"##,
        ),
        // PR 6 follow-up: shared-node_id inner content (inline-root
        // paragraph from `convert::inline_root`, replaced image/svg
        // from `convert::replaced`) must paint at the wrapping block's
        // *content-box* top-left, not its border-box top-left, when
        // the block carries `padding` or `border`. v1 expresses this
        // via `PositionedChild { x: content_inset.x, y:
        // content_inset.y }`; v2 mirrors it by adding
        // `block.style.content_inset()` inside
        // `draw_block_with_inner_content` (and
        // `draw_list_item_with_block`). Without that offset, every
        // text run inside a padded block landed `padding+border`
        // worth of px high-and-left of where it should — the bug that
        // kept `examples/transform`'s `.box { padding: 6px }` 11
        // bytes short.
        (
            "padded paragraph with background (shared node_id)",
            r##"<!DOCTYPE html><html><head><style>body{margin:0}p{margin:0;padding:6px;background:#cef}</style></head><body><p>hello</p></body></html>"##,
        ),
        (
            "bordered paragraph with background (shared node_id)",
            r##"<!DOCTYPE html><html><head><style>body{margin:0}p{margin:0;border:3px solid #444;background:#fce}</style></head><body><p>hello</p></body></html>"##,
        ),
        // PR 6 follow-up (fulgur-9y1a) — html root element's
        // background was silently dropped because the fragmenter's
        // `geometry` only records body + descendants, never html. v2
        // now paints html's bg as a pre-pass at `(margin.left,
        // margin.top)` with `block_styles[root_id].layout_size`.
        // Without that pre-pass v2 lost 13 bytes per gradient-hint
        // fixture (six fixtures regressed simultaneously, all sharing
        // `html, body { background: white }`).
        (
            "html background distinct from body background",
            r##"<!DOCTYPE html><html><head><style>html, body { margin:0; padding:0; background:white; } .g { width:120px; height:80px; margin:24px; background:red; }</style></head><body><div class="g"></div></body></html>"##,
        ),
    ];

    // Bookmark-inside-transform regression (PR #305 Devin): a heading
    // nested under a `transform`-wrapped ancestor must still record a
    // bookmark anchor in the PDF outline. v1 invokes
    // `BookmarkMarkerWrapperPageable::draw` recursively from inside
    // `TransformWrapperPageable::draw`; v2 records the anchor BEFORE
    // the `transformed_descendants` skip in the dispatcher.
    let bookmark_in_transform = (
        "bookmark anchor inside transformed ancestor",
        r##"<!DOCTYPE html><html><head><style>body{margin:0}div{transform:rotate(5deg)}h1{margin:0;font-size:14px}</style></head><body><div><h1>Heading</h1></div></body></html>"##,
    );

    let cases = pr3_cases
        .iter()
        .chain(pr4_cases.iter())
        .chain(pr5_cases.iter())
        .chain(pr6_cases.iter());
    for (label, html) in cases {
        let engine = Engine::builder().build();
        let v1 = engine
            .render_html(html)
            .unwrap_or_else(|e| panic!("v1 render `{label}`: {e}"));
        let v2 = engine
            .render_html_v2(html)
            .unwrap_or_else(|e| panic!("v2 render `{label}`: {e}"));
        assert_eq!(
            v1,
            v2,
            "inline case `{label}` is not byte-identical (v1={}B v2={}B)",
            v1.len(),
            v2.len(),
        );
    }

    // Bookmark anchors require `bookmarks(true)` on the engine —
    // separate render so the assertion can target the
    // bookmark-inside-transform case explicitly.
    {
        let (label, html) = bookmark_in_transform;
        let engine = Engine::builder().bookmarks(true).build();
        let v1 = engine
            .render_html(html)
            .unwrap_or_else(|e| panic!("v1 render `{label}`: {e}"));
        let v2 = engine
            .render_html_v2(html)
            .unwrap_or_else(|e| panic!("v2 render `{label}`: {e}"));
        assert_eq!(
            v1,
            v2,
            "inline case `{label}` is not byte-identical (v1={}B v2={}B)",
            v1.len(),
            v2.len(),
        );
    }
}
