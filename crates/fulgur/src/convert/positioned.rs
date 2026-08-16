use super::*;
use crate::units::{F32Units, Pt, Px};

/// Walk a parent node's child id list, recursing into each via
/// `convert_node`. Replaces v1's `collect_positioned_children`: instead of
/// building a `Vec<PositionedChild>` (with separate in-flow / out-of-flow
/// slots and orphan-marker emission), Drawables-backed v2 simply records
/// every child's NodeId in the appropriate per-NodeId map. Position is
/// derived later from `pagination_layout::PaginationGeometryTable`, which
/// the fragmenter populates from the same DOM.
///
/// Skip rules mirror v1's filters so VRT byte-equality holds:
///   - HTML comments and `<head>`/`<script>`/`<style>` are not visited.
///   - Absolutely-positioned children are visited via the parent's
///     pseudo / abs walk (`pseudo::register_pseudo_content`), not here,
///     to keep the registration order identical to v1's
///     `build_absolute_*_children` paths.
pub(super) fn walk_children_into_drawables(
    doc: &BaseDocument,
    child_ids: &[usize],
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    for &child_id in child_ids {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };
        if matches!(&child_node.data, NodeData::Comment) {
            continue;
        }
        if is_non_visual_element(child_node) {
            continue;
        }
        // Absolutely-positioned descendants are visited by the parent's
        // `register_pseudo_content` pass to preserve v1's ordering
        // (pseudo before / abs / pseudo after).
        if is_absolutely_positioned(child_node) {
            continue;
        }
        convert_node(doc, child_id, ctx, depth + 1, out);
    }
}

/// Whether `node`'s computed `position` is `absolute` or `fixed`.
pub(super) fn is_absolutely_positioned(node: &Node) -> bool {
    node.primary_styles()
        .is_some_and(|s| s.get_box().clone_position().is_absolutely_positioned())
}

/// Whether `node`'s computed `position` is `fixed` (as opposed to `absolute`).
fn is_position_fixed(node: &Node) -> bool {
    use ::style::properties::longhands::position::computed_value::T as Pos;
    node.primary_styles()
        .is_some_and(|s| matches!(s.get_box().clone_position(), Pos::Fixed))
}

/// Whether `node`'s computed `position` is `static`.
fn is_position_static(node: &Node) -> bool {
    use ::style::properties::longhands::position::computed_value::T as Pos;
    node.primary_styles()
        .is_none_or(|s| matches!(s.get_box().clone_position(), Pos::Static))
}

/// Resolved containing block for an absolutely-positioned descendant.
///
/// Carried through `walk_absolute_pseudo_children` so the textless
/// `content:url()` shortcut can still receive the right percentage basis
/// for sizing. The remaining inset-resolution math from v1 was deleted
/// when convert moved to Drawables; the render path consults
/// `pagination_geometry` for absolute positioning instead.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(super) struct AbsCb {
    pub(super) padding_box_size: (Px, Px),
    pub(super) border_top_left: (Px, Px),
    pub(super) parent_offset_in_cb_bp: (f32, f32),
}

fn cb_padding_box(node: &Node) -> ((Px, Px), (Px, Px)) {
    // Reads border widths straight from Taffy's layout instead of routing
    // through `extract_block_style`, which also clones/iterates
    // attacker-controlled `box-shadow` lists. `resolve_cb_for_absolute` calls
    // this per ancestor per node with an absolute pseudo, so a full-style
    // extraction here amplifies box-shadow processing by the ancestor walk.
    let border = node.final_layout.border;
    let sz = node.final_layout.size;
    let pb_w = (sz.width - border.left - border.right).max(0.0).as_px();
    let pb_h = (sz.height - border.top - border.bottom).max(0.0).as_px();
    ((pb_w, pb_h), (border.left.as_px(), border.top.as_px()))
}

fn resolve_cb_for_absolute(
    doc: &BaseDocument,
    parent: &Node,
    is_fixed: bool,
    viewport_size_px: Option<(f32, f32)>,
) -> Option<AbsCb> {
    let mut offset_x = 0.0_f32;
    let mut offset_y = 0.0_f32;
    let mut cur_id = Some(parent.id);
    let mut body_fallback: Option<AbsCb> = None;
    let mut depth: usize = 0;

    while let Some(id) = cur_id {
        if depth >= MAX_DOM_DEPTH {
            break;
        }
        let Some(cur) = doc.get_node(id) else {
            break;
        };
        if !is_fixed && !is_position_static(cur) {
            let (padding_box_size, border_top_left) = cb_padding_box(cur);
            return Some(AbsCb {
                padding_box_size,
                border_top_left,
                parent_offset_in_cb_bp: (offset_x, offset_y),
            });
        }
        if let Some(elem) = cur.element_data()
            && elem.name.local.as_ref() == "body"
        {
            let (mut padding_box_size, border_top_left) = cb_padding_box(cur);
            if let Some((vw, vh)) = viewport_size_px {
                if padding_box_size.0 <= Px::ZERO {
                    padding_box_size.0 = vw.as_px();
                }
                if padding_box_size.1 <= Px::ZERO {
                    padding_box_size.1 = vh.as_px();
                }
            }
            body_fallback = Some(AbsCb {
                padding_box_size,
                border_top_left,
                parent_offset_in_cb_bp: (offset_x, offset_y),
            });
        }
        offset_x += cur.final_layout.location.x;
        offset_y += cur.final_layout.location.y;
        cur_id = cur.parent;
        depth += 1;
    }
    body_fallback
}

// PR 8i regression fix (`pseudo_absolute_content_url::
// absolute_pseudo_with_right_bottom_offsets_by_image_size`):
// `resolve_inset_px` is back, narrowly scoped to textless
// `content: url(...)` abs pseudos whose CB is not their nearest Taffy
// parent. Taffy alone resolves `right` / `bottom` against the pseudo's
// `final_layout.size`, which is `(0, 0)` for textless pseudos — so the
// inset shifts the image by its own w/h. v1 worked around this in
// `build_absolute_pseudo_children`; v2 re-applies the correction by
// writing into `ctx.pagination_geometry` here so the render path
// (which consults the table verbatim) sees the corrected fragment.
//
// We only re-introduce the math that the test requires: explicit `right`
// / `bottom` resolution against the CB's padding-box width/height, with
// the pseudo's effective size taken from the already-built
// `ImageEntry` (which honours the explicit `width`/`height` in
// `build_pseudo_image_entry`). `left`/`top` resolution is left to
// Taffy because it gets that case correct (CB anchor is the parent's
// origin, no size dependence).
fn resolve_inset_px(
    inset: &::style::values::computed::position::Inset,
    basis_px: Px,
) -> Option<Px> {
    use ::style::values::computed::Length;
    use ::style::values::generics::position::GenericInset;
    match inset {
        GenericInset::LengthPercentage(lp) => {
            Some(lp.resolve(Length::new(basis_px.to_f32())).px().as_px())
        }
        _ => None,
    }
}

/// PR 8i regression fix: write a corrected fragment into
/// `pagination_geometry` for a textless `content: url(...)` abs pseudo
/// whose `right` or `bottom` inset was specified.
///
/// `pseudo_w` / `pseudo_h` (pt) come from the just-built `ImageEntry`,
/// which sized the image from the pseudo's CSS `width` / `height`
/// (via `build_pseudo_image_entry`). For pseudos that didn't set a
/// `right` or `bottom` inset, this is a no-op — Taffy's location is
/// already correct in those cases (CB anchor at parent's origin).
///
/// Coordinates flow:
///   - CB padding-box w/h in CSS px (from `cb.padding_box_size`)
///   - Resolve `right`/`bottom` insets against those (CSS 2.1 §10.3.7
///     / §10.6.4 over-constrained: start-side wins, so this only
///     fires when `left`/`top` is `auto`).
///   - Translate from CB padding-box frame → CB border-box frame →
///     parent's frame (subtracting the parent's body-relative offset
///     in CB frame and the parent's body-relative position).
///   - Fragment is written in body-relative CSS px to match every
///     other Fragment in the table.
fn maybe_apply_abs_pseudo_inset_correction(
    pseudo: &Node,
    pseudo_id: usize,
    parent_id: usize,
    cb: AbsCb,
    pseudo_w: Pt,
    pseudo_h: Pt,
    ctx: &mut ConvertContext<'_>,
) {
    // Defer to `append_position_fixed_fragments` for `position: fixed`:
    // that pass writes per-page repeated fragments (`is_repeat = true`)
    // and our single-fragment overwrite would clobber the repetition.
    // Production runs `append_position_fixed_fragments` BEFORE convert
    // (engine.rs), so a fixed pseudo's geometry is already established
    // by the time we get here. Inset correction for `position: fixed`
    // pseudos is its own follow-up.
    if is_position_fixed(pseudo) {
        return;
    }
    let Some(styles) = pseudo.primary_styles() else {
        return;
    };
    let pos = styles.get_position();
    let (cb_w_px, cb_h_px) = cb.padding_box_size;
    let left = resolve_inset_px(&pos.left, cb_w_px);
    let top = resolve_inset_px(&pos.top, cb_h_px);
    let right = resolve_inset_px(&pos.right, cb_w_px);
    let bottom = resolve_inset_px(&pos.bottom, cb_h_px);

    // Skip when neither right nor bottom is set — Taffy already
    // produced the right answer for left/top anchors. Also skip when
    // the start-side inset wins per §10.3.7 / §10.6.4 over-constrained
    // resolution, because Taffy already honoured the start-side value.
    let needs_right = right.is_some() && left.is_none();
    let needs_bottom = bottom.is_some() && top.is_none();
    if !needs_right && !needs_bottom {
        return;
    }

    // The pseudo's existing fragment (written by the fragmenter from
    // Taffy's location) gives us the parent's body-relative origin
    // implicitly: parent.fragment.x + (pseudo's offset relative to
    // parent in border-box frame). We rebuild from scratch using the
    // parent's recorded fragment plus the corrected CB-padding-box →
    // parent translation.
    let Some(parent_geom) = ctx.pagination_geometry.get(&parent_id) else {
        return;
    };
    // Preserve the page the fragmenter originally placed the pseudo on
    // when the parent splits across pages. Falling back to
    // `parent_geom.fragments.first()` would always reattach the
    // correction to the parent's first page, even when the pseudo was
    // legitimately placed on a later one.
    let pseudo_page = ctx
        .pagination_geometry
        .get(&pseudo_id)
        .and_then(|g| g.fragments.first())
        .map(|f| f.page_index);
    let Some(parent_frag) = pseudo_page
        .and_then(|page| {
            parent_geom
                .fragments
                .iter()
                .find(|f| f.page_index == page)
                .cloned()
        })
        .or_else(|| parent_geom.fragments.first().cloned())
    else {
        return;
    };
    let parent_x_px = parent_frag.x;
    let parent_y_px = parent_frag.y;

    let pseudo_w_px = pseudo_w.in_px();
    let pseudo_h_px = pseudo_h.in_px();

    // CSS 2.1 §10.3.7 / §10.6.4: when start-side is auto, end-side
    // determines position. Use the pseudo's effective image size
    // (NOT Taffy's `final_layout.size`, which is `(0, 0)` for
    // textless content:url pseudos).
    let x_in_pp_px = if needs_right {
        // right is Some, left is None
        cb_w_px - pseudo_w_px - right.unwrap()
    } else {
        // left is Some (or both auto -> 0)
        left.unwrap_or(Px::ZERO)
    };
    let y_in_pp_px = if needs_bottom {
        cb_h_px - pseudo_h_px - bottom.unwrap()
    } else {
        top.unwrap_or(Px::ZERO)
    };

    // Padding-box frame → CB border-box frame → parent's frame.
    // `cb.parent_offset_in_cb_bp` is the parent's offset within CB's
    // border-box frame (accumulated `final_layout.location` while
    // resolve_cb_for_absolute climbed). For the simple "parent IS the
    // CB" case this is `(0, 0)`.
    let (bl_px, bt_px) = cb.border_top_left;
    let (ox_px, oy_px) = cb.parent_offset_in_cb_bp;
    let pseudo_local_x_px = x_in_pp_px + bl_px - ox_px.as_px();
    let pseudo_local_y_px = y_in_pp_px + bt_px - oy_px.as_px();

    let new_x_px = parent_x_px + pseudo_local_x_px;
    let new_y_px = parent_y_px + pseudo_local_y_px;

    // Replace any existing fragment(s) — Taffy's geometry is wrong
    // for this case, our correction is the source of truth.
    let entry = ctx.pagination_geometry.entry(pseudo_id).or_default();
    entry.fragments.clear();
    entry.fragments.push(crate::pagination_layout::Fragment {
        page_index: parent_frag.page_index,
        x: new_x_px,
        y: new_y_px,
        width: pseudo_w_px,
        height: pseudo_h_px,
    });
}

/// Walk `::before` / `::after` pseudo slots whose computed `position` is
/// `absolute` or `fixed` and recurse into them via `convert_node`.
///
/// Position information is no longer carried out-of-band on a `PositionedChild`
/// — render time derives the geometry from `pagination_layout` /
/// `multicol_layout` / Taffy's `final_layout`. This helper still walks the
/// pseudos so they register into Drawables; the (x, y) override math from
/// v1 is left to a follow-up that consults the same fragmenter table.
pub(super) fn walk_absolute_pseudo_children(
    doc: &BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    slots: &[Option<usize>],
    out: &mut crate::drawables::Drawables,
) {
    let parent_is_static = is_position_static(node);
    let mut cb_absolute: Option<Option<AbsCb>> = None;
    let mut cb_fixed: Option<Option<AbsCb>> = None;
    for pseudo_id in slots.iter().copied().flatten() {
        let Some(pseudo) = doc.get_node(pseudo_id) else {
            continue;
        };
        if !is_absolutely_positioned(pseudo) {
            continue;
        }
        // Resolve CB so the textless `content:url()` shortcut downstream
        // can use it. We compute it here but discard — the override is
        // applied at render time once the fragmenter records the pseudo's
        // final fragment position.
        let _cb = if is_position_fixed(pseudo) {
            *cb_fixed.get_or_insert_with(|| {
                resolve_cb_for_absolute(doc, node, true, ctx.viewport_size_px)
            })
        } else if parent_is_static {
            *cb_absolute.get_or_insert_with(|| {
                resolve_cb_for_absolute(doc, node, false, ctx.viewport_size_px)
            })
        } else {
            let (padding_box_size, border_top_left) = cb_padding_box(node);
            Some(AbsCb {
                padding_box_size,
                border_top_left,
                parent_offset_in_cb_bp: (0.0, 0.0),
            })
        };
        // Try the textless content:url shortcut; if it produces an image
        // entry, record it directly. Otherwise recurse via `convert_node`.
        if let Some(img) = try_build_absolute_pseudo_image(pseudo, node, _cb, ctx.assets) {
            // PR 8i regression fix: when the pseudo specifies `right` /
            // `bottom`, Taffy resolves them against
            // `pseudo.final_layout.size = (0, 0)` (textless pseudos)
            // and shifts the image off by its own w/h. Re-apply the
            // inset against the pseudo's effective image size and
            // overwrite the fragmenter's wrong placement.
            let (img_w, img_h) = (img.width, img.height);
            out.images.insert(pseudo_id, img);
            if let Some(cb_resolved) = _cb {
                maybe_apply_abs_pseudo_inset_correction(
                    pseudo,
                    pseudo_id,
                    node.id,
                    cb_resolved,
                    img_w,
                    img_h,
                    ctx,
                );
            }
            continue;
        }
        convert_node(doc, pseudo_id, ctx, depth + 1, out);
    }
}

/// Walk direct DOM children whose computed `position` is `absolute` or
/// `fixed` and recurse via `convert_node`.
pub(super) fn walk_absolute_non_pseudo_children(
    doc: &BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let lc_guard = node.layout_children.borrow();
    let effective_children = lc_guard.as_deref().unwrap_or(&node.children);
    for &child_id in effective_children {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };
        if !is_absolutely_positioned(child_node) {
            continue;
        }
        if is_pseudo_node(doc, child_node) {
            continue;
        }
        convert_node(doc, child_id, ctx, depth + 1, out);
    }
}

/// Combined entry: walk `::before` / direct DOM abs / `::after` in source
/// order so paint matches CSS `::after`-on-top semantics.
pub(super) fn walk_absolute_children(
    doc: &BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) {
    walk_absolute_pseudo_children(doc, node, ctx, depth, &[node.before], out);
    walk_absolute_non_pseudo_children(doc, node, ctx, depth, out);
    walk_absolute_pseudo_children(doc, node, ctx, depth, &[node.after], out);
}

/// Shortcut for the textless `content: url(...)` abs pseudo case, returning
/// an `ImageEntry` directly when applicable. Returns `None` for pseudos
/// with text content, visual style, or unresolved `content`.
pub(super) fn try_build_absolute_pseudo_image(
    pseudo: &Node,
    parent: &Node,
    cb: Option<AbsCb>,
    assets: Option<&AssetBundle>,
) -> Option<crate::drawables::ImageEntry> {
    crate::blitz_adapter::extract_content_image_url(pseudo)?;
    let pseudo_style = extract_block_style(pseudo, assets);
    if pseudo_style.has_visual_style() {
        return None;
    }
    let (basis_w, basis_h) = if let Some(cb) = cb {
        cb.padding_box_size
    } else {
        (
            parent.final_layout.size.width.as_px(),
            parent.final_layout.size.height.as_px(),
        )
    };
    pseudo::build_pseudo_image_entry(pseudo, basis_w, basis_h, assets)
}

// `effective_pseudo_size_px` is no longer used — abs/fixed inset
// resolution math moves to the render path in a follow-up.

#[cfg(test)]
mod tests {
    use blitz_html::HtmlDocument;
    use std::ops::{Deref, DerefMut};

    use super::*;

    // ── helpers ──────────────────────────────────────────────────────

    fn parse_doc(html: &str) -> HtmlDocument {
        crate::blitz_adapter::parse_and_layout(
            html,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        )
    }

    fn find_first_by_tag(doc: &BaseDocument, start_id: usize, tag: &str) -> Option<usize> {
        let node = doc.get_node(start_id)?;
        if node
            .element_data()
            .is_some_and(|e| e.name.local.as_ref() == tag)
        {
            return Some(start_id);
        }
        for &c in &node.children {
            if let Some(found) = find_first_by_tag(doc, c, tag) {
                return Some(found);
            }
        }
        None
    }

    fn find_tag(doc: &HtmlDocument, tag: &str) -> usize {
        let root = doc.root_element();
        find_first_by_tag(doc.deref(), root.id, tag)
            .unwrap_or_else(|| panic!("<{tag}> not found in document"))
    }

    // 'store is the lifetime of running_store, which ConvertContext<'store>
    // borrows. doc's lifetime is not stored and can be elided.
    fn make_ctx<'store>(
        doc: &mut HtmlDocument,
        running_store: &'store RunningElementStore,
    ) -> ConvertContext<'store> {
        let column_styles = crate::blitz_adapter::extract_column_style_table(doc);
        let multicol_geometry = crate::multicol_layout::run_pass(doc.deref_mut(), &column_styles);
        let pagination_geometry = crate::pagination_layout::run_pass(doc.deref_mut(), 842.0);
        ConvertContext {
            running_store,
            assets: None,
            font_cache: Default::default(),
            string_set_by_node: Default::default(),
            counter_ops_by_node: Default::default(),
            bookmark_by_node: Default::default(),
            column_styles,
            multicol_geometry,
            pagination_geometry,
            link_cache: Default::default(),
            viewport_size_px: Some((595.0, 842.0)),
        }
    }

    // ── is_absolutely_positioned ──────────────────────────────────────

    #[test]
    fn is_absolutely_positioned_true_for_absolute() {
        let doc = parse_doc(
            r#"<!doctype html><html><body><div style="position:absolute">x</div></body></html>"#,
        );
        let node = doc.get_node(find_tag(&doc, "div")).unwrap();
        assert!(is_absolutely_positioned(node));
    }

    #[test]
    fn is_absolutely_positioned_true_for_fixed() {
        let doc = parse_doc(
            r#"<!doctype html><html><body><div style="position:fixed">x</div></body></html>"#,
        );
        let node = doc.get_node(find_tag(&doc, "div")).unwrap();
        assert!(is_absolutely_positioned(node));
    }

    #[test]
    fn is_absolutely_positioned_false_for_default() {
        let doc = parse_doc(r#"<!doctype html><html><body><div>x</div></body></html>"#);
        let node = doc.get_node(find_tag(&doc, "div")).unwrap();
        assert!(!is_absolutely_positioned(node));
    }

    #[test]
    fn is_absolutely_positioned_false_for_relative() {
        let doc = parse_doc(
            r#"<!doctype html><html><body><div style="position:relative">x</div></body></html>"#,
        );
        let node = doc.get_node(find_tag(&doc, "div")).unwrap();
        assert!(!is_absolutely_positioned(node));
    }

    // ── is_position_fixed ────────────────────────────────────────────

    #[test]
    fn is_position_fixed_true_for_fixed() {
        let doc = parse_doc(
            r#"<!doctype html><html><body><div style="position:fixed">x</div></body></html>"#,
        );
        let node = doc.get_node(find_tag(&doc, "div")).unwrap();
        assert!(is_position_fixed(node));
    }

    #[test]
    fn is_position_fixed_false_for_absolute() {
        let doc = parse_doc(
            r#"<!doctype html><html><body><div style="position:absolute">x</div></body></html>"#,
        );
        let node = doc.get_node(find_tag(&doc, "div")).unwrap();
        assert!(!is_position_fixed(node));
    }

    #[test]
    fn is_position_fixed_false_for_default() {
        let doc = parse_doc(r#"<!doctype html><html><body><div>x</div></body></html>"#);
        let node = doc.get_node(find_tag(&doc, "div")).unwrap();
        assert!(!is_position_fixed(node));
    }

    // ── is_position_static ───────────────────────────────────────────

    #[test]
    fn is_position_static_true_for_default() {
        let doc = parse_doc(r#"<!doctype html><html><body><div>x</div></body></html>"#);
        let node = doc.get_node(find_tag(&doc, "div")).unwrap();
        assert!(is_position_static(node));
    }

    #[test]
    fn is_position_static_false_for_absolute() {
        let doc = parse_doc(
            r#"<!doctype html><html><body><div style="position:absolute">x</div></body></html>"#,
        );
        let node = doc.get_node(find_tag(&doc, "div")).unwrap();
        assert!(!is_position_static(node));
    }

    #[test]
    fn is_position_static_false_for_relative() {
        let doc = parse_doc(
            r#"<!doctype html><html><body><div style="position:relative">x</div></body></html>"#,
        );
        let node = doc.get_node(find_tag(&doc, "div")).unwrap();
        assert!(!is_position_static(node));
    }

    // ── try_build_absolute_pseudo_image ──────────────────────────────

    #[test]
    fn try_build_absolute_pseudo_image_returns_none_without_content_url() {
        let doc = parse_doc(
            r#"<!doctype html><html><body><div style="position:absolute">x</div></body></html>"#,
        );
        let div_id = find_tag(&doc, "div");
        let node = doc.get_node(div_id).unwrap();
        // A regular element with no `content: url(...)` must return None.
        assert!(try_build_absolute_pseudo_image(node, node, None, None).is_none());
    }

    // ── walk_absolute_non_pseudo_children ────────────────────────────

    #[test]
    fn walk_absolute_non_pseudo_children_at_max_depth_short_circuits() {
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section>
                <div style="position:absolute;width:10px;height:10px;">abs</div>
              </section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_id = find_tag(&doc, "section");
        let section_node = doc.get_node(section_id).unwrap();

        walk_absolute_non_pseudo_children(
            doc.deref(),
            section_node,
            &mut ctx,
            crate::MAX_DOM_DEPTH,
            &mut out,
        );

        assert!(
            out.is_empty(),
            "at MAX_DOM_DEPTH walk_absolute_non_pseudo_children must produce nothing"
        );
    }

    #[test]
    fn walk_absolute_non_pseudo_children_registers_abs_child_block_entry() {
        // Container with one abs-positioned child (with text content so
        // block::convert uses the container path, which always inserts a
        // BlockEntry) and one static child.
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section>
                <div style="position:absolute;width:10px;height:10px;">abs</div>
                <div>static</div>
              </section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_id = find_tag(&doc, "section");
        let section_node = doc.get_node(section_id).unwrap();

        walk_absolute_non_pseudo_children(doc.deref(), section_node, &mut ctx, 0, &mut out);

        // The abs-positioned div with text is an inline root, so it produces
        // a ParagraphEntry rather than a BlockEntry. Either is proof of
        // conversion.
        assert!(
            !out.block_styles.is_empty() || !out.paragraphs.is_empty(),
            "expected abs child to produce a draw entry"
        );
    }

    #[test]
    fn walk_absolute_non_pseudo_children_skips_static_children() {
        // Container with only static children: nothing should be registered.
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section>
                <div>first</div>
                <div>second</div>
              </section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_id = find_tag(&doc, "section");
        let section_node = doc.get_node(section_id).unwrap();

        walk_absolute_non_pseudo_children(doc.deref(), section_node, &mut ctx, 0, &mut out);

        assert!(
            out.is_empty(),
            "static children must not be registered by walk_absolute_non_pseudo_children"
        );
    }

    // ── walk_children_into_drawables ────────────────────────────────

    #[test]
    fn walk_children_into_drawables_at_max_depth_short_circuits() {
        // At MAX_DOM_DEPTH the function must return immediately without
        // visiting any children, even though child_ids is non-empty.
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body><div><p>text</p></div></body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let div_id = find_tag(&doc, "div");
        let child_ids: Vec<usize> = doc
            .get_node(div_id)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        walk_children_into_drawables(
            doc.deref(),
            &child_ids,
            &mut ctx,
            crate::MAX_DOM_DEPTH,
            &mut out,
        );

        assert!(
            out.is_empty(),
            "at MAX_DOM_DEPTH nothing should be registered"
        );
    }

    #[test]
    fn walk_children_into_drawables_excludes_abs_positioned_children() {
        // Parent with one abs child and one static child. The abs child must
        // NOT be registered by walk_children_into_drawables (it's handled by
        // walk_absolute_pseudo_children instead).
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section>
                <div style="position:absolute;width:5px;height:5px;"></div>
              </section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_id = find_tag(&doc, "section");
        // Snapshot the children while we still hold the node ref, before
        // borrowing doc as immutable for walk_children_into_drawables.
        let child_ids: Vec<usize> = doc
            .get_node(section_id)
            .map(|n| n.children.clone())
            .unwrap_or_default();

        walk_children_into_drawables(doc.deref(), &child_ids, &mut ctx, 0, &mut out);

        // The abs-positioned div must NOT appear — it is filtered out.
        // The drawables should have no block entry for the abs child.
        let abs_div_id = find_tag(&doc, "div");
        assert!(
            !out.block_styles.contains_key(&abs_div_id),
            "abs child must be excluded by walk_children_into_drawables"
        );
    }

    #[test]
    fn walk_children_into_drawables_skips_non_visual_elements() {
        // A section that contains a <script> (non-visual) child and a <div>
        // child with text. The script should be filtered out; the div must
        // produce at least one draw entry.
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section>
                <script>/* noise */</script>
                <div>content</div>
              </section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_id = find_tag(&doc, "section");
        let child_ids: Vec<usize> = doc
            .get_node(section_id)
            .map(|n| n.children.clone())
            .unwrap_or_default();

        walk_children_into_drawables(doc.deref(), &child_ids, &mut ctx, 0, &mut out);

        // Script node (if present in the DOM) must not have been registered.
        // Div with "content" should produce at least one draw entry.
        if let Some(script_id) = find_first_by_tag(doc.deref(), doc.root_element().id, "script") {
            assert!(
                !out.block_styles.contains_key(&script_id)
                    && !out.paragraphs.contains_key(&script_id),
                "script element must be skipped by walk_children_into_drawables"
            );
        }
        // At a minimum the div with text should produce a paragraph entry.
        assert!(
            !out.block_styles.is_empty() || !out.paragraphs.is_empty(),
            "div child must produce at least one draw entry"
        );
    }

    // ── cb_padding_box ────────────────────────────────────────────────

    #[test]
    fn cb_padding_box_no_border_returns_full_layout_size_and_zero_offsets() {
        // A div with explicit size but no border. The padding box should equal
        // the Taffy layout size (sz.width, sz.height) and border_top_left = (0, 0).
        let doc = parse_doc(
            r#"<!doctype html><html><body>
                <div style="width:100px;height:50px;">x</div>
            </body></html>"#,
        );
        let div_id = find_tag(&doc, "div");
        let node = doc.get_node(div_id).unwrap();
        let ((pb_w, pb_h), (bl, bt)) = cb_padding_box(node);
        let (pb_w, pb_h, bl, bt) = (pb_w.to_f32(), pb_h.to_f32(), bl.to_f32(), bt.to_f32());
        let sz = node.final_layout.size;
        // No border → pb_w == sz.width, pb_h == sz.height.
        assert!(
            (pb_w - sz.width).abs() < 0.01,
            "no border: pb_w should equal layout width; pb_w={pb_w} sz.width={sz_w}",
            sz_w = sz.width
        );
        assert!(
            (pb_h - sz.height).abs() < 0.01,
            "no border: pb_h should equal layout height; pb_h={pb_h} sz.height={sz_h}",
            sz_h = sz.height
        );
        assert!(
            bl.abs() < 0.001,
            "no border: left offset should be 0; got {bl}"
        );
        assert!(
            bt.abs() < 0.001,
            "no border: top offset should be 0; got {bt}"
        );
    }

    #[test]
    fn cb_padding_box_with_border_reduces_padding_box_and_gives_nonzero_offsets() {
        // A div with a 10px uniform border. The padding box must be strictly
        // smaller than the border-box and border_top_left must be > 0.
        let doc = parse_doc(
            r#"<!doctype html><html><body>
                <div style="width:100px;height:100px;border:10px solid black;">x</div>
            </body></html>"#,
        );
        let div_id = find_tag(&doc, "div");
        let node = doc.get_node(div_id).unwrap();
        let ((pb_w, _pb_h), (bl, bt)) = cb_padding_box(node);
        let (pb_w, bl, bt) = (pb_w.to_f32(), bl.to_f32(), bt.to_f32());
        let sz = node.final_layout.size;
        // With 10px border on each side, the border-box is 120×120px and the
        // padding box (content) is 100×100px.
        assert!(
            pb_w < sz.width,
            "10px border: padding box width must be less than border-box; pb_w={pb_w} sz.width={sz_w}",
            sz_w = sz.width
        );
        assert!(bl > 0.0, "10px border: left offset must be > 0; got {bl}");
        assert!(bt > 0.0, "10px border: top offset must be > 0; got {bt}");
    }

    // ── resolve_cb_for_absolute ───────────────────────────────────────

    #[test]
    fn resolve_cb_for_absolute_returns_some_for_positioned_ancestor() {
        // Pass the div child of a position:relative section. The function
        // must walk up, find the relative section, and return Some immediately.
        let doc = parse_doc(
            r#"<!doctype html><html><body>
                <section style="position:relative;width:200px;height:100px;">
                    <div>text</div>
                </section>
            </body></html>"#,
        );
        let div_id = find_tag(&doc, "div");
        let div_node = doc.get_node(div_id).unwrap();
        let result = resolve_cb_for_absolute(doc.deref(), div_node, false, None);
        assert!(
            result.is_some(),
            "position:relative section should be found as containing block"
        );
        let cb = result.unwrap();
        assert!(
            cb.padding_box_size.0 > Px::ZERO,
            "CB padding-box width should be > 0 (section has explicit width)"
        );
    }

    #[test]
    fn resolve_cb_for_absolute_uses_body_fallback_when_all_ancestors_static() {
        // The span's parent div is static, so resolve_cb_for_absolute should
        // walk all the way up to body and return the body fallback.
        let doc = parse_doc(
            r#"<!doctype html><html><body>
                <div style="width:200px;height:100px;">
                    <span>text</span>
                </div>
            </body></html>"#,
        );
        let span_id = find_tag(&doc, "span");
        let span_node = doc.get_node(span_id).unwrap();
        let result = resolve_cb_for_absolute(doc.deref(), span_node, false, Some((595.0, 842.0)));
        // All ancestors are static → body fallback must be returned.
        let cb = result.expect(
            "body fallback should always produce Some when the document has a body element",
        );
        // Body is viewport-wide (~579px after default margins), which is wider
        // than the 200px div. Asserting > 200 proves the function used the body
        // CB and did not stop at the static div ancestor.
        let cb_w = cb.padding_box_size.0.to_f32();
        assert!(
            cb_w > 200.0,
            "body padding-box width should exceed the 200px static div, confirming body fallback was used; got {cb_w}"
        );
    }

    #[test]
    fn resolve_cb_for_absolute_zero_width_body_substitutes_viewport_width() {
        // A body forced to zero width: `cb_padding_box().0 == 0`, so the body
        // fallback substitutes the viewport width. Exercises the
        // `padding_box_size.0 = vw.as_px()` branch (the width sibling of the
        // height fallback already covered by the tests above).
        let doc = parse_doc(
            r#"<!doctype html><html><body style="width:0;">
                <div style="width:200px;height:100px;">
                    <span>text</span>
                </div>
            </body></html>"#,
        );
        let span_id = find_tag(&doc, "span");
        let span_node = doc.get_node(span_id).unwrap();
        let result = resolve_cb_for_absolute(doc.deref(), span_node, false, Some((595.0, 842.0)));
        let cb = result.expect("body fallback should produce Some");
        // Body width resolved to 0 → replaced with the viewport width (595).
        assert_eq!(
            cb.padding_box_size.0,
            595.0_f32.as_px(),
            "zero-width body must substitute the viewport width"
        );
    }

    #[test]
    fn resolve_cb_for_absolute_fixed_skips_positioned_ancestors_and_uses_body_fallback() {
        // With is_fixed=true the `!is_fixed && !is_position_static(cur)` check
        // short-circuits to false, so even a position:relative parent is ignored
        // and the body fallback is used.
        let doc = parse_doc(
            r#"<!doctype html><html><body>
                <section style="position:relative;width:200px;height:100px;">
                    <div>text</div>
                </section>
            </body></html>"#,
        );
        let div_id = find_tag(&doc, "div");
        let div_node = doc.get_node(div_id).unwrap();
        let result = resolve_cb_for_absolute(doc.deref(), div_node, true, Some((595.0, 842.0)));
        // is_fixed=true → relative section is skipped → body fallback returned.
        let cb = result
            .expect("fixed: body fallback should be returned even when a relative parent exists");
        // Body is viewport-wide (~579px after default margins), which is wider
        // than the 200px section. Asserting > 200 proves the function skipped
        // the relative section and used the body CB instead.
        let cb_w = cb.padding_box_size.0.to_f32();
        assert!(
            cb_w > 200.0,
            "fixed: body padding-box width should exceed the 200px relative section, confirming it was skipped; got {cb_w}"
        );
    }

    #[test]
    fn resolve_cb_for_absolute_positioned_node_passed_directly_returns_some() {
        // When we pass a node that is itself non-static, the function returns
        // Some on the very first iteration (cur == parent, is_position_static=false).
        let doc = parse_doc(
            r#"<!doctype html><html><body>
                <div style="position:relative;width:150px;height:80px;">x</div>
            </body></html>"#,
        );
        let div_id = find_tag(&doc, "div");
        let div_node = doc.get_node(div_id).unwrap();
        let result = resolve_cb_for_absolute(doc.deref(), div_node, false, None);
        assert!(result.is_some(), "non-static node passed directly → Some");
        let cb = result.unwrap();
        // parent_offset_in_cb_bp should be (0,0) since the loop fires immediately
        // (no offset accumulated yet).
        assert!(
            (cb.parent_offset_in_cb_bp.0).abs() < 0.001,
            "no offset accumulated on first iteration"
        );
    }

    // ── walk_absolute_children ────────────────────────────────────────

    #[test]
    fn walk_absolute_children_registers_abs_positioned_child() {
        // walk_absolute_children is the combined entry that calls both
        // walk_absolute_pseudo_children and walk_absolute_non_pseudo_children.
        // Verify that an abs-positioned non-pseudo child is registered via the
        // non-pseudo path.
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section style="position:relative;">
                <div style="position:absolute;width:20px;height:20px;">abs</div>
              </section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_id = find_tag(&doc, "section");
        let section_node = doc.get_node(section_id).unwrap();

        walk_absolute_children(doc.deref(), section_node, &mut ctx, 0, &mut out);

        let abs_div_id = find_tag(&doc, "div");
        assert!(
            out.block_styles.contains_key(&abs_div_id) || out.paragraphs.contains_key(&abs_div_id),
            "abs-positioned div must be registered by walk_absolute_children"
        );
    }

    #[test]
    fn walk_absolute_children_on_static_only_node_is_empty() {
        // When no abs-positioned children exist, walk_absolute_children
        // must leave Drawables empty.
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section><div>static</div></section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_id = find_tag(&doc, "section");
        let section_node = doc.get_node(section_id).unwrap();

        walk_absolute_children(doc.deref(), section_node, &mut ctx, 0, &mut out);

        assert!(
            out.is_empty(),
            "walk_absolute_children with no abs children must produce nothing"
        );
    }

    // ── walk_absolute_pseudo_children ───────────────────────────────

    #[test]
    fn walk_absolute_pseudo_children_empty_slots_is_noop() {
        // Passing &[None] as slots: the flattened iterator yields nothing,
        // so no conversion occurs. Covers the function entry and loop header.
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body><section><div>x</div></section></body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_id = find_tag(&doc, "section");
        let section_node = doc.get_node(section_id).unwrap();

        walk_absolute_pseudo_children(doc.deref(), section_node, &mut ctx, 0, &[None], &mut out);

        assert!(out.is_empty(), "empty slots must produce no entries");
    }

    #[test]
    fn walk_absolute_pseudo_children_static_parent_resolves_cb_from_ancestors() {
        // Passing an abs-positioned node's ID as a slot exercises the full
        // loop body when parent_is_static=true:
        //   • resolve_cb_for_absolute climbs to <body>
        //   • cb_padding_box is called for ancestor nodes
        //   • try_build_absolute_pseudo_image returns None (no content:url)
        //   • convert_node is called as fallback, producing draw entries
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section>
                <div style="position:absolute;width:10px;height:10px;">abs</div>
              </section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let section_id = find_tag(&doc, "section");
        let abs_div_id = find_tag(&doc, "div");
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_node = doc.get_node(section_id).unwrap();

        // parent_is_static=true (section default) → cb_absolute branch
        walk_absolute_pseudo_children(
            doc.deref(),
            section_node,
            &mut ctx,
            0,
            &[Some(abs_div_id)],
            &mut out,
        );

        assert!(
            !out.block_styles.is_empty() || !out.paragraphs.is_empty(),
            "abs slot must produce a draw entry via convert_node fallback"
        );
    }

    #[test]
    fn walk_absolute_pseudo_children_non_static_parent_uses_own_padding_box() {
        // When the container has position:relative (parent_is_static=false),
        // the else-branch uses cb_padding_box(node) directly instead of
        // climbing the ancestor chain.
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section style="position:relative;width:200px;height:300px;">
                <div style="position:absolute;width:10px;height:10px;">abs</div>
              </section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let section_id = find_tag(&doc, "section");
        let abs_div_id = find_tag(&doc, "div");
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_node = doc.get_node(section_id).unwrap();

        // parent_is_static=false → else branch: AbsCb from cb_padding_box(section)
        walk_absolute_pseudo_children(
            doc.deref(),
            section_node,
            &mut ctx,
            0,
            &[Some(abs_div_id)],
            &mut out,
        );

        assert!(
            !out.block_styles.is_empty() || !out.paragraphs.is_empty(),
            "non-static parent: abs slot must still produce a draw entry"
        );
    }

    #[test]
    fn walk_absolute_pseudo_children_fixed_slot_takes_fixed_cb_branch() {
        // position:fixed pseudo → cb_fixed branch → resolve_cb_for_absolute
        // with is_fixed=true (climbs the full tree without stopping at
        // non-static ancestors).
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section style="position:relative;width:200px;height:300px;">
                <div style="position:fixed;width:10px;height:10px;">fixed</div>
              </section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let section_id = find_tag(&doc, "section");
        let fixed_div_id = find_tag(&doc, "div");
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_node = doc.get_node(section_id).unwrap();

        // is_position_fixed(pseudo)=true → cb_fixed branch
        walk_absolute_pseudo_children(
            doc.deref(),
            section_node,
            &mut ctx,
            0,
            &[Some(fixed_div_id)],
            &mut out,
        );

        assert!(
            !out.block_styles.is_empty() || !out.paragraphs.is_empty(),
            "fixed slot must produce a draw entry via convert_node fallback"
        );
    }

    // ── resolve_cb_for_absolute: None viewport path ───────────────────────────

    #[test]
    fn resolve_cb_for_absolute_none_viewport_all_static_returns_body_fallback() {
        // When viewport_size_px is None, the `if let Some((vw, vh))` block in
        // resolve_cb_for_absolute is skipped entirely. The body fallback must
        // still be returned with the body's own computed dimensions.
        let doc =
            parse_doc(r#"<!doctype html><html><body><div><span>text</span></div></body></html>"#);
        let span_id = find_tag(&doc, "span");
        let span_node = doc.get_node(span_id).unwrap();
        let result = resolve_cb_for_absolute(doc.deref(), span_node, false, None);
        assert!(
            result.is_some(),
            "body fallback must be returned even when viewport_size_px is None"
        );
        // The body width in a 595-px viewport should be > 0.
        assert!(
            result.unwrap().padding_box_size.0 > Px::ZERO,
            "body padding-box width must be positive"
        );
    }

    // ── walk_absolute_pseudo_children: non-abs slot is skipped ────────────────

    #[test]
    fn walk_absolute_pseudo_children_skips_non_abs_slot() {
        // Passing a static (non-abs-positioned) node as a slot must hit the
        // `if !is_absolutely_positioned(pseudo) { continue }` guard and
        // produce no draw entries.
        let mut doc = crate::blitz_adapter::parse_and_layout(
            r#"<!doctype html><html><body>
              <section><div>static</div></section>
            </body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let section_id = find_tag(&doc, "section");
        let static_div_id = find_tag(&doc, "div");
        let running_store = RunningElementStore::new();
        let mut ctx = make_ctx(&mut doc, &running_store);
        let mut out = crate::drawables::Drawables::new();

        let section_node = doc.get_node(section_id).unwrap();
        walk_absolute_pseudo_children(
            doc.deref(),
            section_node,
            &mut ctx,
            0,
            &[Some(static_div_id)],
            &mut out,
        );

        assert!(
            out.is_empty(),
            "non-abs-positioned slot must be skipped by walk_absolute_pseudo_children"
        );
    }

    // ── try_build_absolute_pseudo_image additional branches ─────────────

    #[test]
    fn try_build_absolute_pseudo_image_background_on_pseudo_returns_none() {
        // A ::before pseudo that has content:url AND background:red.
        // extract_content_image_url returns Some (passes the first ?),
        // but has_visual_style() = true (background) → returns None early.
        let doc = parse_doc(
            r#"<!doctype html><html><head><style>
            div::before {
                content: url("dot.png");
                background: #f00;
                position: absolute;
                width: 10px;
                height: 10px;
            }
        </style></head><body>
            <div style="position:relative;width:100px;height:100px;"></div>
        </body></html>"#,
        );
        let div_id = find_tag(&doc, "div");
        let div_node = doc.get_node(div_id).unwrap();
        let before_id = div_node
            .before
            .expect("::before pseudo-element should exist");
        let before_node = doc
            .get_node(before_id)
            .expect("::before node should be retrievable");
        let ab_cb = AbsCb {
            padding_box_size: (100.0_f32.as_px(), 100.0_f32.as_px()),
            border_top_left: (Px::ZERO, Px::ZERO),
            parent_offset_in_cb_bp: (0.0, 0.0),
        };
        let result = try_build_absolute_pseudo_image(before_node, div_node, Some(ab_cb), None);
        // background → has_visual_style() = true → must return None
        assert!(
            result.is_none(),
            "pseudo with background must return None from try_build_absolute_pseudo_image"
        );
    }

    #[test]
    fn try_build_absolute_pseudo_image_cb_none_uses_parent_layout_size() {
        // A ::before pseudo with content:url but no background (no visual style).
        // Passing cb=None forces the else-branch: basis is taken from
        // parent.final_layout.size rather than cb.padding_box_size.
        // assets=None means build_pseudo_image_entry returns None, but the
        // else-branch is still exercised for coverage.
        let doc = parse_doc(
            r#"<!doctype html><html><head><style>
            div::before {
                content: url("dot.png");
                position: absolute;
                width: 10px;
                height: 10px;
            }
        </style></head><body>
            <div style="position:relative;width:100px;height:100px;"></div>
        </body></html>"#,
        );
        let div_id = find_tag(&doc, "div");
        let div_node = doc.get_node(div_id).unwrap();
        let before_id = div_node
            .before
            .expect("::before pseudo-element should exist");
        let before_node = doc
            .get_node(before_id)
            .expect("::before node should be retrievable");
        // cb=None → else branch uses parent.final_layout.size as basis
        let result = try_build_absolute_pseudo_image(before_node, div_node, None, None);
        // assets=None → build_pseudo_image_entry returns None,
        // but the else branch was exercised.
        assert!(result.is_none());
    }

    // ── maybe_apply_abs_pseudo_inset_correction: Engine smoke tests ───────────
    //
    // These tests exercise the full path through walk_absolute_pseudo_children →
    // try_build_absolute_pseudo_image → maybe_apply_abs_pseudo_inset_correction.
    // That chain fires when a ::before / ::after pseudo has `content: url(...)`
    // and `position: absolute` or `position: fixed`.

    // Minimal 1×1 red PNG for use as a test image asset.
    const RED_1X1_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn render_with_pseudo_css(html: &str, css: &str) -> Vec<u8> {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(css);
        crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(html)
            .expect("render failed")
    }

    fn build_with_geo(
        html: &str,
    ) -> (
        crate::drawables::Drawables,
        crate::pagination_layout::PaginationGeometryTable,
    ) {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        crate::engine::Engine::builder()
            .page_size(crate::config::PageSize::A4)
            .margin(crate::config::Margin::uniform(72.0))
            .assets(bundle)
            .build()
            .build_drawables_and_geometry_for_testing_no_gcpm(html)
    }

    fn find_image_geo(
        drawables: &crate::drawables::Drawables,
        want_w: f32,
        want_h: f32,
    ) -> Option<(usize, &crate::drawables::ImageEntry)> {
        drawables
            .images
            .iter()
            .find(|(_, img)| {
                (img.width.to_f32() - want_w).abs() < 0.5
                    && (img.height.to_f32() - want_h).abs() < 0.5
            })
            .map(|(id, e)| (*id, e))
    }

    fn frag_xy(
        geometry: &crate::pagination_layout::PaginationGeometryTable,
        node_id: usize,
    ) -> (f32, f32) {
        let geom = geometry.get(&node_id).expect("node missing from geometry");
        let frag = geom.fragments.first().expect("node has no fragments");
        (frag.x.to_f32(), frag.y.to_f32())
    }

    // maybe_apply_abs_pseudo_inset_correction: `right` + `bottom` set, `left`
    // + `top` auto → needs_right=true, needs_bottom=true → the correction is
    // applied and new_x / new_y are written into pagination_geometry.
    // Also covers resolve_inset_px with a LengthPercentage inset.
    //
    // Coordinate check: parent 100×100 CSS px (75×75 pt), pseudo 10×10 px
    // (7.5×7.5 pt) at right:5px, bottom:5px.
    //   expected dx = (100 − 10 − 5) × 0.75 = 63.75 pt
    //   bug case (pre-fix, pseudo layout.size = 0): (100 − 0 − 5) × 0.75 = 71.25 pt
    #[test]
    fn smoke_abs_pseudo_content_url_right_bottom_insets() {
        let html = r#"<!DOCTYPE html><html><head><style>
            .marker { position: relative; width: 100px; height: 100px; }
            .marker::before {
                content: url(dot.png);
                position: absolute;
                width: 10px; height: 10px;
                right: 5px; bottom: 5px;
            }
        </style></head><body><div class="marker"></div></body></html>"#;
        let (drawables, geometry) = build_with_geo(html);

        let (image_id, _) = find_image_geo(&drawables, 7.5, 7.5)
            .expect("expected a 7.5×7.5 pt ImageEntry from abs pseudo");
        let (marker_id, _) = drawables
            .block_styles
            .iter()
            .find(|(_, b)| {
                b.layout_size.is_some_and(|s| {
                    (s.width.to_f32() - 75.0).abs() < 0.5 && (s.height.to_f32() - 75.0).abs() < 0.5
                })
            })
            .map(|(id, e)| (*id, e))
            .expect("marker block (75×75 pt) must exist in block_styles");

        let (ix, iy) = frag_xy(&geometry, image_id);
        let (mx, my) = frag_xy(&geometry, marker_id);
        let dx_pt = (ix - mx) * 0.75;
        let dy_pt = (iy - my) * 0.75;
        let want = 63.75_f32;
        assert!(
            (dx_pt - want).abs() < 0.5 && (dy_pt - want).abs() < 0.5,
            "expected pseudo at marker+({want:.2},{want:.2}) pt, got ({dx_pt:.2},{dy_pt:.2}); \
             bug case (no correction) would be ~71.25",
        );
    }

    // maybe_apply_abs_pseudo_inset_correction: position:fixed pseudo with
    // content:url triggers the `is_position_fixed` early return at line 213.
    // This path is distinct from the absolute-positioned path tested above.
    #[test]
    fn smoke_fixed_pseudo_content_url_early_exit() {
        let pdf = render_with_pseudo_css(
            r#"<!doctype html><html><body><div></div></body></html>"#,
            r#"
                div::before {
                    content: url("dot.png");
                    position: fixed;
                    width: 10px; height: 10px;
                    right: 5px; bottom: 5px;
                }
            "#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // maybe_apply_abs_pseudo_inset_correction: `left` + `top` set → needs_right
    // = false, needs_bottom = false → early exit at `!needs_right && !needs_bottom`.
    #[test]
    fn smoke_abs_pseudo_content_url_left_top_no_correction() {
        let pdf = render_with_pseudo_css(
            r#"<!doctype html><html><body><div></div></body></html>"#,
            r#"
                div { position: relative; width: 100px; height: 100px; }
                div::before {
                    content: url("dot.png");
                    position: absolute;
                    width: 10px; height: 10px;
                    left: 5px; top: 5px;
                }
            "#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // maybe_apply_abs_pseudo_inset_correction: `right` set but `left` auto
    // with only `top` set (not `bottom`) → needs_right=true, needs_bottom=false
    // → x_in_pp_px takes the `needs_right` branch; y_in_pp_px falls through to
    // `top.unwrap_or(0.0)` (the non-needs_bottom arm).
    //
    // Coordinate check: parent 100×100 CSS px (75×75 pt), pseudo 10×10 px
    // (7.5×7.5 pt) at right:5px, top:0.
    //   expected dx = (100 − 10 − 5) × 0.75 = 63.75 pt,  dy = 0.0 pt
    #[test]
    fn smoke_abs_pseudo_content_url_right_only_inset() {
        let html = r#"<!DOCTYPE html><html><head><style>
            .marker { position: relative; width: 100px; height: 100px; }
            .marker::before {
                content: url(dot.png);
                position: absolute;
                width: 10px; height: 10px;
                right: 5px; top: 0px;
            }
        </style></head><body><div class="marker"></div></body></html>"#;
        let (drawables, geometry) = build_with_geo(html);

        let (image_id, _) = find_image_geo(&drawables, 7.5, 7.5)
            .expect("expected a 7.5×7.5 pt ImageEntry from abs pseudo");
        let (marker_id, _) = drawables
            .block_styles
            .iter()
            .find(|(_, b)| {
                b.layout_size.is_some_and(|s| {
                    (s.width.to_f32() - 75.0).abs() < 0.5 && (s.height.to_f32() - 75.0).abs() < 0.5
                })
            })
            .map(|(id, e)| (*id, e))
            .expect("marker block (75×75 pt) must exist in block_styles");

        let (ix, iy) = frag_xy(&geometry, image_id);
        let (mx, my) = frag_xy(&geometry, marker_id);
        let dx_pt = (ix - mx) * 0.75;
        let dy_pt = (iy - my) * 0.75;
        assert!(
            (dx_pt - 63.75_f32).abs() < 0.5,
            "expected pseudo x at marker+63.75 pt, got {dx_pt:.2}; bug case ~71.25",
        );
        assert!(
            dy_pt.abs() < 0.5,
            "expected pseudo y at marker (dy=0.0 pt), got {dy_pt:.2}",
        );
    }

    // Branch: needs_right=false, needs_bottom=true → y uses cb_h − pseudo_h − bottom,
    // x uses `left.unwrap_or(0.0)` (the else-branch of `if needs_right`).
    #[test]
    fn smoke_abs_before_bottom_only_covers_needs_right_false() {
        let pdf = render_with_pseudo_css(
            r#"<!doctype html><html><body>
        <div style="position:relative;width:200px;height:200px;"></div>
    </body></html>"#,
            r#"div::before {
                content: url("dot.png");
                position: absolute;
                bottom: 10px;
                width: 20px;
                height: 20px;
            }"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }
}
