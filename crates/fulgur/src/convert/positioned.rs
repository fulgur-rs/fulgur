use super::*;

/// Whether `node` has at least one direct DOM child that is a non-pseudo
/// `position: absolute|fixed` element. Used by `collect_positioned_children`
/// to refuse flattening for zero-size containers whose abs/fixed children
/// would otherwise be silently dropped by the recursive flatten path
/// (it skips abs children and the container itself never reaches the
/// `build_absolute_non_pseudo_children` hoist point). Transitivity is handled
/// naturally: deeper containers re-evaluate this guard on their own
/// recursive call.
fn node_has_absolute_non_pseudo_child(doc: &blitz_dom::BaseDocument, node: &Node) -> bool {
    let lc_guard = node.layout_children.borrow();
    let children = lc_guard.as_deref().unwrap_or(&node.children);
    children.iter().any(|&id| {
        doc.get_node(id)
            .is_some_and(|n| is_absolutely_positioned(n) && !is_pseudo_node(doc, n))
    })
}

/// Collect positioned children, flattening zero-size pass-through containers
/// (like thead, tbody, tr) so their children appear directly in the parent.
///
/// Running element markers discovered on zero-size nodes are buffered and
/// attached to the next real child via `RunningElementWrapperPageable`. This
/// keeps the marker with its associated content when pagination pushes the
/// content to the next page.
pub(super) fn collect_positioned_children(
    doc: &blitz_dom::BaseDocument,
    child_ids: &[usize],
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Vec<PositionedChild> {
    if depth >= MAX_DOM_DEPTH {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut pending_running_markers: Vec<RunningElementMarkerPageable> = Vec::new();

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
        // CSS 2.1 §10.6.4: absolutely-positioned descendants are
        // out-of-flow. They are re-emitted by
        // `build_absolute_non_pseudo_children` (called at the same hoist
        // points as `build_absolute_pseudo_children`) with
        // `out_of_flow: true` so they don't consume in-flow page space.
        // Skipping here prevents double emission. Non-pseudo only —
        // abs pseudos are stored in `node.before`/`node.after`, not in
        // the layout child list, so they don't reach this loop.
        if is_absolutely_positioned(child_node) {
            continue;
        }

        let (cx, cy, cw, ch) = layout_in_pt(&child_node.final_layout);

        // Zero-size leaf nodes (whitespace text, etc.) — skip, but first
        // harvest any string-set entries so `string-set: name attr(...)` on
        // an empty element still propagates into the page tree.
        //
        // Exception: if the 0x0 leaf has a block pseudo image, fall through
        // to `convert_node` so `convert_node_inner`'s `children.is_empty()`
        // branch can emit it. Without this, `<span class="icon"></span>`
        // + `span::before { content: url(...); display: block }` silently
        // drops the image even though the empty-children branch is wired up.
        let child_effective_is_empty = child_node
            .layout_children
            .borrow()
            .as_deref()
            .unwrap_or(&child_node.children)
            .is_empty();

        if ch == 0.0
            && cw == 0.0
            && child_effective_is_empty
            && !pseudo::node_has_block_pseudo_image(doc, child_node)
            && !pseudo::node_has_inline_pseudo_image(doc, child_node)
            && !ctx.column_styles.contains_key(&child_id)
            && !pseudo::node_has_absolute_pseudo(doc, child_node)
            && !node_has_absolute_non_pseudo_child(doc, child_node)
        {
            emit_orphan_string_set_markers(child_id, cx, cy, ctx, &mut result);
            emit_counter_op_markers(child_id, cx, cy, ctx, &mut result);
            emit_orphan_bookmark_marker(child_id, cx, cy, ctx, &mut result);
            if let Some(marker) = take_running_marker(child_id, ctx) {
                pending_running_markers.push(marker);
            }
            continue;
        }

        // Zero-size container (thead, tbody, tr, etc.) — flatten children
        // into the parent. Harvest the container's own string-set entries
        // before recursing so they aren't dropped.
        //
        // Exception: when the container has its own `::before` / `::after`
        // with `position: absolute|fixed`, flattening would drop those
        // pseudos since `build_absolute_pseudo_children` only runs inside
        // `convert_node` for the container itself. Fall through to
        // `convert_node` in that case so the pseudos survive.
        if ch == 0.0
            && cw == 0.0
            && !child_effective_is_empty
            && !pseudo::node_has_absolute_pseudo(doc, child_node)
            && !node_has_absolute_non_pseudo_child(doc, child_node)
        {
            emit_orphan_string_set_markers(child_id, cx, cy, ctx, &mut result);
            emit_counter_op_markers(child_id, cx, cy, ctx, &mut result);
            emit_orphan_bookmark_marker(child_id, cx, cy, ctx, &mut result);
            if let Some(marker) = take_running_marker(child_id, ctx) {
                pending_running_markers.push(marker);
            }
            let child_lc_guard = child_node.layout_children.borrow();
            let child_effective_children =
                child_lc_guard.as_deref().unwrap_or(&child_node.children);
            let mut nested =
                collect_positioned_children(doc, child_effective_children, ctx, depth + 1);
            // Flush pending running markers to the first real nested child so
            // they travel with the flattened content on page break. Without
            // this, the markers would skip over the container's children and
            // incorrectly attach to the next outer sibling.
            if !pending_running_markers.is_empty()
                && let Some(first) = nested.first_mut()
            {
                let original = std::mem::replace(
                    &mut first.child,
                    // Temporary placeholder; overwritten below.
                    Box::new(SpacerPageable::new(0.0)),
                );
                first.child = Box::new(RunningElementWrapperPageable::new(
                    std::mem::take(&mut pending_running_markers),
                    original,
                ));
            }
            result.extend(nested);
            continue;
        }

        let mut child_pageable = convert_node(doc, child_id, ctx, depth + 1);
        if !pending_running_markers.is_empty() {
            child_pageable = Box::new(RunningElementWrapperPageable::new(
                std::mem::take(&mut pending_running_markers),
                child_pageable,
            ));
        }
        result.push(PositionedChild {
            child: child_pageable,
            x: cx,
            y: cy,
            out_of_flow: false,
        });
    }

    // Running markers with no subsequent real child — emit as bare
    // PositionedChild fallback so they aren't lost entirely. This covers the
    // edge case of a running element at the very end of a parent.
    for marker in pending_running_markers {
        result.push(PositionedChild {
            child: Box::new(marker),
            x: 0.0,
            y: 0.0,
            out_of_flow: false,
        });
    }

    result
}

/// Whether `node`'s computed `position` is `absolute` or `fixed`.
///
/// Used to reroute pseudo-elements that Blitz/Parley would otherwise place
/// inline at (0, 0) of the surrounding flow: absolute/fixed pseudos have a
/// Taffy-computed `final_layout.location` we want to honor instead.
pub(super) fn is_absolutely_positioned(node: &Node) -> bool {
    node.primary_styles()
        .is_some_and(|s| s.get_box().clone_position().is_absolutely_positioned())
}

/// Whether `node`'s computed `position` is `fixed` (as opposed to `absolute`).
///
/// CSS 2.1 §10.1.5: `position: fixed` establishes the *initial* containing
/// block (page / viewport) as the CB, not the nearest positioned ancestor.
fn is_position_fixed(node: &Node) -> bool {
    use ::style::properties::longhands::position::computed_value::T as Pos;
    node.primary_styles()
        .is_some_and(|s| matches!(s.get_box().clone_position(), Pos::Fixed))
}

/// Whether `node`'s computed `position` is `static` (the default — does not
/// establish a containing block for absolute descendants).
fn is_position_static(node: &Node) -> bool {
    use ::style::properties::longhands::position::computed_value::T as Pos;
    node.primary_styles()
        .is_none_or(|s| matches!(s.get_box().clone_position(), Pos::Static))
}

/// Resolved containing block for an absolutely-positioned descendant.
///
/// Per CSS 2.1 §10.3.7 / §10.6.4, the CB for `position: absolute` is the
/// **padding box** of the nearest positioned ancestor (or the initial CB
/// at the root). Inset longhands (`top` / `right` / `bottom` / `left`)
/// are resolved against the padding-box dimensions, and the resulting
/// coordinates are in the padding-box frame. We carry the CB's
/// `(border_left, border_top)` separately so callers can convert between
/// the padding-box frame and the CB's border-box frame — which is the
/// frame Taffy's `final_layout.location` values are expressed in.
#[derive(Clone, Copy)]
pub(super) struct AbsCb {
    /// Padding-box dimensions in CSS px.
    padding_box_size: (f32, f32),
    /// CB's `(border_left, border_top)` in CSS px. Padding-box origin
    /// is offset by this amount from the CB's border-box origin.
    border_top_left: (f32, f32),
    /// Pseudo's parent expressed in the CB's border-box frame
    /// (accumulated Taffy `final_layout.location` while climbing).
    parent_offset_in_cb_bp: (f32, f32),
}

/// Compute `(padding_box_size, border_top_left)` for a CB node, both in
/// CSS px. `extract_block_style` returns values in PDF pt (fulgur's
/// internal convention), so we convert back to px because the rest of
/// the absolute-positioning math — Taffy `final_layout`, stylo inset
/// resolution — operates in px.
fn cb_padding_box(node: &Node) -> ((f32, f32), (f32, f32)) {
    let style = extract_block_style(node, None);
    // border_widths = [top, right, bottom, left] in pt.
    let bl_pt = style.border_widths[3];
    let br_pt = style.border_widths[1];
    let bt_pt = style.border_widths[0];
    let bb_pt = style.border_widths[2];
    let sz = node.final_layout.size;
    let pb_w = (sz.width - pt_to_px(bl_pt + br_pt)).max(0.0);
    let pb_h = (sz.height - pt_to_px(bt_pt + bb_pt)).max(0.0);
    ((pb_w, pb_h), (pt_to_px(bl_pt), pt_to_px(bt_pt)))
}

/// Walk ancestors starting at `parent` (the absolutely-positioned descendant's
/// parent) to find the containing block.
///
/// - When `is_fixed` is `false` (`position: absolute`): the first
///   `position: relative | absolute | fixed | sticky` ancestor wins, per
///   CSS 2.1 §10.1.4.
/// - When `is_fixed` is `true` (`position: fixed`): positioned ancestors
///   are ignored and the CB is the initial containing block per CSS 2.1
///   §10.1.5. Fulgur approximates the initial CB with the nearest `<body>`
///   ancestor (the largest box that matches the page content area for
///   the single-page reftests that exercise this path). True per-page
///   viewport anchoring for paginated output is out of scope here.
/// - In both modes we fall back to `<body>` if no stronger match is
///   found. Returns `None` only for truly detached parent chains (no
///   reachable `<body>`).
///
/// A `MAX_DOM_DEPTH` guard protects against pathological / malformed
/// parent chains, matching the defensive bounds applied elsewhere in
/// `convert.rs` (`debug_print_tree`, `collect_positioned_children`,
/// `resolve_enclosing_anchor`).
fn resolve_cb_for_absolute(
    doc: &blitz_dom::BaseDocument,
    parent: &Node,
    is_fixed: bool,
) -> Option<AbsCb> {
    let mut offset_x = parent.final_layout.location.x;
    let mut offset_y = parent.final_layout.location.y;
    let mut cur_id = parent.parent;
    let mut body_fallback: Option<AbsCb> = None;
    let mut depth: usize = 0;

    while let Some(id) = cur_id {
        if depth >= MAX_DOM_DEPTH {
            break;
        }
        let Some(cur) = doc.get_node(id) else {
            break;
        };
        // `(offset_x, offset_y)` = `parent`'s position expressed in `cur`'s
        // Taffy frame (border-box-origin-relative).
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
                let (padding_box_size, border_top_left) = cb_padding_box(cur);
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

/// Resolve a stylo `Inset` value against a CSS-px basis. Returns `None` for
/// `auto` and other non-length variants.
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

/// Build `PositionedChild` entries for any `::before` / `::after` pseudo whose
/// computed `position` is `absolute` or `fixed`. Each child is placed at the
/// position resolved against the appropriate containing block (see below),
/// converted to pt and expressed relative to the pseudo's parent.
///
/// **Why this isn't just `pseudo.final_layout.location`**: Blitz/Taffy
/// compute the pseudo's layout with its Taffy parent as the containing block.
/// When that parent is `position: static` (the CSS default) the result is
/// wrong: CSS specifies that absolute elements resolve against the nearest
/// `position: relative|absolute|fixed|sticky` ancestor, not the immediate
/// parent. For the before-after-positioned-{002,003} WPT reftests, the
/// pseudo's parent is static, so Taffy places the pseudos at `y=0` relative
/// to that parent (origin of parent's box), while the corresponding ref div
/// is placed by Taffy at `y = body.height - 100`. We recover the correct
/// position here by walking up to the real CB and resolving the pseudo's
/// `top`/`right`/`bottom`/`left` against it. When the parent IS positioned,
/// Taffy's answer is correct and we keep it verbatim.
///
/// Runs ALONGSIDE `wrap_with_block_pseudo_images` at the call sites that
/// construct a `BlockPageable` wrapping a node with pseudos; see
/// fulgur-vlr3 for the full investigation.
/// Caller selects which pseudo slots to consider via `slots` (typically
/// `[node.before]`, `[node.after]`, or both). [`build_absolute_children`]
/// uses single-slot calls to interleave `::before` / direct DOM abs /
/// `::after` in generated (source) order so `::after` paints AFTER
/// direct abs siblings.
pub(super) fn build_absolute_pseudo_children(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    slots: &[Option<usize>],
) -> Vec<PositionedChild> {
    let mut out = Vec::new();
    let parent_is_static = is_position_static(node);
    // `resolve_cb_for_absolute` only depends on `node` and `is_fixed`, so
    // memoize the two possible results we might need to avoid walking the
    // ancestor chain repeatedly when both `::before` and `::after` hit.
    let mut cb_absolute: Option<Option<AbsCb>> = None;
    let mut cb_fixed: Option<Option<AbsCb>> = None;
    for pseudo_id in slots.iter().copied().flatten() {
        let Some(pseudo) = doc.get_node(pseudo_id) else {
            continue;
        };
        if !is_absolutely_positioned(pseudo) {
            continue;
        }
        // CB selection:
        //   - `position: fixed` → skip positioned ancestors, use the
        //     initial CB (body approximation). This holds whether or not
        //     the parent is itself positioned.
        //   - `position: absolute` + static parent → walk to nearest
        //     positioned ancestor, else body.
        //   - `position: absolute` + positioned parent → parent IS the CB;
        //     construct an `AbsCb` from the parent directly so inset
        //     resolution can correct for textless `content:url(...)`
        //     pseudos whose `final_layout.size` is `(0, 0)` (Taffy gives
        //     a wrong location for `right` / `bottom` in that case).
        let cb = if is_position_fixed(pseudo) {
            *cb_fixed.get_or_insert_with(|| resolve_cb_for_absolute(doc, node, true))
        } else if parent_is_static {
            *cb_absolute.get_or_insert_with(|| resolve_cb_for_absolute(doc, node, false))
        } else {
            let (padding_box_size, border_top_left) = cb_padding_box(node);
            Some(AbsCb {
                padding_box_size,
                border_top_left,
                parent_offset_in_cb_bp: (0.0, 0.0),
            })
        };
        let (x_pt, y_pt) = if let Some(cb) = cb {
            // Resolve pseudo position against the real CB (body or nearest
            // positioned ancestor), then express relative to the pseudo's
            // parent.
            if let Some(styles) = pseudo.primary_styles() {
                let pos = styles.get_position();
                let (cb_w, cb_h) = cb.padding_box_size;
                // `right` / `bottom` resolve against the pseudo's effective
                // size (`cb_w - pw - r` etc). For textless `content:url(...)`
                // pseudos Taffy leaves `final_layout.size` at `(0, 0)` and
                // the real size only materializes inside `build_pseudo_image`,
                // so reading `final_layout` here would shift the pseudo by
                // its own width/height. `effective_pseudo_size_px` consults
                // the same fallback `build_absolute_pseudo_child` uses so
                // both stay in sync.
                let (pw, ph) = pseudo::effective_pseudo_size_px(pseudo, node, Some(cb), ctx.assets);
                let left = resolve_inset_px(&pos.left, cb_w);
                let right = resolve_inset_px(&pos.right, cb_w);
                let top = resolve_inset_px(&pos.top, cb_h);
                let bottom = resolve_inset_px(&pos.bottom, cb_h);
                // Over-constrained inset resolution per CSS 2.1 §10.3.7
                // (horizontal) and §10.6.4 (vertical): when both inset
                // properties on an axis are specified, `left` wins over
                // `right` (LTR only — we don't support RTL yet) and `top`
                // wins over `bottom`. Only when the start-side inset is
                // `auto` does the end-side inset determine position.
                //
                // `x_in_pp` / `y_in_pp` are in the CB's padding-box frame
                // (where CSS insets live).
                //
                // **Simplification**: when BOTH inset properties on an axis
                // are `auto`, CSS 2.1 says the element takes its
                // "static position" (where it would sit in normal flow).
                // Computing that correctly requires tracking the pseudo's
                // in-flow position before absolute hoisting, which fulgur
                // does not yet do for pseudo-elements. We fall back to 0 —
                // callers today always specify at least one inset (both
                // WPT before-after-positioned-{002,003} tests specify
                // `right`/`bottom`, and typical UI patterns like
                // `::before { position:absolute; left:-9px; }` specify
                // `left` or `right`). Deviation from spec is tracked
                // alongside the rest of fulgur's position:absolute work.
                let x_in_pp = if let Some(l) = left {
                    l
                } else if let Some(r) = right {
                    cb_w - pw - r
                } else {
                    0.0
                };
                let y_in_pp = if let Some(t) = top {
                    t
                } else if let Some(b) = bottom {
                    cb_h - ph - b
                } else {
                    0.0
                };
                // Convert padding-box frame → CB's border-box frame by
                // adding CB's `(border_left, border_top)`, then subtract
                // the parent's border-box offset in CB's frame to get the
                // pseudo's position relative to its parent's border-box
                // (which is what `PositionedChild` expects).
                let (bl, bt) = cb.border_top_left;
                let (ox, oy) = cb.parent_offset_in_cb_bp;
                (px_to_pt(x_in_pp + bl - ox), px_to_pt(y_in_pp + bt - oy))
            } else {
                let (x, y, _, _) = layout_in_pt(&pseudo.final_layout);
                (x, y)
            }
        } else {
            // Parent IS positioned (or CB couldn't be resolved) — Taffy's
            // pseudo.final_layout.location is already correct.
            let (x, y, _, _) = layout_in_pt(&pseudo.final_layout);
            (x, y)
        };
        let child = build_absolute_pseudo_child(doc, node, pseudo, pseudo_id, cb, ctx, depth);
        // CSS 2.1 §10.6.4: abs pseudos are out-of-flow — they don't add to
        // the parent's flow height and replicate across pagination splits
        // anchored to their CB.
        out.push(PositionedChild {
            child,
            x: x_pt,
            y: y_pt,
            out_of_flow: true,
        });
    }
    out
}

/// Build `PositionedChild` entries for any direct DOM child of `node` whose
/// computed `position` is `absolute` or `fixed` and which is not a pseudo
/// (`::before` / `::after`) — pseudos go through
/// `build_absolute_pseudo_children`. Returned entries carry
/// `out_of_flow: true` so they don't contribute to the parent's flow height
/// and replicate across pagination splits anchored to their CB
/// (CSS 2.1 §10.6.4 / fulgur-aijf).
///
/// CB resolution and inset handling mirror the pseudo path verbatim — the
/// only differences are:
///   - we iterate `node.children` (DOM children) instead of pseudo slots;
///   - the child's effective size comes from Taffy's `final_layout.size`
///     directly (real elements with real layout, no `content:url()` zero-size
///     fallback);
///   - the child Pageable is built via `convert_node` unconditionally — no
///     `build_pseudo_image` shortcut.
///
/// Scope (matches the pseudo path's scope): emits at the **direct parent**,
/// not at the deepest CB ancestor. Sufficient for the body-direct abs case
/// (page-background-002/003 reftests). Deeply-nested abs whose CB is several
/// ancestors above the direct parent and which then needs to participate in
/// the CB ancestor's pagination is a future enhancement.
pub(super) fn build_absolute_non_pseudo_children(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Vec<PositionedChild> {
    if depth >= MAX_DOM_DEPTH {
        return Vec::new();
    }
    let mut out = Vec::new();
    let parent_is_static = is_position_static(node);
    let mut cb_absolute: Option<Option<AbsCb>> = None;
    let mut cb_fixed: Option<Option<AbsCb>> = None;

    let lc_guard = node.layout_children.borrow();
    let effective_children = lc_guard.as_deref().unwrap_or(&node.children);

    for &child_id in effective_children {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };
        if !is_absolutely_positioned(child_node) {
            continue;
        }
        // Pseudos are handled by `build_absolute_pseudo_children`.
        if is_pseudo_node(doc, child_node) {
            continue;
        }

        let cb = if is_position_fixed(child_node) {
            *cb_fixed.get_or_insert_with(|| resolve_cb_for_absolute(doc, node, true))
        } else if parent_is_static {
            *cb_absolute.get_or_insert_with(|| resolve_cb_for_absolute(doc, node, false))
        } else {
            let (padding_box_size, border_top_left) = cb_padding_box(node);
            Some(AbsCb {
                padding_box_size,
                border_top_left,
                parent_offset_in_cb_bp: (0.0, 0.0),
            })
        };

        let (x_pt, y_pt) = if let Some(cb) = cb {
            if let Some(styles) = child_node.primary_styles() {
                let pos = styles.get_position();
                let (cb_w, cb_h) = cb.padding_box_size;
                // Real elements (not zero-size content:url pseudos), so use
                // Taffy's final_layout.size directly for over-constrained
                // inset resolution.
                let cw = child_node.final_layout.size.width;
                let ch = child_node.final_layout.size.height;
                let left = resolve_inset_px(&pos.left, cb_w);
                let right = resolve_inset_px(&pos.right, cb_w);
                let top = resolve_inset_px(&pos.top, cb_h);
                let bottom = resolve_inset_px(&pos.bottom, cb_h);
                // CSS 2.1 §10.3.7 / §10.6.4 over-constrained resolution:
                // start-side inset wins (LTR-only). Both `auto` falls back
                // to 0 — same simplification as the pseudo path.
                let x_in_pp = if let Some(l) = left {
                    l
                } else if let Some(r) = right {
                    cb_w - cw - r
                } else {
                    0.0
                };
                let y_in_pp = if let Some(t) = top {
                    t
                } else if let Some(b) = bottom {
                    cb_h - ch - b
                } else {
                    0.0
                };
                let (bl, bt) = cb.border_top_left;
                let (ox, oy) = cb.parent_offset_in_cb_bp;
                (px_to_pt(x_in_pp + bl - ox), px_to_pt(y_in_pp + bt - oy))
            } else {
                let (x, y, _, _) = layout_in_pt(&child_node.final_layout);
                (x, y)
            }
        } else {
            // Parent IS positioned (or CB couldn't be resolved) — Taffy's
            // final_layout.location is already correct.
            let (x, y, _, _) = layout_in_pt(&child_node.final_layout);
            (x, y)
        };

        let child = convert_node(doc, child_id, ctx, depth + 1);
        out.push(PositionedChild {
            child,
            x: x_pt,
            y: y_pt,
            out_of_flow: true,
        });
    }
    out
}

/// Combined entry point: returns ALL absolutely-positioned children that
/// hoist to `node` — both pseudos (`::before`/`::after`) and direct DOM
/// children. Call sites that previously used `build_absolute_pseudo_children`
/// should switch to this so non-pseudo abs descendants are picked up too.
///
/// Output order matches **generated/source order**: `::before` first, then
/// direct DOM abs/fixed children in DOM order, then `::after`. This matches
/// CSS paint order so a `::after` overlay correctly paints on top of the
/// direct abs siblings instead of beneath them.
pub(super) fn build_absolute_children(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Vec<PositionedChild> {
    let mut out = build_absolute_pseudo_children(doc, node, ctx, depth, &[node.before]);
    out.extend(build_absolute_non_pseudo_children(doc, node, ctx, depth));
    out.extend(build_absolute_pseudo_children(
        doc,
        node,
        ctx,
        depth,
        &[node.after],
    ));
    out
}

/// Build the `Pageable` for a single absolutely-positioned pseudo.
///
/// For a textless `content: url(...)` pseudo, Blitz never assigns a
/// non-zero `final_layout.size` (see `build_pseudo_image`'s comment), so
/// the generic `convert_node → convert_content_url` path would size the
/// image to zero and silently drop it. Detect that shape here and route
/// through `build_pseudo_image` so computed `width` / `height` (or the
/// image's intrinsic dimensions) drive the size instead.
///
/// Pseudos with visual style (background, border, padding, box-shadow)
/// fall back to `convert_node` because `build_pseudo_image` produces a
/// bare `ImagePageable` that would drop those decorations. That edge case
/// (absolute pseudo + content:url + visual style + zero final_layout) is
/// narrow enough to defer to a follow-up.
fn build_absolute_pseudo_child(
    doc: &blitz_dom::BaseDocument,
    parent: &Node,
    pseudo: &Node,
    pseudo_id: usize,
    cb: Option<AbsCb>,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    if let Some(img) = try_build_absolute_pseudo_image(pseudo, parent, cb, ctx.assets) {
        return Box::new(img);
    }
    convert_node(doc, pseudo_id, ctx, depth + 1)
}

/// Shortcut for the textless `content: url(...)` abs pseudo case shared by
/// both child construction (`build_absolute_pseudo_child`) and inset
/// resolution (`effective_pseudo_size_px`). Returns `None` when the pseudo
/// is not a content:url shape, has visual style that requires the wrapping
/// path, or `build_pseudo_image` itself returns `None`.
///
/// `cb` must be the same value the caller will use for inset resolution so
/// the size and position stay in sync.
pub(super) fn try_build_absolute_pseudo_image(
    pseudo: &Node,
    parent: &Node,
    cb: Option<AbsCb>,
    assets: Option<&AssetBundle>,
) -> Option<ImagePageable> {
    crate::blitz_adapter::extract_content_image_url(pseudo)?;
    let pseudo_style = extract_block_style(pseudo, assets);
    if pseudo_style.has_visual_style() {
        return None;
    }
    // CSS spec: percentage `width` / `height` on an absolutely-positioned
    // element resolve against the CB's padding-box.
    // - cb=Some: we already resolved the CB → use its padding-box.
    // - cb=None: parent is the CB; approximate with the parent's border-box
    //   dims. Percentage width/height on an absolute pseudo whose parent has
    //   padding resolves slightly off, but content:url() pseudos typically
    //   use pixel sizing so the common case is handled correctly.
    //
    // `build_pseudo_image` expects `parent_*` arguments in PDF pt (it runs
    // them back through `pt_to_px` to set the percentage basis).
    // `AbsCb::padding_box_size` and Taffy's `final_layout.size` are both in
    // CSS px, so convert before calling.
    let (basis_w_pt, basis_h_pt) = if let Some(cb) = cb {
        let (w_px, h_px) = cb.padding_box_size;
        (px_to_pt(w_px), px_to_pt(h_px))
    } else {
        (
            px_to_pt(parent.final_layout.size.width),
            px_to_pt(parent.final_layout.size.height),
        )
    };
    pseudo::build_pseudo_image(pseudo, basis_w_pt, basis_h_pt, assets)
}
