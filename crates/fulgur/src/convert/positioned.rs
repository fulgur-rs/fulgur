use super::*;

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
    pub(super) padding_box_size: (f32, f32),
    pub(super) border_top_left: (f32, f32),
    pub(super) parent_offset_in_cb_bp: (f32, f32),
}

fn cb_padding_box(node: &Node) -> ((f32, f32), (f32, f32)) {
    let style = extract_block_style(node, None);
    let bl_pt = style.border_widths[3];
    let br_pt = style.border_widths[1];
    let bt_pt = style.border_widths[0];
    let bb_pt = style.border_widths[2];
    let sz = node.final_layout.size;
    let pb_w = (sz.width - pt_to_px(bl_pt + br_pt)).max(0.0);
    let pb_h = (sz.height - pt_to_px(bt_pt + bb_pt)).max(0.0);
    ((pb_w, pb_h), (pt_to_px(bl_pt), pt_to_px(bt_pt)))
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
        if let Some(elem) = cur.element_data() {
            if elem.name.local.as_ref() == "body" {
                let (mut padding_box_size, border_top_left) = cb_padding_box(cur);
                if let Some((vw, vh)) = viewport_size_px {
                    if padding_box_size.0 <= 0.0 {
                        padding_box_size.0 = vw;
                    }
                    if padding_box_size.1 <= 0.0 {
                        padding_box_size.1 = vh;
                    }
                }
                body_fallback = Some(AbsCb {
                    padding_box_size,
                    border_top_left,
                    parent_offset_in_cb_bp: (offset_x, offset_y),
                });
            }
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
    basis_px: f32,
) -> Option<f32> {
    use ::style::values::computed::Length;
    use ::style::values::generics::position::GenericInset;
    match inset {
        GenericInset::LengthPercentage(lp) => Some(lp.resolve(Length::new(basis_px)).px()),
        _ => None,
    }
}

/// PR 8i regression fix: write a corrected fragment into
/// `pagination_geometry` for a textless `content: url(...)` abs pseudo
/// whose `right` or `bottom` inset was specified.
///
/// `pseudo_w_pt` / `pseudo_h_pt` come from the just-built `ImageEntry`,
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
    pseudo_w_pt: f32,
    pseudo_h_pt: f32,
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
    let Some(parent_frag) = parent_geom.fragments.first().cloned() else {
        return;
    };
    let parent_x_px = parent_frag.x;
    let parent_y_px = parent_frag.y;

    let pseudo_w_px = pt_to_px(pseudo_w_pt);
    let pseudo_h_px = pt_to_px(pseudo_h_pt);

    // CSS 2.1 §10.3.7 / §10.6.4: when start-side is auto, end-side
    // determines position. Use the pseudo's effective image size
    // (NOT Taffy's `final_layout.size`, which is `(0, 0)` for
    // textless content:url pseudos).
    let x_in_pp_px = if needs_right {
        // right is Some, left is None
        cb_w_px - pseudo_w_px - right.unwrap()
    } else {
        // left is Some (or both auto -> 0)
        left.unwrap_or(0.0)
    };
    let y_in_pp_px = if needs_bottom {
        cb_h_px - pseudo_h_px - bottom.unwrap()
    } else {
        top.unwrap_or(0.0)
    };

    // Padding-box frame → CB border-box frame → parent's frame.
    // `cb.parent_offset_in_cb_bp` is the parent's offset within CB's
    // border-box frame (accumulated `final_layout.location` while
    // resolve_cb_for_absolute climbed). For the simple "parent IS the
    // CB" case this is `(0, 0)`.
    let (bl_px, bt_px) = cb.border_top_left;
    let (ox_px, oy_px) = cb.parent_offset_in_cb_bp;
    let pseudo_local_x_px = x_in_pp_px + bl_px - ox_px;
    let pseudo_local_y_px = y_in_pp_px + bt_px - oy_px;

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
            let (img_w_pt, img_h_pt) = (img.width, img.height);
            out.images.insert(pseudo_id, img);
            if let Some(cb_resolved) = _cb {
                maybe_apply_abs_pseudo_inset_correction(
                    pseudo,
                    pseudo_id,
                    node.id,
                    cb_resolved,
                    img_w_pt,
                    img_h_pt,
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
    let (basis_w_pt, basis_h_pt) = if let Some(cb) = cb {
        let (w_px, h_px) = cb.padding_box_size;
        (px_to_pt(w_px), px_to_pt(h_px))
    } else {
        (
            px_to_pt(parent.final_layout.size.width),
            px_to_pt(parent.final_layout.size.height),
        )
    };
    pseudo::build_pseudo_image_entry(pseudo, basis_w_pt, basis_h_pt, assets)
}

// `effective_pseudo_size_px` is no longer used — abs/fixed inset
// resolution math moves to the render path in a follow-up.
