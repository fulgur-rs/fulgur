//! Thin adapter over Blitz APIs. All Blitz-specific code is isolated here
//! so that upstream API changes only require changes in this module.
//!
//! # Thread safety
//!
//! Blitz (as of `blitz-dom 0.2.4` / `blitz-html 0.2.0`) is thread-safe for the
//! operations fulgur uses: multiple threads may call `parse`, `resolve`, and
//! pass application concurrently on independent documents. The adapter does
//! not take any process-wide lock.
//!
//! # Stdout hygiene
//!
//! `blitz-html` prints `println!("ERROR: {error}")` in its `TreeSink::finish`
//! implementation for every non-fatal html5ever parse error (unclosed tags,
//! unexpected tokens, etc.). fulgur does *not* suppress these at the library
//! level because doing so required manipulating process-wide fd 1, which is
//! fundamentally racy in multi-threaded contexts. Callers that need clean
//! stdout (notably `fulgur-cli` when rendering to stdout with `-o -`) must
//! redirect fd 1 at their own call site where single-threaded execution is
//! guaranteed. See `docs/plans/2026-04-11-blitz-thread-safety-investigation.md`
//! for the full investigation and rationale.

use blitz_dom::DocumentConfig;
use blitz_dom::net::Resource;
use blitz_html::HtmlDocument;

// Type re-exports for adapter isolation (fulgur-x92a)
//
// `blitz_dom` 型は alias として再公開する。call site が `use crate::blitz_adapter::Node`
// と書けば、Blitz 内部での move/rename を adapter 内 1 箇所で吸収できる。
// 同じ alias を adapter 自身の public API（例: `extract_content_image_url(node: &blitz_dom::Node)`）
// が引数で受けるかは無関係——alias なので呼び出し元には透過。
pub use blitz_dom::node::{ListItemLayoutPosition, Marker};
pub use blitz_dom::{BaseDocument, Node, NodeData};

/// `Marker` を空白追加なしの `String` に変換する。
///
/// `Marker::Char(c)` → `c.to_string()`、`Marker::String(s)` → `s.clone()`。
/// `extract_marker_lines` と `find_marker_font` で使う。
///
/// Blitz 0.3 系で variant が増えた場合は adapter 内でハンドリングを追加すれば
/// 呼び出し側 (convert/list_marker.rs) は無変更。
pub fn marker_to_string(marker: &Marker) -> String {
    match marker {
        Marker::Char(c) => c.to_string(),
        Marker::String(s) => s.clone(),
    }
}

/// `Marker` を skrifa shape 入力用テキストに変換する（**非対称な空白付与**）。
///
/// - `Marker::Char(c)` → `format!("{c} ")`（**末尾空白あり**: Blitz の
///   `build_inline_layout` が `format!("{char} ")` で生成するのと整合）
/// - `Marker::String(s)` → `s.clone()`（空白なし: 通常 `"1. "` のように
///   既に trailing space を含む形式が来る前提）
///
/// `shape_marker_with_skrifa` でのみ使用する。
pub fn marker_skrifa_text(marker: &Marker) -> String {
    match marker {
        Marker::Char(c) => format!("{c} "),
        Marker::String(s) => s.clone(),
    }
}

/// `ListItemLayoutPosition::Outside(layout)` の場合に `Some(&layout)`、
/// `Inside` の場合に `None` を返す。
///
/// `Outside` variant は `Box<parley::Layout<TextBrush>>` を持つが、call site が
/// `.lines()` で auto-deref することを前提に `&parley::Layout<TextBrush>` を返す。
///
/// Blitz 0.3 系で variant 追加・rename された場合は adapter 内で吸収する。
/// 呼び出し側 (convert/list_marker.rs / convert/list_item.rs) は本 helper 経由にすることで
/// pattern match に直接さらされない。
pub fn list_position_outside_layout(
    pos: &ListItemLayoutPosition,
) -> Option<&parley::Layout<blitz_dom::node::TextBrush>> {
    match pos {
        ListItemLayoutPosition::Outside(layout) => Some(layout.as_ref()),
        ListItemLayoutPosition::Inside => None,
    }
}

/// `ListItemLayoutPosition::Inside` であるかを返す boolean accessor。
pub fn is_list_position_inside(pos: &ListItemLayoutPosition) -> bool {
    matches!(pos, ListItemLayoutPosition::Inside)
}

/// Extract per-line height (CSS px) from a `parley::Layout`.
///
/// Returns one `f32` per line, matching `layout.lines().count()`. The
/// height is parley's absolute `LineMetrics::line_height` field — the
/// pen-advance the renderer applies when stacking the line below the
/// previous one. This matches what `convert::inline_root` already reads
/// in `crates/fulgur/src/convert/inline_root.rs:458` (which uses
/// `line.metrics()` for its own per-line render math).
///
/// Used by `multicol_layout::slice_lines_by_budget` to split an
/// inline-root paragraph across multicol columns (fulgur-6q5).
pub fn parley_line_heights(layout: &parley::Layout<blitz_dom::node::TextBrush>) -> Vec<f32> {
    layout
        .lines()
        .map(|line| line.metrics().line_height)
        .collect()
}
use blitz_traits::net::NetProvider;
#[cfg(not(target_arch = "wasm32"))]
use blitz_traits::net::Url;
use blitz_traits::shell::{ColorScheme, Viewport};
use parley::FontContext;
use std::path::Path;
use std::sync::Arc;

/// Re-exports of the Blitz network trait surface used by [`crate::net`].
///
/// All `blitz_traits::net::*` and `blitz_dom::net::*` references in fulgur
/// flow through this module so that upstream Blitz API churn (e.g. the
/// `NetProvider<Resource>` generic removal in blitz-dom 0.3) is contained
/// to `blitz_adapter.rs` — the same isolation rule documented in the file
/// header. `crate::net::FulgurNetProvider`'s `impl NetProvider` block still
/// has to be touched when the trait shape changes, but downstream call sites
/// and tests no longer name the upstream crates directly.
pub mod net {
    pub use blitz_dom::net::Resource;
    pub use blitz_traits::net::{
        BoxedHandler, Bytes, NetHandler, NetProvider, Request, SharedCallback, Url,
    };
}

/// Parse HTML and return a fully resolved document (styles + layout computed).
///
/// We pass the content width as the viewport width so Taffy wraps text
/// at the right column. The viewport height is forwarded to stylo so that
/// `vh` units and viewport-height media queries resolve consistently with
/// `Engine::render_html`'s `parse_html_with_local_resources` path; this
/// helper used to drop the height into a 10000px placeholder, which broke
/// `100vh`-anchored fixtures.
///
/// **Layout parity with `Engine::render_html`**: in addition to stylo + Taffy
/// resolution this also runs [`relayout_position_fixed`], so unit tests
/// that build documents through this helper observe the same fixed-position
/// sizing the renderer produces. Pass the same viewport dimensions the
/// renderer would use (`Config::content_width` / `Config::content_height`
/// in CSS px) for an exact match.
pub fn parse_and_layout(
    html: &str,
    viewport_width: f32,
    viewport_height: f32,
    font_data: &[Arc<Vec<u8>>],
    system_fonts: bool,
) -> HtmlDocument {
    let mut doc = parse_inner(
        html,
        viewport_width,
        viewport_height as u32,
        font_data,
        system_fonts,
        None,
        None,
    );
    resolve(&mut doc);
    relayout_position_fixed(&mut doc, viewport_width, viewport_height);
    doc
}

/// Context available to each DOM pass.
pub struct PassContext<'a> {
    pub font_data: &'a [Arc<Vec<u8>>],
}

/// A single transformation step applied to the parsed DOM before layout resolution.
pub trait DomPass {
    fn apply(&self, doc: &mut HtmlDocument, ctx: &PassContext<'_>);
}

/// Parse HTML into a document without resolving styles or layout.
///
/// Uses Blitz's built-in `DummyNetProvider`, so `<link rel="stylesheet">`
/// and `@import` are silently ignored. For real-world rendering call
/// [`parse_html_with_local_resources`] instead, which wires fulgur's
/// own [`crate::net::FulgurNetProvider`] into Blitz.
pub fn parse(html: &str, viewport_width: f32, font_data: &[Arc<Vec<u8>>]) -> HtmlDocument {
    parse_inner(html, viewport_width, 10000, font_data, true, None, None)
}

/// Parse HTML and load any `<link rel="stylesheet">` / `@import` files
/// found inside the configured `base_path`.
///
/// Returns the parsed document plus a [`crate::gcpm::GcpmContext`]
/// containing every GCPM construct extracted from the loaded
/// stylesheets. The caller is expected to merge that context into its
/// own engine-level context (typically the one derived from `--css`).
///
/// This is the **only** entry point engine code should use for
/// production rendering: it owns construction of
/// [`crate::net::FulgurNetProvider`], the trait-object cast,
/// `base_path → file://` URL derivation, and resource draining,
/// keeping the Blitz API surface fully encapsulated in
/// `blitz_adapter.rs` (CLAUDE.md adapter-isolation rule).
///
/// Canonicalise `path` and return its `file://` directory URL.
///
/// Returns `None` on WASM (no filesystem) or when canonicalisation fails.
/// Used both by [`parse_html_with_local_resources`] (Blitz base URL) and
/// by `EngineBuilder::build` (`AssetBundle::set_base_url`) so the two paths
/// agree on the directory URL byte-for-byte.
pub fn canonical_directory_url(path: &Path) -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        path.canonicalize()
            .ok()
            .and_then(|p| Url::from_directory_path(&p).ok())
            .map(|u| u.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        None
    }
}

/// # Known limitation (tracked as beads fulgur-owa)
///
/// Each `<link rel=stylesheet media=X>` that is rewritten to
/// `<style>@import url(Y) X;</style>` triggers two fetches of `Y`
/// through [`crate::net::FulgurNetProvider`]: once during the initial
/// HTML parse (discarded at the resource level here) and once when the
/// synthetic `<style>` is processed. The second fetch pushes a fresh
/// [`crate::gcpm::GcpmContext`] into the provider's buffer, but the
/// first fetch's context is still there because this function cannot
/// currently tell them apart. As a result, `@page` margin boxes,
/// running elements, and counters declared in media-restricted
/// stylesheets get registered twice. Fixing this requires
/// URL-tagged GCPM drain semantics in `FulgurNetProvider`; see
/// `bd show fulgur-owa`.
pub fn parse_html_with_local_resources(
    html: &str,
    viewport_width: f32,
    viewport_height_px: u32,
    font_data: &[Arc<Vec<u8>>],
    system_fonts: bool,
    base_path: Option<&Path>,
) -> (HtmlDocument, crate::gcpm::GcpmContext) {
    use std::collections::HashSet;

    let net_provider = Arc::new(crate::net::FulgurNetProvider::new(
        base_path.map(|p| p.to_path_buf()),
    ));
    let provider: Arc<dyn NetProvider<Resource>> = net_provider.clone();
    let base_url = base_path.and_then(canonical_directory_url);

    let mut doc = parse_inner(
        html,
        viewport_width,
        viewport_height_px,
        font_data,
        system_fonts,
        Some(provider),
        base_url,
    );

    // Identify <link rel=stylesheet media=X> nodes *before* mutating so
    // their attributes are stable, and before loading so we can filter
    // out the (wrong-media) resources that blitz's parser already
    // triggered for them.
    let rewrites = collect_link_media_rewrites(&doc);
    let rewrite_node_ids: HashSet<usize> = rewrites.iter().map(|r| r.link_node_id).collect();

    // First drain: load resources that correspond to <link> elements
    // WITHOUT a media rewrite. Discard resources for nodes we are about
    // to rewrite — their @import replacements will re-fetch with the
    // correct MediaList.
    for resource in net_provider.drain_pending_resources() {
        if let Resource::Css(node_id, _) = &resource {
            if rewrite_node_ids.contains(node_id) {
                continue;
            }
        }
        doc.load_resource(resource);
    }

    // Apply the DOM rewrite. Mutator's `drop` synchronously triggers
    // `process_style_element` for each new <style>, which parses the
    // @import, calls StylesheetLoader → NetProvider::fetch → CssHandler
    // with `MediaList` properly propagated, and pushes new Resources.
    apply_link_media_rewrites(&mut doc, &rewrites);

    // Second drain: load the correctly-fetched stylesheets.
    for resource in net_provider.drain_pending_resources() {
        doc.load_resource(resource);
    }

    // Fold the per-stylesheet GCPM contexts into one. The caller still
    // has to merge this with its own AssetBundle-derived context.
    let mut gcpm = crate::gcpm::GcpmContext::default();
    for ctx in net_provider.drain_gcpm_contexts() {
        gcpm.extend_from(ctx);
    }
    (doc, gcpm)
}

/// The single primitive that actually constructs an `HtmlDocument`.
/// All other `parse*` functions in this module funnel through here.
fn parse_inner(
    html: &str,
    viewport_width: f32,
    viewport_height_px: u32,
    font_data: &[Arc<Vec<u8>>],
    system_fonts: bool,
    net_provider: Option<Arc<dyn NetProvider<Resource>>>,
    base_url: Option<String>,
) -> HtmlDocument {
    let viewport = Viewport::new(
        viewport_width as u32,
        viewport_height_px,
        1.0,
        ColorScheme::Light,
    );

    let font_ctx = if font_data.is_empty() && system_fonts {
        // No bundled fonts and system fonts allowed: let Blitz use its own
        // default FontContext (system fonts enabled).
        None
    } else {
        let collection = parley::fontique::Collection::new(parley::fontique::CollectionOptions {
            system_fonts,
            ..Default::default()
        });
        let mut ctx = FontContext {
            collection,
            source_cache: parley::fontique::SourceCache::new(Default::default()),
        };
        for data in font_data {
            let blob: parley::fontique::Blob<u8> = (**data).clone().into();
            ctx.collection.register_fonts(blob, None);
        }
        Some(ctx)
    };

    let config = DocumentConfig {
        viewport: Some(viewport),
        font_ctx,
        base_url: Some(base_url.unwrap_or_else(|| "file:///".to_string())),
        net_provider,
        ..DocumentConfig::default()
    };

    HtmlDocument::from_html(html, config)
}

/// Apply a sequence of DOM passes to a parsed document.
pub fn apply_passes(doc: &mut HtmlDocument, passes: &[Box<dyn DomPass>], ctx: &PassContext<'_>) {
    for pass in passes {
        pass.apply(doc, ctx);
    }
}

/// Apply a single `DomPass` to a document.
///
/// Thin adapter that lets callers invoke a typed pass directly while still
/// going through `blitz_adapter`, preserving the module's role as the single
/// Blitz API surface (see `CLAUDE.md`: "Adapter isolation: Blitz API surface
/// is contained in `blitz_adapter.rs`"). Callers can retain access to
/// pass-specific accessors (for example `RunningElementPass::into_running_store`)
/// by borrowing the pass here rather than consuming it via `apply_passes`.
pub fn apply_single_pass<P: DomPass + ?Sized>(
    pass: &P,
    doc: &mut HtmlDocument,
    ctx: &PassContext<'_>,
) {
    pass.apply(doc, ctx);
}

/// Resolve styles (Stylo) and compute layout (Taffy).
pub fn resolve(doc: &mut HtmlDocument) {
    doc.resolve(0.0);
}

/// Update the document's viewport size (in CSS px) and mark Stylo's stylist
/// dirty so the next `resolve()` re-cascades viewport-relative units (`vh`,
/// `vw`, `vmin`, `vmax`, `vi`, `vb`, etc.) against the new dimensions.
///
/// fulgur calls this after extracting `@page` size / margin overrides from
/// inline `<style>` blocks, but before the first `resolve()` pass. Without
/// this, Stylo binds `100vh` to the *initial* viewport height passed at parse
/// time (the full page height in CSS px), so a `height: 100vh` element
/// resolves to the page area instead of the @page-resolved content area —
/// the bottom margin gets ignored. See `engine.rs` and beads `fulgur-lv0a`.
///
/// `set_viewport()` updates the Stylo `Device` via `make_device()` and marks
/// stylesheets dirty (see blitz-dom 0.2.4 `document.rs:1002-1011`); the
/// downstream `resolve()` then re-runs the cascade with the corrected screen
/// size.
pub fn set_viewport_size_px(doc: &mut HtmlDocument, width_px: f32, height_px: f32) {
    let mut new_viewport = doc.viewport().clone();
    new_viewport.window_size = (width_px as u32, height_px as u32);
    doc.set_viewport(new_viewport);
}

/// Re-run Taffy layout on every `position: fixed` subtree using the page area
/// as available space. CSS 2.1 §10.1.5 specifies the initial containing block
/// (viewport) as the CB for `position: fixed`, but `stylo_taffy::convert`
/// flattens `Position::Fixed` to `Position::Absolute` (see crate
/// `stylo_taffy/src/convert.rs:215`), so the main `doc.resolve(0.0)` pass
/// lays each fixed element out against its nearest positioned ancestor
/// instead. The result is a fixed element shrink-to-fit-clipped to the
/// ancestor's narrow box, which surfaces as wrap differences in
/// `css/css-page/fixedpos-*` reftests.
///
/// This second pass walks the DOM, collects every `position: fixed` node id,
/// and calls `taffy::compute_root_layout` on each subtree as if it were a
/// document root, with `available_space` set to the page area in CSS px.
/// Taffy's caching keys on `(node_id, inputs)`, so the new inputs (different
/// available space than the first pass used) bypass the cache and overwrite
/// `unrounded_layout`. We then `round_layout` to populate `final_layout`.
///
/// The position math (which page bottom, which CB origin) is still resolved
/// by `convert::positioned::build_absolute_*_children` against the
/// viewport-anchored CB, so this pass only needs to fix the *size*.
///
/// Page-repetition of fixed content (Chrome's "should repeat on every page"
/// behavior in WPT fixedpos-* tests) is intentionally **not** done here —
/// the slice path replicates `is_fixed` children at their viewport-relative
/// coordinates on every page.
pub fn relayout_position_fixed(doc: &mut HtmlDocument, viewport_w_px: f32, viewport_h_px: f32) {
    use ::style::properties::longhands::position::computed_value::T as Pos;
    use ::style::values::computed::Length;
    use ::style::values::generics::length::GenericSize;
    use ::style::values::generics::position::GenericInset;
    use std::ops::DerefMut;

    let mut fixed_ids: Vec<usize> = Vec::new();
    let root_id = doc.root_element().id;
    collect_position_fixed_ids(doc, root_id, &mut fixed_ids, 0);
    if fixed_ids.is_empty() {
        return;
    }

    // fulgur-6wap: pre-compute per-element CSS 2.1 §10.3.7 shrink-to-fit
    // metadata. A `position: fixed` element with `width: auto` and
    // exactly one of `left`/`right` set must shrink-to-fit on the inline
    // axis; passing `Definite(viewport_w_px)` would make Taffy fill the
    // viewport, and `resolve_viewport_cb_location` would then anchor
    // `right: 0` at x=0 (WPT fixedpos-009-print, pencil at left edge).
    //
    // §10.3.7's used width formula is:
    //   width = min(max(min_content, available_w), max_content)
    // where `available_w = cb_w - inset_start_or_end`. We approximate by
    // measuring max-content with a probe pass (`AvailableSpace::MaxContent`)
    // and clamping at `available_w`. Pure `MaxContent` is *not* a valid
    // substitute on its own — Taffy's `AvailableSpace::MaxContent` means
    // "indefinite, no soft line wrapping", so wrappable text would lay
    // out on a single unwrapped line and overflow the viewport.
    //
    // The block axis stays `Definite(viewport_h_px)`: Taffy's
    // block-layout algorithm already gives content-height for `auto`
    // height roots even with definite available space (verified by
    // WPT fixedpos-001/002/008 PASSing with this input).
    enum InlineMode {
        FillViewport,
        ShrinkToFit { available_w: f32 },
    }
    let mut inline_modes: Vec<InlineMode> = Vec::with_capacity(fixed_ids.len());
    for &id in &fixed_ids {
        let mode = doc
            .get_node(id)
            .and_then(|n| n.primary_styles())
            .and_then(|s| {
                let pos = s.get_position();
                let width_is_auto = matches!(pos.width, GenericSize::Auto);
                let left = match &pos.left {
                    GenericInset::LengthPercentage(lp) => {
                        Some(lp.resolve(Length::new(viewport_w_px)).px())
                    }
                    _ => None,
                };
                let right = match &pos.right {
                    GenericInset::LengthPercentage(lp) => {
                        Some(lp.resolve(Length::new(viewport_w_px)).px())
                    }
                    _ => None,
                };
                let needs_shrink = width_is_auto && (left.is_some() ^ right.is_some());
                if !needs_shrink {
                    return None;
                }
                let inset = left.or(right).unwrap_or(0.0);
                Some(InlineMode::ShrinkToFit {
                    available_w: (viewport_w_px - inset).max(0.0),
                })
            })
            .unwrap_or(InlineMode::FillViewport);
        inline_modes.push(mode);
    }

    let base = doc.deref_mut();
    for (id, mode) in fixed_ids.into_iter().zip(inline_modes) {
        let nid = taffy::NodeId::from(id);
        let width_avail = match mode {
            InlineMode::FillViewport => taffy::AvailableSpace::Definite(viewport_w_px),
            InlineMode::ShrinkToFit { available_w } => {
                // Probe pass: measure max-content width. Taffy caches on
                // (node_id, inputs); the MaxContent inputs differ from
                // the final pass below so both run.
                let probe = taffy::Size {
                    width: taffy::AvailableSpace::MaxContent,
                    height: taffy::AvailableSpace::Definite(viewport_h_px),
                };
                taffy::compute_root_layout(base, nid, probe);
                let max_content_w = base
                    .get_node(id)
                    .map(|n| n.final_layout.size.width)
                    .unwrap_or(available_w);
                // §10.3.7: clamp max-content to the available width so
                // wrappable content wraps at the viewport edge instead
                // of overflowing on a single line.
                taffy::AvailableSpace::Definite(max_content_w.min(available_w))
            }
        };
        let avail = taffy::Size {
            width: width_avail,
            height: taffy::AvailableSpace::Definite(viewport_h_px),
        };
        taffy::compute_root_layout(base, nid, avail);
        taffy::round_layout(base, nid);
    }

    fn collect_position_fixed_ids(
        doc: &HtmlDocument,
        node_id: usize,
        out: &mut Vec<usize>,
        depth: usize,
    ) {
        if depth >= crate::MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };
        let is_fixed = node
            .primary_styles()
            .is_some_and(|s| matches!(s.get_box().clone_position(), Pos::Fixed));
        if is_fixed {
            out.push(node_id);
        }
        // Use raw children, not layout_children — layout_children may already
        // be invalidated by the time the second-pass relayout runs, and we
        // want every styled fixed element regardless of whether the first
        // pass touched it. `::before` / `::after` pseudo-elements live in
        // `node.before` / `node.after`, *not* in `node.children`, so we
        // must visit them explicitly or a `::before { position: fixed; ... }`
        // would keep the stale first-pass layout.
        if let Some(before_id) = node.before {
            collect_position_fixed_ids(doc, before_id, out, depth + 1);
        }
        let children = node.children.clone();
        for child_id in children {
            collect_position_fixed_ids(doc, child_id, out, depth + 1);
        }
        if let Some(after_id) = node.after {
            collect_position_fixed_ids(doc, after_id, out, depth + 1);
        }
    }
}

/// Walk the DOM for inline `<style>` elements and fold every stylesheet's
/// GCPM context into one. This is the inline-HTML counterpart of the
/// link-loaded context returned by [`parse_html_with_local_resources`].
///
/// Inline `<style>` blocks are normally consumed by stylo for regular
/// CSS, but fulgur's [`crate::gcpm::parser::parse_gcpm`] is only wired to
/// stylesheets fetched via the `NetProvider` path (`<link rel="stylesheet">`
/// / `@import`). Without this helper, `@page`, margin-box, running-element,
/// and counter rules placed directly in an inline `<style>` are lost
/// (fulgur-mq5).
///
/// Synthetic `<style>` elements injected by [`apply_link_media_rewrites`]
/// contain `@import url(...) media;` only, so `parse_gcpm` returns an empty
/// context for them — harmless and intentionally not filtered.
pub fn extract_gcpm_from_inline_styles(doc: &HtmlDocument) -> crate::gcpm::GcpmContext {
    let mut out = crate::gcpm::GcpmContext::default();
    let root = doc.root_element();
    walk_for_inline_styles(doc, root.id, &mut out, 0);
    out
}

fn walk_for_inline_styles(
    doc: &HtmlDocument,
    node_id: usize,
    out: &mut crate::gcpm::GcpmContext,
    depth: usize,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    if let Some(el) = node.element_data()
        && el.name.local.as_ref() == "style"
    {
        let mut css = String::new();
        for &child_id in &node.children {
            if let Some(child) = doc.get_node(child_id)
                && let blitz_dom::node::NodeData::Text(t) = &child.data
            {
                css.push_str(&t.content);
            }
        }
        if !css.is_empty() {
            let ctx = crate::gcpm::parser::parse_gcpm(&css);
            out.extend_from(ctx);
        }
        // `<style>` element children are text nodes only — no need to
        // recurse into them.
        return;
    }
    for &child_id in &node.children {
        walk_for_inline_styles(doc, child_id, out, depth + 1);
    }
}

/// Harvest the column-* properties that blitz/stylo (servo feature) does not
/// expose and return them as a [`crate::column_css::ColumnStyleTable`] keyed by
/// DOM node id.
///
/// This is the Phase A production entry point for the side-table built by
/// [`crate::column_css::build_column_style_table`]: it walks every top-level
/// `<style>` element, concatenates their text content, parses the aggregate
/// as a stylesheet, then hands it back to `build_column_style_table` which
/// also folds inline `style="..."` attributes per element.
///
/// Two traversals are intentional and trivial: the first collects stylesheet
/// bytes (O(nodes + css_bytes)), the second applies the cascade (also
/// O(nodes + css_bytes)). Keeping them separate mirrors the CSS cascade —
/// stylesheets are parsed once, then matched against every node.
pub fn extract_column_style_table(doc: &HtmlDocument) -> crate::column_css::ColumnStyleTable {
    // 1. Concatenate every top-level <style> block's text content.
    let mut css = String::new();
    let root_id = doc.root_element().id;
    walk_for_column_styles(doc, root_id, &mut css, 0);

    // 2. Parse the aggregate as a stylesheet. `parse_stylesheet` silently
    //    drops malformed rules / unsupported selectors — matches the
    //    project-wide "no panic on bad CSS" invariant.
    let rules = crate::column_css::parse_stylesheet(&css);

    // 3. Build the side-table (cascade: stylesheets folded in source order,
    //    inline `style` attributes applied last, per-node).
    crate::column_css::build_column_style_table(doc, &rules)
}

/// Extract the text content of the HTML `<title>` element, if present.
///
/// Returns `None` when no `<title>` element exists or its text content is
/// blank. The returned string is whitespace-trimmed.
pub fn extract_html_title(doc: &HtmlDocument) -> Option<String> {
    use std::ops::Deref;

    fn find_title(doc: &blitz_dom::BaseDocument, node_id: usize, depth: usize) -> Option<usize> {
        if depth >= crate::MAX_DOM_DEPTH {
            return None;
        }
        let node = doc.get_node(node_id)?;
        if let Some(el) = node.element_data() {
            if el.name.local.as_ref() == "title" && el.name.ns == blitz_dom::ns!(html) {
                return Some(node_id);
            }
        }
        for &child_id in &node.children {
            if let Some(found) = find_title(doc, child_id, depth + 1) {
                return Some(found);
            }
        }
        None
    }

    let base = doc.deref();
    let title_id = find_title(base, doc.root_element().id, 0)?;
    let title_node = base.get_node(title_id)?;

    let mut text = String::new();
    for &child_id in &title_node.children {
        if let Some(child) = base.get_node(child_id) {
            if let blitz_dom::node::NodeData::Text(t) = &child.data {
                text.push_str(&t.content);
            }
        }
    }

    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Per-element used page-name endpoints (CSS Page 3 §5.3, fulgur-uebl).
///
/// Each entry is `(start, end)`:
/// - `start` is the page name the element exposes at its **first** in-flow
///   leaf — what the previous sibling compares against to decide whether
///   to break before this element.
/// - `end` is the page name at its **last** in-flow leaf — what the next
///   sibling compares against.
///
/// For a leaf element (no in-flow block children) `start == end`. For a
/// container with descendants whose `page` differs, `start` and `end`
/// can differ — that's how propagated-008-style "page name changes inside
/// a subtree" surface to the parent's sibling comparison.
///
/// `None` means the unnamed/auto page. The table covers every element
/// node in the document so consumers can index unconditionally; non-
/// element nodes are absent.
pub type UsedPageNameTable = std::collections::BTreeMap<usize, (Option<String>, Option<String>)>;

/// Compute the start/end used page-names for every element in `doc`.
///
/// Per CSS Page 3 §5.3, a forced break is induced between two adjacent
/// boxes whose used page-names differ. To detect "page name changes
/// inside a subtree" (e.g. `<div page:b><div page:c><div page:a>...`)
/// we record two values per element:
///
/// - `start`: the used page-name of the element's **first** in-flow
///   block-level descendant — propagated upward so a parent sees the
///   "first leaf's name" of each child subtree. Falls back to own
///   page (when applicable) and then to the inherited space name.
/// - `end`: the used page-name of the element's **last** in-flow
///   block-level descendant. Same fallback chain.
///
/// The fragmenter compares `prev_sibling.end` against `curr_sibling.start`
/// to decide whether to induce a forced break — that's the "adjacent
/// boxes with different page names" rule applied at every level.
///
/// **Block-level gate.** `page` only applies to elements with
/// `display.outside == Block` (CSS Page 3 §5.3 / CSS Fragmentation 3
/// class A break points). Inline-level elements (`<canvas>`, `<img>`
/// with default `display: inline`) ignore their own `page` and inherit
/// from the parent. This is what makes the WPT `page-name-canvas-001`
/// and `page-name-img-002` references work without breaking inline
/// usage.
///
/// **Flex / grid containers.** Children of a flex or grid container are
/// flex / grid items, not class A break points. The spec excludes them
/// from page-name propagation, so when the container is a flex / grid
/// container we treat it as opaque from the parent's perspective:
/// `start` and `end` revert to the container's own / inherited name,
/// independent of the items' names. Sibling comparison among the items
/// is suppressed by the fragmenter (see `fragment_block_subtree`).
///
/// **Out-of-flow children** (`position: absolute / fixed`, `float`) do
/// not contribute to the in-flow first/last child computation — same
/// filter the fragmenter applies when walking siblings.
pub fn compute_used_page_names(
    doc: &BaseDocument,
    table: &crate::column_css::ColumnStyleTable,
) -> UsedPageNameTable {
    let mut out = UsedPageNameTable::new();
    let root_id = doc.root_element().id;
    walk_used_page_names(doc, table, root_id, None, &mut out, 0);
    out
}

fn walk_used_page_names(
    doc: &BaseDocument,
    table: &crate::column_css::ColumnStyleTable,
    node_id: usize,
    parent_space: Option<&str>,
    out: &mut UsedPageNameTable,
    depth: usize,
) -> (Option<String>, Option<String>) {
    let inherit = || parent_space.map(|s| s.to_string());
    if depth >= MAX_DOM_DEPTH {
        return (inherit(), inherit());
    }
    let Some(node) = doc.get_node(node_id) else {
        return (inherit(), inherit());
    };
    if node.element_data().is_none() {
        return (inherit(), inherit());
    }

    // Constraint-space page name for this element's children: own
    // (when applicable) overrides parent's, otherwise inherit.
    let own_named: Option<String> = if is_block_level_outside(node) {
        table
            .get(&node_id)
            .and_then(|props| props.page.as_ref())
            .and_then(|p| match p {
                crate::column_css::PageName::Named(s) => Some(s.clone()),
                crate::column_css::PageName::Auto => None,
            })
    } else {
        None
    };
    let space_name: Option<String> = own_named.clone().or_else(inherit);

    // Walk children, tracking first and last in-flow block-level
    // child's start / end pair. Suppress propagation for flex / grid
    // containers (CSS Page 3 / CSS Fragmentation 3 — children are not
    // class A break points) and for atomic inline boxes
    // (`inline-block` etc. — internal block flow does not paginate
    // independently from the parent line box).
    let suppress_propagation =
        is_flex_or_grid_container(node) || is_atomic_inline_container_node(node);
    let mut first_child_start: Option<Option<String>> = None;
    let mut last_child_end: Option<Option<String>> = None;
    for &child_id in &node.children {
        let (child_start, child_end) =
            walk_used_page_names(doc, table, child_id, space_name.as_deref(), out, depth + 1);
        if suppress_propagation {
            continue;
        }
        let Some(child) = doc.get_node(child_id) else {
            continue;
        };
        if child.element_data().is_none() || !is_block_level_outside(child) {
            continue;
        }
        if child_is_out_of_flow(child) {
            continue;
        }
        // Orthogonal-flow child: treat as atomic from this parent's
        // perspective — its internal page-name propagation does not
        // surface here (CSS Writing Modes 4 §9 / WPT
        // `page-name-orthogonal-writing-001`). The child's *internal*
        // start/end pair is hidden, but the child's own declared
        // `page` (when block-level) must still drive a forced break
        // around the orthogonal box itself — collapsing every
        // orthogonal child to the parent's space would silently
        // suppress that. Resolve the child's own / inherited space
        // name and use that uniformly for both endpoints.
        let (effective_start, effective_end) = if is_orthogonal_to_parent(node, child) {
            let child_space = if is_block_level_outside(child) {
                table
                    .get(&child_id)
                    .and_then(|props| props.page.as_ref())
                    .and_then(|p| match p {
                        crate::column_css::PageName::Named(s) => Some(s.clone()),
                        crate::column_css::PageName::Auto => None,
                    })
                    .or_else(|| space_name.clone())
            } else {
                space_name.clone()
            };
            (child_space.clone(), child_space)
        } else {
            (child_start, child_end)
        };
        if first_child_start.is_none() {
            first_child_start = Some(effective_start);
        }
        last_child_end = Some(effective_end);
    }

    let start = first_child_start.unwrap_or_else(|| space_name.clone());
    let end = last_child_end.unwrap_or_else(|| space_name.clone());
    out.insert(node_id, (start.clone(), end.clone()));
    (start, end)
}

fn is_flex_or_grid_container(node: &Node) -> bool {
    is_flex_or_grid_container_node(node)
}

/// fulgur-uebl: true if `node` establishes a flex / grid formatting
/// context (its inside display is `Flex` or `Grid`). Children of such
/// containers are flex / grid items, which the spec excludes from
/// class A break points — the `page` property does not apply to them
/// and implicit page-name forced breaks should not fire among them.
pub fn is_flex_or_grid_container_node(node: &Node) -> bool {
    use ::style::values::specified::box_::DisplayInside;
    node.primary_styles().is_some_and(|s| {
        let inside = s.clone_display().inside();
        matches!(inside, DisplayInside::Flex | DisplayInside::Grid)
    })
}

/// fulgur-uebl: true if `node` establishes an atomic inline-level box
/// (`display: inline-block`, `inline-flex`, `inline-grid`, …). Atomic
/// inlines participate in their parent's line-box layout but their
/// internal block flow does not paginate independently — break
/// opportunities inside them are not class A break points from the
/// parent's perspective. The implicit page-name forced-break rule
/// must therefore be suppressed when descending through an atomic
/// inline (matching the WPT `page-name-inline-block-001` reference
/// which produces a single page despite differing page names inside).
///
/// **Plain `display: inline` is NOT atomic.** It establishes no BFC
/// of its own; its block-level descendants belong to the surrounding
/// block flow. Suppressing propagation through an inline wrapper
/// would silently hide page-name breaks the spec wants honoured. The
/// inside-display check (`!= Flow`) rejects it.
pub fn is_atomic_inline_container_node(node: &Node) -> bool {
    use ::style::values::specified::box_::{DisplayInside, DisplayOutside};
    node.primary_styles().is_some_and(|s| {
        let display = s.clone_display();
        display.outside() == DisplayOutside::Inline && display.inside() != DisplayInside::Flow
    })
}

/// fulgur-uebl: true if `child` is in an orthogonal flow with respect
/// to `parent` — i.e. their inline / block axes are perpendicular
/// (CSS Writing Modes 4 §7.3). An orthogonal block child is laid out
/// on the perpendicular axis and is sized atomically inside the
/// parent's flow (browsers treat it like an inline-block-shaped
/// atomic for pagination purposes). Page-name forced breaks must
/// therefore be suppressed when crossing the orthogonal boundary,
/// matching the WPT `page-name-orthogonal-writing-001/003` references
/// which produce a single page despite differing page names inside
/// the orthogonal child.
///
/// "Orthogonal" specifically means horizontal-vs-vertical: a parent of
/// `vertical-rl` and a child of `vertical-lr` share the vertical
/// axis (only their block-progression direction differs) and are
/// **not** orthogonal — comparing the raw enum would over-match those
/// pairs.
pub fn is_orthogonal_to_parent(parent: &Node, child: &Node) -> bool {
    let Some(parent_styles) = parent.primary_styles() else {
        return false;
    };
    let Some(child_styles) = child.primary_styles() else {
        return false;
    };
    let parent_mode = parent_styles.get_inherited_box().clone_writing_mode();
    let child_mode = child_styles.get_inherited_box().clone_writing_mode();
    is_vertical_writing_mode(parent_mode) != is_vertical_writing_mode(child_mode)
}

fn is_vertical_writing_mode(
    mode: ::style::properties::longhands::writing_mode::computed_value::T,
) -> bool {
    use ::style::properties::longhands::writing_mode::computed_value::T as W;
    !matches!(mode, W::HorizontalTb)
}

fn child_is_out_of_flow(node: &Node) -> bool {
    use ::style::properties::longhands::position::computed_value::T as Pos;
    node.primary_styles().is_some_and(|s| {
        matches!(s.get_box().clone_position(), Pos::Absolute | Pos::Fixed)
            || s.get_box().clone_float().is_floating()
    })
}

/// fulgur-uebl: true if `node` is floated (`float: left | right | inline-start
/// | inline-end`). Floated children are removed from their containing
/// block's normal flow, so they are not adjacent siblings in the
/// page-name break sense — same logical exclusion as `position:
/// absolute / fixed`. Re-exposed publicly for `pagination_layout` to
/// share the filter.
pub fn node_is_floating(node: &Node) -> bool {
    node.primary_styles()
        .is_some_and(|s| s.get_box().clone_float().is_floating())
}

fn is_block_level_outside(node: &Node) -> bool {
    use ::style::values::specified::box_::DisplayOutside;
    node.primary_styles()
        .is_some_and(|s| s.clone_display().outside() == DisplayOutside::Block)
}

// TODO(phase-b): unify with walk_for_inline_styles — both are "find every
// top-level <style>, concatenate children text, stop recursing past the
// <style>". A shared `fn visit_style_blocks<F>(doc, F)` closure-based visitor
// would collapse them. Kept duplicated for now to avoid risky refactoring
// during v7a Phase A.
fn walk_for_column_styles(doc: &HtmlDocument, node_id: usize, out: &mut String, depth: usize) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    if let Some(el) = node.element_data()
        && el.name.local.as_ref() == "style"
    {
        // Honour `<style media="...">`: skip sheets whose media query
        // excludes the PDF render target. Phase A uses a simple token scan
        // that accepts the attribute when absent, empty, or contains `all`
        // / `print`. `screen` / `speech` / anything else means "don't fold
        // these rules into the column side-table". Full media-query
        // evaluation (size ranges, logical operators, etc.) is deferred —
        // the common author intent of `media="screen"` vs `media="print"`
        // is what we need to get right here.
        if let Some(media) = el.attr(blitz_dom::LocalName::from("media")) {
            let lower = media.to_ascii_lowercase();
            let applies = lower.split(',').any(|tok| {
                let t = tok.trim();
                t.is_empty() || t == "all" || t.contains("print")
            });
            if !applies {
                return;
            }
        }
        for &child_id in &node.children {
            if let Some(child) = doc.get_node(child_id)
                && let blitz_dom::node::NodeData::Text(t) = &child.data
            {
                out.push_str(&t.content);
                // Separate adjacent <style> blocks so the last declaration
                // of one cannot leak into the first selector of the next.
                out.push('\n');
            }
        }
        return;
    }
    for &child_id in &node.children {
        walk_for_column_styles(doc, child_id, out, depth + 1);
    }
}

use crate::MAX_DOM_DEPTH;

/// Walk the DOM tree to find the first element with the given tag name.
/// Returns the node id if found.
fn find_element_by_tag(doc: &HtmlDocument, tag: &str) -> Option<usize> {
    let root = doc.root_element();
    find_element_by_tag_recursive(doc, root.id, tag, 0)
}

fn find_element_by_tag_recursive(
    doc: &HtmlDocument,
    node_id: usize,
    tag: &str,
    depth: usize,
) -> Option<usize> {
    if depth >= MAX_DOM_DEPTH {
        return None;
    }
    let node = doc.get_node(node_id)?;
    if let Some(el) = node.element_data() {
        if el.name.local.as_ref() == tag {
            return Some(node_id);
        }
    }
    for &child_id in &node.children {
        if let Some(found) = find_element_by_tag_recursive(doc, child_id, tag, depth + 1) {
            return Some(found);
        }
    }
    None
}

/// Get the value of an attribute by name from an element.
pub fn get_attr<'a>(elem: &'a blitz_dom::node::ElementData, name: &str) -> Option<&'a str> {
    elem.attrs()
        .iter()
        .find(|a| a.name.local.as_ref() == name)
        .map(|a| a.value.as_ref())
}

/// Concatenate all descendant text under `node_id` into a single String (DFS).
///
/// Used to build a PDF link's `alt_text` (tooltip / accessibility label) from
/// the visible text of an `<a>` element. Returns an empty string if the node
/// has no text descendants or does not exist.
///
/// Word-boundary preservation: a single space is inserted before descending
/// into a child element when that child is `<br>` or a block-level element
/// (block boundaries act as implicit whitespace in the visual rendering).
/// Without this, `<a><div>foo</div><div>bar</div></a>` would collapse to
/// `"foobar"` — meaningless for screen readers. Spaces are deduped by
/// checking the accumulator's trailing char before pushing.
pub fn element_text(doc: &blitz_dom::BaseDocument, node_id: usize) -> String {
    fn is_block_boundary_tag(tag: &str) -> bool {
        matches!(
            tag,
            "br" | "div"
                | "p"
                | "li"
                | "ul"
                | "ol"
                | "blockquote"
                | "section"
                | "article"
                | "header"
                | "footer"
                | "nav"
                | "aside"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "table"
                | "tr"
                | "td"
                | "th"
                | "pre"
                | "figure"
                | "hr"
        )
    }

    fn push_separator(out: &mut String) {
        if !out.is_empty() && !out.ends_with(|c: char| c.is_whitespace()) {
            out.push(' ');
        }
    }

    fn walk(doc: &blitz_dom::BaseDocument, id: usize, depth: usize, out: &mut String) {
        if depth >= MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(id) else {
            return;
        };
        if let blitz_dom::node::NodeData::Text(t) = &node.data {
            out.push_str(&t.content);
        }
        for &child_id in &node.children {
            if let Some(child) = doc.get_node(child_id) {
                if let blitz_dom::node::NodeData::Element(el) = &child.data {
                    if is_block_boundary_tag(el.name.local.as_ref()) {
                        push_separator(out);
                    }
                }
            }
            walk(doc, child_id, depth + 1, out);
        }
    }
    let mut out = String::new();
    walk(doc, node_id, 0, &mut out);
    out
}

/// Extract the parsed `usvg::Tree` from an inline `<svg>` element, if present.
///
/// Blitz parses inline `<svg>` elements during DOM construction (default `svg`
/// feature on `blitz-dom`) and stores the result on `ElementData::image_data()`
/// as `ImageData::Svg(Box<usvg::Tree>)`. This helper hides the `ImageData`
/// enum and the deref-and-clone dance so callers in `convert.rs` don't need
/// to import blitz internals — preserving the adapter isolation rule.
///
/// The clone is required because Blitz only exposes `&Box<Tree>` via `&Node`;
/// it is shallow in practice because `usvg::Tree`'s heavy collections (paths,
/// gradients, fontdb) are `Arc`-shared internally.
pub fn extract_inline_svg_tree(
    elem: &blitz_dom::node::ElementData,
) -> Option<std::sync::Arc<usvg::Tree>> {
    use blitz_dom::node::ImageData;
    match elem.image_data()? {
        ImageData::Svg(tree) => Some(std::sync::Arc::new((**tree).clone())),
        _ => None,
    }
}

/// Inspect a node's computed `content` property and return the first `Image`
/// variant's URL as an owned `String` if the content is a single
/// `url(...)` / `image-set(url(...))` item.
///
/// This works for both pseudo-element nodes (`::before` / `::after`) and normal
/// element nodes — the underlying `primary_styles().get_counters().content`
/// path is the same for both.
///
/// This exists because `blitz-dom` 0.2.4 does not materialize `content: url(...)`
/// into a child image node — the match arm in
/// `blitz-dom/src/layout/construct.rs` for non-`String` ContentItem variants is
/// a TODO. fulgur bypasses that by reading the stylo computed value directly
/// and constructing an `ImageRender` itself (see `convert::build_pseudo_image`
/// and the normal-element `content: url()` path in `convert::convert_node_inner`).
///
/// Scope: only single-item content is matched (per the fulgur-ai3 design scope
/// — multi-item content that mixes text + image is out-of-scope). The URL is
/// returned owned because `primary_styles()` yields a short-lived borrow guard
/// that cannot outlive this function; callers normalize it (e.g. via
/// `convert::extract_asset_name`) before querying `AssetBundle`.
pub fn extract_content_image_url(node: &blitz_dom::Node) -> Option<String> {
    use style::values::generics::counters::{Content, ContentItem};
    let styles = node.primary_styles()?;
    let content = &styles.get_counters().content;
    let item_data = match content {
        Content::Items(item_data) => item_data,
        _ => return None,
    };
    // Only inspect the "main" items (before `alt_start`). Content after
    // `alt_start` is alt-text in CSS Level 3 Content.
    let main = &item_data.items[..item_data.alt_start];
    if main.len() != 1 {
        return None;
    }
    match &main[0] {
        ContentItem::Image(img) => extract_url_from_stylo_image(img).map(|s| s.to_string()),
        _ => None,
    }
}

/// Unwrap a `style::values::computed::image::Image` into a URL string when it
/// is an `Image::Url(ComputedUrl)` — or `image-set(...)` that selects one.
///
/// `image-set(...)` note: stylo tracks `selected_index` on `GenericImageSet`,
/// picking a specific candidate at computed-value time based on device pixel
/// ratio. We follow that index and then recurse once into the selected item's
/// own `Image`. Stylo does not produce nested image-sets in practice, so a
/// shallow recursion is sufficient.
fn extract_url_from_stylo_image(image: &style::values::computed::image::Image) -> Option<&str> {
    use style::servo::url::ComputedUrl;
    use style::values::generics::image::Image as StyloImage;
    match image {
        StyloImage::Url(ComputedUrl::Valid(url)) => Some(url.as_str()),
        StyloImage::Url(ComputedUrl::Invalid(s)) => Some(s.as_str()),
        StyloImage::ImageSet(image_set) => {
            let idx = image_set.selected_index;
            let item = image_set.items.get(idx)?;
            extract_url_from_stylo_image(&item.image)
        }
        _ => None,
    }
}

/// Extract the CSS `vertical-align` value from a node's computed styles and
/// map it to fulgur's `VerticalAlign` enum.
pub fn extract_vertical_align(node: &blitz_dom::Node) -> crate::paragraph::VerticalAlign {
    use crate::paragraph::VerticalAlign;
    let Some(styles) = node.primary_styles() else {
        return VerticalAlign::Baseline;
    };
    let va = styles.clone_vertical_align();
    match va {
        style::values::generics::box_::VerticalAlign::Keyword(kw) => {
            use style::values::generics::box_::VerticalAlignKeyword;
            match kw {
                VerticalAlignKeyword::Baseline => VerticalAlign::Baseline,
                VerticalAlignKeyword::Middle => VerticalAlign::Middle,
                VerticalAlignKeyword::Top => VerticalAlign::Top,
                VerticalAlignKeyword::Bottom => VerticalAlign::Bottom,
                VerticalAlignKeyword::Sub => VerticalAlign::Sub,
                VerticalAlignKeyword::Super => VerticalAlign::Super,
                VerticalAlignKeyword::TextTop => VerticalAlign::TextTop,
                VerticalAlignKeyword::TextBottom => VerticalAlign::TextBottom,
                #[allow(unreachable_patterns)]
                _ => VerticalAlign::Baseline,
            }
        }
        style::values::generics::box_::VerticalAlign::Length(lp) => {
            if let Some(pct) = lp.to_percentage() {
                VerticalAlign::Percent(pct.0)
            } else {
                // `.px()` here is parley/stylo's CSS-px scalar. The Pageable
                // tree is in pt, so convert. For calc() with percentage
                // components the basis-0 resolve silently drops them —
                // acceptable because calc() on vertical-align is rare.
                let px = lp.resolve(style::values::computed::Length::new(0.0)).px();
                VerticalAlign::Length(crate::convert::px_to_pt(px))
            }
        }
    }
}

/// Resolved multicol container properties.
///
/// Only populated when at least one of `column-count` or `column-width` is
/// non-auto, matching the CSS definition of a multicol container.
///
/// ## stylo engine caveat
///
/// `stylo 0.8.0` gates `column-rule-*` and `column-fill` to its gecko engine,
/// and blitz uses the servo engine, so those properties are *not* exposed on
/// `ComputedValues`. A future custom-CSS-parser layer (planned for phase A-4)
/// will read them directly from the stylesheet source. This struct covers the
/// engine-available subset only.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MulticolProps {
    /// `column-count: N` as `Some(N)`, `auto` as `None`.
    pub column_count: Option<u32>,
    /// `column-width` in CSS pixels, or `None` for `auto`.
    pub column_width: Option<f32>,
    /// `column-gap` in CSS pixels. The CSS default `normal` is resolved to
    /// `1em` here per CSS Multi-column Level 1, so callers never see 0 for
    /// an unset property.
    pub column_gap: f32,
}

/// Extract multicol container properties from a node.
///
/// Returns `None` when the node is not a multicol container (i.e. both
/// `column-count` and `column-width` are `auto`).
/// Cheap check: is this node a multicol container?
///
/// Uses stylo's `ComputedValues::is_multicol` (`column-width` or
/// `column-count` non-auto). Prefer this over `extract_multicol_props` when
/// only the bool is needed — it avoids the `clone_column_*` calls used to
/// build the struct.
pub fn is_multicol_container(node: &blitz_dom::Node) -> bool {
    node.primary_styles()
        .map(|s| s.is_multicol())
        .unwrap_or(false)
}

pub fn extract_multicol_props(node: &blitz_dom::Node) -> Option<MulticolProps> {
    use style::values::computed::length::{
        NonNegativeLengthOrAuto, NonNegativeLengthPercentageOrNormal,
    };
    use style::values::generics::column::ColumnCount;

    let styles = node.primary_styles()?;
    if !styles.is_multicol() {
        return None;
    }

    let column_count = match styles.clone_column_count() {
        ColumnCount::Integer(n) => Some(n.0.max(1) as u32),
        ColumnCount::Auto => None,
    };

    let column_width = match styles.clone_column_width() {
        NonNegativeLengthOrAuto::LengthPercentage(l) => Some(l.px()),
        NonNegativeLengthOrAuto::Auto => None,
    };

    let column_gap = match styles.clone_column_gap() {
        NonNegativeLengthPercentageOrNormal::LengthPercentage(lp) => {
            lp.0.to_used_value(style::values::computed::Length::new(0.0).into())
                .to_f32_px()
        }
        // CSS Multi-column Level 1 `§4 column-gap`: used value of `normal`
        // is `1em`. Resolve against the element's computed font-size so a
        // column-gap-less multicol still has visual separation.
        NonNegativeLengthPercentageOrNormal::Normal => styles.clone_font_size().used_size().px(),
    };

    Some(MulticolProps {
        column_count,
        column_width,
        column_gap,
    })
}

/// Returns true if this node carries `column-span: all`.
///
/// Used by the multicol converter to slice a multicol container into
/// alternating column-group / span-all segments.
pub fn has_column_span_all(node: &blitz_dom::Node) -> bool {
    let Some(styles) = node.primary_styles() else {
        return false;
    };
    matches!(
        styles.clone_column_span(),
        style::properties::longhands::column_span::computed_value::T::All
    )
}

fn make_qual_name(local: &str) -> blitz_dom::QualName {
    blitz_dom::QualName::new(
        None,
        blitz_dom::ns!(html),
        blitz_dom::LocalName::from(local),
    )
}

/// Create a `<style>` element with the given CSS text, attach it to the DOM,
/// and register it with Stylo. Returns the style node id.
///
/// If `insert_before` is `Some(sibling_id)`, the style element is inserted before
/// that sibling. Otherwise it is appended to `parent_id`.
fn inject_style_node(
    doc: &mut HtmlDocument,
    parent_id: usize,
    css: &str,
    insert_before: Option<usize>,
) -> usize {
    let style_id = {
        let mut mutator = doc.mutate();
        let style_id = mutator.create_element(make_qual_name("style"), vec![]);
        let text_id = mutator.create_text_node(css);
        if let Some(sibling) = insert_before {
            mutator.insert_nodes_before(sibling, &[style_id]);
        } else {
            mutator.append_children(parent_id, &[style_id]);
        }
        mutator.append_children(style_id, &[text_id]);
        style_id
    };
    doc.upsert_stylesheet_for_node(style_id);
    style_id
}

/// Injects CSS text as a `<style>` element into the document's `<head>`.
pub struct InjectCssPass {
    pub css: String,
}

impl DomPass for InjectCssPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.css.is_empty() {
            return;
        }

        // Find or create <head>
        let head_id = match find_element_by_tag(doc, "head") {
            Some(id) => id,
            None => {
                // Create <head> as first child of <html>
                let html_id = doc.root_element().id;
                let mut mutator = doc.mutate();
                let head_id = mutator.create_element(make_qual_name("head"), vec![]);
                let children = mutator.child_ids(html_id);
                if let Some(&first_child) = children.first() {
                    mutator.insert_nodes_before(first_child, &[head_id]);
                } else {
                    mutator.append_children(html_id, &[head_id]);
                }
                drop(mutator);
                head_id
            }
        };

        inject_style_node(doc, head_id, &self.css, None);
    }
}

use crate::gcpm::bookmark::{BookmarkLevel, BookmarkMapping};
use crate::gcpm::counter::{CounterState, format_counter, format_counter_chain};
use crate::gcpm::running::{RunningElementStore, serialize_node};
use crate::gcpm::string_set::{StringSetEntry, StringSetStore, extract_text_content};
use crate::gcpm::{
    ContentCounterMapping, ContentItem, CounterMapping, CounterOp, ParsedSelector, PseudoElement,
    RunningMapping, StringSetMapping, StringSetValue,
};
use std::cell::RefCell;
use std::collections::BTreeMap;

/// Returns true for elements that should never be walked for GCPM detection
/// (head, script, style, etc.) — they contain no user-visible content.
fn is_non_visual_tag(tag: &str) -> bool {
    matches!(
        tag,
        "head" | "script" | "style" | "link" | "meta" | "title" | "noscript"
    )
}

/// Check whether a `ParsedSelector` (simple class/id/tag selector) matches the given element.
fn selector_matches(selector: &ParsedSelector, elem: &blitz_dom::node::ElementData) -> bool {
    match selector {
        ParsedSelector::Class(name) => get_attr(elem, "class")
            .is_some_and(|cls| cls.split_whitespace().any(|c| c == name.as_str())),
        ParsedSelector::Id(name) => get_attr(elem, "id") == Some(name.as_str()),
        ParsedSelector::Tag(name) => elem.name.local.as_ref().eq_ignore_ascii_case(name),
    }
}

/// Extracts running elements from the DOM and stores their serialized HTML.
pub struct RunningElementPass {
    mappings: Vec<RunningMapping>,
    store: RefCell<RunningElementStore>,
}

impl RunningElementPass {
    pub fn new(mappings: Vec<RunningMapping>) -> Self {
        Self {
            mappings,
            store: RefCell::new(RunningElementStore::new()),
        }
    }

    pub fn into_running_store(self) -> RunningElementStore {
        self.store.into_inner()
    }
}

impl DomPass for RunningElementPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.mappings.is_empty() {
            return;
        }
        let root = doc.root_element();
        let root_id = root.id;
        self.walk_tree(doc, root_id, 0);
    }
}

impl RunningElementPass {
    fn walk_tree(&self, doc: &HtmlDocument, node_id: usize, depth: usize) {
        if depth >= MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };

        if let Some(elem) = node.element_data() {
            if is_non_visual_tag(elem.name.local.as_ref()) {
                return;
            }
            if let Some(running_name) = self.find_running_name(elem) {
                let html = serialize_node(doc, node_id);
                self.store
                    .borrow_mut()
                    .register(node_id, running_name, html);
                // Running elements are removed from flow — don't recurse.
                return;
            }
        }

        for &child_id in &node.children {
            self.walk_tree(doc, child_id, depth + 1);
        }
    }

    fn find_running_name(&self, elem: &blitz_dom::node::ElementData) -> Option<String> {
        self.mappings
            .iter()
            .find(|m| selector_matches(&m.parsed, elem))
            .map(|m| m.running_name.clone())
    }
}

/// Extracts string-set values from the DOM by walking the tree and resolving text content.
pub struct StringSetPass {
    mappings: Vec<StringSetMapping>,
    store: RefCell<StringSetStore>,
    /// Running map `name -> latest value` updated as the DOM walk
    /// encounters string-set assignments. Snapshotted into
    /// `node_snapshots` at every visited element so `BookmarkPass` can
    /// resolve `string(name)` at the element's DOM position.
    running: RefCell<BTreeMap<String, String>>,
    /// Per-node snapshot of `running` taken at every visited element
    /// after that element's own string-set assignment (if any) has been
    /// applied. Only populated when `record_node_snapshots` is enabled.
    /// Empty otherwise, including when `apply` did not visit any nodes
    /// (e.g. `mappings` is empty).
    node_snapshots: RefCell<BTreeMap<usize, BTreeMap<String, String>>>,
    /// Gate for `node_snapshots`. Mirrors `CounterPass`. Off by default
    /// so renders that do not emit bookmarks skip the per-element clone.
    record_node_snapshots: bool,
}

impl StringSetPass {
    pub fn new(mappings: Vec<StringSetMapping>) -> Self {
        Self {
            mappings,
            store: RefCell::new(StringSetStore::new()),
            running: RefCell::new(BTreeMap::new()),
            node_snapshots: RefCell::new(BTreeMap::new()),
            record_node_snapshots: false,
        }
    }

    /// Enable per-node string-set snapshot recording. Required when a
    /// downstream `BookmarkPass` resolves `string(name)` inside
    /// `bookmark-label` (fulgur-70c). Off by default to avoid the
    /// per-element clone overhead on renders that don't emit bookmarks.
    pub fn with_snapshot_recording(mut self) -> Self {
        self.record_node_snapshots = true;
        self
    }

    pub fn into_store(self) -> StringSetStore {
        self.store.into_inner()
    }

    /// Take ownership of the per-node string-set snapshot map. Mirrors
    /// `CounterPass::take_node_snapshots`. Empty if snapshot recording
    /// was not enabled, if `apply` did not visit any nodes, or if this
    /// method has already been called once — call before `into_store`.
    pub fn take_node_snapshots(&self) -> BTreeMap<usize, BTreeMap<String, String>> {
        std::mem::take(&mut *self.node_snapshots.borrow_mut())
    }
}

impl DomPass for StringSetPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.mappings.is_empty() {
            return;
        }
        let root = doc.root_element();
        let root_id = root.id;
        self.walk_tree(doc, root_id, 0);
    }
}

impl StringSetPass {
    fn walk_tree(&self, doc: &HtmlDocument, node_id: usize, depth: usize) {
        if depth >= MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };

        if let Some(elem) = node.element_data() {
            if is_non_visual_tag(elem.name.local.as_ref()) {
                return;
            }
            // Fold every matching mapping in cascade order. A single
            // element may match multiple `string-set` rules from different
            // selectors (and they may set distinct named strings); the
            // previous "first-match wins" lookup silently dropped the
            // remainder, which is observable from `bookmark-label:
            // string(...)` once snapshots are wired in.
            for mapping in self
                .mappings
                .iter()
                .filter(|m| selector_matches(&m.parsed, elem))
            {
                let value = resolve_string_set_values(doc, node_id, elem, &mapping.values);
                self.running
                    .borrow_mut()
                    .insert(mapping.name.clone(), value.clone());
                self.store.borrow_mut().push(StringSetEntry {
                    name: mapping.name.clone(),
                    value,
                    node_id,
                });
            }
            // Snapshot the running map AFTER this element's own
            // assignment is folded in — bookmark-label on the same
            // element should observe its own string-set value.
            // Mirrors `CounterPass`'s per-node snapshot timing. Gated
            // on `record_node_snapshots` so non-bookmark renders skip
            // the clone (fulgur-70c).
            if self.record_node_snapshots {
                self.node_snapshots
                    .borrow_mut()
                    .insert(node_id, self.running.borrow().clone());
            }
        }

        // string-set targets stay in document flow — always recurse into children.
        for &child_id in &node.children {
            self.walk_tree(doc, child_id, depth + 1);
        }
    }
}

/// Walks the DOM applying counter-reset/increment/set operations and resolves
/// `content: counter()` in ::before/::after pseudo-elements by generating override CSS.
pub struct CounterPass {
    counter_mappings: Vec<CounterMapping>,
    content_mappings: Vec<ContentCounterMapping>,
    state: RefCell<CounterState>,
    generated_css: RefCell<String>,
    counter_id: RefCell<usize>,
    /// Counter ops keyed by node_id, for later use in Pageable markers.
    ops_by_node: RefCell<Vec<(usize, Vec<CounterOp>)>>,
    /// Counter-state snapshot taken at each visited element after the
    /// element's own `counter-reset` / `counter-increment` / `counter-set`
    /// operations have been applied (Phase 2). Consumed by `BookmarkPass`
    /// to resolve `counter()` inside `bookmark-label`. Only populated when
    /// `record_node_snapshots` is enabled (the default is off, since most
    /// renders do not use `bookmark-label`).
    node_snapshots: RefCell<BTreeMap<usize, BTreeMap<String, Vec<i32>>>>,
    /// Gate for `node_snapshots`: the per-element snapshot clones the
    /// counter map and is only useful when a downstream `BookmarkPass`
    /// will consume it. Engine flips this on via
    /// [`with_snapshot_recording`] when bookmarks are emitted.
    record_node_snapshots: bool,
    /// Pass-2 cross-reference table built at the end of pass 1. When
    /// present, `target-counter()` / `target-counters()` / `target-text()`
    /// inside `::before` / `::after` `content` resolve against this map.
    /// When absent (pass 1, or single-pass renders without target refs),
    /// those variants substitute short placeholder strings so pass 1 has
    /// *some* width to lay out — width is a rough approximation only, see
    /// [`resolve_content`] and the fulgur-38y2 fixed-point follow-up.
    /// Set exactly once via [`with_anchor_map`] at construction; never
    /// mutated during `apply`, so no interior mutability is needed.
    anchor_map: Option<crate::gcpm::target_ref::AnchorMap>,
}

impl CounterPass {
    pub fn new(
        counter_mappings: Vec<CounterMapping>,
        content_mappings: Vec<ContentCounterMapping>,
    ) -> Self {
        Self {
            counter_mappings,
            content_mappings,
            state: RefCell::new(CounterState::new()),
            generated_css: RefCell::new(String::new()),
            counter_id: RefCell::new(0),
            ops_by_node: RefCell::new(Vec::new()),
            node_snapshots: RefCell::new(BTreeMap::new()),
            record_node_snapshots: false,
            anchor_map: None,
        }
    }

    /// Supply the pass-2 cross-reference table built from pass-1 output.
    /// When set, `target-counter()` / `target-counters()` / `target-text()`
    /// inside `::before` / `::after` `content` resolve against this map.
    /// When unset (pass 1, or single-pass renders without target refs),
    /// those variants substitute short placeholder strings; the width is
    /// a rough approximation only — line breaks may shift between passes.
    pub fn with_anchor_map(mut self, map: crate::gcpm::target_ref::AnchorMap) -> Self {
        self.anchor_map = Some(map);
        self
    }

    /// Enable per-node counter snapshot recording. Required when a
    /// downstream `BookmarkPass` needs to resolve `counter()` inside
    /// `bookmark-label` (fulgur-70c). Off by default to avoid the
    /// O(|elements| × |counters|) clone cost on renders that do not
    /// consume bookmarks.
    pub fn with_snapshot_recording(mut self) -> Self {
        self.record_node_snapshots = true;
        self
    }

    pub fn generated_css(&self) -> String {
        self.generated_css.borrow().clone()
    }

    /// Take ownership of the per-node counter snapshots collected during
    /// `apply`. Each entry maps a DOM node id to its counter state at the
    /// point right after the element's own `counter-reset` /
    /// `counter-increment` / `counter-set` operations have been applied —
    /// the same timing CSS GCPM specifies for `counter()` inside
    /// `bookmark-label`. Empty if `apply` was never called, if there were
    /// no counter / content mappings (the early-return path), or if
    /// snapshot recording was not enabled via [`with_snapshot_recording`].
    /// Subsequent calls return an empty map (the snapshot is moved out).
    pub fn take_node_snapshots(&self) -> BTreeMap<usize, BTreeMap<String, Vec<i32>>> {
        std::mem::take(&mut *self.node_snapshots.borrow_mut())
    }

    /// Consume self and return (ops_by_node for Pageable markers, generated CSS for body).
    pub fn into_parts(self) -> (Vec<(usize, Vec<CounterOp>)>, String) {
        (
            self.ops_by_node.into_inner(),
            self.generated_css.into_inner(),
        )
    }
}

impl DomPass for CounterPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.counter_mappings.is_empty() && self.content_mappings.is_empty() {
            return;
        }
        let root = doc.root_element();
        let root_id = root.id;
        self.walk_tree(doc, root_id, 0);
    }
}

impl CounterPass {
    fn walk_tree(&self, doc: &mut HtmlDocument, node_id: usize, depth: usize) {
        if depth >= MAX_DOM_DEPTH {
            return;
        }
        // Phase 1: Read element data immutably to collect matched operations
        // and matched content mapping indices. We must drop the immutable borrow
        // before calling doc.get_node_mut().
        let phase1 = {
            let Some(node) = doc.get_node(node_id) else {
                return;
            };
            let Some(elem) = node.element_data() else {
                // Not an element — just recurse into children
                let children: Vec<usize> = node.children.clone();
                for child_id in children {
                    self.walk_tree(doc, child_id, depth + 1);
                }
                return;
            };

            if is_non_visual_tag(elem.name.local.as_ref()) {
                return;
            }

            // Collect counter operations
            let mut matched_ops = Vec::new();
            for mapping in &self.counter_mappings {
                if selector_matches(&mapping.parsed, elem) {
                    matched_ops.extend(mapping.ops.clone());
                }
            }

            // Collect indices of matching content mappings (resolve values
            // after counter state is updated in phase 2)
            let mut matched_content_indices: Vec<usize> = Vec::new();
            for (i, mapping) in self.content_mappings.iter().enumerate() {
                if selector_matches(&mapping.parsed, elem) {
                    matched_content_indices.push(i);
                }
            }

            Some((matched_ops, matched_content_indices))
        };
        // immutable borrow of doc is now dropped

        let Some((matched_ops, matched_content_indices)) = phase1 else {
            return;
        };

        // Phase 2: Apply counter state changes (no doc borrow needed).
        // CSS Lists 3 §4.5: a counter instance created by `counter-reset`
        // is scoped to the originating element + its descendants +
        // following siblings. We model that by tagging the instance with
        // the originating element's *DOM parent*, then popping all
        // instances tagged with `node_id` in a post-order `leave_element`
        // call (see end of this function). Increment/set never push a
        // new instance, so their parent_id is unused — we still pass it
        // for API symmetry.
        if !matched_ops.is_empty() {
            let parent_id = doc
                .get_node(node_id)
                .and_then(|n| n.parent)
                .unwrap_or(crate::gcpm::counter::COUNTER_ROOT_PARENT);
            let mut state = self.state.borrow_mut();
            for op in &matched_ops {
                match op {
                    CounterOp::Reset { name, value } => {
                        state.reset_in_scope(name, *value, parent_id)
                    }
                    CounterOp::Increment { name, value } => {
                        state.increment_in_scope(name, *value, parent_id)
                    }
                    CounterOp::Set { name, value } => state.set_in_scope(name, *value, parent_id),
                }
            }
            drop(state);
            self.ops_by_node.borrow_mut().push((node_id, matched_ops));
        }

        // Snapshot the counter state at this element's "after own ops, before
        // children" position. This is the value bookmark-label sees (matches
        // `::before` content resolution timing). We snapshot unconditionally
        // for visited elements — even ones with no own counter ops — so that
        // `BookmarkPass` can still resolve inherited counter values for any
        // element that authors target with `bookmark-label`. Gated on
        // `record_node_snapshots` so renders that don't emit bookmarks pay
        // nothing for this clone (fulgur-70c).
        if self.record_node_snapshots {
            self.node_snapshots
                .borrow_mut()
                .insert(node_id, self.state.borrow().chain_snapshot());
        }

        // Phase 3: Split ::before (resolve now) and ::after (resolve after children).
        // CSS spec: ::before is a first child, ::after is a last child, so
        // ::after must see counter state changes from descendants.
        let (before_indices, after_indices): (Vec<usize>, Vec<usize>) = matched_content_indices
            .into_iter()
            .partition(|&idx| self.content_mappings[idx].pseudo == PseudoElement::Before);

        // Allocate a cid if any content mappings matched (needed for both phases)
        let attr_value = if !before_indices.is_empty() || !after_indices.is_empty() {
            let mut id = self.counter_id.borrow_mut();
            let v = format!("{}", *id);
            *id += 1;
            drop(id);

            // Set data attribute once
            let qual = make_qual_name("data-fulgur-cid");
            if let Some(node_mut) = doc.get_node_mut(node_id) {
                if let Some(elem_mut) = node_mut.element_data_mut() {
                    elem_mut.attrs.set(qual, &v);
                }
            }
            Some(v)
        } else {
            None
        };

        // Resolve ::before now (before child traversal)
        if let Some(ref cid) = attr_value {
            use std::fmt::Write;
            let element_href = doc
                .get_node(node_id)
                .and_then(|n| n.element_data())
                .and_then(|e| get_attr(e, "href").map(str::to_owned));
            let mut css = self.generated_css.borrow_mut();
            for idx in &before_indices {
                let mapping = &self.content_mappings[*idx];
                let resolved = self.resolve_content(&mapping.content, element_href.as_deref());
                let _ = write!(
                    css,
                    "[data-fulgur-cid=\"{}\"]::before{{content:\"{}\"}}",
                    cid,
                    css_escape_string(&resolved)
                );
            }
        }

        // Recurse into children (re-read children since we may have mutated doc)
        let children: Vec<usize> = doc
            .get_node(node_id)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        for child_id in children {
            self.walk_tree(doc, child_id, depth + 1);
        }

        // Resolve ::after now (after child traversal, sees descendant counter changes)
        if let Some(ref cid) = attr_value {
            use std::fmt::Write;
            let element_href = doc
                .get_node(node_id)
                .and_then(|n| n.element_data())
                .and_then(|e| get_attr(e, "href").map(str::to_owned));
            let mut css = self.generated_css.borrow_mut();
            for idx in &after_indices {
                let mapping = &self.content_mappings[*idx];
                let resolved = self.resolve_content(&mapping.content, element_href.as_deref());
                let _ = write!(
                    css,
                    "[data-fulgur-cid=\"{}\"]::after{{content:\"{}\"}}",
                    cid,
                    css_escape_string(&resolved)
                );
            }
        }

        // Post-order: pop instances created by this node's children. Per
        // CSS Lists 3 §4.5, an instance's scope is the originating
        // element + descendants + following siblings. Instances pushed
        // by children of `node_id` are scoped to `node_id`'s subtree, so
        // they must die now (we are returning from `node_id`). Placed
        // after the ::after resolution so descendant counter state is
        // still visible to `::after`'s `counter()` lookup.
        self.state.borrow_mut().leave_element(node_id);
    }

    fn resolve_content(&self, items: &[ContentItem], element_href: Option<&str>) -> String {
        let state = self.state.borrow();
        let mut out = String::new();
        for item in items {
            match item {
                ContentItem::String(s) => out.push_str(s),
                ContentItem::Counter { name, style } => {
                    let value = state.get(name);
                    out.push_str(&format_counter(value, *style));
                }
                ContentItem::Counters {
                    name,
                    separator,
                    style,
                } => {
                    let chain = state.chain(name);
                    out.push_str(&format_counter_chain(&chain, separator, *style));
                }
                // Pass-1 placeholders for the three target-* arms below.
                // Width is a rough approximation only: `"00"` is a constant
                // 2 chars regardless of the final counter's digit count or
                // chain depth, and `' '` is a constant 1 space regardless
                // of the resolved label length. Line breaks may therefore
                // shift between pass 1 and pass 2.
                ContentItem::TargetCounter {
                    url_attr,
                    counter_name,
                    style,
                } => {
                    if url_attr != "href" {
                        continue;
                    }
                    let href = element_href.unwrap_or("");
                    match self.anchor_map.as_ref() {
                        Some(map) => {
                            out.push_str(&crate::gcpm::target_ref::resolve_target_counter(
                                href,
                                counter_name,
                                *style,
                                map,
                            ))
                        }
                        None => out.push_str("00"),
                    }
                }
                ContentItem::TargetCounters {
                    url_attr,
                    counter_name,
                    separator,
                    style,
                } => {
                    if url_attr != "href" {
                        continue;
                    }
                    let href = element_href.unwrap_or("");
                    match self.anchor_map.as_ref() {
                        Some(map) => {
                            out.push_str(&crate::gcpm::target_ref::resolve_target_counters(
                                href,
                                counter_name,
                                separator,
                                *style,
                                map,
                            ))
                        }
                        None => out.push_str("00"),
                    }
                }
                ContentItem::TargetText { url_attr } => {
                    if url_attr != "href" {
                        continue;
                    }
                    let href = element_href.unwrap_or("");
                    match self.anchor_map.as_ref() {
                        Some(map) => {
                            out.push_str(&crate::gcpm::target_ref::resolve_target_text(href, map))
                        }
                        None => out.push(' '),
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Resolved bookmark attributes for a single DOM element, as produced by
/// [`BookmarkPass`]. Consumed by the PDF outline emitter (`render.rs`) to
/// populate the document's bookmark tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkInfo {
    /// 1-based outline depth (1 is top-level).
    pub level: u8,
    /// Resolved label text for the PDF outline entry.
    pub label: String,
}

/// Walks the DOM and, for each element, applies the cascade of
/// [`BookmarkMapping`] rules to decide whether a PDF outline entry should
/// be emitted for that element.
///
/// # Cascade semantics
///
/// Mappings are iterated in the order they were collected from the CSS
/// stylesheet(s). For each matching mapping, the pass overlays its
/// `level` / `label` fields onto a per-node accumulator — later matches
/// overwrite earlier ones per field. This mirrors CSS property cascade
/// ("last declaration wins") while letting an author split a selector's
/// level and label into separate rules.
///
/// # Suppression
///
/// If the final resolved level is [`BookmarkLevel::None_`], no entry is
/// emitted for the node. This is the spec-prescribed way for an author
/// stylesheet to override the fulgur UA default that bookmarks every
/// `h1`–`h6`.
///
/// # Label fallback
///
/// When a mapping sets `bookmark-level` but leaves `bookmark-label`
/// unset, the label falls back to the element's rendered text content
/// (`content()` equivalent, matching GCPM's "unset label ⇒ use the
/// element's text" rule).
pub struct BookmarkPass {
    mappings: Vec<BookmarkMapping>,
    results: RefCell<Vec<(usize, BookmarkInfo)>>,
    /// Per-node counter-state snapshots produced by `CounterPass`.
    /// Each value is the full nesting chain (outer-to-inner) per CSS
    /// Lists 3 §4.5; `counter()` takes the innermost (`last()`) value.
    /// Empty map ⇒ `counter()` resolves to 0 (CSS spec: undefined
    /// counter is 0).
    counter_snapshots: BTreeMap<usize, BTreeMap<String, Vec<i32>>>,
    /// Per-node named-string snapshots produced by `StringSetPass`.
    /// Empty map ⇒ `string()` resolves to "" (no string ever set).
    string_snapshots: BTreeMap<usize, BTreeMap<String, String>>,
}

impl BookmarkPass {
    /// Construct a pass without snapshot connections. `counter()` and
    /// `string()` inside `bookmark-label` will fall back to their
    /// CSS-spec defaults (0 and "" respectively). Convenient for tests
    /// and for the ergonomic case where no GCPM counter / string
    /// machinery is in play.
    pub fn new(mappings: Vec<BookmarkMapping>) -> Self {
        Self::new_with_snapshots(mappings, BTreeMap::new(), BTreeMap::new())
    }

    /// Construct a pass that resolves `counter(name)` and `string(name)`
    /// inside `bookmark-label` against snapshots harvested from
    /// `CounterPass` / `StringSetPass` (see their `take_node_snapshots`
    /// methods). The snapshots are keyed by Blitz DOM node id and
    /// represent the running state at that element's DOM position
    /// (after own `counter-*` / `string-set` rules, before children) —
    /// matching CSS GCPM resolution timing for `bookmark-label`.
    pub fn new_with_snapshots(
        mappings: Vec<BookmarkMapping>,
        counter_snapshots: BTreeMap<usize, BTreeMap<String, Vec<i32>>>,
        string_snapshots: BTreeMap<usize, BTreeMap<String, String>>,
    ) -> Self {
        Self {
            mappings,
            results: RefCell::new(Vec::new()),
            counter_snapshots,
            string_snapshots,
        }
    }

    pub fn into_results(self) -> Vec<(usize, BookmarkInfo)> {
        self.results.into_inner()
    }
}

impl DomPass for BookmarkPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.mappings.is_empty() {
            return;
        }
        let root = doc.root_element();
        let root_id = root.id;
        self.walk_tree(doc, root_id, 0);
    }
}

impl BookmarkPass {
    fn walk_tree(&self, doc: &HtmlDocument, node_id: usize, depth: usize) {
        if depth >= MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };

        if let Some(elem) = node.element_data() {
            if is_non_visual_tag(elem.name.local.as_ref()) {
                return;
            }
            self.resolve_node(doc, node_id, elem);
        }

        for &child_id in &node.children {
            self.walk_tree(doc, child_id, depth + 1);
        }
    }

    /// Apply the cascade of matching mappings to a single element and,
    /// if the resolution yields a visible bookmark, record it.
    fn resolve_node(
        &self,
        doc: &HtmlDocument,
        node_id: usize,
        elem: &blitz_dom::node::ElementData,
    ) {
        // Overlay accumulator — iterate forward; each matching mapping
        // overwrites the fields it sets.
        let mut level: Option<BookmarkLevel> = None;
        let mut label: Option<Vec<ContentItem>> = None;
        let mut any_match = false;
        for mapping in &self.mappings {
            if selector_matches(&mapping.selector, elem) {
                any_match = true;
                if let Some(l) = &mapping.level {
                    level = Some(l.clone());
                }
                if let Some(lbl) = &mapping.label {
                    label = Some(lbl.clone());
                }
            }
        }
        if !any_match {
            return;
        }

        // `bookmark-level: none` suppresses the entry outright, regardless
        // of label.
        let level_int = match level {
            Some(BookmarkLevel::Integer(n)) => n,
            Some(BookmarkLevel::None_) => return,
            // A rule that only set `bookmark-label` without a level is
            // effectively inert — GCPM requires a level for an outline
            // entry. Skip silently.
            None => return,
        };

        // Label fallback: if no `bookmark-label` was declared, use the
        // element's text content (equivalent to `content()`).
        let resolved_label = match label {
            Some(items) => resolve_label(
                &items,
                doc,
                node_id,
                elem,
                self.counter_snapshots.get(&node_id),
                self.string_snapshots.get(&node_id),
            ),
            None => extract_text_content(doc, node_id),
        };

        // Skip entries with an empty resolved label. Emitting an outline
        // node with an empty title is observable but carries no useful
        // information — this matches the previous hardcoded h1-h6 path
        // which bailed out when `extract_text_content` was empty.
        if resolved_label.is_empty() {
            return;
        }

        self.results.borrow_mut().push((
            node_id,
            BookmarkInfo {
                level: level_int,
                label: resolved_label,
            },
        ));
    }
}

/// Resolve a `bookmark-label` content list against a concrete DOM element
/// into a flat string.
///
/// Supported items:
/// - [`ContentItem::String`] — literal text.
/// - [`ContentItem::ContentText`] — the element's normalized text
///   content (same extraction as `string-set: … content(text)`).
/// - [`ContentItem::Attr`] — the named HTML attribute, or empty if
///   absent.
/// - [`ContentItem::Counter`] — resolved against `counter_snapshot`,
///   the per-node counter map produced by `CounterPass`. Missing
///   snapshot or missing counter ⇒ 0 (CSS spec: undefined counter is 0).
/// - [`ContentItem::StringRef`] — resolved against `string_snapshot`,
///   the per-node named-string map produced by `StringSetPass`. Since
///   bookmark-label resolves at a single DOM position rather than over a
///   page, all `StringPolicy` variants reduce to "the latest value seen
///   at this point in document order" — equivalent to a direct lookup.
///   Missing snapshot or missing name ⇒ "".
///
/// Skipped (no-op) items, tracked in beads `fulgur-yfx`:
/// - [`ContentItem::ContentBefore`] / [`ContentItem::ContentAfter`] —
///   pseudo-element text extraction is not yet wired in.
/// - [`ContentItem::Element`] — running-element references are
///   margin-box constructs that don't resolve in a bookmark-label
///   context.
/// - [`ContentItem::Leader`] — produces a fill character at render
///   time, not a plain label string; emit nothing here.
fn resolve_label(
    items: &[ContentItem],
    doc: &HtmlDocument,
    node_id: usize,
    elem: &blitz_dom::node::ElementData,
    counter_snapshot: Option<&BTreeMap<String, Vec<i32>>>,
    string_snapshot: Option<&BTreeMap<String, String>>,
) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => out.push_str(s),
            ContentItem::ContentText => {
                out.push_str(&extract_text_content(doc, node_id));
            }
            ContentItem::Attr(name) => {
                if let Some(v) = get_attr(elem, name) {
                    out.push_str(v);
                }
            }
            ContentItem::Counter { name, style } => {
                let value = counter_snapshot
                    .and_then(|s| s.get(name))
                    .and_then(|chain| chain.last().copied())
                    .unwrap_or(0);
                out.push_str(&format_counter(value, *style));
            }
            ContentItem::Counters {
                name,
                separator,
                style,
            } => {
                if let Some(chain) = counter_snapshot.and_then(|s| s.get(name)) {
                    out.push_str(&format_counter_chain(chain, separator, *style));
                }
            }
            ContentItem::StringRef { name, .. } => {
                if let Some(v) = string_snapshot.and_then(|s| s.get(name)) {
                    out.push_str(v);
                }
            }
            ContentItem::ContentBefore
            | ContentItem::ContentAfter
            | ContentItem::Element { .. }
            | ContentItem::Leader { .. }
            | ContentItem::TargetCounter { .. }
            | ContentItem::TargetCounters { .. }
            | ContentItem::TargetText { .. } => {}
        }
    }
    out
}

fn css_escape_string(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\a "),
            '\r' => out.push_str("\\d "),
            '\0' => out.push_str("\\0 "),
            c if c.is_control() => {
                out.push_str(&format!("\\{:x} ", c as u32));
            }
            _ => out.push(ch),
        }
    }
    out
}

fn resolve_string_set_values(
    doc: &HtmlDocument,
    node_id: usize,
    elem: &blitz_dom::node::ElementData,
    values: &[StringSetValue],
) -> String {
    let mut out = String::new();
    for val in values {
        match val {
            StringSetValue::ContentText => {
                out.push_str(&extract_text_content(doc, node_id));
            }
            // content(before)/content(after) require pseudo-element computed styles.
            StringSetValue::ContentBefore | StringSetValue::ContentAfter => {}
            StringSetValue::Attr(attr_name) => {
                if let Some(v) = get_attr(elem, attr_name) {
                    out.push_str(v);
                }
            }
            StringSetValue::Literal(s) => out.push_str(s),
        }
    }
    out
}

// ─── transform support ────────────────────────────────────

use crate::draw_primitives::{Affine2D, Point2};

/// Read the computed `transform` and `transform-origin` from `styles` and
/// fold the `TransformOperation` list into a single pre-resolved affine
/// matrix.
///
/// Percentages in `translate` and `transform-origin` are resolved against
/// the caller-supplied `border_box_width` / `border_box_height` (Taffy's
/// final layout size — CSS `transform` does not affect layout, so this is
/// correct).
///
/// Returns `None` if the transform is absent, folds to identity, or
/// produces a non-finite matrix. Callers use this to suppress wrapper
/// construction in the fast path.
///
/// 3D operations (`translate3d`, `rotate3d`, `scale3d`, `matrix3d`,
/// `perspective`, etc.) are treated as identity with a `log::warn!`.
/// fulgur is a 2D PDF renderer.
pub(crate) fn compute_transform(
    styles: &style::properties::ComputedValues,
    border_box_width: f32,
    border_box_height: f32,
) -> Option<(Affine2D, Point2)> {
    use style::values::computed::Length;

    // Fast path: most DOM nodes have no transform. Reading the
    // `OwnedSlice` through `get_box()` avoids cloning it for the empty
    // case, and lets the non-empty path borrow instead of clone.
    let transform = &styles.get_box().transform.0;
    if transform.is_empty() {
        return None;
    }

    let mut m = Affine2D::IDENTITY;
    for op in transform.iter() {
        m = m * op_to_matrix(op, border_box_width, border_box_height);
    }

    if !m.a.is_finite()
        || !m.b.is_finite()
        || !m.c.is_finite()
        || !m.d.is_finite()
        || !m.e.is_finite()
        || !m.f.is_finite()
    {
        log::warn!("transform produced non-finite matrix; falling back to identity");
        return None;
    }
    if m.is_identity() {
        return None;
    }

    let origin = styles.clone_transform_origin();
    let origin_x = origin
        .horizontal
        .resolve(Length::new(border_box_width))
        .px();
    let origin_y = origin.vertical.resolve(Length::new(border_box_height)).px();

    Some((m, Point2::new(origin_x, origin_y)))
}

fn op_to_matrix(
    op: &style::values::computed::transform::TransformOperation,
    w: f32,
    h: f32,
) -> Affine2D {
    use style::values::computed::Length;
    use style::values::generics::transform::GenericTransformOperation::*;

    match op {
        Matrix(m) => Affine2D {
            a: m.a,
            b: m.b,
            c: m.c,
            d: m.d,
            e: m.e,
            f: m.f,
        },
        Translate(x, y) => Affine2D::translation(
            x.resolve(Length::new(w)).px(),
            y.resolve(Length::new(h)).px(),
        ),
        TranslateX(x) => Affine2D::translation(x.resolve(Length::new(w)).px(), 0.0),
        TranslateY(y) => Affine2D::translation(0.0, y.resolve(Length::new(h)).px()),
        Scale(sx, sy) => Affine2D::scale(*sx, *sy),
        ScaleX(sx) => Affine2D::scale(*sx, 1.0),
        ScaleY(sy) => Affine2D::scale(1.0, *sy),
        Rotate(angle) | RotateZ(angle) => Affine2D::rotation(angle.radians()),
        Skew(ax, ay) => Affine2D::skew(ax.radians(), ay.radians()),
        SkewX(ax) => Affine2D::skew(ax.radians(), 0.0),
        SkewY(ay) => Affine2D::skew(0.0, ay.radians()),
        Matrix3D(_)
        | Translate3D(..)
        | TranslateZ(_)
        | Scale3D(..)
        | ScaleZ(_)
        | Rotate3D(..)
        | RotateX(_)
        | RotateY(_)
        | Perspective(_)
        | InterpolateMatrix { .. }
        | AccumulateMatrix { .. } => {
            log::warn!("unsupported 3D/animation transform op: {:?}", op);
            Affine2D::IDENTITY
        }
    }
}

/// Rewrite `::marker { content: url(...) }` rules into `list-style-image` equivalents.
///
/// Blitz 0.2.4 does not expose `::marker` pseudo-element computed styles, so we
/// rewrite the CSS text to convert `::marker { content: url(...) }` into
/// `list-style-image: url(...)` on the parent selector, which Blitz already supports.
///
/// The original rule is preserved verbatim (for forward-compat); the rewritten rule
/// is appended immediately after it.
pub fn rewrite_marker_content_url(css: &str) -> String {
    let chars: Vec<char> = css.chars().collect();
    let len = chars.len();

    // We'll collect "rewrites" — each is an extra CSS text string.
    // After scanning we append them all at the end of the CSS text.
    let mut rewrites: Vec<String> = Vec::new();

    // Track at-rule wrappers (e.g. @media print) so we can re-wrap the
    // generated rule in the same at-rule context.
    // Stack entries: (brace_depth_when_opened, header_text).
    let mut at_stack: Vec<(u32, String)> = Vec::new();

    // Unified brace-depth counter — incremented on every `{`, decremented
    // on every `}` (including those inside rule blocks we scan over).
    let mut brace_depth: u32 = 0;

    let mut i = 0;
    while i < len {
        let ch = chars[i];

        // Skip string literals so we don't match ::marker inside them.
        if ch == '"' || ch == '\'' {
            let quote = ch;
            i += 1;
            while i < len && chars[i] != quote {
                if chars[i] == '\\' {
                    i += 1; // skip escaped char
                }
                i += 1;
            }
            i += 1; // skip closing quote
            continue;
        }

        // Skip CSS comments.
        if ch == '/' && i + 1 < len && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2; // skip */
            continue;
        }

        // Detect at-rule start: @something ... { OR @charset/import ... ;
        if ch == '@' {
            let at_start = i;
            // Scan to the first `{` or `;` — whichever comes first.
            while i < len && chars[i] != '{' && chars[i] != ';' {
                i += 1;
            }
            if i < len {
                if chars[i] == ';' {
                    // Statement at-rule (@charset, @import, etc.) — skip it
                    // without pushing onto at_stack.
                    i += 1; // skip ;
                } else {
                    // Block at-rule — push header and open brace.
                    let header: String = chars[at_start..i].iter().collect();
                    at_stack.push((brace_depth, header.trim_end().to_string()));
                    brace_depth += 1;
                    i += 1; // skip {
                }
            }
            continue;
        }

        // Detect closing brace — could close an at-rule or a rule block.
        if ch == '}' {
            brace_depth = brace_depth.saturating_sub(1);
            // If the top at-rule was opened at the current depth, pop it.
            if let Some(&(depth, _)) = at_stack.last() {
                if depth == brace_depth {
                    at_stack.pop();
                }
            }
            i += 1;
            continue;
        }

        // Anything else might be the start of a selector.
        // Scan the selector up to '{'.
        let selector_start = i;
        let mut found_brace = false;
        while i < len {
            if chars[i] == '{' {
                found_brace = true;
                break;
            }
            // If we hit } or ; outside a block, this isn't a rule.
            if chars[i] == '}' || chars[i] == ';' {
                break;
            }
            i += 1;
        }
        if !found_brace {
            i += 1;
            continue;
        }

        let selector: String = chars[selector_start..i].iter().collect();
        let selector = selector.trim();
        brace_depth += 1;
        i += 1; // skip {

        // Now scan the declaration block to the matching }.
        let block_start = i;
        let mut depth = 1u32;
        while i < len && depth > 0 {
            match chars[i] {
                '{' => {
                    depth += 1;
                    brace_depth += 1;
                }
                '}' => {
                    depth -= 1;
                    brace_depth = brace_depth.saturating_sub(1);
                }
                '"' | '\'' => {
                    let q = chars[i];
                    i += 1;
                    while i < len && chars[i] != q {
                        if chars[i] == '\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        // i now points just past the closing }
        let block_end = i - 1; // index of the closing }
        let declarations: String = chars[block_start..block_end].iter().collect();

        // Check if selector contains ::marker
        if !selector.contains("::marker") {
            continue;
        }

        // Extract content: url(...) from declarations
        let url_value = extract_content_url(&declarations);
        if let Some(url) = url_value {
            // Build the stripped selector (remove ::marker)
            let stripped = selector.replace("::marker", "");
            let stripped = stripped.trim();

            // Skip if selector becomes empty (e.g., bare `::marker` without element)
            if stripped.is_empty() {
                continue;
            }

            // Build the new rule
            let new_rule = if at_stack.is_empty() {
                format!("\n{stripped}{{list-style-image:url({url})}}")
            } else {
                let (_, at_header) = at_stack.last().unwrap();
                format!("\n{at_header}{{{stripped}{{list-style-image:url({url})}}}}")
            };

            rewrites.push(new_rule);
        }
    }

    if rewrites.is_empty() {
        return css.to_string();
    }

    // Append all generated rules at the end of the CSS text so they are
    // never accidentally nested inside an existing at-rule block.
    let mut result = css.to_string();
    for extra in rewrites {
        result.push_str(&extra);
    }

    result
}

/// Rewrite `::marker { content: url(...) }` inside `<style>` blocks in HTML.
///
/// Finds all `<style>...</style>` regions in the HTML string and applies
/// [`rewrite_marker_content_url`] to each one's contents. Non-style
/// content is passed through unchanged.
pub fn rewrite_marker_content_url_in_html(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    // Quick check: bail early if no <style tag at all.
    if !lower.contains("<style") {
        return html.to_string();
    }

    let mut result = String::with_capacity(html.len());
    let mut cursor = 0;

    loop {
        // Find <style (case-insensitive) from cursor.
        let search = lower[cursor..].find("<style");
        let Some(rel_start) = search else {
            // No more <style tags; copy remainder.
            result.push_str(&html[cursor..]);
            break;
        };
        let tag_start = cursor + rel_start;

        // Find the end of the opening tag `>`.
        let Some(rel_gt) = html[tag_start..].find('>') else {
            // Malformed — no closing `>`; copy remainder as-is.
            result.push_str(&html[cursor..]);
            break;
        };
        let content_start = tag_start + rel_gt + 1;

        // Find </style (case-insensitive).
        let Some(rel_end) = lower[content_start..].find("</style") else {
            // No closing tag; copy remainder as-is.
            result.push_str(&html[cursor..]);
            break;
        };
        let content_end = content_start + rel_end;

        // Copy everything before the CSS content (including the <style> tag).
        result.push_str(&html[cursor..content_start]);

        // Rewrite the CSS content.
        let css_content = &html[content_start..content_end];
        let rewritten = rewrite_marker_content_url(css_content);
        result.push_str(&rewritten);

        // Advance cursor past the CSS content.
        cursor = content_end;
    }

    result
}

/// Extract the URL from a `content: url(...)` declaration, if present.
/// Returns the inner URL string (without the `url()` wrapper).
fn extract_content_url(declarations: &str) -> Option<String> {
    // Find `content` property followed by `:` and then `url(`
    let decls = declarations.trim();
    for decl in decls.split(';') {
        let decl = decl.trim();
        if let Some(value) = decl.strip_prefix("content") {
            let value = value.trim();
            if let Some(value) = value.strip_prefix(':') {
                let value = value.trim();
                if let Some(rest) = value.strip_prefix("url(") {
                    // Find the matching closing paren, respecting quotes.
                    let rest = rest.trim();
                    // The URL might be quoted or unquoted
                    let (url_inner, _) = if let Some(after_quote) = rest.strip_prefix('"') {
                        // Quoted with double quotes — find closing quote,
                        // then verify `)` follows. This handles parens
                        // inside the URL like `url("image(1).png")`.
                        let close_quote = after_quote.find('"')?;
                        let inner = &after_quote[..close_quote];
                        (inner, close_quote + 2) // +2 for both quotes
                    } else if let Some(after_quote) = rest.strip_prefix('\'') {
                        let close_quote = after_quote.find('\'')?;
                        let inner = &after_quote[..close_quote];
                        (inner, close_quote + 2)
                    } else {
                        // Unquoted — find the closing paren
                        let url_end = rest.find(')')?;
                        let inner = rest[..url_end].trim();
                        (inner, url_end)
                    };
                    return Some(format!("\"{}\"", url_inner));
                }
            }
        }
    }
    None
}

/// Description of a `<link rel="stylesheet" media=...>` node that needs
/// to be rewritten into `<style>@import url("...") media;</style>`.
///
/// Collected by [`collect_link_media_rewrites`] before DOM mutation so
/// the href and media values remain borrowed from a stable document
/// state (no interleaved mutation concerns).
#[derive(Debug, Clone)]
pub(crate) struct LinkMediaRewrite {
    pub link_node_id: usize,
    pub href: String,
    pub media: String,
}

/// Walk the parsed document and return every `<link rel=... stylesheet ...>`
/// element that carries a non-empty `media` attribute other than `all`.
///
/// Returned entries follow pre-order DOM traversal so the resulting
/// `<style>` elements keep the same cascade order as the original
/// `<link>` elements — insertion order matters for stylo's origin
/// sorting.
pub(crate) fn collect_link_media_rewrites(doc: &HtmlDocument) -> Vec<LinkMediaRewrite> {
    fn walk(doc: &HtmlDocument, node_id: usize, depth: usize, out: &mut Vec<LinkMediaRewrite>) {
        if depth >= MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };
        if let Some(el) = node.element_data() {
            if el.name.local.as_ref() == "link" {
                let rel_ok = get_attr(el, "rel")
                    .map(|rel| {
                        rel.split_ascii_whitespace()
                            .any(|t| t.eq_ignore_ascii_case("stylesheet"))
                    })
                    .unwrap_or(false);
                let href = get_attr(el, "href").unwrap_or("").trim();
                let media = get_attr(el, "media").unwrap_or("").trim();
                let media_active = !media.is_empty() && !media.eq_ignore_ascii_case("all");
                if rel_ok && !href.is_empty() && media_active {
                    out.push(LinkMediaRewrite {
                        link_node_id: node_id,
                        href: href.to_string(),
                        media: media.to_string(),
                    });
                }
            }
        }
        for &child in &node.children {
            walk(doc, child, depth + 1, out);
        }
    }

    let mut out = Vec::new();
    let root = doc.root_element().id;
    walk(doc, root, 0, &mut out);
    out
}

/// Escape a URL so it can appear inside a CSS `url("...")` literal.
///
/// Per CSS Syntax Module Level 3 §4.3.5, double quote and backslash
/// must be escaped as `\"` and `\\`. Newlines are disallowed inside
/// quoted strings but can be expressed as a numeric escape `\a`
/// (followed by a single space that the tokenizer consumes) — we do
/// the same for carriage return (`\d`).
fn escape_css_url(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str(r#"\""#),
            '\n' => out.push_str(r"\a "),
            '\r' => out.push_str(r"\d "),
            '\x0c' => out.push_str(r"\c "),
            _ => out.push(ch),
        }
    }
    out
}

/// Replace every collected `<link rel=stylesheet media=X href=Y>` with
/// a `<style>@import url("Y") X;</style>` element inserted in the same
/// document position.
///
/// Why this shape: blitz-dom 0.2.4's `CssHandler` hardcodes
/// `MediaList::empty()` when loading `<link>` stylesheets, so the
/// `media` attribute is silently dropped. However the `@import`
/// resolution path (`StylesheetLoaderInner::request_stylesheet`) does
/// propagate the media query into stylo's `ImportRule`, so routing the
/// load through `@import` re-activates the media restriction.
///
/// The `<style>` is inserted *before* the original `<link>` to preserve
/// cascade order; the `<link>` is then removed. The caller (Task 6
/// integration) must filter any stylesheet resources that blitz already
/// fetched for the `<link>` node before DOM mutation, otherwise the
/// empty-media copy would also apply.
pub(crate) fn apply_link_media_rewrites(doc: &mut HtmlDocument, rewrites: &[LinkMediaRewrite]) {
    for rw in rewrites {
        let css = format!(
            r#"@import url("{}") {};"#,
            escape_css_url(&rw.href),
            rw.media
        );

        let mut mutator = doc.mutate();
        let style_id = mutator.create_element(make_qual_name("style"), vec![]);
        let text_id = mutator.create_text_node(&css);
        mutator.append_children(style_id, &[text_id]);
        mutator.insert_nodes_before(rw.link_node_id, &[style_id]);
        mutator.remove_and_drop_node(rw.link_node_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `relayout_position_fixed` must reshape every `position: fixed`
    /// subtree against the supplied viewport, not against the nearest
    /// positioned ancestor (the size that Taffy assigned during the
    /// first pass through `stylo_taffy::convert::map_position` flattens
    /// Fixed → Absolute). We construct a fixed div nested inside a
    /// 50px-wide abs ancestor with text wider than 50px, run the
    /// relayout, and assert the fixed element's `final_layout.size.width`
    /// expanded past the parent's 50px constraint — which only happens
    /// if Taffy was re-invoked with viewport-sized available space.
    #[test]
    fn relayout_position_fixed_reshapes_against_viewport() {
        let html = r#"<!doctype html>
<html><body style="margin:0">
<div id="abs" style="position:absolute; width:50px">
  <div id="fix" style="position:fixed">This text needs more than fifty pixels</div>
</div>
</body></html>"#;
        let mut doc = parse(html, 600.0, &[]);
        resolve(&mut doc);

        // Locate the fixed div before relayout to capture its size.
        fn find_by_id(doc: &HtmlDocument, id: &str) -> Option<usize> {
            fn walk(doc: &HtmlDocument, node_id: usize, target: &str) -> Option<usize> {
                let n = doc.get_node(node_id)?;
                if let Some(elem) = n.element_data()
                    && elem.attr(blitz_dom::LocalName::from("id")) == Some(target)
                {
                    return Some(node_id);
                }
                for &c in &n.children {
                    if let Some(found) = walk(doc, c, target) {
                        return Some(found);
                    }
                }
                None
            }
            walk(doc, doc.root_element().id, id)
        }
        let fix_id = find_by_id(&doc, "fix").expect("fixed div id=fix");
        let pre_width = doc.get_node(fix_id).unwrap().final_layout.size.width;
        // Sanity: first pass constrained the fixed element to the
        // abs ancestor's 50px box, so the long text wraps to multiple
        // lines and the box stays narrow.
        assert!(
            pre_width <= 50.5,
            "first-pass width should be capped at the abs's 50px; got {pre_width}"
        );

        // Now run the second pass with a 600 × 800 viewport and recheck.
        relayout_position_fixed(&mut doc, 600.0, 800.0);
        let post_width = doc.get_node(fix_id).unwrap().final_layout.size.width;
        assert!(
            post_width > 50.5,
            "viewport relayout should widen the fixed box past the 50px parent; \
             got {post_width} (pre was {pre_width})"
        );
    }

    /// fulgur-6wap: when a `position: fixed` element has `width: auto`
    /// with exactly one of `left`/`right` set, CSS 2.1 §10.3.7 says the
    /// width shrinks to fit content. Without the §10.3.7 path,
    /// `compute_root_layout` with `available_space = Definite(viewport_w)`
    /// makes `width = viewport_w`, and the start-side of a `right: 0`
    /// element resolves to `cb_w - viewport_w - 0 = 0`, painting at the
    /// left edge instead of the right. The repro mirrors WPT
    /// fixedpos-009-print: a fixed div with `right: 0` containing a
    /// 36×36 child should shrink to 36 px, not the viewport width.
    #[test]
    fn relayout_position_fixed_shrinks_to_fit_when_only_right_inset_set() {
        let html = r#"<!doctype html>
<html><body style="margin:0">
<div id="fix" style="position:fixed; right:0">
  <div id="child" style="width:36px; height:36px; background:black"></div>
</div>
</body></html>"#;
        let mut doc = parse(html, 600.0, &[]);
        resolve(&mut doc);

        fn find_by_id(doc: &HtmlDocument, id: &str) -> Option<usize> {
            fn walk(doc: &HtmlDocument, node_id: usize, target: &str) -> Option<usize> {
                let n = doc.get_node(node_id)?;
                if let Some(elem) = n.element_data()
                    && elem.attr(blitz_dom::LocalName::from("id")) == Some(target)
                {
                    return Some(node_id);
                }
                for &c in &n.children {
                    if let Some(found) = walk(doc, c, target) {
                        return Some(found);
                    }
                }
                None
            }
            walk(doc, doc.root_element().id, id)
        }
        let fix_id = find_by_id(&doc, "fix").expect("fixed div id=fix");

        relayout_position_fixed(&mut doc, 600.0, 800.0);
        let post_width = doc.get_node(fix_id).unwrap().final_layout.size.width;
        assert!(
            post_width < 600.0,
            "shrink-to-fit per CSS 2.1 §10.3.7: right:0 + width:auto must \
             not fill viewport (600); got {post_width}"
        );
        // The 36 px child sets max-content width; allow small slack for
        // box-model differences (borders, scrollbar) but not enough to
        // accommodate viewport-fill (~600).
        assert!(
            (post_width - 36.0).abs() < 1.0,
            "expected ~36 px shrink-to-fit width, got {post_width}"
        );
    }

    /// fulgur-6wap (regression net for the wrappable-text branch): CSS
    /// 2.1 §10.3.7 shrink-to-fit must clamp to the available width,
    /// `min(max(min-content, available), max-content)`. For a fixed
    /// element with `right: 0; width: auto` containing a long text node,
    /// the box must wrap to fit within the viewport — passing
    /// `AvailableSpace::MaxContent` to `compute_root_layout` would
    /// produce pure max-content (single unwrapped line) and overflow,
    /// which is exactly the regression coderabbit flagged on PR #342.
    #[test]
    fn relayout_position_fixed_shrink_to_fit_clamps_breakable_text_to_viewport() {
        let html = r#"<!doctype html>
<html><body style="margin:0">
<div id="fix" style="position:fixed; right:0; font-size:16px">This is a long sentence that should wrap to fit within the available viewport width when CSS 2.1 §10.3.7 shrink-to-fit is applied correctly, instead of laying out on a single very wide unwrapped line.</div>
</body></html>"#;
        let mut doc = parse(html, 600.0, &[]);
        resolve(&mut doc);

        fn find_by_id(doc: &HtmlDocument, id: &str) -> Option<usize> {
            fn walk(doc: &HtmlDocument, node_id: usize, target: &str) -> Option<usize> {
                let n = doc.get_node(node_id)?;
                if let Some(elem) = n.element_data()
                    && elem.attr(blitz_dom::LocalName::from("id")) == Some(target)
                {
                    return Some(node_id);
                }
                for &c in &n.children {
                    if let Some(found) = walk(doc, c, target) {
                        return Some(found);
                    }
                }
                None
            }
            walk(doc, doc.root_element().id, id)
        }
        let fix_id = find_by_id(&doc, "fix").expect("fixed div id=fix");

        // Viewport is 600 CSS px wide; shrink-to-fit available width
        // for `right: 0` is `viewport_w - 0 = 600`. The text's
        // max-content width is well over 600, so the box must clamp at
        // 600 and wrap, not stretch to max-content.
        relayout_position_fixed(&mut doc, 600.0, 800.0);
        let post_width = doc.get_node(fix_id).unwrap().final_layout.size.width;
        assert!(
            post_width <= 600.5,
            "shrink-to-fit must clamp at the available width (600); \
             got {post_width} — pure max-content would overflow viewport"
        );
    }

    #[test]
    fn parse_html_with_local_resources_orders_imports_before_parent() {
        // CSS cascade: `@import "child.css"` in parent.css must be
        // treated as if child.css were inlined at the top of parent.css,
        // so the parent's *own* rules override the imported ones when
        // they have the same specificity. The merged `cleaned_css` that
        // comes back from `parse_html_with_local_resources` feeds the
        // Pass-2 margin-box renderer, so the ordering there must match:
        // child rules first, parent rules last (so that later rules win).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("parent.css"),
            r#"@import "child.css"; .parent-rule { color: red; }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("child.css"),
            r#".child-rule { color: blue; }"#,
        )
        .unwrap();

        let html = r#"<!DOCTYPE html>
<html><head><link rel="stylesheet" href="parent.css"></head>
<body><p class="parent-rule child-rule">x</p></body></html>"#;

        let (_doc, gcpm) =
            parse_html_with_local_resources(html, 400.0, 10000, &[], true, Some(dir.path()));

        let cleaned = &gcpm.cleaned_css;
        let child_pos = cleaned
            .find(".child-rule")
            .expect("child.css content should be in cleaned_css");
        let parent_pos = cleaned
            .find(".parent-rule")
            .expect("parent.css content should be in cleaned_css");
        assert!(
            child_pos < parent_pos,
            "child @import rules must come before parent's own rules in cleaned_css \
             to preserve CSS cascade. child at {child_pos}, parent at {parent_pos}.\n\
             cleaned_css:\n{cleaned}"
        );
    }

    struct NoOpPass;
    impl DomPass for NoOpPass {
        fn apply(&self, _doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {}
    }

    #[test]
    fn test_parse_resolve_roundtrip() {
        let html = "<html><body><p>Hello</p></body></html>";
        let mut doc = parse(html, 400.0, &[]);
        let ctx = PassContext { font_data: &[] };
        apply_passes(&mut doc, &[Box::new(NoOpPass)], &ctx);
        resolve(&mut doc);
        let root = doc.root_element();
        assert!(!root.children.is_empty());
    }

    #[test]
    fn test_parse_and_layout_unchanged() {
        let html = "<html><body><p>Test</p></body></html>";
        let doc = parse_and_layout(html, 400.0, 600.0, &[], true);
        let root = doc.root_element();
        assert!(!root.children.is_empty());
    }

    #[test]
    fn test_inject_css_pass_adds_style() {
        let html = "<html><head></head><body><p>Hello</p></body></html>";
        let mut doc = parse(html, 400.0, &[]);
        let pass = InjectCssPass {
            css: "p { color: red; }".to_string(),
        };
        let ctx = PassContext { font_data: &[] };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_some(),
            "Expected a <style> element to be injected into the DOM"
        );
    }

    #[test]
    fn test_inject_css_pass_empty_css_is_noop() {
        let html = "<html><body><p>Hello</p></body></html>";
        let mut doc = parse(html, 400.0, &[]);
        let pass = InjectCssPass { css: String::new() };
        let ctx = PassContext { font_data: &[] };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_none(),
            "Expected no <style> element when CSS is empty"
        );
    }

    #[test]
    fn test_running_element_pass_extracts_by_class() {
        let html = r#"<html><head><style>.header { display: none; }</style></head><body>
            <div class="header">Header Content</div>
            <p>Body text</p>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);

        let gcpm = crate::gcpm::GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![crate::gcpm::RunningMapping {
                parsed: crate::gcpm::ParsedSelector::Class("header".to_string()),
                running_name: "pageHeader".to_string(),
            }],
            string_set_mappings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            page_settings: vec![],
            bookmark_mappings: vec![],
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm.running_mappings);
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert_eq!(
            store.instance_count(),
            1,
            "Expected exactly one running element instance to be registered"
        );
        assert_eq!(store.name_of(0), Some("pageHeader"));
        let html_content = store.get_html(0).unwrap();
        assert!(
            html_content.contains("Header Content"),
            "Expected serialized HTML to contain 'Header Content', got: {html_content}"
        );
    }

    #[test]
    fn test_running_element_pass_extracts_by_id() {
        let html = r#"<html><head><style>#title { display: none; }</style></head><body>
            <h1 id="title">Doc Title</h1>
            <p>Body text</p>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);

        let gcpm = crate::gcpm::GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![crate::gcpm::RunningMapping {
                parsed: crate::gcpm::ParsedSelector::Id("title".to_string()),
                running_name: "pageTitle".to_string(),
            }],
            string_set_mappings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            page_settings: vec![],
            bookmark_mappings: vec![],
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm.running_mappings);
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert_eq!(store.instance_count(), 1);
        assert_eq!(store.name_of(0), Some("pageTitle"));
        assert!(store.get_html(0).unwrap().contains("Doc Title"));
    }

    #[test]
    fn test_running_element_pass_no_mappings_is_noop() {
        let html = "<html><body><p>Hello</p></body></html>";
        let mut doc = parse(html, 400.0, &[]);

        let gcpm = crate::gcpm::GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![],
            string_set_mappings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            page_settings: vec![],
            bookmark_mappings: vec![],
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm.running_mappings);
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert_eq!(store.instance_count(), 0);
    }

    #[test]
    fn test_running_element_pass_skips_head_elements() {
        let html = r#"<html><head><style id="injected">p { color: red; }</style></head><body>
            <p>Body text</p>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);

        let gcpm = crate::gcpm::GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![crate::gcpm::RunningMapping {
                parsed: crate::gcpm::ParsedSelector::Id("injected".to_string()),
                running_name: "shouldNotMatch".to_string(),
            }],
            string_set_mappings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            page_settings: vec![],
            bookmark_mappings: vec![],
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm.running_mappings);
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert_eq!(
            store.instance_count(),
            0,
            "Elements inside <head> (like <style>) should not be matched as running elements"
        );
    }

    // NOTE: The previous LinkStylesheetPass tests have been removed.
    // <link rel="stylesheet"> resolution now happens via Blitz's own
    // loader, driven by `crate::net::FulgurNetProvider`. Path-traversal,
    // http(s) rejection and missing-file behaviour are tested in
    // `crates/fulgur/src/net.rs`.

    #[test]
    fn test_counter_pass_generates_css() {
        use crate::gcpm::{
            ContentCounterMapping, CounterMapping, CounterOp, CounterStyle, PseudoElement,
        };

        let html = r#"<html><body>
            <h2>Chapter One</h2>
            <h2>Chapter Two</h2>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        let ctx = PassContext { font_data: &[] };

        let counter_mappings = vec![
            CounterMapping {
                parsed: crate::gcpm::ParsedSelector::Tag("body".into()),
                ops: vec![CounterOp::Reset {
                    name: "chapter".into(),
                    value: 0,
                }],
            },
            CounterMapping {
                parsed: crate::gcpm::ParsedSelector::Tag("h2".into()),
                ops: vec![CounterOp::Increment {
                    name: "chapter".into(),
                    value: 1,
                }],
            },
        ];

        let content_mappings = vec![ContentCounterMapping {
            parsed: crate::gcpm::ParsedSelector::Tag("h2".into()),
            pseudo: PseudoElement::Before,
            content: vec![
                crate::gcpm::ContentItem::Counter {
                    name: "chapter".into(),
                    style: CounterStyle::Decimal,
                },
                crate::gcpm::ContentItem::String(". ".into()),
            ],
        }];

        let pass = CounterPass::new(counter_mappings, content_mappings);
        pass.apply(&mut doc, &ctx);

        let css = pass.generated_css();
        // Should contain resolved values "1. " and "2. "
        assert!(
            css.contains("1. "),
            "CSS should contain resolved '1. ', got: {css}"
        );
        assert!(
            css.contains("2. "),
            "CSS should contain resolved '2. ', got: {css}"
        );

        let (ops_by_node, _) = pass.into_parts();
        // Should have 3 ops: body reset + h2 increment + h2 increment
        assert_eq!(
            ops_by_node.len(),
            3,
            "Should have exactly 3 ops: body reset + 2 h2 increments"
        );
    }

    #[test]
    fn counter_pass_records_per_node_snapshot() {
        use crate::gcpm::{CounterMapping, CounterOp, ParsedSelector};

        let html = r#"<html><body>
            <h1 id="a">A</h1><h1 id="b">B</h1>
        </body></html>"#;
        let mappings = vec![CounterMapping {
            parsed: ParsedSelector::Tag("h1".into()),
            ops: vec![CounterOp::Increment {
                name: "chapter".into(),
                value: 1,
            }],
        }];
        let mut doc = parse(html, 400.0, &[]);
        let pass = CounterPass::new(mappings, Vec::new()).with_snapshot_recording();
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let snapshots = pass.take_node_snapshots();

        let mut h1_ids: Vec<usize> = Vec::new();
        fn walk(doc: &HtmlDocument, id: usize, out: &mut Vec<usize>) {
            if let Some(node) = doc.get_node(id) {
                if let Some(el) = node.element_data() {
                    if el.name.local.as_ref() == "h1" {
                        out.push(id);
                    }
                }
                for &c in &node.children {
                    walk(doc, c, out);
                }
            }
        }
        walk(&doc, doc.root_element().id, &mut h1_ids);
        assert_eq!(h1_ids.len(), 2);
        let a = snapshots.get(&h1_ids[0]).expect("snapshot at first h1");
        let b = snapshots.get(&h1_ids[1]).expect("snapshot at second h1");
        assert_eq!(a.get("chapter"), Some(&vec![1]));
        assert_eq!(b.get("chapter"), Some(&vec![2]));
    }

    #[test]
    fn counter_pass_skips_snapshot_when_recording_disabled() {
        use crate::gcpm::{CounterMapping, CounterOp, ParsedSelector};

        let html = r#"<html><body><h1>A</h1></body></html>"#;
        let mappings = vec![CounterMapping {
            parsed: ParsedSelector::Tag("h1".into()),
            ops: vec![CounterOp::Increment {
                name: "chapter".into(),
                value: 1,
            }],
        }];
        let mut doc = parse(html, 400.0, &[]);
        let pass = CounterPass::new(mappings, Vec::new());
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        // Default (no `with_snapshot_recording`) skips per-element clones.
        assert!(
            pass.take_node_snapshots().is_empty(),
            "snapshot map must be empty when recording is disabled"
        );
    }

    #[test]
    fn counter_pass_nested_reset_records_chain_snapshot() {
        use crate::gcpm::{CounterMapping, CounterOp, ParsedSelector};

        // Outer ol resets `item`. Inner ol nested under outer's first li
        // resets `item` again — at that scope chain is length 2.
        let html = r#"<html><body>
            <ol><li><ol><li id="inner">Inner</li></ol></li></ol>
        </body></html>"#;
        let mappings = vec![
            CounterMapping {
                parsed: ParsedSelector::Tag("ol".into()),
                ops: vec![CounterOp::Reset {
                    name: "item".into(),
                    value: 0,
                }],
            },
            CounterMapping {
                parsed: ParsedSelector::Tag("li".into()),
                ops: vec![CounterOp::Increment {
                    name: "item".into(),
                    value: 1,
                }],
            },
        ];
        let mut doc = parse(html, 400.0, &[]);
        let pass = CounterPass::new(mappings, Vec::new()).with_snapshot_recording();
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let snapshots = pass.take_node_snapshots();

        fn find_inner_li(doc: &HtmlDocument, id: usize, ol_depth: usize) -> Option<usize> {
            if let Some(node) = doc.get_node(id) {
                if let Some(el) = node.element_data() {
                    if el.name.local.as_ref() == "li" && ol_depth >= 2 {
                        return Some(id);
                    }
                }
                let next_depth = node
                    .element_data()
                    .map(|el| {
                        if el.name.local.as_ref() == "ol" {
                            ol_depth + 1
                        } else {
                            ol_depth
                        }
                    })
                    .unwrap_or(ol_depth);
                for &c in &node.children {
                    if let Some(found) = find_inner_li(doc, c, next_depth) {
                        return Some(found);
                    }
                }
            }
            None
        }

        let inner_id = find_inner_li(&doc, doc.root_element().id, 0).expect("inner li");
        let snap = snapshots.get(&inner_id).expect("snapshot at inner li");
        let chain = snap.get("item").expect("item chain at inner li");
        assert_eq!(
            chain.len(),
            2,
            "nested counter-reset must yield chain of length 2, got {chain:?}"
        );
        // Both outer-li and inner-li have been incremented once; snapshot
        // is taken at inner-li after its own ops, so chain == [1, 1].
        assert_eq!(chain, &vec![1, 1]);
    }

    /// Snapshot test for the spec-compliant `counters()` resolution: a
    /// nested `<ol>` with `counters(item, ".") ". "` in `li::before`
    /// must produce one CSS rule per matched `<li>` whose resolved
    /// `content:"…"` contains exactly the expected chain markers.
    /// Asserts on `CounterPass::generated_css` — fully deterministic
    /// (no PDF rendering, no font/CMap dependency), unlike PDF text
    /// extraction which cannot decode krilla's ToUnicode CMap.
    #[test]
    fn counter_pass_resolves_counters_function() {
        use crate::gcpm::{
            ContentCounterMapping, CounterMapping, CounterOp, CounterStyle, ParsedSelector,
            PseudoElement,
        };

        let html = r#"<html><body>
            <ol>
              <li>Alpha</li>
              <li>Beta
                <ol>
                  <li>Beta-one</li>
                  <li>Beta-two</li>
                </ol>
              </li>
              <li>Gamma</li>
            </ol>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        let counter_mappings = vec![
            CounterMapping {
                parsed: ParsedSelector::Tag("ol".into()),
                ops: vec![CounterOp::Reset {
                    name: "item".into(),
                    value: 0,
                }],
            },
            CounterMapping {
                parsed: ParsedSelector::Tag("li".into()),
                ops: vec![CounterOp::Increment {
                    name: "item".into(),
                    value: 1,
                }],
            },
        ];
        let content_mappings = vec![ContentCounterMapping {
            parsed: ParsedSelector::Tag("li".into()),
            pseudo: PseudoElement::Before,
            content: vec![
                crate::gcpm::ContentItem::Counters {
                    name: "item".into(),
                    separator: ".".into(),
                    style: CounterStyle::Decimal,
                },
                crate::gcpm::ContentItem::String(". ".into()),
            ],
        }];

        let pass = CounterPass::new(counter_mappings, content_mappings);
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let css = pass.generated_css();

        // Five li::before rules. Order in `generated_css` mirrors DOM
        // traversal: outer 1 → outer 2 (Beta) → inner 2.1 → inner 2.2 →
        // outer 3. After leaving the inner ol, `leave_element(inner-ol)`
        // pops the inner-ol instance, so the third top-level li reads
        // chain `[3]` — emitted as `3.`, NOT `3.1.` (regression marker).
        for needle in [
            "content:\"1. \"",
            "content:\"2. \"",
            "content:\"2.1. \"",
            "content:\"2.2. \"",
            "content:\"3. \"",
        ] {
            assert!(
                css.contains(needle),
                "expected {needle:?} in generated CSS, got: {css}"
            );
        }
        assert!(
            !css.contains("content:\"3.1. \""),
            "leave_element regression: outer counter at third top-level li \
             should be `3.`, not `3.1.` (got: {css})"
        );
    }

    /// `target-counter(attr(href), <name>)` inside `::after` content must
    /// resolve against the supplied `AnchorMap`. Pass-1 (no map) emits the
    /// "00" placeholder; pass-2 (with map) emits the actual counter value.
    #[test]
    fn counter_pass_resolves_target_counter_with_anchor_map() {
        use crate::gcpm::target_ref::{AnchorEntry, AnchorMap};
        use crate::gcpm::{
            ContentCounterMapping, ContentItem, CounterStyle, ParsedSelector, PseudoElement,
        };

        let html = r##"<html><body><a class="ref" href="#sec1">Sec1</a></body></html>"##;
        let mut doc = parse(html, 400.0, &[]);
        let content = vec![ContentItem::TargetCounter {
            url_attr: "href".into(),
            counter_name: "page".into(),
            style: CounterStyle::Decimal,
        }];
        let mappings = vec![ContentCounterMapping {
            parsed: ParsedSelector::Class("ref".into()),
            pseudo: PseudoElement::After,
            content,
        }];

        let mut anchor = AnchorMap::new();
        let mut counters = BTreeMap::new();
        counters.insert("page".into(), vec![3]);
        anchor.insert(
            "sec1",
            AnchorEntry {
                page_num: 3,
                counters,
                text: String::new(),
            },
        );

        let pass = CounterPass::new(Vec::new(), mappings).with_anchor_map(anchor);
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let (_, css) = pass.into_parts();
        assert!(css.contains("\"3\""), "CSS = {css}");
    }

    /// Pass-1 placeholder path: when no `AnchorMap` is supplied (the
    /// pre-pagination pass), `target-counter()` inside `::after` content
    /// must substitute the `"00"` placeholder. Width is a rough
    /// approximation only — line breaks may shift between passes.
    #[test]
    fn counter_pass_target_counter_emits_placeholder_in_pass_one() {
        use crate::gcpm::{
            ContentCounterMapping, ContentItem, CounterStyle, ParsedSelector, PseudoElement,
        };

        let html = r##"<html><body><a class="ref" href="#sec1">Sec1</a></body></html>"##;
        let mut doc = parse(html, 400.0, &[]);
        let content = vec![ContentItem::TargetCounter {
            url_attr: "href".into(),
            counter_name: "page".into(),
            style: CounterStyle::Decimal,
        }];
        let mappings = vec![ContentCounterMapping {
            parsed: ParsedSelector::Class("ref".into()),
            pseudo: PseudoElement::After,
            content,
        }];

        // No `with_anchor_map` — exercise the pass-1 placeholder branch.
        let pass = CounterPass::new(Vec::new(), mappings);
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let (_, css) = pass.into_parts();
        assert!(css.contains("\"00\""), "CSS = {css}");
    }

    /// `target-text(attr(href))` inside `::after` content must resolve
    /// against the supplied `AnchorMap`'s `text` field (the anchor's
    /// resolved text, e.g. an `<h2>` heading's body) when the map is
    /// present in pass 2.
    #[test]
    fn counter_pass_resolves_target_text_with_anchor_map() {
        use crate::gcpm::target_ref::{AnchorEntry, AnchorMap};
        use crate::gcpm::{ContentCounterMapping, ContentItem, ParsedSelector, PseudoElement};

        let html = r##"<html><body><a class="ref" href="#sec1">Sec1</a></body></html>"##;
        let mut doc = parse(html, 400.0, &[]);
        let content = vec![ContentItem::TargetText {
            url_attr: "href".into(),
        }];
        let mappings = vec![ContentCounterMapping {
            parsed: ParsedSelector::Class("ref".into()),
            pseudo: PseudoElement::After,
            content,
        }];

        let mut anchor = AnchorMap::new();
        anchor.insert(
            "sec1",
            AnchorEntry {
                page_num: 0,
                counters: BTreeMap::new(),
                text: "Hello".into(),
            },
        );

        let pass = CounterPass::new(Vec::new(), mappings).with_anchor_map(anchor);
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let (_, css) = pass.into_parts();
        assert!(css.contains("Hello"), "CSS = {css}");
    }

    /// Covers `BookmarkPass::resolve_label`'s `Counters` arm. A single
    /// `bookmark-label: counters(section, ".")` rule against a chain
    /// snapshot of `[1, 2]` must resolve to `"1.2"`. Empty / missing
    /// chains resolve to `""` (CSS Lists 3 §3.3).
    #[test]
    fn bookmark_pass_resolves_counters_with_chain_snapshot() {
        use crate::gcpm::bookmark::{BookmarkLevel, BookmarkMapping};
        use crate::gcpm::{ContentItem, CounterStyle};

        let html = r#"<html><body><h1 id="t">Title</h1></body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        fn find_h1(doc: &HtmlDocument, id: usize) -> Option<usize> {
            if let Some(node) = doc.get_node(id) {
                if let Some(el) = node.element_data() {
                    if el.name.local.as_ref() == "h1" {
                        return Some(id);
                    }
                }
                for &c in &node.children {
                    if let Some(found) = find_h1(doc, c) {
                        return Some(found);
                    }
                }
            }
            None
        }
        let h1_id = find_h1(&doc, doc.root_element().id).expect("h1 node");

        let mut counter_snapshots = BTreeMap::new();
        let mut chain_for_h1 = BTreeMap::new();
        chain_for_h1.insert("section".to_string(), vec![1, 2]);
        counter_snapshots.insert(h1_id, chain_for_h1);

        let mappings = vec![BookmarkMapping {
            selector: ParsedSelector::Tag("h1".into()),
            level: Some(BookmarkLevel::Integer(1)),
            label: Some(vec![ContentItem::Counters {
                name: "section".into(),
                separator: ".".into(),
                style: CounterStyle::Decimal,
            }]),
        }];
        let pass = BookmarkPass::new_with_snapshots(mappings, counter_snapshots, BTreeMap::new());
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let results = pass.into_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.label, "1.2");
    }

    #[test]
    fn string_set_pass_records_per_node_snapshot() {
        use crate::gcpm::StringSetValue;

        let html = r#"<html><body>
            <h1 id="a">First</h1>
            <p id="p1">Body</p>
            <h1 id="b">Second</h1>
            <p id="p2">Body2</p>
        </body></html>"#;
        let mappings = vec![StringSetMapping {
            parsed: ParsedSelector::Tag("h1".into()),
            name: "title".into(),
            values: vec![StringSetValue::ContentText],
        }];
        let mut doc = parse(html, 400.0, &[]);
        let pass = StringSetPass::new(mappings).with_snapshot_recording();
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let snapshots = pass.take_node_snapshots();

        let mut tag_ids: Vec<(String, usize)> = Vec::new();
        fn walk(doc: &HtmlDocument, id: usize, out: &mut Vec<(String, usize)>) {
            if let Some(node) = doc.get_node(id) {
                if let Some(el) = node.element_data() {
                    let tag = el.name.local.as_ref().to_string();
                    if matches!(tag.as_str(), "h1" | "p") {
                        out.push((tag, id));
                    }
                }
                for &c in &node.children {
                    walk(doc, c, out);
                }
            }
        }
        walk(&doc, doc.root_element().id, &mut tag_ids);

        let p_ids: Vec<usize> = tag_ids
            .iter()
            .filter(|(t, _)| t == "p")
            .map(|(_, id)| *id)
            .collect();
        assert_eq!(p_ids.len(), 2);
        let p1 = p_ids[0];
        let p2 = p_ids[1];

        assert_eq!(
            snapshots.get(&p1).and_then(|m| m.get("title").cloned()),
            Some("First".to_string()),
            "first <p> should see title=First (set by preceding h1)"
        );
        assert_eq!(
            snapshots.get(&p2).and_then(|m| m.get("title").cloned()),
            Some("Second".to_string()),
            "second <p> should see title=Second (set by preceding h1)"
        );
    }

    #[test]
    fn string_set_pass_folds_multiple_matching_mappings() {
        use crate::gcpm::StringSetValue;

        // Two distinct selectors target the same element with different
        // named strings. The pass must apply BOTH (cascade order), not
        // short-circuit on the first match — otherwise `bookmark-label:
        // string(other-name)` reads stale / empty values.
        let html = r#"<html><body>
            <h1 class="ch">Heading</h1>
            <p>Body</p>
        </body></html>"#;
        let mappings = vec![
            StringSetMapping {
                parsed: ParsedSelector::Tag("h1".into()),
                name: "title".into(),
                values: vec![StringSetValue::ContentText],
            },
            StringSetMapping {
                parsed: ParsedSelector::Class("ch".into()),
                name: "section".into(),
                values: vec![StringSetValue::Literal("ch-1".into())],
            },
        ];
        let mut doc = parse(html, 400.0, &[]);
        let pass = StringSetPass::new(mappings).with_snapshot_recording();
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let snapshots = pass.take_node_snapshots();
        let store = pass.into_store();

        // Both store entries from the h1 should be present.
        let names: Vec<&str> = store.entries().iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"title"),
            "store should contain the `title` entry; got {names:?}"
        );
        assert!(
            names.contains(&"section"),
            "store should contain the `section` entry; got {names:?}"
        );

        // The <p>'s snapshot should observe BOTH named strings set by
        // the preceding h1 — proves the running map folded both
        // mappings, not just the first one.
        let mut p_id: Option<usize> = None;
        fn walk(doc: &HtmlDocument, id: usize, out: &mut Option<usize>) {
            if out.is_some() {
                return;
            }
            if let Some(node) = doc.get_node(id) {
                if let Some(el) = node.element_data() {
                    if el.name.local.as_ref() == "p" {
                        *out = Some(id);
                        return;
                    }
                }
                for &c in &node.children {
                    walk(doc, c, out);
                }
            }
        }
        walk(&doc, doc.root_element().id, &mut p_id);
        let p = p_id.expect("<p> in fixture");

        let snap = snapshots.get(&p).expect("snapshot at <p>");
        assert_eq!(snap.get("title").cloned(), Some("Heading".to_string()));
        assert_eq!(snap.get("section").cloned(), Some("ch-1".to_string()));
    }

    /// Walk the DOM tree to find the first element with the given local name.
    /// Used by pseudo-content tests below.
    fn find_element_by_local_name(doc: &HtmlDocument, name: &str) -> Option<usize> {
        fn walk(doc: &blitz_dom::BaseDocument, id: usize, name: &str) -> Option<usize> {
            let node = doc.get_node(id)?;
            if let Some(ed) = node.element_data() {
                if ed.name.local.as_ref() == name {
                    return Some(id);
                }
            }
            for &c in &node.children {
                if let Some(v) = walk(doc, c, name) {
                    return Some(v);
                }
            }
            None
        }
        use std::ops::Deref;
        walk(doc.deref(), doc.root_element().id, name)
    }

    #[test]
    fn test_extract_content_image_url_simple() {
        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("logo.png"); }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = parse(html, 800.0, &[]);
        resolve(&mut doc);
        let h1_id = find_element_by_local_name(&doc, "h1").expect("h1");
        let before_id = doc
            .get_node(h1_id)
            .unwrap()
            .before
            .expect("::before pseudo");
        let url = extract_content_image_url(doc.get_node(before_id).unwrap());
        assert!(url.is_some(), "expected Some(url), got None");
        let url = url.unwrap();
        assert!(url.ends_with("logo.png"), "unexpected url: {url}");
    }

    #[test]
    fn test_extract_content_image_url_returns_none_for_string_content() {
        let html = r#"<!doctype html><html><head><style>
            h1::before { content: "prefix "; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = parse(html, 800.0, &[]);
        resolve(&mut doc);
        let h1_id = find_element_by_local_name(&doc, "h1").expect("h1");
        let before_id = doc
            .get_node(h1_id)
            .unwrap()
            .before
            .expect("::before pseudo");
        assert!(
            extract_content_image_url(doc.get_node(before_id).unwrap()).is_none(),
            "string content should not return a url"
        );
    }

    #[test]
    fn test_extract_content_image_url_image_set() {
        // image-set(url(...) 1x) should resolve to the same URL after stylo
        // picks the selected candidate.
        let html = r#"<!doctype html><html><head><style>
            h1::before { content: image-set(url("hi.png") 1x); }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = parse(html, 800.0, &[]);
        resolve(&mut doc);
        let h1_id = find_element_by_local_name(&doc, "h1").expect("h1");
        let before_id = doc
            .get_node(h1_id)
            .unwrap()
            .before
            .expect("::before pseudo");
        let url = extract_content_image_url(doc.get_node(before_id).unwrap());
        assert!(url.is_some(), "expected Some from image-set, got None");
        assert!(
            url.unwrap().ends_with("hi.png"),
            "image-set should resolve to the selected url"
        );
    }

    #[test]
    fn collect_link_media_rewrites_picks_only_linked_sheets_with_non_empty_media() {
        let html = r#"
            <html><head>
                <link rel="stylesheet" href="a.css" media="print">
                <link rel="stylesheet" href="b.css">
                <link rel="stylesheet" href="c.css" media="all">
                <link rel="stylesheet" href="d.css" media="">
                <link rel="stylesheet" href="e.css" media="screen and (min-width: 600px)">
                <link rel="stylesheet" href="g.css" media="screen, print">
                <link rel="alternate stylesheet" href="f.css" media="print">
                <link rel="icon" href="favicon.ico" media="print">
            </head><body><p>hi</p></body></html>
        "#;
        let doc = parse(html, 800.0, &[]);
        let rewrites = collect_link_media_rewrites(&doc);

        // a.css and e.css should be rewritten. f.css has `rel="alternate stylesheet"`
        // which tokenizes to ["alternate", "stylesheet"]; since "stylesheet" is a
        // token, include it. `media="all"` and `media=""` are treated as identity
        // (skipped). favicon is not a stylesheet.
        let hrefs: Vec<&str> = rewrites.iter().map(|r| r.href.as_str()).collect();
        assert_eq!(hrefs, vec!["a.css", "e.css", "g.css", "f.css"]);
        let medias: Vec<&str> = rewrites.iter().map(|r| r.media.as_str()).collect();
        assert_eq!(
            medias,
            vec![
                "print",
                "screen and (min-width: 600px)",
                "screen, print",
                "print"
            ]
        );
    }

    #[test]
    fn escape_css_url_escapes_backslash_and_quote() {
        assert_eq!(escape_css_url("a.css"), "a.css");
        assert_eq!(escape_css_url(r#"a"b.css"#), r#"a\"b.css"#);
        assert_eq!(escape_css_url(r"a\b.css"), r"a\\b.css");
        assert_eq!(escape_css_url("a\nb.css"), r"a\a b.css");
        assert_eq!(escape_css_url("a\rb.css"), r"a\d b.css");
        assert_eq!(escape_css_url("a\x0cb.css"), r"a\c b.css");
    }

    #[test]
    fn apply_link_media_rewrites_replaces_link_with_style_import() {
        let html = r#"
            <html><head>
                <link rel="stylesheet" href="a.css" media="print">
                <link rel="stylesheet" href="b.css">
            </head><body><p>hi</p></body></html>
        "#;
        let mut doc = parse(html, 800.0, &[]);
        let rewrites = collect_link_media_rewrites(&doc);
        assert_eq!(rewrites.len(), 1);

        apply_link_media_rewrites(&mut doc, &rewrites);

        let head = find_element_by_tag(&doc, "head").expect("head exists");
        let head_node = doc.get_node(head).unwrap();

        let mut style_text_found: Option<String> = None;
        let mut a_css_link_found = false;
        let mut b_css_link_found = false;
        for &cid in &head_node.children {
            let child = doc.get_node(cid).unwrap();
            if let Some(el) = child.element_data() {
                match el.name.local.as_ref() {
                    "style" => {
                        for &gc in &child.children {
                            let gnode = doc.get_node(gc).unwrap();
                            if let blitz_dom::node::NodeData::Text(t) = &gnode.data {
                                style_text_found = Some(t.content.clone());
                            }
                        }
                    }
                    "link" => match get_attr(el, "href") {
                        Some("a.css") => a_css_link_found = true,
                        Some("b.css") => b_css_link_found = true,
                        _ => {}
                    },
                    _ => {}
                }
            }
        }

        assert!(!a_css_link_found, "<link href=a.css> must be removed");
        assert!(b_css_link_found, "<link href=b.css> must be preserved");
        let text = style_text_found.expect("<style> with @import must exist");
        assert_eq!(text, r#"@import url("a.css") print;"#);
    }

    #[test]
    fn element_text_does_not_stack_overflow_on_deep_nesting() {
        // Regression guard: element_text used to recurse without a depth
        // bound, so attacker-controlled HTML with thousands of nested
        // elements could overflow the thread stack. MAX_DOM_DEPTH now caps
        // the recursion — building ~2000 nested divs must return (possibly
        // truncated) rather than panic.
        let mut html = String::from("<html><body>");
        for _ in 0..2000 {
            html.push_str("<div>");
        }
        html.push_str("leaf");
        for _ in 0..2000 {
            html.push_str("</div>");
        }
        html.push_str("</body></html>");

        let (doc, _gcpm) = parse_html_with_local_resources(&html, 400.0, 10000, &[], true, None);
        use std::ops::Deref;
        let root = doc.root_element();
        let _ = element_text(doc.deref(), root.id);
    }

    /// Walk the DOM to find the first element whose `id` attribute equals `id_value`.
    fn find_element_by_attr_id(doc: &blitz_dom::BaseDocument, id_value: &str) -> usize {
        fn walk(
            doc: &blitz_dom::BaseDocument,
            node_id: usize,
            want: &str,
            depth: usize,
        ) -> Option<usize> {
            if depth >= MAX_DOM_DEPTH {
                return None;
            }
            let node = doc.get_node(node_id)?;
            if let Some(el) = node.element_data() {
                if get_attr(el, "id") == Some(want) {
                    return Some(node_id);
                }
            }
            for &child_id in &node.children {
                if let Some(found) = walk(doc, child_id, want, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        let root_id = doc.root_element().id;
        walk(doc, root_id, id_value, 0)
            .unwrap_or_else(|| panic!("element with id={id_value:?} not found"))
    }

    #[test]
    fn element_text_inserts_space_between_block_children() {
        let html = "<html><body><a id='x'><div>foo</div><div>bar</div></a></body></html>";
        let (doc, _gcpm) = parse_html_with_local_resources(html, 400.0, 10000, &[], true, None);
        use std::ops::Deref;
        let a_id = find_element_by_attr_id(doc.deref(), "x");
        let text = element_text(doc.deref(), a_id);
        assert_eq!(text.trim(), "foo bar", "got {text:?}");
    }

    #[test]
    fn element_text_inserts_space_for_br() {
        let html = "<html><body><a id='x'>foo<br>bar</a></body></html>";
        let (doc, _gcpm) = parse_html_with_local_resources(html, 400.0, 10000, &[], true, None);
        use std::ops::Deref;
        let a_id = find_element_by_attr_id(doc.deref(), "x");
        let text = element_text(doc.deref(), a_id);
        assert_eq!(text.trim(), "foo bar");
    }

    #[test]
    fn element_text_does_not_double_whitespace() {
        // If the text already ends in whitespace, a block boundary should
        // not add another space.
        let html = "<html><body><a id='x'>foo <div>bar</div></a></body></html>";
        let (doc, _gcpm) = parse_html_with_local_resources(html, 400.0, 10000, &[], true, None);
        use std::ops::Deref;
        let a_id = find_element_by_attr_id(doc.deref(), "x");
        let text = element_text(doc.deref(), a_id);
        // Should be "foo bar" (single space), not "foo  bar".
        assert_eq!(text.trim(), "foo bar");
        assert!(!text.contains("  "));
    }

    // ── extract_gcpm_from_inline_styles (fulgur-mq5) ───────────────

    #[test]
    fn extract_gcpm_from_inline_styles_picks_up_at_page() {
        let html = r#"<!doctype html><html><head>
            <style>@page { size: A4 landscape; }</style>
        </head><body>x</body></html>"#;
        let (doc, _) = parse_html_with_local_resources(html, 400.0, 10000, &[], true, None);
        let gcpm = extract_gcpm_from_inline_styles(&doc);
        assert_eq!(
            gcpm.page_settings.len(),
            1,
            "expected the @page rule to be extracted"
        );
    }

    #[test]
    fn extract_gcpm_from_inline_styles_returns_empty_for_no_style_tag() {
        let html = r#"<!doctype html><html><body>x</body></html>"#;
        let (doc, _) = parse_html_with_local_resources(html, 400.0, 10000, &[], true, None);
        let gcpm = extract_gcpm_from_inline_styles(&doc);
        assert!(gcpm.page_settings.is_empty());
    }

    #[test]
    fn extract_gcpm_from_inline_styles_folds_multiple_style_blocks() {
        // Two `@page` rules with `size` or `margin` declarations both
        // produce PageSettingsRule entries (parser.rs:105 only emits a
        // rule when at least one of size/margin is set). The second
        // would be missed if the walker stopped after the first <style>.
        let html = r#"<!doctype html><html><head>
            <style>@page { size: A4 landscape; }</style>
            <style>@page { margin: 2cm; }</style>
        </head><body>x</body></html>"#;
        let (doc, _) = parse_html_with_local_resources(html, 400.0, 10000, &[], true, None);
        let gcpm = extract_gcpm_from_inline_styles(&doc);
        assert_eq!(
            gcpm.page_settings.len(),
            2,
            "expected both <style> blocks' @page rules to be folded"
        );
    }

    #[test]
    fn multicol_props_absent_on_plain_block() {
        let html = r#"<html><body><div id="p">plain</div></body></html>"#;
        let doc = parse_and_layout(html, 400.0, 2000.0, &[], true);
        let id = find_element_by_local_name(&doc, "div").expect("div");
        assert!(extract_multicol_props(doc.get_node(id).unwrap()).is_none());
    }

    #[test]
    fn multicol_props_column_count() {
        let html = r#"<html><body>
            <div id="m" style="column-count: 3; column-gap: 12px;">a</div>
        </body></html>"#;
        let doc = parse_and_layout(html, 400.0, 2000.0, &[], true);
        let id = find_element_by_local_name(&doc, "div").expect("div");
        let props = extract_multicol_props(doc.get_node(id).unwrap()).expect("should be multicol");
        assert_eq!(props.column_count, Some(3));
        assert_eq!(props.column_width, None);
        assert!((props.column_gap - 12.0).abs() < 0.01);
    }

    #[test]
    fn multicol_props_column_width() {
        let html = r#"<html><body>
            <div id="m" style="column-width: 180px;">a</div>
        </body></html>"#;
        let doc = parse_and_layout(html, 400.0, 2000.0, &[], true);
        let id = find_element_by_local_name(&doc, "div").expect("div");
        let props = extract_multicol_props(doc.get_node(id).unwrap()).expect("should be multicol");
        assert_eq!(props.column_count, None);
        assert_eq!(props.column_width, Some(180.0));
        // CSS Multi-column Level 1: `column-gap: normal` is `1em`. At the
        // body's default 16px font-size, that lands at 16.
        assert!(
            (props.column_gap - 16.0).abs() < 0.01,
            "column-gap: normal should resolve to 1em (16px at default font), got {}",
            props.column_gap
        );
    }

    #[test]
    fn vertical_align_length_returns_pt_not_px() {
        use crate::paragraph::VerticalAlign;
        // vertical-align: 8px → 8 × 0.75 = 6pt. Prior to the fix this
        // returned 8.0 (CSS px), which then got subtracted from pt-denominated
        // baselines in paragraph.rs, producing a 4/3-off visual shift. Guards
        // against regression of the PR #101 unit-consolidation.
        let html = r#"<html><body><img style="vertical-align: 8px;" src=""></body></html>"#;
        let doc = parse_and_layout(html, 400.0, 2000.0, &[], true);
        let id = find_element_by_local_name(&doc, "img").expect("img");
        let va = extract_vertical_align(doc.get_node(id).unwrap());
        match va {
            VerticalAlign::Length(v) => {
                assert!((v - 6.0).abs() < 0.01, "expected 6pt (8px × 0.75), got {v}");
            }
            other => panic!("expected VerticalAlign::Length(6.0), got {other:?}"),
        }
    }

    #[test]
    fn vertical_align_percent_is_unit_agnostic_ratio() {
        use crate::paragraph::VerticalAlign;
        // `vertical-align: 50%` still returns a unitless ratio — the px→pt
        // fix on the Length branch must not touch Percent semantics.
        let html = r#"<html><body><img style="vertical-align: 50%;" src=""></body></html>"#;
        let doc = parse_and_layout(html, 400.0, 2000.0, &[], true);
        let id = find_element_by_local_name(&doc, "img").expect("img");
        let va = extract_vertical_align(doc.get_node(id).unwrap());
        match va {
            VerticalAlign::Percent(p) => {
                assert!((p - 0.5).abs() < 1e-4, "expected 0.5, got {p}");
            }
            other => panic!("expected VerticalAlign::Percent(0.5), got {other:?}"),
        }
    }

    #[test]
    fn column_span_all_detected() {
        let html = r#"<html><body>
            <h1 style="column-span: all;">Big</h1>
            <p>plain</p>
        </body></html>"#;
        let doc = parse_and_layout(html, 400.0, 2000.0, &[], true);
        let h1 = find_element_by_local_name(&doc, "h1").expect("h1");
        let p = find_element_by_local_name(&doc, "p").expect("p");
        assert!(has_column_span_all(doc.get_node(h1).unwrap()));
        assert!(!has_column_span_all(doc.get_node(p).unwrap()));
    }

    // ── extract_column_style_table (fulgur-v7a Task 2) ─────────────

    #[test]
    fn extract_column_style_table_picks_up_inline_and_stylesheet() {
        let html = r#"<html><head><style>
            .mc { column-fill: auto; column-rule: 1pt solid blue; }
        </style></head><body>
            <div class="mc" id="a"></div>
            <div class="mc" id="b" style="column-rule: 2pt dashed red"></div>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        resolve(&mut doc);
        use std::ops::Deref;
        let a = find_element_by_attr_id(doc.deref(), "a");
        let b = find_element_by_attr_id(doc.deref(), "b");

        let table = extract_column_style_table(&doc);

        // `a` picks up both declarations from the stylesheet.
        let a_props = table.get(&a).expect("a in table");
        assert_eq!(a_props.fill, Some(crate::column_css::ColumnFill::Auto));
        let a_rule = a_props.rule.expect("a rule");
        assert_eq!(a_rule.color, [0, 0, 255, 255]); // blue
        assert_eq!(a_rule.style, crate::column_css::ColumnRuleStyle::Solid);

        // `b` inherits `column-fill: auto` from the stylesheet, but its
        // inline `column-rule` beats the stylesheet's rule declaration.
        let b_props = table.get(&b).expect("b in table");
        let b_rule = b_props.rule.expect("b rule");
        assert!((b_rule.width - 2.0).abs() < 1e-3);
        assert_eq!(b_rule.style, crate::column_css::ColumnRuleStyle::Dashed);
        assert_eq!(b_rule.color, [255, 0, 0, 255]); // inline red beats stylesheet blue
        // Inline `column-rule` overrides only the rule field — `column-fill`
        // is still populated from the stylesheet.
        assert_eq!(b_props.fill, Some(crate::column_css::ColumnFill::Auto));
    }

    #[test]
    fn extract_column_style_table_respects_media_attribute() {
        // `<style media="screen">` must not feed the column side-table —
        // the PDF render path is the `print` medium. `<style media="print">`
        // and `<style media="all">` (or absent) must apply.
        let html = r#"<html><head>
            <style media="screen">
                .screen-only { column-rule: 1pt solid red; }
            </style>
            <style media="print">
                .print-only { column-rule: 2pt solid blue; }
            </style>
            <style media="all">
                .always { column-fill: auto; }
            </style>
            <style>
                .no-media { column-rule: 3pt solid green; }
            </style>
        </head><body>
            <div class="screen-only" id="s"></div>
            <div class="print-only" id="p"></div>
            <div class="always" id="a"></div>
            <div class="no-media" id="n"></div>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        resolve(&mut doc);
        use std::ops::Deref;
        let s = find_element_by_attr_id(doc.deref(), "s");
        let p = find_element_by_attr_id(doc.deref(), "p");
        let a = find_element_by_attr_id(doc.deref(), "a");
        let n = find_element_by_attr_id(doc.deref(), "n");

        let table = extract_column_style_table(&doc);

        // `screen` media: rule must NOT appear in the table.
        assert!(
            !table.contains_key(&s),
            "media=screen rule leaked into side-table"
        );
        // `print` media: rule applies.
        let p_rule = table.get(&p).and_then(|props| props.rule).expect("p rule");
        assert_eq!(p_rule.color, [0, 0, 255, 255]);
        // `all` media: fill applies.
        assert_eq!(
            table.get(&a).and_then(|props| props.fill),
            Some(crate::column_css::ColumnFill::Auto)
        );
        // No media attribute: rule applies (default = all).
        let n_rule = table.get(&n).and_then(|props| props.rule).expect("n rule");
        assert_eq!(n_rule.color, [0, 128, 0, 255]);
    }

    #[test]
    fn extract_column_style_table_populates_break_inside() {
        // Regression guard: `break-inside: avoid` declared in a <style> block
        // must survive the parser → ColumnStyleTable roundtrip keyed by node
        // id. column_css.rs is unit-tested directly; this test pins down the
        // blitz-dom integration so the property cannot silently get dropped
        // between parser and side-table.
        let html = r#"<!doctype html><html><head><style>
            .k { break-inside: avoid; }
        </style></head><body>
          <div class="k" id="k"></div>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        resolve(&mut doc);
        use std::ops::Deref;
        let k = find_element_by_attr_id(doc.deref(), "k");

        let table = extract_column_style_table(&doc);

        let props = table.get(&k).expect("k in table");
        assert_eq!(
            props.break_inside,
            Some(crate::draw_primitives::BreakInside::Avoid)
        );
    }

    #[test]
    fn parley_layout_line_heights_matches_lines_count_and_sum() {
        // Render a fixed paragraph at a fixed width through Blitz, recover
        // the parley layout via the inline-root NodeId, and inspect the
        // helper's output against parley::Line::metrics directly.
        let html = r#"<!doctype html><html><body><p style="width: 80px; font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p></body></html>"#;
        let mut doc = parse(html, 200.0, &[]);
        resolve(&mut doc);

        fn find_p(doc: &HtmlDocument, node_id: usize) -> Option<usize> {
            let n = doc.get_node(node_id)?;
            if let Some(elem) = n.element_data()
                && elem.name.local.as_ref() == "p"
            {
                return Some(node_id);
            }
            for &c in &n.children {
                if let Some(found) = find_p(doc, c) {
                    return Some(found);
                }
            }
            None
        }
        let p_id = find_p(&doc, doc.root_element().id).expect("<p> must exist");
        let parley_layout = &doc
            .get_node(p_id)
            .unwrap()
            .element_data()
            .unwrap()
            .inline_layout_data
            .as_ref()
            .unwrap()
            .layout;
        let heights = parley_line_heights(parley_layout);
        assert_eq!(heights.len(), parley_layout.lines().count());
        let sum: f32 = heights.iter().sum();
        let total_height = parley_layout
            .lines()
            .last()
            .map(|l| {
                let m = l.metrics();
                m.baseline + m.descent + m.leading
            })
            .unwrap_or(0.0);
        // Cross-validation between two formulas that should agree by
        // parley's design: cumulative `line_height` strides versus the
        // last line's `baseline + descent + leading`. Tolerance is
        // intentionally permissive because parley splits leading
        // between `leading_above` / `leading_below` differently across
        // platforms (macOS observed 1.0px drift for a 5-line layout)
        // and we only need to catch shape bugs (e.g. helper returning
        // `ascent` instead of `line_height`, which would diverge by
        // ~14px per line) — not fight font-metrics noise.
        let multi_line_drift_budget = (heights.len() as f32).max(1.0);
        assert!(
            (sum - total_height).abs() < multi_line_drift_budget,
            "sum of per-line heights {sum} should approximate total layout height {total_height} \
             (within ±{multi_line_drift_budget} for {} lines)",
            heights.len(),
        );
        assert!(heights.iter().all(|h| *h > 0.0));
    }
}

#[cfg(test)]
mod transform_tests {
    use super::*;
    use crate::draw_primitives::matrix_test_util::approx;
    use crate::draw_primitives::{Affine2D, Point2};

    /// Parse a minimal HTML snippet and return the computed transform of
    /// the first `<div>` it contains, via `compute_transform()`.
    fn compute_for_div(html: &str, box_w: f32, box_h: f32) -> Option<(Affine2D, Point2)> {
        let doc = parse_and_layout(html, 400.0, 2000.0, &[], true);
        let div_id = find_element_by_tag(&doc, "div")?;
        let node = doc.get_node(div_id)?;
        let styles = node.primary_styles()?;
        compute_transform(&styles, box_w, box_h)
    }

    #[test]
    fn no_transform_returns_none() {
        let html = r#"<!DOCTYPE html><html><body><div>hi</div></body></html>"#;
        assert!(compute_for_div(html, 100.0, 100.0).is_none());
    }

    #[test]
    fn translate_px_returns_translation_matrix() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate(10px, 20px)">hi</div>
        </body></html>"#;
        let (m, _) = compute_for_div(html, 100.0, 100.0).expect("should have transform");
        assert!(approx(m.e, 10.0));
        assert!(approx(m.f, 20.0));
        assert!(approx(m.a, 1.0));
        assert!(approx(m.d, 1.0));
    }

    #[test]
    fn translate_percent_resolves_against_border_box() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate(50%, 25%)">hi</div>
        </body></html>"#;
        let (m, _) = compute_for_div(html, 200.0, 80.0).expect("should have transform");
        assert!(approx(m.e, 100.0), "expected 100 (50% of 200), got {}", m.e);
        assert!(approx(m.f, 20.0), "expected 20 (25% of 80), got {}", m.f);
    }

    #[test]
    fn matrix_is_preserved_verbatim() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: matrix(1, 2, 3, 4, 5, 6)">hi</div>
        </body></html>"#;
        let (m, _) = compute_for_div(html, 100.0, 100.0).expect("should have transform");
        assert!(approx(m.a, 1.0));
        assert!(approx(m.b, 2.0));
        assert!(approx(m.c, 3.0));
        assert!(approx(m.d, 4.0));
        assert!(approx(m.e, 5.0));
        assert!(approx(m.f, 6.0));
    }

    #[test]
    fn origin_default_is_center() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: rotate(45deg)">hi</div>
        </body></html>"#;
        let (_, origin) = compute_for_div(html, 100.0, 60.0).expect("should have transform");
        assert!(
            approx(origin.x, 50.0),
            "default origin x should be 50% of 100, got {}",
            origin.x
        );
        assert!(
            approx(origin.y, 30.0),
            "default origin y should be 50% of 60, got {}",
            origin.y
        );
    }

    #[test]
    fn identity_transform_returns_none() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate(0, 0)">hi</div>
        </body></html>"#;
        assert!(compute_for_div(html, 100.0, 100.0).is_none());
    }

    #[test]
    fn three_d_op_folds_to_identity_and_is_suppressed() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate3d(0, 0, 50px)">hi</div>
        </body></html>"#;
        assert!(compute_for_div(html, 100.0, 100.0).is_none());
    }

    #[test]
    fn rotate_z_is_treated_as_2d_rotation() {
        // CSS spec: rotateZ(angle) is equivalent to rotate(angle).
        // Both must produce the same 2D rotation matrix, not fall back
        // to identity through the 3D arm.
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: rotateZ(90deg); transform-origin: 0 0">hi</div>
        </body></html>"#;
        let (m, _) = compute_for_div(html, 100.0, 100.0).expect("rotateZ should produce a wrapper");
        // 90° rotation: (1, 0) → (0, 1).
        let x = m.a * 1.0 + m.c * 0.0 + m.e;
        let y = m.b * 1.0 + m.d * 0.0 + m.f;
        assert!(approx(x, 0.0), "x expected 0.0, got {x}");
        assert!(approx(y, 1.0), "y expected 1.0, got {y}");
    }
}

#[cfg(test)]
mod marker_rewrite_tests {
    use super::*;

    /// Locate the first element with the given local tag name in
    /// document order. Mirrors `tests::find_element_by_local_name` —
    /// duplicated here because each `#[cfg(test)] mod` is a separate
    /// scope and the helper isn't worth promoting to the crate API.
    fn find_first_element(doc: &HtmlDocument, name: &str) -> Option<usize> {
        fn walk(doc: &blitz_dom::BaseDocument, id: usize, name: &str) -> Option<usize> {
            let node = doc.get_node(id)?;
            if let Some(ed) = node.element_data() {
                if ed.name.local.as_ref() == name {
                    return Some(id);
                }
            }
            for &c in &node.children {
                if let Some(v) = walk(doc, c, name) {
                    return Some(v);
                }
            }
            None
        }
        use std::ops::Deref;
        walk(doc.deref(), doc.root_element().id, name)
    }

    #[test]
    fn test_rewrite_marker_content_url_simple() {
        let css = r#"li::marker { content: url("star.png"); }"#;
        let result = rewrite_marker_content_url(css);
        assert!(result.contains("::marker"), "original rule preserved");
        assert!(result.contains("list-style-image"), "new rule appended");
        assert!(result.contains("star.png"), "URL preserved");
    }

    #[test]
    fn test_rewrite_marker_content_url_compound_selector() {
        let css = r#".list li.custom::marker { content: url("check.svg"); }"#;
        let result = rewrite_marker_content_url(css);
        assert!(result.contains(".list li.custom") && result.contains("list-style-image"));
    }

    #[test]
    fn test_rewrite_marker_content_url_no_marker_passthrough() {
        let css = "p { color: red; }";
        let result = rewrite_marker_content_url(css);
        assert_eq!(result, css);
    }

    #[test]
    fn test_rewrite_marker_content_url_non_url_content_ignored() {
        let css = r#"li::marker { content: "→ "; }"#;
        let result = rewrite_marker_content_url(css);
        assert!(!result.contains("list-style-image"));
    }

    #[test]
    fn test_rewrite_marker_content_url_at_media() {
        let css = r#"@media print { li::marker { content: url("print-bullet.png"); } }"#;
        let result = rewrite_marker_content_url(css);

        // The original @media block must remain intact.
        assert!(
            result.starts_with(css),
            "original CSS must be preserved at the start, got:\n{result}"
        );

        // The generated list-style-image rule must appear AFTER the
        // original @media block, wrapped in its own @media print { }.
        let suffix = &result[css.len()..];
        assert!(
            suffix.contains("@media print"),
            "generated rule must be wrapped in @media print, suffix:\n{suffix}"
        );
        assert!(
            suffix.contains("li{list-style-image:url("),
            "generated rule must contain li{{list-style-image:...}}, suffix:\n{suffix}"
        );

        // There must be exactly two @media print occurrences — the original
        // and the generated one — proving there is no double-wrapping.
        let count = result.matches("@media print").count();
        assert_eq!(
            count, 2,
            "expected exactly 2 @media print occurrences (original + generated), got {count}\nresult:\n{result}"
        );
    }

    #[test]
    fn test_rewrite_marker_content_url_preserves_other_rules() {
        let css =
            "h1 { font-size: 2em; }\nli::marker { content: url(\"icon.png\"); }\np { margin: 0; }";
        let result = rewrite_marker_content_url(css);
        assert!(result.contains("h1 { font-size: 2em; }"));
        assert!(result.contains("p { margin: 0; }"));
        assert!(result.contains("list-style-image"));
    }

    #[test]
    fn test_rewrite_marker_content_url_with_charset() {
        let css = r#"@charset "UTF-8"; li::marker { content: url("star.png"); }"#;
        let result = rewrite_marker_content_url(css);
        assert!(
            result.contains("list-style-image"),
            "should work with @charset prefix, got: {result}"
        );
        assert!(
            result.contains(r#"@charset "UTF-8";"#),
            "charset rule preserved"
        );
    }

    #[test]
    fn test_rewrite_marker_content_url_with_import() {
        let css = r#"@import url("base.css"); li::marker { content: url("icon.png"); }"#;
        let result = rewrite_marker_content_url(css);
        assert!(
            result.contains("list-style-image"),
            "should work with @import prefix, got: {result}"
        );
    }

    #[test]
    fn test_rewrite_marker_content_url_in_html_rewrites_style() {
        let html = r#"<html><head><style>
li::marker { content: url("star.png"); }
</style></head><body><ul><li>x</li></ul></body></html>"#;
        let result = rewrite_marker_content_url_in_html(html);
        assert!(
            result.contains("list-style-image"),
            "should rewrite inside <style>, got: {result}"
        );
    }

    #[test]
    fn test_rewrite_marker_content_url_in_html_no_style_passthrough() {
        let html = "<html><body><p>Hello</p></body></html>";
        let result = rewrite_marker_content_url_in_html(html);
        assert_eq!(result, html);
    }

    #[test]
    fn test_rewrite_marker_content_url_in_html_multiple_style_blocks() {
        let html = r#"<html><head>
<style>p { color: red; }</style>
<style>li::marker { content: url("a.png"); }</style>
</head><body><ul><li>x</li></ul></body></html>"#;
        let result = rewrite_marker_content_url_in_html(html);
        assert!(
            result.contains("list-style-image"),
            "second style block rewritten"
        );
        assert!(
            result.contains("p { color: red; }"),
            "first style block preserved"
        );
    }

    #[test]
    fn test_extract_content_url_quoted_parens() {
        let url = extract_content_url(r#"content: url("image(1).png")"#);
        assert_eq!(
            url.as_deref(),
            Some("\"image(1).png\""),
            "should handle parentheses inside quoted URL"
        );
    }

    #[test]
    fn test_rewrite_marker_content_url_bare_marker_selector() {
        // A bare `::marker` selector (no element) should not produce an
        // empty-selector rule like `{list-style-image:...}`.
        let css = r#"::marker { content: url("star.png"); }"#;
        let result = rewrite_marker_content_url(css);
        assert!(
            !result.contains("\n{"),
            "bare ::marker should not produce empty-selector rule, got: {result}"
        );
    }

    #[test]
    fn test_rewrite_marker_content_url_in_html_uppercase_style_with_attrs() {
        let html = r#"<html><head><STYLE type="text/css">
li::marker { content: url("star.png"); }
</STYLE></head><body></body></html>"#;
        let result = rewrite_marker_content_url_in_html(html);
        assert!(
            result.contains("list-style-image"),
            "should handle uppercase STYLE with attributes, got: {result}"
        );
    }

    // ─── BookmarkPass ──────────────────────────────────────────────

    fn run_bookmark_pass(html: &str, mappings: Vec<BookmarkMapping>) -> Vec<(usize, BookmarkInfo)> {
        let mut doc = parse(html, 400.0, &[]);
        let pass = BookmarkPass::new(mappings);
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        pass.into_results()
    }

    #[test]
    fn bookmark_pass_matches_class_selector() {
        let html = r#"<html><body><div class="ch" data-title="Intro">X</div></body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![BookmarkMapping {
                selector: ParsedSelector::Class("ch".into()),
                level: Some(BookmarkLevel::Integer(1)),
                label: Some(vec![ContentItem::Attr("data-title".into())]),
            }],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.level, 1);
        assert_eq!(results[0].1.label, "Intro");
    }

    #[test]
    fn bookmark_pass_resolves_content_text() {
        let html = r#"<html><body><h2>Hello World</h2></body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![BookmarkMapping {
                selector: ParsedSelector::Tag("h2".into()),
                level: Some(BookmarkLevel::Integer(2)),
                label: Some(vec![ContentItem::ContentText]),
            }],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.level, 2);
        assert_eq!(results[0].1.label, "Hello World");
    }

    #[test]
    fn bookmark_pass_resolves_literal_and_mixed() {
        let html = r#"<html><body>
            <section class="ch" data-num="1"><h2>Intro</h2></section>
        </body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![BookmarkMapping {
                selector: ParsedSelector::Class("ch".into()),
                level: Some(BookmarkLevel::Integer(1)),
                label: Some(vec![
                    ContentItem::String("Ch. ".into()),
                    ContentItem::Attr("data-num".into()),
                    ContentItem::String(": ".into()),
                    ContentItem::ContentText,
                ]),
            }],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.level, 1);
        assert_eq!(results[0].1.label, "Ch. 1: Intro");
    }

    #[test]
    fn bookmark_pass_unset_counter_resolves_to_zero() {
        use crate::gcpm::CounterStyle;

        let html = r#"<html><body><h1>Title</h1></body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![BookmarkMapping {
                selector: ParsedSelector::Tag("h1".into()),
                level: Some(BookmarkLevel::Integer(1)),
                label: Some(vec![
                    ContentItem::Counter {
                        name: "chapter".into(),
                        style: CounterStyle::Decimal,
                    },
                    ContentItem::String(": ".into()),
                    ContentItem::ContentText,
                ]),
            }],
        );
        assert_eq!(results.len(), 1);
        // CSS spec: undefined counter resolves to 0.
        // BookmarkPass::new (no snapshots) ⇒ counter() falls back to 0.
        assert_eq!(results[0].1.label, "0: Title");
    }

    #[test]
    fn bookmark_pass_resolves_counter_with_snapshot() {
        use crate::gcpm::CounterStyle;
        use std::collections::BTreeMap;

        let html = r#"<html><body><h1>Title</h1></body></html>"#;
        let mappings = vec![BookmarkMapping {
            selector: ParsedSelector::Tag("h1".into()),
            level: Some(BookmarkLevel::Integer(1)),
            label: Some(vec![
                ContentItem::Counter {
                    name: "chapter".into(),
                    style: CounterStyle::Decimal,
                },
                ContentItem::String(". ".into()),
                ContentItem::ContentText,
            ]),
        }];
        let mut doc = parse(html, 400.0, &[]);
        let h1 = find_first_element(&doc, "h1").expect("h1 in fixture");

        let mut counter_snap = BTreeMap::new();
        let mut h1_state = BTreeMap::new();
        h1_state.insert("chapter".to_string(), vec![1]);
        counter_snap.insert(h1, h1_state);

        let pass = BookmarkPass::new_with_snapshots(mappings, counter_snap, BTreeMap::new());
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let results = pass.into_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.label, "1. Title");
    }

    #[test]
    fn bookmark_pass_resolves_string_ref_with_snapshot() {
        use crate::gcpm::StringPolicy;
        use std::collections::BTreeMap;

        let html = r#"<html><body><h1>Body Heading</h1></body></html>"#;
        let mappings = vec![BookmarkMapping {
            selector: ParsedSelector::Tag("h1".into()),
            level: Some(BookmarkLevel::Integer(1)),
            label: Some(vec![ContentItem::StringRef {
                name: "section".into(),
                policy: StringPolicy::First,
            }]),
        }];
        let mut doc = parse(html, 400.0, &[]);
        let h1 = find_first_element(&doc, "h1").expect("h1 in fixture");

        let mut string_snap = BTreeMap::new();
        let mut h1_state = BTreeMap::new();
        h1_state.insert("section".to_string(), "Intro".to_string());
        string_snap.insert(h1, h1_state);

        let pass = BookmarkPass::new_with_snapshots(mappings, BTreeMap::new(), string_snap);
        let ctx = PassContext { font_data: &[] };
        pass.apply(&mut doc, &ctx);
        let results = pass.into_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.label, "Intro");
    }

    #[test]
    fn bookmark_pass_none_suppresses_entry() {
        let html = r#"<html><body><h1>Title</h1></body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![
                BookmarkMapping {
                    selector: ParsedSelector::Tag("h1".into()),
                    level: Some(BookmarkLevel::Integer(1)),
                    label: Some(vec![ContentItem::ContentText]),
                },
                BookmarkMapping {
                    selector: ParsedSelector::Tag("h1".into()),
                    level: Some(BookmarkLevel::None_),
                    label: None,
                },
            ],
        );
        assert!(
            results.is_empty(),
            "bookmark-level: none must suppress the entry, got: {results:?}"
        );
    }

    #[test]
    fn bookmark_pass_fallback_label_when_level_only() {
        let html = r#"<html><body><div class="aside">Note text</div></body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![BookmarkMapping {
                selector: ParsedSelector::Class("aside".into()),
                level: Some(BookmarkLevel::Integer(2)),
                label: None,
            }],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.level, 2);
        assert_eq!(results[0].1.label, "Note text");
    }

    #[test]
    fn bookmark_pass_cascade_last_wins() {
        let html = r#"<html><body><h1>Heading</h1></body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![
                BookmarkMapping {
                    selector: ParsedSelector::Tag("h1".into()),
                    level: Some(BookmarkLevel::Integer(1)),
                    label: Some(vec![ContentItem::String("A".into())]),
                },
                BookmarkMapping {
                    selector: ParsedSelector::Tag("h1".into()),
                    level: Some(BookmarkLevel::Integer(2)),
                    label: Some(vec![ContentItem::String("B".into())]),
                },
            ],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.level, 2);
        assert_eq!(results[0].1.label, "B");
    }

    #[test]
    fn bookmark_pass_no_mappings_is_noop() {
        let html = r#"<html><body><h1>Title</h1></body></html>"#;
        let results = run_bookmark_pass(html, vec![]);
        assert!(results.is_empty());
    }

    #[test]
    fn bookmark_pass_skips_non_visual_tags() {
        // <style> content must not leak into the bookmark label even when
        // a broad `*` or matching tag selector hits it.
        let html = r#"<html><head><style>h1 { color: red; }</style></head>
            <body><h1>Heading</h1></body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![BookmarkMapping {
                selector: ParsedSelector::Tag("style".into()),
                level: Some(BookmarkLevel::Integer(1)),
                label: Some(vec![ContentItem::ContentText]),
            }],
        );
        assert!(
            results.is_empty(),
            "<style> is a non-visual tag and must be skipped"
        );
    }

    #[test]
    fn bookmark_pass_label_only_without_level_is_skipped() {
        // A mapping with only `bookmark-label` and no level is inert —
        // GCPM requires a level to emit an outline entry.
        let html = r#"<html><body><h1>Title</h1></body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![BookmarkMapping {
                selector: ParsedSelector::Tag("h1".into()),
                level: None,
                label: Some(vec![ContentItem::ContentText]),
            }],
        );
        assert!(results.is_empty());
    }

    #[test]
    fn bookmark_pass_skips_entry_when_resolved_label_is_empty() {
        // Regression guard: the previous hardcoded h1-h6 path in
        // `convert.rs::maybe_wrap_heading` bailed out when the extracted
        // text was empty, so `<h1></h1>` produced no outline entry. The
        // CSS-driven path must preserve that behaviour — emitting an
        // outline node with an empty title is observable but silent.

        // Case 1: `<h1></h1>` with the UA-style `bookmark-label: content()`
        // resolves to "" and must not emit an entry.
        let html = r#"<html><body><h1></h1></body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![BookmarkMapping {
                selector: ParsedSelector::Tag("h1".into()),
                level: Some(BookmarkLevel::Integer(1)),
                label: Some(vec![ContentItem::ContentText]),
            }],
        );
        assert!(
            results.is_empty(),
            "empty content() must skip the outline entry, got: {results:?}"
        );

        // Case 2: level-only rule on an empty element — label falls back
        // to `extract_text_content`, which is also "", so the entry must
        // still be skipped.
        let html = r#"<html><body><div class="ch"></div></body></html>"#;
        let results = run_bookmark_pass(
            html,
            vec![BookmarkMapping {
                selector: ParsedSelector::Class("ch".into()),
                level: Some(BookmarkLevel::Integer(1)),
                label: None,
            }],
        );
        assert!(
            results.is_empty(),
            "empty text-content fallback must skip the outline entry, got: {results:?}"
        );
    }
}

#[cfg(test)]
mod marker_helper_tests {
    use super::*;

    #[test]
    fn marker_to_string_char_returns_single_char_string() {
        let m = Marker::Char('•');
        assert_eq!(marker_to_string(&m), "•");
    }

    #[test]
    fn marker_to_string_string_returns_owned_clone() {
        let m = Marker::String("1.".to_string());
        assert_eq!(marker_to_string(&m), "1.");
    }

    #[test]
    fn marker_skrifa_text_char_appends_trailing_space() {
        let m = Marker::Char('•');
        assert_eq!(marker_skrifa_text(&m), "• ");
    }

    #[test]
    fn marker_skrifa_text_string_keeps_as_is_no_trailing_space() {
        // Marker::String は既に "1. " のように trailing space を含むケースを想定するため、
        // helper では追加のスペースを付けない（list_marker.rs:336-339 と同等）。
        let m = Marker::String("1.".to_string());
        assert_eq!(marker_skrifa_text(&m), "1.");
    }
}

#[cfg(test)]
mod list_position_helper_tests {
    use super::*;

    #[test]
    fn list_position_outside_layout_returns_none_for_inside() {
        // ListItemLayoutPosition::Outside の生成は parley::Layout<TextBrush> を Box で持つため
        // ユニットテストで構築するのが煩雑（parley::Layout は private な builder 経由）。
        // ここでは "Inside の場合 None を返す" 性質だけ確認し、Outside の挙動は VRT に任せる。
        assert!(list_position_outside_layout(&ListItemLayoutPosition::Inside).is_none());
    }

    #[test]
    fn is_list_position_inside_returns_true_for_inside() {
        assert!(is_list_position_inside(&ListItemLayoutPosition::Inside));
    }
}

#[cfg(test)]
mod extract_html_title_tests {
    use super::*;

    fn parse(html: &str, width: f32, _resources: &[()]) -> HtmlDocument {
        crate::blitz_adapter::parse(html, width, &[])
    }

    #[test]
    fn extract_html_title_finds_title_element() {
        let doc = parse(
            r#"<html><head><title>My Document</title></head><body></body></html>"#,
            600.0,
            &[],
        );
        assert_eq!(
            super::extract_html_title(&doc),
            Some("My Document".to_string())
        );
    }

    #[test]
    fn extract_html_title_returns_none_when_absent() {
        let doc = parse(
            r#"<html><head></head><body><p>No title here</p></body></html>"#,
            600.0,
            &[],
        );
        assert_eq!(super::extract_html_title(&doc), None);
    }

    #[test]
    fn extract_html_title_trims_whitespace() {
        let doc = parse(
            r#"<html><head><title>  Padded Title  </title></head><body></body></html>"#,
            600.0,
            &[],
        );
        assert_eq!(
            super::extract_html_title(&doc),
            Some("Padded Title".to_string())
        );
    }

    #[test]
    fn extract_html_title_returns_none_for_empty_title() {
        let doc = parse(
            r#"<html><head><title>   </title></head><body></body></html>"#,
            600.0,
            &[],
        );
        assert_eq!(super::extract_html_title(&doc), None);
    }

    #[test]
    fn extract_html_title_ignores_svg_title() {
        let doc = parse(
            r#"<html><head></head><body><svg><title>SVG Title</title></svg></body></html>"#,
            600.0,
            &[],
        );
        assert_eq!(super::extract_html_title(&doc), None);
    }
}
