use serde::Serialize;
use std::path::Path;
use std::rc::Rc;

use crate::{
    MAX_PDF_CONTENT_BYTES, MAX_PDF_FONT_NAME_BYTES, MAX_PDF_GS_STACK_DEPTH,
    MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES, MAX_PDF_INSPECT_ITEMS, MAX_PDF_INSPECT_OPERATIONS,
    MAX_PDF_INSPECT_TEXT_BYTES, MAX_PDF_METADATA_FIELD_BYTES, MAX_PDF_PARENT_DEPTH,
    PDF_CONTENT_STREAM_COST_FLOOR_BYTES,
};

const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

#[derive(Debug, Serialize, PartialEq)]
pub struct InspectResult {
    pub pages: u32,
    pub metadata: Metadata,
    pub text_items: Vec<TextItem>,
    pub images: Vec<ImageItem>,
}

#[derive(Debug, Serialize, PartialEq, Default)]
pub struct Metadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct TextItem {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub text: String,
    pub font: String,
    pub font_size: f32,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ImageItem {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub format: String,
    pub width_px: u32,
    pub height_px: u32,
}

/// The whole-document resource budgets one `inspect` call may spend.
///
/// Held in a struct rather than read from the constants directly so that tests
/// can drive each bound with a small budget. Reaching a production budget means
/// actually doing the work it allows — 20M operations takes ~70 s in a debug
/// build — which would otherwise force a choice between an untested bound and a
/// constant sized for its test rather than for real documents.
///
/// [`Default`] is the production configuration; nothing outside tests
/// constructs it any other way.
struct InspectLimits {
    /// See [`MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES`].
    content_total_bytes: usize,
    /// See [`MAX_PDF_INSPECT_OPERATIONS`].
    operations: usize,
    /// See [`MAX_PDF_INSPECT_TEXT_BYTES`].
    text_bytes: usize,
    /// See [`MAX_PDF_INSPECT_ITEMS`].
    items: usize,
}

impl Default for InspectLimits {
    fn default() -> Self {
        Self {
            content_total_bytes: MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES,
            operations: MAX_PDF_INSPECT_OPERATIONS,
            text_bytes: MAX_PDF_INSPECT_TEXT_BYTES,
            items: MAX_PDF_INSPECT_ITEMS,
        }
    }
}

pub fn inspect(path: &Path) -> crate::Result<InspectResult> {
    let doc = lopdf::Document::load(path)
        .map_err(|e| crate::Error::Other(format!("Failed to load PDF: {e}")))?;

    let limits = InspectLimits::default();
    // `get_pages()` is `page_iter().enumerate().collect()`, so counting straight
    // off the iterator is the same walk without materialising a `BTreeMap` whose
    // size is bounded only by the document's object count.
    let pages = doc.page_iter().count() as u32;
    let metadata = extract_metadata(&doc);
    let text_items = extract_text_items(&doc, &limits)?;
    let images = extract_image_items(&doc, &limits)?;

    Ok(InspectResult {
        pages,
        metadata,
        text_items,
        images,
    })
}

fn obj_as_name_str(obj: &lopdf::Object) -> Option<&str> {
    obj.as_name().ok().and_then(|b| std::str::from_utf8(b).ok())
}

/// Truncate `s` to at most `max_bytes`, rounding *down* to a UTF-8 character
/// boundary so a clamp point inside a multi-byte character cuts before that
/// character rather than panicking on a sliced code point.
fn clamp_str_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Clamp a `/Tf` font resource name to [`MAX_PDF_FONT_NAME_BYTES`].
///
/// Every [`TextItem`] retains a copy of the font name in effect, so an
/// unclamped name multiplies into the result once per record — see
/// [`MAX_PDF_FONT_NAME_BYTES`] for why the per-page content bound does not
/// cover that product.
fn clamp_font_name(name: &str) -> &str {
    clamp_str_bytes(name, MAX_PDF_FONT_NAME_BYTES)
}

/// Follow reference aliases to the object an id really names.
///
/// `Document::get_page_contents` returns the ids written in a `/Contents` array
/// without dereferencing them, while `get_object` does follow chains — so two
/// distinct alias objects pointing at one stream reach the same content, and a
/// cache keyed on the immediate id would miss for every alias.
///
/// Returns `None` when the chain does not terminate within
/// [`MAX_PDF_PARENT_DEPTH`] links, which is what makes this the *only*
/// dereference deciding the cache key. `get_object` starts a fresh `DEREF_LIMIT`
/// budget from whatever id it is handed, so stopping mid-chain and letting it
/// finish the walk would compose two limits: aliases converging past this bound
/// would each yield a different key while still resolving to one stream, and
/// every one would re-run its filter chain.
///
/// The bound is the *link* budget, so the loop runs one iteration more than that:
/// the extra pass performs no hop, it only probes the object the last permitted
/// hop landed on. This matches lopdf exactly — `Document::dereference` increments
/// after following a reference and fails on `nb_deref > DEREF_LIMIT`, so a chain
/// of precisely [`MAX_PDF_PARENT_DEPTH`] references resolves there and must
/// resolve here too. Returning `None` one link early would silently skip a stream
/// the previous extraction path read.
fn canonical_object_id(doc: &lopdf::Document, id: lopdf::ObjectId) -> Option<lopdf::ObjectId> {
    let mut hops = MAX_PDF_PARENT_DEPTH;
    canonical_object_id_within(doc, id, &mut hops)
}

/// [`canonical_object_id`] against a caller-owned hop budget, for walks that
/// canonicalise repeatedly and must bound the *total* rather than each chain.
///
/// Spends one hop per reference followed and decrements `hops` in place, so a
/// caller threading one budget through many calls pays for every hop once.
/// Probing the target costs nothing, which is what keeps a fresh
/// `MAX_PDF_PARENT_DEPTH` budget exactly equivalent to lopdf's `DEREF_LIMIT`:
/// 128 references resolve, 129 do not.
fn canonical_object_id_within(
    doc: &lopdf::Document,
    mut id: lopdf::ObjectId,
    hops: &mut usize,
) -> Option<lopdf::ObjectId> {
    loop {
        match doc.objects.get(&id) {
            Some(lopdf::Object::Reference(next)) => {
                if *hops == 0 {
                    return None;
                }
                *hops -= 1;
                id = *next;
            }
            // Not a reference (the target), or absent — either way this is the id
            // the chain names; a missing object is skipped downstream.
            _ => return Some(id),
        }
    }
}

/// Decoded content-stream bytes, keyed by *canonical* stream object id, so a
/// stream shared by several pages — directly or through aliases — is decoded
/// once. Retained bytes are bounded by the same
/// [`MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES`] budget that charges each distinct
/// stream as it is decoded.
type DecodedStreams = std::collections::BTreeMap<lopdf::ObjectId, Rc<Vec<u8>>>;

/// Outcome of gathering one page's content, which distinguishes "skip this
/// page" from "stop walking pages".
enum PageContent {
    /// Decoded content for this page, within both the per-page and the
    /// whole-document bound.
    Ready(Vec<u8>),
    /// This page exceeds [`MAX_PDF_CONTENT_BYTES`]. Later pages are still
    /// walked — only this one is skipped.
    Skip,
    /// The whole-document budget is spent; the caller stops walking pages.
    Exhausted,
}

/// Gather a page's decoded content streams, abandoning the page as soon as the
/// running total exceeds [`MAX_PDF_CONTENT_BYTES`] and reporting exhaustion once
/// `doc_budget` — the document's remaining
/// [`MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES`] allowance — is spent.
///
/// This exists instead of `lopdf::Document::get_page_content` because that
/// method inflates *every* content stream of the page and concatenates them
/// before returning: checking the length of its result would bound only what
/// gets parsed, not what gets allocated. Accumulating with a running budget
/// bounds the concatenated total.
///
/// For a page within both bounds the result is byte-for-byte what
/// `get_page_content` returns, which is what keeps extraction unchanged for
/// real documents: each stream contributes its decoded bytes — or its raw,
/// still-encoded bytes when decoding fails, as `lopdf` does — followed by the
/// `\n` separator that stops tokens merging across a stream boundary
/// (`…Tj` + `q…` would otherwise lex as `Tjq`). Content stream ids that do not
/// resolve to a stream object are skipped individually, not treated as
/// abandoning the page.
///
/// The `\n` separators count against the per-page budget, so the per-page
/// accept/reject decision is identical to testing `get_page_content`'s total:
/// the running total rises monotonically to exactly that sum, so some prefix
/// exceeds the bound if and only if the total does.
///
/// `doc_budget` is charged *before* the per-page verdict is known, so a page that
/// is about to be skipped still pays for the work its decoding cost. Charging
/// only usable pages would leave the aggregate unbounded, since a page can be
/// made to decode a full per-page allowance and then be rejected.
///
/// `decoded_cache` holds each content stream's decoded bytes, so a stream shared
/// by many pages is decoded once. That matters beyond the saved time: a stream's
/// *intermediate* filter output is invisible from here — `/Filter [/FlateDecode
/// /ASCII85Decode]` can expand a small payload into megabytes of whitespace that
/// the second stage reduces to nothing, leaving both the encoded and decoded
/// lengths small — so no charge computed from those two lengths can bound
/// repeated decoding of it. Decoding each stream once reduces that to a single
/// occurrence per document, which is the same residual as a single oversized
/// stream (see [`MAX_PDF_CONTENT_BYTES`]).
///
/// Charges split accordingly: filter work once per distinct stream, and the copy
/// into the page's content on every reference to it.
fn gather_page_content(
    doc: &lopdf::Document,
    page_id: lopdf::ObjectId,
    doc_budget: &mut usize,
    decoded_cache: &mut DecodedStreams,
) -> PageContent {
    let mut content: Vec<u8> = Vec::new();
    for entry_id in doc.get_page_contents(page_id) {
        // Charge the attempt before anything else — canonicalising, resolving and
        // decoding all cost a lookup regardless of how they turn out, so an entry
        // that resolves to nothing, or whose chain is too deep to follow, must not
        // be free. A `/Contents` array of such entries is shareable across pages
        // like any other array.
        *doc_budget = doc_budget.saturating_sub(PDF_CONTENT_STREAM_COST_FLOOR_BYTES);
        if *doc_budget == 0 {
            return PageContent::Exhausted;
        }
        // Alias objects must collapse to the stream they name, or each alias is
        // a fresh cache miss and re-runs the filter chain.
        let Some(object_id) = canonical_object_id(doc, entry_id) else {
            continue;
        };
        let bytes: Rc<Vec<u8>> = match decoded_cache.get(&object_id) {
            // Decoded already for an earlier page: no filter work to charge, only
            // the copy into `content` below.
            Some(cached) => Rc::clone(cached),
            None => {
                let Ok(stream) = doc.get_object(object_id).and_then(lopdf::Object::as_stream)
                else {
                    continue;
                };
                let decoded = match stream.decompressed_content() {
                    Ok(d) => d,
                    Err(_) => stream.content.clone(),
                };
                // Filter work, charged once per distinct stream: the encoded
                // length, because a filter reads every one of those bytes even
                // when it yields nothing — `/ASCII85Decode` over a megabyte of
                // whitespace decodes to zero after scanning all of it.
                let encoded = stream.content.len();
                *doc_budget = doc_budget
                    .saturating_sub(encoded.saturating_sub(PDF_CONTENT_STREAM_COST_FLOOR_BYTES));
                let cached = Rc::new(decoded);
                decoded_cache.insert(object_id, Rc::clone(&cached));
                if *doc_budget == 0 {
                    return PageContent::Exhausted;
                }
                cached
            }
        };
        // Charged on every reference, hit or miss: these bytes are copied into
        // this page's `content` even when the decode was cached, so the
        // concatenation stays bounded independently of the filter work. The
        // per-reference floor is already paid at the top of the loop.
        *doc_budget = doc_budget.saturating_sub(bytes.len() + 1);
        if *doc_budget == 0 {
            return PageContent::Exhausted;
        }
        if content.len() + bytes.len() + 1 > MAX_PDF_CONTENT_BYTES {
            return PageContent::Skip;
        }
        content.extend_from_slice(&bytes);
        content.push(b'\n');
    }
    PageContent::Ready(content)
}

fn extract_metadata(doc: &lopdf::Document) -> Metadata {
    let mut meta = Metadata::default();
    let info_id = match doc.trailer.get(b"Info") {
        Ok(obj) => match obj.as_reference() {
            Ok(id) => id,
            Err(_) => return meta,
        },
        Err(_) => return meta,
    };
    let info = match doc.get_object(info_id) {
        Ok(lopdf::Object::Dictionary(d)) => d,
        _ => return meta,
    };

    // Clamped: an `/Info` dictionary can live inside a Flate-compressed object
    // stream, so a small file can carry an arbitrarily large `/Title`. This pass
    // has no per-page or per-record structure for the text and item budgets to
    // bound, so without a clamp a metadata-only document produces output
    // proportional to the decompressed payload. See
    // `MAX_PDF_METADATA_FIELD_BYTES`.
    let get_str = |dict: &lopdf::Dictionary, key: &[u8]| -> Option<String> {
        dict.get(key)
            .ok()
            .and_then(|o| o.as_str().ok())
            .map(|bytes| {
                // Decode a bounded *prefix*: decoding the whole value and truncating
                // afterwards would still allocate all of it, and the Latin-1 path
                // widens each byte to as much as two UTF-8 bytes on the way.
                let decoded = decode_pdf_string(metadata_prefix(bytes));
                clamp_str_bytes(&decoded, MAX_PDF_METADATA_FIELD_BYTES).to_owned()
            })
    };

    meta.title = get_str(info, b"Title");
    meta.author = get_str(info, b"Author");
    meta.creator = get_str(info, b"Creator");
    meta.created_at = get_str(info, b"CreationDate");
    meta.modified_at = get_str(info, b"ModDate");
    meta
}

fn extract_text_items(
    doc: &lopdf::Document,
    limits: &InspectLimits,
) -> crate::Result<Vec<TextItem>> {
    use lopdf::content::Operation;
    let mut items = Vec::new();
    // Whole-document budgets. Page count is bounded only by input size, so the
    // per-page content bound and the record *count* cap both leave a document
    // total unbounded — see the constants for the measured shapes.
    let mut content_budget = limits.content_total_bytes;
    let mut op_budget = limits.operations;
    let mut text_bytes: usize = 0;
    let mut decoded_cache = DecodedStreams::new();

    // Iterated lazily rather than through `get_pages()`, which collects the whole
    // page tree into a `BTreeMap` before the first budget check can break. Page
    // entries are cheap to mass-produce, so that traversal and allocation ran in
    // full even when page 1 exhausted a budget. `get_pages()` numbers pages
    // `enumerate() + 1` off this same iterator, so the numbering is unchanged.
    for (i, page_id) in doc.page_iter().enumerate() {
        let page_num = (i + 1) as u32;
        if items.len() >= limits.items || text_bytes >= limits.text_bytes || op_budget == 0 {
            break;
        }
        // Every page costs a floor, charged before any work and before any early
        // exit, so no page can be walked for free. Page objects are the cheapest
        // thing to mass-produce in a compressed object stream.
        content_budget = content_budget.saturating_sub(PDF_CONTENT_STREAM_COST_FLOOR_BYTES);
        if content_budget == 0 {
            break;
        }
        let content_bytes =
            match gather_page_content(doc, page_id, &mut content_budget, &mut decoded_cache) {
                PageContent::Ready(b) => b,
                PageContent::Skip => continue,
                PageContent::Exhausted => break,
            };
        let content = match lopdf::content::Content::decode(&content_bytes) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // Charged after decoding, so this page — already paid for — is still
        // walked and the *next* page is the one refused.
        op_budget = op_budget.saturating_sub(content.operations.len());

        let identity = IDENTITY;
        // Graphics state stack: (CTM, font_name, font_size).
        // Tf is part of the graphics state (PDF §8.4.5 Table 52), so q/Q save/restore it.
        //
        // The font name is an `Rc<str>` rather than a `String` because `q` saves a
        // copy of the whole entry: a deep run of `q` operators against a wide
        // `/Tf` resource name would otherwise duplicate that name once per level.
        let mut gs_stack: Vec<([f32; 6], Rc<str>, f32)> =
            vec![(identity, Rc::from("unknown"), 12.0)];
        // `q` pushes dropped for exceeding MAX_PDF_GS_STACK_DEPTH. Counted so the
        // matching `Q` operators unwind them instead of popping real entries.
        let mut dropped_pushes: usize = 0;
        // Text matrix linear components (scale/rotation); updated by Tm, reset by BT.
        let mut tm_a: f32 = 1.0;
        let mut tm_b: f32 = 0.0;
        let mut tm_c: f32 = 0.0;
        let mut tm_d: f32 = 1.0;
        // Text line matrix translation in user space; updated by Tm/Td/TD/T*, reset by BT.
        let mut tlm_e: f32 = 0.0;
        let mut tlm_f: f32 = 0.0;
        // Current text origin in page space.
        let mut tx: f32 = 0.0;
        let mut ty: f32 = 0.0;
        let mut font_name: Rc<str> = Rc::from("unknown");
        let mut font_size: f32 = 12.0;
        let mut text_leading: f32 = 0.0;

        for Operation { operator, operands } in &content.operations {
            if items.len() >= limits.items || text_bytes >= limits.text_bytes {
                break;
            }
            // Inside a `q` frame dropped for exceeding MAX_PDF_GS_STACK_DEPTH.
            // The frame has no stack entry of its own, so a state change here
            // would mutate the deepest *retained* entry and survive the
            // matching `Q`, leaking into the enclosing scope. Discard the
            // frame's contents entirely — only the `q`/`Q` bookkeeping that
            // finds the end of the frame is honoured — which is what keeps the
            // outer state exactly restorable.
            if dropped_pushes > 0 {
                match operator.as_str() {
                    "q" => dropped_pushes += 1,
                    "Q" => dropped_pushes -= 1,
                    _ => {}
                }
                continue;
            }
            match operator.as_str() {
                "q" if gs_stack.len() >= MAX_PDF_GS_STACK_DEPTH => {
                    dropped_pushes += 1;
                }
                "q" => {
                    let top = gs_stack.last().expect("gs_stack non-empty").clone();
                    gs_stack.push(top);
                }
                "Q" if gs_stack.len() > 1 => {
                    gs_stack.pop();
                    let (_, ref saved_font, saved_size) =
                        *gs_stack.last().expect("gs_stack non-empty after Q");
                    font_name = Rc::clone(saved_font);
                    font_size = saved_size;
                }
                "cm" if operands.len() == 6 => {
                    let new_m = [
                        obj_to_f32(&operands[0]),
                        obj_to_f32(&operands[1]),
                        obj_to_f32(&operands[2]),
                        obj_to_f32(&operands[3]),
                        obj_to_f32(&operands[4]),
                        obj_to_f32(&operands[5]),
                    ];
                    if let Some(gs) = gs_stack.last_mut() {
                        gs.0 = concat_matrix(&gs.0, &new_m);
                    }
                }
                "Tf" => {
                    if let (Some(name_obj), Some(size)) = (operands.first(), operands.get(1)) {
                        font_name = Rc::from(clamp_font_name(
                            obj_as_name_str(name_obj).unwrap_or("unknown"),
                        ));
                        font_size = obj_to_f32(size);
                        if let Some(gs) = gs_stack.last_mut() {
                            gs.1 = Rc::clone(&font_name);
                            gs.2 = font_size;
                        }
                    }
                }
                "TL" if !operands.is_empty() => {
                    text_leading = obj_to_f32(&operands[0]);
                }
                // BT resets the text matrix and text line matrix to identity (PDF §9.4.1).
                // tx/ty are derived from tlm + CTM: text origin is the CTM translation.
                "BT" => {
                    tm_a = 1.0;
                    tm_b = 0.0;
                    tm_c = 0.0;
                    tm_d = 1.0;
                    tlm_e = 0.0;
                    tlm_f = 0.0;
                    let ctm = gs_stack.last().map(|gs| &gs.0).unwrap_or(&identity);
                    tx = ctm[4];
                    ty = ctm[5];
                }
                "Tm" if operands.len() >= 6 => {
                    tm_a = obj_to_f32(&operands[0]);
                    tm_b = obj_to_f32(&operands[1]);
                    tm_c = obj_to_f32(&operands[2]);
                    tm_d = obj_to_f32(&operands[3]);
                    tlm_e = obj_to_f32(&operands[4]);
                    tlm_f = obj_to_f32(&operands[5]);
                    let ctm = gs_stack.last().map(|gs| &gs.0).unwrap_or(&identity);
                    tx = ctm[0] * tlm_e + ctm[2] * tlm_f + ctm[4];
                    ty = ctm[1] * tlm_e + ctm[3] * tlm_f + ctm[5];
                }
                // Td/TD advances the text line matrix in text space (PDF §9.4.2).
                // The offset (dx, dy) is in text coordinates; multiply through the
                // linear part of the text matrix to get user-space displacement.
                // TD also sets the text leading to -dy (PDF §9.4.2).
                "Td" | "TD" if operands.len() >= 2 => {
                    let dx = obj_to_f32(&operands[0]);
                    let dy = obj_to_f32(&operands[1]);
                    if operator == "TD" {
                        text_leading = -dy;
                    }
                    tlm_e += dx * tm_a + dy * tm_c;
                    tlm_f += dx * tm_b + dy * tm_d;
                    let ctm = gs_stack.last().map(|gs| &gs.0).unwrap_or(&identity);
                    tx = ctm[0] * tlm_e + ctm[2] * tlm_f + ctm[4];
                    ty = ctm[1] * tlm_e + ctm[3] * tlm_f + ctm[5];
                }
                // T* ≡ Td 0 -text_leading (PDF §9.4.2).
                "T*" => {
                    tlm_e += (-text_leading) * tm_c;
                    tlm_f += (-text_leading) * tm_d;
                    let ctm = gs_stack.last().map(|gs| &gs.0).unwrap_or(&identity);
                    tx = ctm[0] * tlm_e + ctm[2] * tlm_f + ctm[4];
                    ty = ctm[1] * tlm_e + ctm[3] * tlm_f + ctm[5];
                }
                "Tj" => {
                    if let Some(text_obj) = operands.first()
                        && let Ok(bytes) = text_obj.as_str()
                    {
                        let text = decode_pdf_string(bytes);
                        if !text.trim().is_empty() {
                            let w = estimate_width(&text, font_size);
                            text_bytes += text.len();
                            items.push(TextItem {
                                page: page_num,
                                x: tx,
                                y: ty,
                                width: w,
                                height: font_size,
                                text,
                                font: font_name.to_string(),
                                font_size,
                            });
                            tx += w;
                        }
                    }
                }
                "TJ" => {
                    if let Some(array_obj) = operands.first()
                        && let Ok(array) = array_obj.as_array()
                    {
                        let mut combined = String::new();
                        for elem in array {
                            if let Ok(bytes) = elem.as_str() {
                                combined.push_str(&decode_pdf_string(bytes));
                            }
                        }
                        if !combined.trim().is_empty() {
                            let w = estimate_width(&combined, font_size);
                            text_bytes += combined.len();
                            items.push(TextItem {
                                page: page_num,
                                x: tx,
                                y: ty,
                                width: w,
                                height: font_size,
                                text: combined,
                                font: font_name.to_string(),
                                font_size,
                            });
                            tx += w;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(items)
}

/// Find the `Resources` dictionary in effect for a page, following `/Parent`
/// inheritance.
///
/// Returns the dictionary *by reference*, along with an identity the caller can
/// use to recognise when two pages share it.
///
/// Both matter for bounding work: a resources dictionary is routinely shared by
/// every page in a document, so cloning it — as this used to — made a page's cost
/// proportional to the dictionary's size, and the identity is what lets the
/// caller scan it only once.
///
/// The identity is [`ResourcesOrigin`] rather than the dereferenced object id,
/// because an object id alone does not identify the dictionary — see that type.
///
/// A fixed depth bound guarantees termination on malformed inputs with cyclic
/// (`A -> B -> A`) or pathologically long `/Parent` references, without needing
/// a heap-allocated visited set — real page trees are shallow enough that
/// [`MAX_PDF_PARENT_DEPTH`] is orders of magnitude above any legitimate document.
///
/// That bound is a single budget **shared** between the `/Parent` walk and the
/// alias canonicalisation at each node, not one bound per axis. Nesting them
/// would multiply: `/Parent` references may themselves be alias chains, so
/// per-axis bounds let one page occurrence cost `MAX_PDF_PARENT_DEPTH` squared
/// object lookups — 16k rather than 128 — while the caller charges only the
/// per-page floor. A page repeated across `/Kids` then replays that cost per
/// occurrence, and a 1.5 MB file spent 16.7 s here against a 128 MiB budget it
/// never came close to using. Sharing the budget keeps the total probes per page
/// a small constant and, since no legitimate document has aliased `/Parent`
/// links at all, costs real inputs nothing.
fn resolve_page_resources(
    doc: &lopdf::Document,
    page_id: lopdf::ObjectId,
) -> Option<(ResourcesOrigin, &lopdf::Dictionary)> {
    let mut current_id = page_id;
    let mut hops = MAX_PDF_PARENT_DEPTH;
    loop {
        // Canonicalise before both the lookup and the identity: page-tree `Kids`
        // entries and `/Parent` back-references may be alias objects, and
        // `get_object` follows them silently. Keying `DirectIn` on the immediate
        // id would hand one shared page dictionary a distinct identity per alias,
        // so the cache would rescan and retain a separate map for each.
        let owner_id = canonical_object_id_within(doc, current_id, &mut hops)?;
        let dict = match doc.get_object(owner_id) {
            Ok(lopdf::Object::Dictionary(d)) => d,
            _ => return None,
        };
        if let Ok(res) = dict.get(b"Resources")
            && let Ok((id, lopdf::Object::Dictionary(resources))) = doc.dereference(res)
        {
            let origin = match id {
                // `/Resources N 0 R`: the dictionary *is* object N.
                Some(id) => ResourcesOrigin::Object(id),
                // A direct dictionary nested inside the object being examined
                // — whether that is the page itself or an ancestor node.
                None => ResourcesOrigin::DirectIn(owner_id),
            };
            return Some((origin, resources));
        }
        // Following `/Parent` is a hop against the same budget, so a chain of
        // ancestors and a chain of aliases cannot be spent independently.
        match dict.get(b"Parent").and_then(|p| p.as_reference()) {
            Ok(parent_id) => {
                if hops == 0 {
                    return None;
                }
                hops -= 1;
                current_id = parent_id;
            }
            Err(_) => return None,
        }
    }
}

/// Identity of the resources dictionary in effect for a page — the cache key for
/// [`collect_image_xobjects`].
///
/// An object id alone is not an identity, which is why this is an enum. Object
/// `N` can name *two different dictionaries* here: as `/Resources N 0 R` the
/// dictionary is object `N` itself, while as a page-tree node it may hold a
/// direct `/Resources` sub-dictionary, which is something else entirely. Keying
/// both on `N` let whichever page was visited first decide the other's images.
///
/// Every variant is cacheable — there is no "cannot be shared" case. A direct
/// dictionary on a page looks unique but is not: a page tree may list the same
/// `/Page` object more than once, and each occurrence is walked separately, so
/// leaving it uncached rescans it per occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ResourcesOrigin {
    /// Reached through `/Resources N 0 R` — the dictionary is object `N`.
    Object(lopdf::ObjectId),
    /// A direct `/Resources` dictionary written inside object `N`, reachable by
    /// any page whose `/Parent` walk arrives at `N` — including `N` itself when
    /// it is the page.
    DirectIn(lopdf::ObjectId),
}

/// Identity of the `/XObject` dictionary a page's images are scanned from — the
/// cache key for [`collect_image_xobjects`].
///
/// Keyed on the `/XObject` dictionary rather than on the enclosing resources
/// dictionary, because that is what the scan result actually depends on. The two
/// differ whenever pages carry *distinct* resources dictionaries that all point
/// `/XObject` at one shared indirect dictionary: keying on the resources
/// dictionary would rescan the shared one once per page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum XObjectsOrigin {
    /// `/XObject N 0 R` — the dictionary is object `N`, shareable by any number
    /// of otherwise-unrelated resources dictionaries.
    Object(lopdf::ObjectId),
    /// A direct `/XObject` dictionary written inside the resources dictionary
    /// with this origin — or no `/XObject` entry at all, whose empty result is
    /// equally a property of that resources dictionary.
    DirectIn(ResourcesOrigin),
}

/// The identity of the `/XObject` dictionary reachable from `resources`.
///
/// Uses the id `Document::dereference` ends on, not the immediate one, because it
/// follows reference chains: `/XObject 10 0 R` where object 10 is itself
/// `11 0 R` reads the dictionary in object 11. Keying on the immediate id would
/// give every page a distinct key for one shared dictionary whenever each is
/// handed its own cheap alias object.
fn xobjects_origin(
    doc: &lopdf::Document,
    resources: &lopdf::Dictionary,
    origin: ResourcesOrigin,
) -> XObjectsOrigin {
    match resources.get(b"XObject").and_then(|o| doc.dereference(o)) {
        // The dictionary is the object the chain ends on.
        Ok((Some(id), _)) => XObjectsOrigin::Object(id),
        // Direct dictionary, or a broken/absent entry whose empty result is a
        // property of this resources dictionary.
        Ok((None, _)) | Err(_) => XObjectsOrigin::DirectIn(origin),
    }
}

/// Image XObjects declared by a page's resources: name -> (format, width_px,
/// height_px).
type ImageXObjects = std::collections::BTreeMap<String, (String, u32, u32)>;

/// Collect the image XObjects a page's resources declare.
///
/// Dereferences every entry of the `/XObject` dictionary to check its
/// `/Subtype`, so the cost is proportional to that dictionary's size — which is
/// why [`extract_image_items`] memoises the result rather than repeating this
/// per page. Non-image XObjects (`/Form`, …) are skipped, so a page can pay the
/// full scan and collect nothing.
///
/// Charges `budget` as it goes, one per-object-touch price per entry, and stops
/// when it runs out. Charging *after* the scan would let a single oversized
/// dictionary do all of the work — dereferencing every entry and cloning every
/// image name — before any accounting applied, which is what the budget exists to
/// prevent. Neither the content nor the operation budget covers this work
/// otherwise: a page collecting no image skips content decoding entirely.
///
/// Returns the collected images and whether the budget survived the scan; `false`
/// means the caller should stop walking pages.
fn collect_image_xobjects(
    doc: &lopdf::Document,
    resources: &lopdf::Dictionary,
    budget: &mut usize,
) -> (ImageXObjects, bool) {
    let mut image_xobjects = ImageXObjects::new();
    if let Ok(xo) = resources.get(b"XObject")
        && let Ok((_, lopdf::Object::Dictionary(xobjects))) = doc.dereference(xo)
    {
        for (name, obj_ref) in xobjects.iter() {
            // Charged before the dereference and the name clone below, so an
            // over-budget dictionary stops partway instead of completing.
            *budget = budget.saturating_sub(PDF_CONTENT_STREAM_COST_FLOOR_BYTES);
            if *budget == 0 {
                return (image_xobjects, false);
            }
            if let Ok((_, lopdf::Object::Stream(xobj))) = doc.dereference(obj_ref) {
                let subtype = xobj
                    .dict
                    .get(b"Subtype")
                    .ok()
                    .and_then(|o| obj_as_name_str(o))
                    .unwrap_or_default();
                if subtype == "Image" {
                    let fmt = detect_image_format(&xobj.dict);
                    let w_px = xobj
                        .dict
                        .get(b"Width")
                        .ok()
                        .and_then(|o| o.as_i64().ok())
                        .unwrap_or(0) as u32;
                    let h_px = xobj
                        .dict
                        .get(b"Height")
                        .ok()
                        .and_then(|o| o.as_i64().ok())
                        .unwrap_or(0) as u32;
                    let name_str = String::from_utf8_lossy(name).into_owned();
                    image_xobjects.insert(name_str, (fmt, w_px, h_px));
                }
            }
        }
    }
    (image_xobjects, true)
}

fn transform_point(m: &[f32; 6], x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn extract_image_items(
    doc: &lopdf::Document,
    limits: &InspectLimits,
) -> crate::Result<Vec<ImageItem>> {
    let mut items = Vec::new();
    // Each pass decodes content independently, so each carries its own
    // whole-document budget. See `extract_text_items`.
    let mut content_budget = limits.content_total_bytes;
    let mut op_budget = limits.operations;
    let mut decoded_cache = DecodedStreams::new();
    // Memoised `collect_image_xobjects` results, keyed by the object id of the
    // resources dictionary they were scanned from.
    //
    // A resources dictionary is normally shared by every page in a document, and
    // scanning it costs one dereference per `/XObject` entry. Without this, a
    // page's cost is proportional to that dictionary's size even when the scan
    // finds no image at all — and a page that collects nothing goes on to skip
    // content decoding, so it charges neither the content nor the operation
    // budget. Work was therefore proportional to `pages × XObjects` while the
    // file stores each dimension once. Measured: 4,000 non-image XObjects shared
    // by 2,400 pages is 0.55 MB on disk, and the cost grew with the page count.
    let mut xobject_cache: std::collections::BTreeMap<XObjectsOrigin, Rc<ImageXObjects>> =
        std::collections::BTreeMap::new();

    // Lazy for the same reason as the text pass — see there.
    for (i, page_id) in doc.page_iter().enumerate() {
        let page_num = (i + 1) as u32;
        if items.len() >= limits.items || op_budget == 0 {
            break;
        }
        // Charged before resolving resources, which walks the `/Parent` chain and
        // can `continue` out below. This pass has two early exits ahead of
        // `gather_page_content`, so a floor charged only in there would leave
        // resource-free pages free. See `extract_text_items`.
        content_budget = content_budget.saturating_sub(PDF_CONTENT_STREAM_COST_FLOOR_BYTES);
        if content_budget == 0 {
            break;
        }
        // Step 1: XObject から画像情報を収集 (共有 resources なら memoise 済みを再利用)
        // Resources は親 /Pages ノードから継承される場合があるため、継承チェーンを辿る。
        let Some((res_origin, resources)) = resolve_page_resources(doc, page_id) else {
            continue;
        };
        // Scan once per distinct `/XObject` dictionary, reused by every page that
        // reaches it. Every origin is cacheable; see `XObjectsOrigin`.
        let origin = xobjects_origin(doc, resources, res_origin);
        let image_xobjects: Rc<ImageXObjects> = match xobject_cache.get(&origin) {
            Some(cached) => Rc::clone(cached),
            None => {
                // The scan charges as it walks, so an over-budget dictionary stops
                // partway rather than completing and then being billed. This is
                // what bounds the cache as well as the work: every insertion is
                // preceded by charges proportional to the entries it retains.
                let (scanned, within_budget) =
                    collect_image_xobjects(doc, resources, &mut content_budget);
                let scanned = Rc::new(scanned);
                xobject_cache.insert(origin, Rc::clone(&scanned));
                if !within_budget {
                    break;
                }
                scanned
            }
        };

        if image_xobjects.is_empty() {
            continue;
        }

        // Step 2: content stream から Do オペレータで位置を取得し、突き合わせて push
        let content_bytes =
            match gather_page_content(doc, page_id, &mut content_budget, &mut decoded_cache) {
                PageContent::Ready(b) => b,
                PageContent::Skip => continue,
                PageContent::Exhausted => break,
            };
        let content = match lopdf::content::Content::decode(&content_bytes) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // See `extract_text_items`: charged after decoding this page.
        op_budget = op_budget.saturating_sub(content.operations.len());

        let identity = IDENTITY;
        let mut ctm_stack: Vec<[f32; 6]> = vec![identity];
        // See `extract_text_items`: dropped `q` pushes are counted so the matching
        // `Q` operators unwind them rather than popping a real entry.
        let mut dropped_pushes: usize = 0;
        for op in &content.operations {
            if items.len() >= limits.items {
                break;
            }
            // See `extract_text_items`: the contents of a `q` frame dropped for
            // exceeding MAX_PDF_GS_STACK_DEPTH are discarded with the frame, so
            // a `cm` inside it cannot leak past the matching `Q`.
            if dropped_pushes > 0 {
                match op.operator.as_str() {
                    "q" => dropped_pushes += 1,
                    "Q" => dropped_pushes -= 1,
                    _ => {}
                }
                continue;
            }
            match op.operator.as_str() {
                "q" if ctm_stack.len() >= MAX_PDF_GS_STACK_DEPTH => {
                    dropped_pushes += 1;
                }
                "q" => {
                    let top = *ctm_stack.last().unwrap_or(&identity);
                    ctm_stack.push(top);
                }
                "Q" if ctm_stack.len() > 1 => {
                    ctm_stack.pop();
                }
                "cm" if op.operands.len() == 6 => {
                    let new_m = [
                        obj_to_f32(&op.operands[0]),
                        obj_to_f32(&op.operands[1]),
                        obj_to_f32(&op.operands[2]),
                        obj_to_f32(&op.operands[3]),
                        obj_to_f32(&op.operands[4]),
                        obj_to_f32(&op.operands[5]),
                    ];
                    let current = *ctm_stack.last().unwrap_or(&identity);
                    *ctm_stack.last_mut().unwrap() = concat_matrix(&current, &new_m);
                }
                "Do" => {
                    if let Some(name_obj) = op.operands.first()
                        && let Some(name) = obj_as_name_str(name_obj)
                        && let Some((fmt, w_px, h_px)) = image_xobjects.get(name)
                    {
                        let ctm = ctm_stack.last().unwrap_or(&identity);
                        // PDF images occupy the unit square [0,1]x[0,1].
                        // Transform all 4 corners through the CTM and take
                        // the axis-aligned bounding box so rotated/sheared
                        // images produce correct width/height.
                        let corners = [
                            transform_point(ctm, 0.0, 0.0),
                            transform_point(ctm, 1.0, 0.0),
                            transform_point(ctm, 0.0, 1.0),
                            transform_point(ctm, 1.0, 1.0),
                        ];
                        let min_x = corners
                            .iter()
                            .map(|(x, _)| *x)
                            .fold(f32::INFINITY, f32::min);
                        let max_x = corners
                            .iter()
                            .map(|(x, _)| *x)
                            .fold(f32::NEG_INFINITY, f32::max);
                        let min_y = corners
                            .iter()
                            .map(|(_, y)| *y)
                            .fold(f32::INFINITY, f32::min);
                        let max_y = corners
                            .iter()
                            .map(|(_, y)| *y)
                            .fold(f32::NEG_INFINITY, f32::max);
                        items.push(ImageItem {
                            page: page_num,
                            x: min_x,
                            y: min_y,
                            width: max_x - min_x,
                            height: max_y - min_y,
                            format: fmt.clone(),
                            width_px: *w_px,
                            height_px: *h_px,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    Ok(items)
}

fn obj_to_f32(obj: &lopdf::Object) -> f32 {
    match obj {
        lopdf::Object::Integer(i) => *i as f32,
        lopdf::Object::Real(f) => *f,
        _ => 0.0,
    }
}

/// Concatenate two PDF transformation matrices.
///
/// PDF transformation matrices use the row-vector convention:
/// ```text
/// a c e
/// b d f
/// 0 0 1
/// ```
/// This function computes `M_result = M_new × M_current`.
fn concat_matrix(current: &[f32; 6], new: &[f32; 6]) -> [f32; 6] {
    let (a, b, c, d, e, f) = (new[0], new[1], new[2], new[3], new[4], new[5]);
    let (a2, b2, c2, d2, e2, f2) = (
        current[0], current[1], current[2], current[3], current[4], current[5],
    );
    [
        a * a2 + b * c2,
        a * b2 + b * d2,
        c * a2 + d * c2,
        c * b2 + d * d2,
        e * a2 + f * c2 + e2,
        e * b2 + f * d2 + f2,
    ]
}

/// Decode a PDF string to a Rust String.
///
/// Handles UTF-16 BE (BOM `\xFE\xFF`) strings. For all other strings,
/// falls back to treating each byte as a Latin-1 code point.
///
/// Note: fulgur-generated PDFs use CID fonts where text in the content
/// stream consists of glyph IDs, not Unicode code points. The decoded
/// text for such PDFs will appear as raw byte sequences, not readable text.
/// Full Unicode reconstruction requires ToUnicode CMap parsing, which is
/// not yet implemented.
fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let chars: Vec<u16> = bytes[2..]
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&chars);
    }
    bytes.iter().map(|&b| b as char).collect()
}

/// The longest prefix of a raw metadata string worth decoding, bounded so an
/// oversized `/Info` value cannot force a large allocation before the clamp
/// applies.
///
/// Decoding widens: the Latin-1 fallback turns each high byte into two UTF-8
/// bytes, so a prefix of [`MAX_PDF_METADATA_FIELD_BYTES`] yields at most twice
/// that before [`clamp_str_bytes`] trims it. A UTF-16BE value is cut on an even
/// boundary after its BOM, so a code unit is never split.
fn metadata_prefix(bytes: &[u8]) -> &[u8] {
    if bytes.len() <= MAX_PDF_METADATA_FIELD_BYTES {
        return bytes;
    }
    let mut end = MAX_PDF_METADATA_FIELD_BYTES;
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF && !(end - 2).is_multiple_of(2) {
        end -= 1;
    }
    &bytes[..end]
}

fn estimate_width(text: &str, font_size: f32) -> f32 {
    text.chars().count() as f32 * font_size * 0.5
}

fn detect_image_format(dict: &lopdf::Dictionary) -> String {
    if let Ok(filter) = dict.get(b"Filter") {
        let name = match filter {
            lopdf::Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
            lopdf::Object::Array(arr) => arr
                .last()
                .and_then(|o| obj_as_name_str(o))
                .unwrap_or("")
                .to_string(),
            _ => String::new(),
        };
        match name.as_str() {
            "DCTDecode" => return "jpeg".to_string(),
            "JPXDecode" => return "jp2".to_string(),
            "CCITTFaxDecode" => return "tiff".to_string(),
            "FlateDecode" => return "flate".to_string(),
            _ => {}
        }
    }
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_test_pdf(html: &str) -> Vec<u8> {
        crate::engine::Engine::builder()
            .build()
            .render(html)
            .unwrap()
    }

    fn inspect_bytes(bytes: &[u8]) -> InspectResult {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), bytes).unwrap();
        inspect(tmp.path()).unwrap()
    }

    #[test]
    fn inspect_page_count() {
        let pdf = render_test_pdf("<html><body><p>Hello</p></body></html>");
        let result = inspect_bytes(&pdf);
        assert_eq!(result.pages, 1);
    }

    #[test]
    fn inspect_metadata_title() {
        let pdf = crate::engine::Engine::builder()
            .title("Test Title".to_string())
            .build()
            .render("<html><body><p>Hi</p></body></html>")
            .unwrap();
        let result = inspect_bytes(&pdf);
        assert_eq!(result.metadata.title.as_deref(), Some("Test Title"));
    }

    #[test]
    fn inspect_text_items_non_empty() {
        let pdf = render_test_pdf("<html><body><p>Hello World</p></body></html>");
        let result = inspect_bytes(&pdf);
        assert!(!result.text_items.is_empty(), "expected text items");
    }

    #[test]
    fn inspect_text_item_fields() {
        let pdf = render_test_pdf("<html><body><p>Hello</p></body></html>");
        let result = inspect_bytes(&pdf);
        let item = result
            .text_items
            .first()
            .expect("text items should not be empty");
        assert!(item.page >= 1);
        assert!(item.font_size > 0.0);
        assert!(!item.text.is_empty());
    }

    #[test]
    fn inspect_result_serializes_to_json() {
        let pdf = render_test_pdf("<html><body><p>Test</p></body></html>");
        let result = inspect_bytes(&pdf);
        let json = serde_json::to_string_pretty(&result).unwrap();
        assert!(json.contains("\"pages\""));
        assert!(json.contains("\"metadata\""));
        assert!(json.contains("\"text_items\""));
        assert!(json.contains("\"images\""));
    }

    #[test]
    fn inspect_error_on_nonexistent_file() {
        let result = inspect(std::path::Path::new("/nonexistent/path/to.pdf"));
        assert!(result.is_err(), "expected error for nonexistent file");
    }

    #[test]
    fn inspect_multi_page_pdf() {
        // Force two pages by making content taller than a single A4 page
        let html = "<html><body>\
            <p style='margin-bottom:2000pt'>Page one</p>\
            <p>Page two</p>\
            </body></html>";
        let pdf = render_test_pdf(html);
        let result = inspect_bytes(&pdf);
        assert!(result.pages >= 2, "expected at least 2 pages");
    }

    #[test]
    fn inspect_metadata_all_fields() {
        let pdf = crate::engine::Engine::builder()
            .title("My Title".to_string())
            .authors(vec!["Alice".to_string()])
            .creator("TestApp".to_string())
            .build()
            .render("<html><body><p>x</p></body></html>")
            .unwrap();
        let result = inspect_bytes(&pdf);
        assert_eq!(result.metadata.title.as_deref(), Some("My Title"));
        assert_eq!(result.metadata.author.as_deref(), Some("Alice"));
        assert_eq!(result.metadata.creator.as_deref(), Some("TestApp"));
    }

    #[test]
    fn inspect_image_embedded() {
        // Generate a valid 4x4 red PNG via the image crate (already a dev-dep)
        let img = image::RgbImage::from_fn(4, 4, |_, _| image::Rgb([255u8, 0, 0]));
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        let mut bundle = crate::asset::AssetBundle::new();
        bundle.add_image("test.png", png_bytes);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(r#"<html><body><img src="test.png" width="50" height="50"></body></html>"#)
            .unwrap();
        let result = inspect_bytes(&pdf);
        assert!(!result.images.is_empty(), "expected at least one image");
        let img = &result.images[0];
        assert_eq!(img.page, 1);
        assert!(img.width > 0.0, "image width should be positive");
        assert!(img.height > 0.0, "image height should be positive");
    }

    // --- pure function unit tests ---

    #[test]
    fn decode_pdf_string_latin1() {
        let bytes = b"Hello";
        assert_eq!(decode_pdf_string(bytes), "Hello");
    }

    #[test]
    fn decode_pdf_string_utf16be() {
        // UTF-16 BE BOM + "Hi" (U+0048, U+0069)
        let bytes = &[0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69];
        assert_eq!(decode_pdf_string(bytes), "Hi");
    }

    #[test]
    fn decode_pdf_string_utf16be_odd_trailing_byte_ignored() {
        // BOM + one complete pair + one orphan byte
        let bytes = &[0xFE, 0xFF, 0x00, 0x41, 0xFF];
        let s = decode_pdf_string(bytes);
        assert_eq!(s, "A"); // orphan byte filtered by chunks(2) + len==2
    }

    #[test]
    fn detect_image_format_jpeg() {
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Filter", lopdf::Object::Name(b"DCTDecode".to_vec()));
        assert_eq!(detect_image_format(&dict), "jpeg");
    }

    #[test]
    fn detect_image_format_flate() {
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Filter", lopdf::Object::Name(b"FlateDecode".to_vec()));
        assert_eq!(detect_image_format(&dict), "flate");
    }

    #[test]
    fn detect_image_format_jp2() {
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Filter", lopdf::Object::Name(b"JPXDecode".to_vec()));
        assert_eq!(detect_image_format(&dict), "jp2");
    }

    #[test]
    fn detect_image_format_tiff() {
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Filter", lopdf::Object::Name(b"CCITTFaxDecode".to_vec()));
        assert_eq!(detect_image_format(&dict), "tiff");
    }

    #[test]
    fn detect_image_format_unknown() {
        let dict = lopdf::Dictionary::new(); // no Filter key
        assert_eq!(detect_image_format(&dict), "unknown");
    }

    #[test]
    fn detect_image_format_array_filter() {
        // Array filter — last entry wins
        let mut dict = lopdf::Dictionary::new();
        dict.set(
            b"Filter",
            lopdf::Object::Array(vec![
                lopdf::Object::Name(b"ASCII85Decode".to_vec()),
                lopdf::Object::Name(b"DCTDecode".to_vec()),
            ]),
        );
        assert_eq!(detect_image_format(&dict), "jpeg");
    }

    #[test]
    fn concat_matrix_identity() {
        let id = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
        let m = [1.0f32, 0.0, 0.0, 1.0, 10.0, 20.0];
        let result = concat_matrix(&id, &m);
        // id × m = m
        assert!((result[4] - 10.0).abs() < 1e-4);
        assert!((result[5] - 20.0).abs() < 1e-4);
    }

    #[test]
    fn concat_matrix_translation() {
        let a = [1.0f32, 0.0, 0.0, 1.0, 5.0, 10.0];
        let b = [1.0f32, 0.0, 0.0, 1.0, 3.0, 4.0];
        let result = concat_matrix(&a, &b);
        // Translations add: e = 3+5=8, f = 4+10=14
        assert!((result[4] - 8.0).abs() < 1e-4);
        assert!((result[5] - 14.0).abs() < 1e-4);
    }

    // --- obj_to_f32 edge case ---

    #[test]
    fn obj_to_f32_returns_zero_for_non_numeric_object() {
        // The `_ => 0.0` arm is hit when the Object is neither Integer nor Real
        // (e.g., a Name or Null). This covers the fallback branch.
        assert_eq!(obj_to_f32(&lopdf::Object::Null), 0.0);
        assert_eq!(obj_to_f32(&lopdf::Object::Boolean(true)), 0.0);
        assert_eq!(obj_to_f32(&lopdf::Object::Name(b"F1".to_vec())), 0.0);
        // The covered variants
        assert_eq!(obj_to_f32(&lopdf::Object::Integer(5)), 5.0);
        assert!((obj_to_f32(&lopdf::Object::Real(2.5)) - 2.5).abs() < 1e-4);
    }

    // --- detect_image_format edge cases ---

    #[test]
    fn detect_image_format_unrecognized_name_filter() {
        // Filter is a Name but not one of the four recognized values.
        // Hits the `_ => {}` arm in the inner match, then falls through to "unknown".
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Filter", lopdf::Object::Name(b"ASCII85Decode".to_vec()));
        assert_eq!(detect_image_format(&dict), "unknown");
    }

    #[test]
    fn detect_image_format_non_name_non_array_filter_object() {
        // Filter is neither a Name nor an Array (e.g., an Integer).
        // Hits the `_ => String::new()` arm in the outer match.
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Filter", lopdf::Object::Integer(1));
        assert_eq!(detect_image_format(&dict), "unknown");
    }

    // --- lopdf-constructed PDF: TL / Td / TD / T* text-positioning operators ---

    /// Build a minimal lopdf-native PDF whose content stream uses the
    /// TL / Td / TD / T* text-positioning operators.  fulgur-generated PDFs
    /// use Tm/Tj for all text positioning, so these operator paths in
    /// `extract_text_items` are only reachable via synthetically crafted PDFs.
    fn make_pdf_with_text_positioning_ops() -> Vec<u8> {
        use lopdf::content::{Content, Operation};
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type1",
                    "BaseFont" => "Courier",
                },
            },
        });

        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new(
                    "Tf",
                    vec![Object::Name(b"F1".to_vec()), Object::Integer(12)],
                ),
                // TL sets text leading to 14 pt
                Operation::new("TL", vec![Object::Real(14.0)]),
                // Td moves text position
                Operation::new("Td", vec![Object::Real(100.0), Object::Real(700.0)]),
                Operation::new("Tj", vec![Object::string_literal("Line1")]),
                // TD moves and also resets leading to abs(-14) = 14
                Operation::new("TD", vec![Object::Real(0.0), Object::Real(-14.0)]),
                Operation::new("Tj", vec![Object::string_literal("Line2")]),
                // T* advances by the current text leading (set via TL / TD)
                Operation::new("T*", vec![]),
                Operation::new("Tj", vec![Object::string_literal("Line3")]),
                Operation::new("ET", vec![]),
            ],
        };

        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(595),
                Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    #[test]
    fn inspect_text_operators_tl_td_td_tstar_produce_text_items() {
        let pdf = make_pdf_with_text_positioning_ops();
        let result = inspect_bytes(&pdf);
        // Three Tj calls → three text items (Line1 / Line2 / Line3)
        assert_eq!(
            result.text_items.len(),
            3,
            "expected 3 text items from Td/TD/T* positioned text"
        );
        let texts: Vec<&str> = result.text_items.iter().map(|i| i.text.as_str()).collect();
        assert!(texts.contains(&"Line1"), "missing Line1");
        assert!(texts.contains(&"Line2"), "missing Line2");
        assert!(texts.contains(&"Line3"), "missing Line3");
    }

    #[test]
    fn inspect_text_td_advances_position() {
        // After `Td 100 700`, tx should be ~100 + advance; verify the first
        // item's x is near 100 (within the default identity CTM).
        let pdf = make_pdf_with_text_positioning_ops();
        let result = inspect_bytes(&pdf);
        let first = result.text_items.first().expect("expected text items");
        assert!(
            (first.x - 100.0).abs() < 1e-4,
            "expected text x to be exactly 100.0, got {}",
            first.x
        );
    }

    #[test]
    fn inspect_text_td_updates_leading_via_td_operator() {
        // TD 0 -14 sets text_leading = 14 (abs(-(-14))), then T* moves by that
        // amount. The third item (after T*) should have a y offset different
        // from the second item (after TD).
        let pdf = make_pdf_with_text_positioning_ops();
        let result = inspect_bytes(&pdf);
        assert!(result.text_items.len() >= 3, "need at least 3 text items");
        let y1 = result.text_items[1].y;
        let y2 = result.text_items[2].y;
        // y2 (after T*) should differ from y1 (after TD) by ~14 pt in either
        // direction (depending on the coordinate convention in use).
        let diff = (y2 - y1).abs();
        assert!(
            (diff - 14.0).abs() < 1e-4,
            "expected T* to advance y by exactly 14.0, got {diff}"
        );
    }

    // --- metadata absent ---

    #[test]
    fn inspect_metadata_returns_all_none_when_no_info_dict() {
        // A PDF without an Info entry in the trailer exercises the
        // `Err(_) => return meta` branch (line 80 in extract_metadata).
        use lopdf::content::{Content, Operation};
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new(
                    "Tf",
                    vec![Object::Name(b"F1".to_vec()), Object::Integer(12)],
                ),
                Operation::new(
                    "Tm",
                    vec![
                        Object::Real(1.0),
                        Object::Real(0.0),
                        Object::Real(0.0),
                        Object::Real(1.0),
                        Object::Real(72.0),
                        Object::Real(720.0),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal("hi")]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type1",
                    "BaseFont" => "Courier",
                },
            },
        });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(595),
                Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        // No trailer "Info" key → exercises the Err branch in extract_metadata
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        let result = inspect_bytes(&buf);
        assert_eq!(result.metadata.title, None);
        assert_eq!(result.metadata.author, None);
        assert_eq!(result.metadata.creator, None);
        assert_eq!(result.metadata.created_at, None);
        assert_eq!(result.metadata.modified_at, None);
    }

    /// Cyclic /Parent chain (A -> B -> A, no /Resources anywhere) must terminate.
    /// Without a visited-set cap, `resolve_page_resources` alternates between the two
    /// parent dictionaries forever.
    #[test]
    fn resolve_page_resources_stops_on_multi_node_parent_cycle() {
        use lopdf::{Document, Object, dictionary};

        let mut doc = Document::new();
        // Page 1 -> Pages 2 -> Pages 3 -> Pages 2 (cycle, none have /Resources).
        doc.objects.insert(
            (1, 0),
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference((2, 0)),
            }),
        );
        doc.objects.insert(
            (2, 0),
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Parent" => Object::Reference((3, 0)),
            }),
        );
        doc.objects.insert(
            (3, 0),
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Parent" => Object::Reference((2, 0)),
            }),
        );

        assert!(resolve_page_resources(&doc, (1, 0)).is_none());
    }

    /// The `/Parent` walk and the alias canonicalisation at each node draw on one
    /// shared hop budget, so spending on one axis reduces the other.
    ///
    /// Bounding each axis separately multiplies them: `/Parent` references may
    /// themselves be alias chains, so `MAX_PDF_PARENT_DEPTH` ancestors each
    /// reached through `MAX_PDF_PARENT_DEPTH` aliases costs that bound *squared*
    /// in object lookups for a single page, replayed per occurrence when the page
    /// is repeated across `/Kids` — all for one per-page floor charge.
    #[test]
    fn parent_walk_and_alias_hops_share_one_budget() {
        use lopdf::{Document, Object, dictionary};

        /// Page -> `depth` ancestors, each `/Parent` reference reached through
        /// `alias` reference objects, with `/Resources` on the last ancestor.
        /// Reaching it costs `depth` parent hops plus `depth * alias` alias hops.
        fn resolves(depth: usize, alias: usize) -> bool {
            let mut doc = Document::new();
            let resources = doc.add_object(dictionary! { "XObject" => Object::Null });
            let page_id = doc.new_object_id();
            let mut prev = page_id;
            for step in 0..depth {
                // The ancestor `prev` will point at, behind `alias` aliases.
                let node = doc.new_object_id();
                let mut target = node;
                for _ in 0..alias {
                    target = doc.add_object(Object::Reference(target));
                }
                let dict = if step + 1 == depth {
                    dictionary! { "Type" => "Pages", "Resources" => resources }
                } else {
                    dictionary! { "Type" => "Pages" }
                };
                doc.objects.insert(node, Object::Dictionary(dict));
                let parented = match doc.objects.remove(&prev) {
                    Some(Object::Dictionary(mut d)) => {
                        d.set("Parent", Object::Reference(target));
                        d
                    }
                    _ => dictionary! {
                        "Type" => if prev == page_id { "Page" } else { "Pages" },
                        "Parent" => Object::Reference(target),
                    },
                };
                doc.objects.insert(prev, Object::Dictionary(parented));
                prev = node;
            }
            resolve_page_resources(&doc, page_id).is_some()
        }

        // Neither axis alone exceeds the budget.
        assert!(resolves(4, 0), "4 ancestors, no aliases");
        assert!(
            resolves(MAX_PDF_PARENT_DEPTH, 0),
            "ancestors up to the bound"
        );
        assert!(
            !resolves(MAX_PDF_PARENT_DEPTH + 1, 0),
            "one ancestor past the bound must be refused",
        );

        // Combined, they are charged against the same budget. 4 * (31 + 1) is
        // exactly MAX_PDF_PARENT_DEPTH hops; one alias more is over.
        assert!(resolves(4, 31), "4 * 32 == MAX_PDF_PARENT_DEPTH hops");
        assert!(
            !resolves(4, 32),
            "4 * 33 hops must be refused — per-axis bounds would allow this, \
             since 4 ancestors and 32 aliases are each far inside the bound",
        );
    }

    // --- Resource bounds on attacker-controlled content streams ---

    /// Build a PDF with one page per entry in `page_contents`, each page's
    /// content stream being exactly those bytes.
    ///
    /// Unlike [`make_pdf_with_text_positioning_ops`] this takes pre-encoded
    /// operator bytes, so a test can hand it a stream that is deliberately
    /// larger — or more deeply nested — than any operation list it would be
    /// convenient to build through `lopdf::content::Content`.
    fn make_pdf_with_raw_contents(page_contents: Vec<Vec<u8>>) -> Vec<u8> {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type1",
                    "BaseFont" => "Courier",
                },
            },
        });
        let page_count = page_contents.len();
        let mut kids = Vec::with_capacity(page_count);
        for raw_content in page_contents {
            let content_id = doc.add_object(Stream::new(dictionary! {}, raw_content));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "Resources" => resources_id,
                "MediaBox" => vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(595),
                    Object::Integer(842),
                ],
            });
            kids.push(Object::Reference(page_id));
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => Object::Integer(page_count as i64),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    fn make_pdf_with_raw_content(raw_content: Vec<u8>) -> Vec<u8> {
        make_pdf_with_raw_contents(vec![raw_content])
    }

    /// Build a *single-page* PDF whose `/Contents` is an array of several
    /// streams, each carrying pre-encoded operator bytes.
    ///
    /// Unlike [`make_pdf_with_raw_contents`], which puts one stream on each of
    /// several pages, this puts several streams on one page — the shape that
    /// distinguishes a per-stream content budget from a per-page one.
    fn make_pdf_with_multiple_content_streams(streams: Vec<Vec<u8>>) -> Vec<u8> {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type1",
                    "BaseFont" => "Courier",
                },
            },
        });
        let content_refs: Vec<Object> = streams
            .into_iter()
            .map(|raw| Object::Reference(doc.add_object(Stream::new(dictionary! {}, raw))))
            .collect();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_refs,
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(595),
                Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    /// Build a single-page PDF carrying one image XObject named `/Im0`, whose
    /// content stream is exactly `raw_content`.
    ///
    /// `extract_image_items` only walks a page's content stream when that page
    /// resolves at least one image XObject, so the image pass is unreachable
    /// without a resource dictionary like this one.
    fn make_pdf_with_image_and_raw_content(raw_content: Vec<u8>) -> Vec<u8> {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => Object::Integer(4),
                "Height" => Object::Integer(4),
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => Object::Integer(8),
            },
            vec![0u8; 16],
        ));
        let resources_id = doc.add_object(dictionary! {
            "XObject" => dictionary! {
                "Im0" => Object::Reference(image_id),
            },
        });
        let content_id = doc.add_object(Stream::new(dictionary! {}, raw_content));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(595),
                Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    /// A decoded content stream above [`MAX_PDF_CONTENT_BYTES`] is skipped
    /// instead of being parsed into an operation list.
    ///
    /// The stream below carries real `Tj` operators, so without the bound the
    /// same input yields text items; the assertion is that the cap fires, not
    /// that the process survives.
    #[test]
    fn content_stream_above_byte_cap_is_skipped() {
        let mut content = b"BT /F1 12 Tf\n".to_vec();
        while content.len() <= MAX_PDF_CONTENT_BYTES {
            content.extend_from_slice(b"(A) Tj\n");
        }
        content.extend_from_slice(b"ET\n");
        assert!(content.len() > MAX_PDF_CONTENT_BYTES);

        let result = inspect_bytes(&make_pdf_with_raw_content(content));
        assert!(
            result.text_items.is_empty(),
            "oversized content stream must be skipped, got {} items",
            result.text_items.len()
        );
        // The page itself is still counted; only its content is not parsed.
        assert_eq!(result.pages, 1);
    }

    /// A stream just under [`MAX_PDF_CONTENT_BYTES`] is still parsed, so the
    /// cap does not reject content merely for being large.
    #[test]
    fn content_stream_below_byte_cap_is_parsed() {
        let mut content = b"BT /F1 12 Tf\n".to_vec();
        while content.len() + 7 <= MAX_PDF_CONTENT_BYTES - 16 {
            content.extend_from_slice(b"(A) Tj\n");
        }
        content.extend_from_slice(b"ET\n");
        assert!(content.len() <= MAX_PDF_CONTENT_BYTES);

        let result = inspect_bytes(&make_pdf_with_raw_content(content));
        assert!(
            !result.text_items.is_empty(),
            "content stream under the cap must still be parsed"
        );
    }

    /// A run of `q` operators deeper than [`MAX_PDF_GS_STACK_DEPTH`] must not
    /// grow the graphics-state stack past the bound, and the dropped pushes
    /// must stay balanced against their matching `Q` operators: text drawn
    /// after the nesting closes has to land where the outer CTM puts it.
    #[test]
    fn graphics_state_nesting_beyond_cap_stays_balanced() {
        let depth = MAX_PDF_GS_STACK_DEPTH + 500;

        // Outer `cm` shifts the page by (100, 700). Then `depth` nested `q`s,
        // each of which would be dropped past the cap, an inner `cm` that must
        // be discarded with them, and `depth` matching `Q`s. The trailing text
        // must therefore be positioned by the *outer* CTM alone.
        let mut content = b"1 0 0 1 100 700 cm\n".to_vec();
        for _ in 0..depth {
            content.extend_from_slice(b"q\n");
        }
        content.extend_from_slice(b"1 0 0 1 50 50 cm\n");
        for _ in 0..depth {
            content.extend_from_slice(b"Q\n");
        }
        content.extend_from_slice(b"BT /F1 12 Tf (Anchor) Tj ET\n");

        let result = inspect_bytes(&make_pdf_with_raw_content(content));
        let item = result
            .text_items
            .first()
            .expect("text after balanced nesting must still be extracted");
        assert_eq!(item.text, "Anchor");
        assert!(
            (item.x - 100.0).abs() < 0.01 && (item.y - 700.0).abs() < 0.01,
            "expected outer CTM origin (100, 700), got ({}, {})",
            item.x,
            item.y
        );
    }

    /// A long `/Tf` resource name must not be duplicated once per `q`: the
    /// saved graphics states share the name instead of cloning it. Deep
    /// nesting with a wide name is the highest-amplification shape for this
    /// module, so the bound is asserted through the restored state.
    ///
    /// The name is clamped to [`MAX_PDF_FONT_NAME_BYTES`] on the way in, so what
    /// must survive the round trip is the clamped form, not the input.
    #[test]
    fn wide_font_name_survives_deep_nesting_and_restore() {
        let name = "F".repeat(4096);
        let mut content = format!("BT /{name} 12 Tf\n").into_bytes();
        for _ in 0..(MAX_PDF_GS_STACK_DEPTH + 500) {
            content.extend_from_slice(b"q\n");
        }
        for _ in 0..(MAX_PDF_GS_STACK_DEPTH + 500) {
            content.extend_from_slice(b"Q\n");
        }
        content.extend_from_slice(b"(Deep) Tj ET\n");

        let result = inspect_bytes(&make_pdf_with_raw_content(content));
        let item = result
            .text_items
            .first()
            .expect("text after balanced nesting must still be extracted");
        assert_eq!(
            item.font,
            "F".repeat(MAX_PDF_FONT_NAME_BYTES),
            "clamped font name must survive save/restore intact"
        );
    }

    /// A `/Tf` name longer than [`MAX_PDF_FONT_NAME_BYTES`] is clamped in every
    /// emitted record, so the retained size of a result is bounded by
    /// `items × MAX_PDF_FONT_NAME_BYTES` rather than by the name's length.
    ///
    /// A per-page content budget alone does not bound that product: this shape
    /// spends one page on a wide name plus a run of cheap `Tj` operators, and a
    /// document may repeat it per page.
    #[test]
    fn wide_font_name_is_clamped_in_every_record() {
        let name = "F".repeat(64 * 1024);
        let mut content = format!("BT /{name} 12 Tf\n").into_bytes();
        for _ in 0..1000 {
            content.extend_from_slice(b"(A) Tj\n");
        }
        content.extend_from_slice(b"ET\n");

        let result = inspect_bytes(&make_pdf_with_raw_content(content));
        assert_eq!(result.text_items.len(), 1000);
        for item in &result.text_items {
            assert_eq!(item.font.len(), MAX_PDF_FONT_NAME_BYTES);
        }
    }

    /// Clamping a `/Tf` name must round down to a UTF-8 character boundary: a
    /// name whose clamp point falls inside a multi-byte character is cut before
    /// that character instead of panicking on a sliced code point.
    #[test]
    fn wide_font_name_clamp_respects_char_boundaries() {
        // 3 bytes per `あ`, so no multiple of 3 lands on 127 — the clamp point
        // falls strictly inside a character and has to round down to 126.
        let name = "あ".repeat(128);
        assert!(name.len() > MAX_PDF_FONT_NAME_BYTES);
        let content = format!("BT /{name} 12 Tf (Multi) Tj ET\n").into_bytes();

        let result = inspect_bytes(&make_pdf_with_raw_content(content));
        let item = result
            .text_items
            .first()
            .expect("text with a multi-byte font name must still be extracted");
        assert_eq!(item.font, "あ".repeat(MAX_PDF_FONT_NAME_BYTES / 3));
        assert!(item.font.len() <= MAX_PDF_FONT_NAME_BYTES);
    }

    /// Extraction stops at [`MAX_PDF_INSPECT_ITEMS`] rather than returning a
    /// result whose size is proportional to the operator count.
    ///
    /// The cap is a whole-document total, so the input spreads its items over
    /// several pages: each page stays comfortably under
    /// [`MAX_PDF_CONTENT_BYTES`], which isolates the item bound from the byte
    /// bound and also proves a per-page cap would not have sufficed.
    #[test]
    fn text_item_count_is_capped_across_pages() {
        const ITEMS_PER_PAGE: usize = 50_000;
        let mut page = b"BT /F1 12 Tf\n".to_vec();
        page.reserve(ITEMS_PER_PAGE * 7 + 32);
        for _ in 0..ITEMS_PER_PAGE {
            page.extend_from_slice(b"(A) Tj\n");
        }
        page.extend_from_slice(b"ET\n");
        assert!(
            page.len() <= MAX_PDF_CONTENT_BYTES,
            "each page must stay under the byte cap to isolate the item cap"
        );

        // One page more than the cap needs, so extraction has to stop mid-document.
        let pages = MAX_PDF_INSPECT_ITEMS / ITEMS_PER_PAGE + 1;
        let pdf = make_pdf_with_raw_contents(vec![page; pages]);

        let result = inspect_bytes(&pdf);
        assert_eq!(result.pages as usize, pages);
        assert_eq!(result.text_items.len(), MAX_PDF_INSPECT_ITEMS);
    }

    /// The image pass re-reads the same content stream, so it carries the same
    /// byte bound: an oversized stream yields no image placements even though
    /// the page does resolve an image XObject.
    #[test]
    fn image_pass_skips_content_stream_above_byte_cap() {
        let mut content = Vec::new();
        while content.len() <= MAX_PDF_CONTENT_BYTES {
            content.extend_from_slice(b"q 1 0 0 1 1 1 cm /Im0 Do Q\n");
        }
        assert!(content.len() > MAX_PDF_CONTENT_BYTES);

        let result = inspect_bytes(&make_pdf_with_image_and_raw_content(content));
        assert!(
            result.images.is_empty(),
            "oversized content stream must be skipped, got {} images",
            result.images.len()
        );
        assert_eq!(result.pages, 1);
    }

    /// The image pass keeps its own CTM stack, so it needs the same balanced
    /// unwinding of dropped `q` pushes: an image placed after nesting deeper
    /// than [`MAX_PDF_GS_STACK_DEPTH`] closes must land at the outer CTM.
    #[test]
    fn image_placement_survives_graphics_state_nesting_beyond_cap() {
        let depth = MAX_PDF_GS_STACK_DEPTH + 500;

        let mut content = b"1 0 0 1 100 700 cm\n".to_vec();
        for _ in 0..depth {
            content.extend_from_slice(b"q\n");
        }
        // Discarded along with the dropped pushes.
        content.extend_from_slice(b"1 0 0 1 33 44 cm\n");
        for _ in 0..depth {
            content.extend_from_slice(b"Q\n");
        }
        // Scale the unit image square to 50x50 at the outer origin.
        content.extend_from_slice(b"50 0 0 50 0 0 cm\n/Im0 Do\n");

        let result = inspect_bytes(&make_pdf_with_image_and_raw_content(content));
        let img = result
            .images
            .first()
            .expect("image after balanced nesting must still be extracted");
        assert!(
            (img.x - 100.0).abs() < 0.01 && (img.y - 700.0).abs() < 0.01,
            "expected outer CTM origin (100, 700), got ({}, {})",
            img.x,
            img.y
        );
        assert!((img.width - 50.0).abs() < 0.01, "width {}", img.width);
        assert!((img.height - 50.0).abs() < 0.01, "height {}", img.height);
    }

    /// The bounds must not change what a realistic document extracts, and must
    /// sit far above what one needs.
    ///
    /// (The extracted `text` is glyph-id soup rather than readable text because
    /// `inspect` does not consult the ToUnicode CMap — a separate, pre-existing
    /// limitation. This test asserts on record structure, which is what the
    /// bounds can affect.)
    #[test]
    fn bounds_do_not_alter_realistic_extraction() {
        let html = "<html><body>\
            <h1>Heading</h1>\
            <p>First paragraph with several words.</p>\
            <p>Second paragraph, also with words.</p>\
            </body></html>";
        let result = inspect_bytes(&render_test_pdf(html));
        // At least one record per source block: heading plus the two
        // paragraphs. The exact count depends on line breaking, which depends
        // on font metrics, so only the floor is asserted.
        assert!(
            result.text_items.len() >= 3,
            "expected >=3 text items, got {}",
            result.text_items.len()
        );
        for item in &result.text_items {
            assert_eq!(item.page, 1);
            assert!(!item.text.is_empty());
            assert!(item.font_size > 0.0);
            assert!(item.width > 0.0);
        }
        // Headroom check: the cap is orders of magnitude above this document,
        // so the result must be nowhere near truncation.
        assert!(result.text_items.len() < MAX_PDF_INSPECT_ITEMS / 1000);
    }

    /// A `q` past [`MAX_PDF_GS_STACK_DEPTH`] is dropped, so the frame it opens
    /// has no stack entry of its own. State set inside that frame must not
    /// survive its matching `Q`: without isolation the `cm` below would mutate
    /// the deepest *retained* entry, and because that entry is never popped it
    /// would keep transforming everything that follows.
    ///
    /// The existing `graphics_state_nesting_beyond_cap_stays_balanced` closes
    /// *all* the nesting, which pops the polluted entry and hides the leak; the
    /// distinguishing shape is closing only the dropped frame.
    #[test]
    fn dropped_frame_state_does_not_leak_past_matching_q() {
        // Fill the stack exactly, then one more `q` — that last one is dropped.
        let mut content = b"1 0 0 1 100 700 cm\n".to_vec();
        for _ in 0..MAX_PDF_GS_STACK_DEPTH {
            content.extend_from_slice(b"q\n");
        }
        // Belongs to the dropped frame, and must be discarded with it.
        content.extend_from_slice(b"1 0 0 1 50 50 cm\n");
        // Closes only the dropped frame; the real entries stay on the stack.
        content.extend_from_slice(b"Q\n");
        content.extend_from_slice(b"BT /F1 12 Tf (Anchor) Tj ET\n");

        let result = inspect_bytes(&make_pdf_with_raw_content(content));
        let item = result
            .text_items
            .first()
            .expect("text after the dropped frame closes must still be extracted");
        assert_eq!(item.text, "Anchor");
        assert!(
            (item.x - 100.0).abs() < 0.01 && (item.y - 700.0).abs() < 0.01,
            "dropped-frame `cm` leaked: expected outer CTM (100, 700), got ({}, {})",
            item.x,
            item.y
        );
    }

    /// The image pass keeps its own CTM stack, so it needs the same isolation of
    /// dropped-frame state as the text pass.
    #[test]
    fn image_dropped_frame_state_does_not_leak_past_matching_q() {
        let mut content = b"1 0 0 1 100 700 cm\n".to_vec();
        for _ in 0..MAX_PDF_GS_STACK_DEPTH {
            content.extend_from_slice(b"q\n");
        }
        // Belongs to the dropped frame: a 10x scale that must not reach the `Do`.
        content.extend_from_slice(b"10 0 0 10 33 44 cm\n");
        content.extend_from_slice(b"Q\n");
        // Scale the unit image square to 50x50 at the outer origin.
        content.extend_from_slice(b"50 0 0 50 0 0 cm\n/Im0 Do\n");

        let result = inspect_bytes(&make_pdf_with_image_and_raw_content(content));
        let img = result
            .images
            .first()
            .expect("image after the dropped frame closes must still be extracted");
        assert!(
            (img.x - 100.0).abs() < 0.01 && (img.y - 700.0).abs() < 0.01,
            "dropped-frame `cm` leaked: expected outer CTM (100, 700), got ({}, {})",
            img.x,
            img.y
        );
        assert!((img.width - 50.0).abs() < 0.01, "width {}", img.width);
        assert!((img.height - 50.0).abs() < 0.01, "height {}", img.height);
    }

    /// [`MAX_PDF_CONTENT_BYTES`] bounds a page's *whole* content, not each of
    /// its streams: a page may carry any number of `/Contents` streams, so a
    /// per-stream check would leave the concatenated total unbounded.
    #[test]
    fn page_content_total_is_capped_across_streams() {
        // Ten streams, each an eighth of the budget: every one is individually
        // well under the cap, but together they are over it.
        let mut stream = b"BT /F1 12 Tf\n".to_vec();
        while stream.len() < MAX_PDF_CONTENT_BYTES / 8 {
            stream.extend_from_slice(b"(A) Tj\n");
        }
        stream.extend_from_slice(b"ET\n");
        assert!(stream.len() < MAX_PDF_CONTENT_BYTES);
        let streams = vec![stream; 10];
        assert!(streams.iter().map(Vec::len).sum::<usize>() > MAX_PDF_CONTENT_BYTES);

        let result = inspect_bytes(&make_pdf_with_multiple_content_streams(streams));
        assert!(
            result.text_items.is_empty(),
            "page over the total budget must be skipped, got {} items",
            result.text_items.len()
        );
        assert_eq!(result.pages, 1);
    }

    /// Several content streams summing to under [`MAX_PDF_CONTENT_BYTES`] are
    /// still parsed, and are joined with the separator that keeps tokens from
    /// merging across the boundary: `…Tj` followed by `q…` must not lex as
    /// `Tjq`. Each stream's text is extracted, in order.
    #[test]
    fn page_content_under_cap_is_joined_across_streams() {
        let streams = vec![
            b"BT /F1 12 Tf 1 0 0 1 10 800 Tm (First) Tj".to_vec(),
            b"1 0 0 1 10 780 Tm (Second) Tj".to_vec(),
            b"1 0 0 1 10 760 Tm (Third) Tj ET".to_vec(),
        ];

        let result = inspect_bytes(&make_pdf_with_multiple_content_streams(streams));
        let texts: Vec<&str> = result.text_items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            texts,
            ["First", "Second", "Third"],
            "streams must be joined with a separator and parsed in order"
        );
    }

    /// Build a PDF with `pages` page objects that all share a **single**
    /// `/Contents` stream object holding `payload`.
    ///
    /// This is the shape that distinguishes a per-page bound from a
    /// whole-document one: page count is bounded only by input size, and
    /// sharing the stream means each additional page costs ~60 bytes on disk
    /// while re-paying the full per-page decode and extraction cost.
    fn make_pdf_with_shared_content_stream(
        pages: usize,
        payload: Vec<u8>,
        with_image: bool,
    ) -> Vec<u8> {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut resources = dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                },
            },
        };
        if with_image {
            // `extract_image_items` skips a page that resolves no image XObject,
            // so the image pass is unreachable without this. Kept opt-in: adding
            // it unconditionally would make the image pass re-walk the content of
            // every text-only fixture too.
            let image_id = doc.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image",
                    "Width" => Object::Integer(4), "Height" => Object::Integer(4),
                    "ColorSpace" => "DeviceGray", "BitsPerComponent" => Object::Integer(8),
                },
                vec![0u8; 16],
            ));
            resources.set(
                "XObject",
                dictionary! { "Im0" => Object::Reference(image_id) },
            );
        }
        let resources_id = doc.add_object(resources);
        let shared_content = doc.add_object(Stream::new(dictionary! {}, payload));
        let mut kids = Vec::with_capacity(pages);
        for _ in 0..pages {
            kids.push(Object::Reference(doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => Object::Reference(shared_content),
                "Resources" => resources_id,
                "MediaBox" => vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ],
            })));
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => Object::Integer(pages as i64),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    /// [`MAX_PDF_INSPECT_TEXT_BYTES`] bounds retained payload, which a record
    /// *count* cannot: one `Tj` operand can be nearly a whole page's content
    /// allowance, so many pages sharing one such stream retain far more text
    /// than [`MAX_PDF_INSPECT_ITEMS`] records of realistic size ever would.
    #[test]
    fn retained_text_bytes_are_capped_across_pages() {
        const STRING_LEN: usize = 512 * 1024;
        let payload = {
            let mut p = b"BT /F1 12 Tf (".to_vec();
            p.extend(std::iter::repeat_n(b'A', STRING_LEN));
            p.extend_from_slice(b") Tj ET\n");
            p
        };
        assert!(
            payload.len() <= MAX_PDF_CONTENT_BYTES,
            "page must be in bound"
        );

        // Twice as many pages as the text budget can hold, so it has to stop
        // mid-document rather than at the end of the input.
        let pages = (MAX_PDF_INSPECT_TEXT_BYTES / STRING_LEN) * 2;
        let result = inspect_bytes(&make_pdf_with_shared_content_stream(pages, payload, false));

        let total: usize = result.text_items.iter().map(|i| i.text.len()).sum();
        // Bounded, with the overshoot limited to the last record pushed.
        assert!(
            total <= MAX_PDF_INSPECT_TEXT_BYTES + STRING_LEN,
            "retained text {total} exceeds the budget plus one record"
        );
        // And it genuinely truncated rather than the input being too small.
        assert!(
            result.text_items.len() < pages,
            "expected truncation: got {} records from {} pages",
            result.text_items.len(),
            pages
        );
        assert!(
            !result.text_items.is_empty(),
            "must still extract some text"
        );
    }

    /// A page whose content is mostly filler: one cheap record, then enough
    /// bytes to spend nearly a full [`MAX_PDF_CONTENT_BYTES`] of the
    /// whole-document budget.
    ///
    /// The filler is whitespace rather than the bare `q` operators an attacker
    /// would use, because it charges the byte budget identically while costing
    /// ~240× less to decode (measured: 4 MiB of `q` takes 523 ms, the same
    /// whitespace 2.2 ms). The bound under test counts bytes, not operators, so
    /// the cheap filler exercises it just as well and keeps the test fast.
    fn budget_filler_page(op: &[u8]) -> Vec<u8> {
        let mut payload = op.to_vec();
        payload.resize(MAX_PDF_CONTENT_BYTES - 64, b' ');
        payload
    }

    /// [`MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES`] bounds aggregate decoding work,
    /// which the per-page bound cannot: every page re-pays it, and page count is
    /// bounded only by input size.
    ///
    /// Runs at the production budget, which is affordable here because the filler
    /// is whitespace. The `limits_*` tests below cover the same stop conditions
    /// at small budgets; this one additionally pins the real constant.
    ///
    /// Asserted through the page walk stopping early — later pages carry the
    /// same extractable record as earlier ones, so a document of `pages` pages
    /// yielding fewer than `pages` records can only have been truncated.
    #[test]
    fn aggregate_content_decoding_is_capped_across_pages() {
        let payload = budget_filler_page(b"BT /F1 12 Tf (P) Tj ET\n");
        let per_page = payload.len() + 1; // +1 for the stream separator
        // Half again as many pages as the budget can pay for.
        let pages = (MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES / per_page) * 3 / 2;

        let result = inspect_bytes(&make_pdf_with_shared_content_stream(pages, payload, false));
        // Every page is still counted: the page tree is walked cheaply and the
        // bound is on content decoded, not on pages listed.
        assert_eq!(result.pages as usize, pages);
        assert!(
            !result.text_items.is_empty(),
            "pages within the budget must still be extracted"
        );
        assert!(
            result.text_items.len() < pages,
            "expected truncation: got {} records from {} pages",
            result.text_items.len(),
            pages
        );
    }

    /// The image pass decodes content independently, so it carries its own
    /// whole-document budget and stops the page walk the same way.
    #[test]
    fn aggregate_content_decoding_is_capped_across_pages_for_images() {
        let payload = budget_filler_page(b"q 50 0 0 50 0 0 cm /Im0 Do Q\n");
        let per_page = payload.len() + 1;
        let pages = (MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES / per_page) * 3 / 2;

        let result = inspect_bytes(&make_pdf_with_shared_content_stream(pages, payload, true));
        assert_eq!(result.pages as usize, pages);
        assert!(
            !result.images.is_empty(),
            "pages within the budget must still be extracted"
        );
        assert!(
            result.images.len() < pages,
            "expected truncation: got {} images from {} pages",
            result.images.len(),
            pages
        );
    }

    /// Run both extraction passes against explicit [`InspectLimits`].
    ///
    /// Small budgets reach a stop condition in milliseconds. The alternative is
    /// paying for the production budget: 20M operations is ~70 s in a debug
    /// build, and the per-page content bound caps a page at ~500k operations, so
    /// there is no shortcut to it.
    fn inspect_bytes_with_limits(
        bytes: &[u8],
        limits: &InspectLimits,
    ) -> (Vec<TextItem>, Vec<ImageItem>) {
        let doc = lopdf::Document::load_mem(bytes).expect("test PDF must load");
        (
            extract_text_items(&doc, limits).expect("text extraction must not error"),
            extract_image_items(&doc, limits).expect("image extraction must not error"),
        )
    }

    /// A document of `pages` identical pages, each carrying one extractable text
    /// record and one image placement, plus `ops_per_page` bare `q` operators.
    fn limits_fixture(pages: usize, ops_per_page: usize) -> Vec<u8> {
        let mut payload = b"BT /F1 12 Tf (P) Tj ET\nq 50 0 0 50 0 0 cm /Im0 Do Q\n".to_vec();
        payload.extend_from_slice(&b"q\n".repeat(ops_per_page));
        make_pdf_with_shared_content_stream(pages, payload, true)
    }

    /// The operation budget stops the page walk in both passes.
    ///
    /// This is the bound a byte budget cannot cover tightly — a `q` is 2 input
    /// bytes and ~570 bytes of operation list, ~285× the work per byte that
    /// [`MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES`] is calibrated against — so it
    /// needs its own coverage. The other budgets are set high enough here that
    /// only this one can bind.
    #[test]
    fn limits_operation_budget_stops_the_page_walk() {
        const PAGES: usize = 40;
        const OPS_PER_PAGE: usize = 100;
        let pdf = limits_fixture(PAGES, OPS_PER_PAGE);

        let limits = InspectLimits {
            // Enough for a few pages, not for all of them.
            operations: OPS_PER_PAGE * PAGES / 4,
            ..InspectLimits::default()
        };
        let (text, images) = inspect_bytes_with_limits(&pdf, &limits);

        assert!(
            !text.is_empty() && text.len() < PAGES,
            "text {}",
            text.len()
        );
        assert!(
            !images.is_empty() && images.len() < PAGES,
            "images {}",
            images.len()
        );

        // Same input, ample operation budget: nothing is truncated. This is what
        // shows the truncation above came from the operation budget and not from
        // some other property of the fixture.
        let (text, images) = inspect_bytes_with_limits(&pdf, &InspectLimits::default());
        assert_eq!(text.len(), PAGES);
        assert_eq!(images.len(), PAGES);
    }

    /// The whole-document content budget stops the page walk in both passes.
    ///
    /// The budget is a multiple of [`PDF_CONTENT_STREAM_COST_FLOOR_BYTES`], which
    /// is what each of these small pages costs, so it pays for several pages and
    /// then runs out — the assertions can require that extraction both *happened*
    /// and stopped early. At exactly the floor the budget would be spent on the
    /// first page's first stream, and "0 records" would satisfy a bare
    /// `len() < PAGES` however broken the mechanism was.
    #[test]
    fn limits_content_budget_stops_the_page_walk() {
        const PAGES: usize = 40;
        const AFFORDABLE: usize = 10;
        let pdf = limits_fixture(PAGES, 0);

        let limits = InspectLimits {
            content_total_bytes: PDF_CONTENT_STREAM_COST_FLOOR_BYTES * AFFORDABLE,
            ..InspectLimits::default()
        };
        let (text, images) = inspect_bytes_with_limits(&pdf, &limits);
        assert!(
            !text.is_empty() && text.len() < PAGES,
            "expected 1..{PAGES} text records, got {}",
            text.len()
        );
        assert!(
            !images.is_empty() && images.len() < PAGES,
            "expected 1..{PAGES} images, got {}",
            images.len()
        );

        // Same input, ample budget: nothing is truncated, so the truncation above
        // is attributable to the content budget and not to the fixture.
        let (text, images) = inspect_bytes_with_limits(&pdf, &InspectLimits::default());
        assert_eq!(text.len(), PAGES);
        assert_eq!(images.len(), PAGES);
    }

    /// The text-byte budget stops extraction, and does so independently of the
    /// record count — the item budget stays at its default here.
    #[test]
    fn limits_text_byte_budget_stops_extraction() {
        const PAGES: usize = 40;
        let pdf = limits_fixture(PAGES, 0);

        let limits = InspectLimits {
            text_bytes: 8,
            ..InspectLimits::default()
        };
        let (text, _) = inspect_bytes_with_limits(&pdf, &limits);
        assert!(!text.is_empty(), "the first record is always admitted");
        assert!(text.len() < PAGES, "text {}", text.len());
    }

    /// A content stream is charged at least
    /// [`PDF_CONTENT_STREAM_COST_FLOOR_BYTES`] even when it decodes to nothing,
    /// so a `/Contents` array of many zero-length streams cannot be walked for
    /// free.
    ///
    /// The real text stream sits *after* the empty ones, so it is only reached if
    /// the empties did not exhaust the budget — which makes the charge
    /// observable in the extracted output.
    #[test]
    fn limits_empty_content_streams_are_charged_a_cost_floor() {
        const EMPTIES: usize = 200;
        let mut streams = vec![Vec::new(); EMPTIES];
        streams.push(b"BT /F1 12 Tf 1 0 0 1 10 800 Tm (Reached) Tj ET".to_vec());
        let pdf = make_pdf_with_multiple_content_streams(streams);

        // The empty streams alone cost EMPTIES * floor, well over this budget.
        let starved = InspectLimits {
            content_total_bytes: EMPTIES * PDF_CONTENT_STREAM_COST_FLOOR_BYTES / 4,
            ..InspectLimits::default()
        };
        let (text, _) = inspect_bytes_with_limits(&pdf, &starved);
        assert!(
            text.is_empty(),
            "zero-length streams must draw down the budget, got {text:?}"
        );

        // With a budget that covers them, the same document reaches the text.
        let ample = InspectLimits {
            content_total_bytes: EMPTIES * PDF_CONTENT_STREAM_COST_FLOOR_BYTES * 4,
            ..InspectLimits::default()
        };
        let (text, _) = inspect_bytes_with_limits(&pdf, &ample);
        assert_eq!(
            text.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            ["Reached"]
        );
    }

    /// A `/Contents` reference that does *not* resolve to a stream is still
    /// charged the cost floor.
    ///
    /// The lookup costs the same whether it resolves or not, and a `/Contents`
    /// array of references to dictionaries or missing objects is as shareable as
    /// any other array — so skipping the charge would leave `pages × references`
    /// work unbounded.
    #[test]
    fn limits_unresolvable_content_references_are_charged() {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                },
            },
        });
        // References that resolve to a dictionary rather than a stream, followed
        // by a real text stream — so the charge is visible in the output.
        const DUDS: usize = 200;
        let dud = doc.add_object(dictionary! { "Type" => "ExampleNotAStream" });
        let mut contents: Vec<Object> = vec![Object::Reference(dud); DUDS];
        contents.push(Object::Reference(doc.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf 1 0 0 1 10 800 Tm (Reached) Tj ET".to_vec(),
        ))));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => contents,
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        let starved = InspectLimits {
            content_total_bytes: DUDS * PDF_CONTENT_STREAM_COST_FLOOR_BYTES / 4,
            ..InspectLimits::default()
        };
        let (text, _) = inspect_bytes_with_limits(&buf, &starved);
        assert!(
            text.is_empty(),
            "non-stream references must draw down the budget, got {text:?}"
        );

        let ample = InspectLimits {
            content_total_bytes: DUDS * PDF_CONTENT_STREAM_COST_FLOOR_BYTES * 4,
            ..InspectLimits::default()
        };
        let (text, _) = inspect_bytes_with_limits(&buf, &ample);
        assert_eq!(
            text.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            ["Reached"]
        );
    }

    /// [`resolve_page_resources`] reports where a dictionary lives, which is what
    /// makes it a usable cache key.
    ///
    /// A direct dictionary on an ancestor is [`ResourcesOrigin::DirectIn`] that
    /// ancestor — it has no id of its own, yet every descendant inherits it, so
    /// reporting it as unshared would rescan it per page. A direct dictionary on
    /// the page itself is `DirectIn` the page, for the same reason: a page tree
    /// may list the same `/Page` object more than once.
    #[test]
    fn resources_origin_records_where_the_dictionary_lives() {
        use lopdf::{Document, Object, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let shared_resources = doc.add_object(dictionary! { "ProcSet" => vec!["PDF".into()] });
        let inheriting = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
        });
        let own_resources = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            // Direct dictionary on the page itself.
            "Resources" => dictionary! { "ProcSet" => vec!["PDF".into()] },
        });
        let by_reference = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Resources" => Object::Reference(shared_resources),
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![
                    Object::Reference(inheriting),
                    Object::Reference(own_resources),
                    Object::Reference(by_reference),
                ],
                "Count" => Object::Integer(3),
                // Direct dictionary on the shared parent: no id, yet inherited.
                "Resources" => dictionary! { "ProcSet" => vec!["Text".into()] },
            }),
        );

        let (origin, _) = resolve_page_resources(&doc, inheriting).expect("inherited resources");
        assert_eq!(origin, ResourcesOrigin::DirectIn(pages_id));

        let (origin, _) = resolve_page_resources(&doc, own_resources).expect("own resources");
        assert_eq!(origin, ResourcesOrigin::DirectIn(own_resources));

        let (origin, _) = resolve_page_resources(&doc, by_reference).expect("referenced resources");
        assert_eq!(origin, ResourcesOrigin::Object(shared_resources));
    }

    /// One object id can name two *different* resources dictionaries, and the
    /// cache must not conflate them.
    ///
    /// Here object `pages_id` is a `/Pages` node holding a direct `/Resources`
    /// that declares an image, and one page also points `/Resources` straight at
    /// `pages_id` — for which the resources dictionary is the `/Pages` node
    /// itself, declaring no image. Keying both on the bare object id let the
    /// first page visited decide the other's images: with the offending page
    /// first, the inheriting page silently lost its image.
    #[test]
    fn resources_cache_distinguishes_an_object_from_its_nested_dictionary() {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => Object::Integer(4), "Height" => Object::Integer(4),
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => Object::Integer(8),
            },
            vec![0u8; 16],
        ));
        let content = b"q 50 0 0 50 0 0 cm /Im0 Do Q\n".to_vec();
        let content_a = doc.add_object(Stream::new(dictionary! {}, content.clone()));
        let content_b = doc.add_object(Stream::new(dictionary! {}, content));

        // Visited first: its resources dictionary *is* the /Pages node, which
        // declares no /XObject, so it must contribute no image.
        let points_at_pages_node = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Resources" => Object::Reference(pages_id),
            "Contents" => content_a,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ],
        });
        // Visited second: inherits the /Pages node's *nested* dictionary, which
        // does declare the image.
        let inheriting = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_b,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![
                    Object::Reference(points_at_pages_node),
                    Object::Reference(inheriting),
                ],
                "Count" => Object::Integer(2),
                "Resources" => dictionary! {
                    "XObject" => dictionary! { "Im0" => Object::Reference(image_id) },
                },
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        let result = inspect_bytes(&buf);
        // Page 2 inherits the nested dictionary and must still find its image,
        // uninfluenced by page 1 having scanned a different dictionary.
        let pages_with_images: Vec<u32> = result.images.iter().map(|i| i.page).collect();
        assert_eq!(
            pages_with_images,
            [2],
            "expected exactly page 2 to report an image, got {:?}",
            result.images
        );
    }

    /// A content stream is charged for the bytes its filter *consumed*, not only
    /// for what it produced.
    ///
    /// `/ASCII85Decode` ignores whitespace, so a stream of nothing but whitespace
    /// and the `~>` terminator is fully scanned and decodes to zero bytes. Charging
    /// the decoded length alone would price a megabyte of that at the floor.
    #[test]
    fn limits_encoded_bytes_are_charged_even_when_decoding_yields_nothing() {
        use lopdf::{Document, Object, Stream, dictionary};

        const ENCODED: usize = 64 * 1024;
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                },
            },
        });
        // Whitespace plus the end-of-data marker: scanned in full, decodes empty.
        let mut a85 = vec![b' '; ENCODED];
        a85.extend_from_slice(b"~>");
        let mut a85_stream = Stream::new(dictionary! {}, a85);
        a85_stream.dict.set("Filter", "ASCII85Decode");
        assert_eq!(
            a85_stream.decompressed_content().ok().map(|d| d.len()),
            Some(0),
            "test relies on this stream decoding to nothing"
        );
        let contents = vec![
            Object::Reference(doc.add_object(a85_stream)),
            Object::Reference(doc.add_object(Stream::new(
                dictionary! {},
                b"BT /F1 12 Tf 1 0 0 1 10 800 Tm (Reached) Tj ET".to_vec(),
            ))),
        ];
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => contents,
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        // A budget that the *encoded* length alone exhausts, but which the decoded
        // length (zero, so just the floor) would leave nearly untouched.
        let starved = InspectLimits {
            content_total_bytes: ENCODED / 4,
            ..InspectLimits::default()
        };
        let (text, _) = inspect_bytes_with_limits(&buf, &starved);
        assert!(
            text.is_empty(),
            "encoded bytes must draw down the budget, got {text:?}"
        );

        let ample = InspectLimits {
            content_total_bytes: ENCODED * 4,
            ..InspectLimits::default()
        };
        let (text, _) = inspect_bytes_with_limits(&buf, &ample);
        assert_eq!(
            text.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            ["Reached"]
        );
    }

    /// One shared indirect `/XObject` dictionary is scanned once even when the
    /// resources dictionaries pointing at it are all distinct.
    ///
    /// The cache is keyed on the `/XObject` dictionary, so distinct per-page
    /// resources wrappers around one shared dictionary collapse to a single
    /// origin — the identity that the scan result actually depends on.
    #[test]
    fn xobjects_origin_follows_the_shared_dictionary_not_the_wrapper() {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => Object::Integer(4), "Height" => Object::Integer(4),
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => Object::Integer(8),
            },
            vec![0u8; 16],
        ));
        // One indirect XObject dictionary, wrapped by two *distinct* direct
        // resources dictionaries.
        let shared_xobjects = doc.add_object(dictionary! { "Im0" => Object::Reference(image_id) });
        let mut origins = Vec::new();
        let mut kids = Vec::new();
        for _ in 0..2 {
            let page = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Resources" => dictionary! { "XObject" => Object::Reference(shared_xobjects) },
            });
            kids.push(Object::Reference(page));
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids.clone(),
                "Count" => Object::Integer(2),
            }),
        );
        for kid in &kids {
            let page_id = kid.as_reference().unwrap();
            let (res_origin, resources) = resolve_page_resources(&doc, page_id).expect("resources");
            // Each page's *resources* dictionary is its own.
            assert_eq!(res_origin, ResourcesOrigin::DirectIn(page_id));
            origins.push(xobjects_origin(&doc, resources, res_origin));
        }
        // But both resolve to the same XObject dictionary, so one cache entry.
        assert_eq!(origins[0], XObjectsOrigin::Object(shared_xobjects));
        assert_eq!(origins[0], origins[1]);
    }

    /// `/Info` metadata strings are clamped: the metadata pass has no per-page or
    /// per-record structure, so the text and item budgets do not reach it.
    #[test]
    fn metadata_fields_are_clamped() {
        use lopdf::{Document, Object, dictionary};

        let long = "T".repeat(MAX_PDF_METADATA_FIELD_BYTES * 4);
        let multibyte = "あ".repeat(MAX_PDF_METADATA_FIELD_BYTES);
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => Vec::<Object>::new(), "Count" => Object::Integer(0),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        let info_id = doc.add_object(dictionary! {
            "Title" => Object::string_literal(long.as_str()),
            "Author" => Object::string_literal(multibyte.as_str()),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.trailer.set("Info", Object::Reference(info_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        let result = inspect_bytes(&buf);
        let title = result.metadata.title.expect("title");
        assert_eq!(title.len(), MAX_PDF_METADATA_FIELD_BYTES);
        let author = result.metadata.author.expect("author");
        assert!(
            author.len() <= MAX_PDF_METADATA_FIELD_BYTES,
            "author {} bytes",
            author.len()
        );
        // Latin-1 fallback widens each byte to a 2-byte code point, so the clamp
        // has to land on a character boundary rather than mid-sequence.
        assert!(
            author.chars().all(|c| c != '\u{FFFD}'),
            "clamped mid-character"
        );
    }

    /// A shared content stream is decoded once, not once per referring page.
    ///
    /// This is what bounds a multi-filter stream whose *intermediate* stage is
    /// large while both its encoded and decoded lengths are small — no charge
    /// derived from those two lengths can see that expansion, so the defence is
    /// to not decode it repeatedly. Asserted through the budget: the same stream
    /// referenced many times costs its filter work once.
    #[test]
    fn shared_content_streams_are_decoded_once() {
        use lopdf::{Document, Object, Stream, dictionary};

        const ENCODED: usize = 32 * 1024;
        const REFS: usize = 64;
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                },
            },
        });
        let mut a85 = vec![b' '; ENCODED];
        a85.extend_from_slice(b"~>");
        let mut a85_stream = Stream::new(dictionary! {}, a85);
        a85_stream.dict.set("Filter", "ASCII85Decode");
        let shared = doc.add_object(a85_stream);
        // The same stream object referenced many times from one /Contents array.
        let contents: Vec<Object> = vec![Object::Reference(shared); REFS];
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => contents,
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        let loaded = Document::load_mem(&buf).unwrap();
        let page = *loaded.get_pages().values().next().unwrap();

        // Enough for one decode of the encoded length plus a floor per reference,
        // but nowhere near `REFS` decodes of it.
        let mut budget = ENCODED + REFS * PDF_CONTENT_STREAM_COST_FLOOR_BYTES * 2;
        assert!(
            budget < ENCODED * REFS,
            "budget must be too small for one decode per reference"
        );
        let mut cache = DecodedStreams::new();
        assert!(
            matches!(
                gather_page_content(&loaded, page, &mut budget, &mut cache),
                PageContent::Ready(_)
            ),
            "one decode plus per-reference copies must fit the budget"
        );
        assert_eq!(cache.len(), 1, "the shared stream must be cached once");
    }

    /// A page with neither `/Contents` nor `/Resources` is still charged, in
    /// *both* passes, so a page set that costs nothing to produce cannot be
    /// walked for free.
    ///
    /// The charge lives at the top of each pass's page loop rather than inside
    /// `gather_page_content`, because the image pass reaches that function only
    /// after two early exits — resolving no resources, or collecting no images —
    /// and a floor charged inside it would leave those pages free.
    ///
    /// Made observable by putting the cheap pages *first* and a page carrying
    /// both text and an image last: with a budget the leading pages exhaust, the
    /// final page is never reached.
    #[test]
    fn limits_pages_without_content_are_charged_in_both_passes() {
        use lopdf::{Document, Object, Stream, dictionary};

        const EMPTIES: usize = 200;
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        // Neither /Contents nor /Resources: both passes exit early on these.
        for _ in 0..EMPTIES {
            kids.push(Object::Reference(doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ],
            })));
        }
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => Object::Integer(4), "Height" => Object::Integer(4),
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => Object::Integer(8),
            },
            vec![0u8; 16],
        ));
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                },
            },
            "XObject" => dictionary! { "Im0" => Object::Reference(image_id) },
        });
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf 1 0 0 1 10 800 Tm (Reached) Tj ET\nq 50 0 0 50 0 0 cm /Im0 Do Q\n"
                .to_vec(),
        ));
        kids.push(Object::Reference(doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ],
        })));
        let count = kids.len() as i64;
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => Object::Integer(count),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        let starved = InspectLimits {
            content_total_bytes: EMPTIES * PDF_CONTENT_STREAM_COST_FLOOR_BYTES / 4,
            ..InspectLimits::default()
        };
        let (text, images) = inspect_bytes_with_limits(&buf, &starved);
        assert!(
            text.is_empty(),
            "text pass must charge cheap pages, got {text:?}"
        );
        assert!(images.is_empty(), "image pass must charge cheap pages too");

        // With a budget that covers them, the final page is reached by both.
        let (text, images) = inspect_bytes_with_limits(&buf, &InspectLimits::default());
        assert_eq!(
            text.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            ["Reached"]
        );
        assert_eq!(images.len(), 1);
    }

    /// Canonicalisation accepts a chain exactly when lopdf's own `get_object`
    /// does, and rejects deeper ones rather than decoding under a mid-chain key.
    ///
    /// Asserting the *equivalence* rather than a hardcoded link count is the
    /// point: two independent limits are in play — this loop's and lopdf's
    /// `DEREF_LIMIT` — and either being one link stricter than the other is a
    /// silent bug in a different direction. Stricter here skips a stream
    /// `get_object` would have read; looser hands each alias converging past
    /// lopdf's bound its own cache key for one shared stream. A sweep over
    /// lengths straddling the bound pins both edges and cannot drift.
    #[test]
    fn canonicalisation_matches_lopdf_dereference_limit() {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let real = doc.add_object(Stream::new(dictionary! {}, b"BT ET".to_vec()));
        // `chain[k]` is the id whose alias chain reaches `real` in `k` links, so
        // `chain[0]` is the stream itself — its own canonical form.
        let mut chain = vec![real];
        for _ in 0..MAX_PDF_PARENT_DEPTH + 2 {
            let next = doc.add_object(Object::Reference(*chain.last().unwrap()));
            chain.push(next);
        }

        let mut accepted = 0usize;
        for (links, &id) in chain.iter().enumerate() {
            let canonical = canonical_object_id(&doc, id);
            let lopdf_reads_it = doc.get_object(id).is_ok();
            assert_eq!(
                canonical.is_some(),
                lopdf_reads_it,
                "{links}-link chain: canonicalised to {canonical:?} but get_object \
                 {}; the two limits have drifted apart",
                if lopdf_reads_it {
                    "succeeded"
                } else {
                    "failed"
                },
            );
            if canonical.is_some() {
                assert_eq!(canonical, Some(real), "{links}-link chain resolved short");
                accepted += 1;
            }
        }
        // Guard the sweep itself: it must straddle the bound, not sit entirely on
        // one side of it and pass vacuously.
        assert_eq!(
            accepted,
            MAX_PDF_PARENT_DEPTH + 1,
            "expected chains of 0..={MAX_PDF_PARENT_DEPTH} links to resolve and \
             longer ones to be rejected",
        );
    }

    /// An `/XObject` reached through an alias object resolves to the same origin
    /// as one reached directly, so per-page aliases share a cache entry.
    #[test]
    fn xobjects_origin_canonicalises_reference_aliases() {
        use lopdf::{Document, Object, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let real = doc.add_object(dictionary! { "Im0" => Object::Null });
        // Two distinct alias objects, each just a reference to `real`.
        let alias_a = doc.add_object(Object::Reference(real));
        let alias_b = doc.add_object(Object::Reference(real));
        let mut origins = Vec::new();
        for alias in [alias_a, alias_b, real] {
            let page = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Resources" => dictionary! { "XObject" => Object::Reference(alias) },
            });
            let (res_origin, resources) = resolve_page_resources(&doc, page).expect("resources");
            origins.push(xobjects_origin(&doc, resources, res_origin));
        }
        // All three name the dictionary the chain ends on, not the alias.
        assert_eq!(origins[0], XObjectsOrigin::Object(real));
        assert_eq!(origins[0], origins[1]);
        assert_eq!(origins[0], origins[2]);
    }

    /// A page reached through an alias id yields the same origin as the page
    /// itself, so a shared `/Page` with *direct* `/Resources` is scanned once.
    ///
    /// `Kids` entries and `/Parent` back-references may be alias objects, and
    /// `get_object` follows them silently — so the dictionary is the same one
    /// every time, but keying on the immediate id would name it differently per
    /// alias and make the cache rescan and retain a map for each.
    #[test]
    fn direct_resources_origin_canonicalises_page_aliases() {
        use lopdf::{Document, Object, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        // One page, whose /Resources and /XObject are both direct dictionaries.
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => Object::Null } },
        });
        // Two Kids ids that are alias objects for that one page.
        let alias_a = doc.add_object(Object::Reference(page_id));
        let alias_b = doc.add_object(Object::Reference(page_id));

        let mut origins = Vec::new();
        for id in [alias_a, alias_b, page_id] {
            let (res_origin, resources) = resolve_page_resources(&doc, id).expect("resources");
            origins.push((res_origin, xobjects_origin(&doc, resources, res_origin)));
        }
        assert_eq!(
            origins[0].0,
            ResourcesOrigin::DirectIn(page_id),
            "origin must name the page the alias resolves to, not the alias",
        );
        assert_eq!(origins[0], origins[1]);
        assert_eq!(origins[0], origins[2]);
        assert_eq!(
            origins[0].1,
            XObjectsOrigin::DirectIn(ResourcesOrigin::DirectIn(page_id)),
        );
    }

    /// A `/Contents` entry that is an *alias* — a reference object pointing at
    /// the real stream — collapses to the same cache key as the stream itself.
    ///
    /// `get_page_contents` returns the ids written in the array without following
    /// them, so without canonicalisation every alias is a fresh cache miss and
    /// re-runs the filter chain that the cache exists to avoid.
    #[test]
    fn content_stream_aliases_share_one_cache_entry() {
        use lopdf::{Document, Object, Stream, dictionary};

        const ENCODED: usize = 32 * 1024;
        const ALIASES: usize = 32;
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                },
            },
        });
        let mut a85 = vec![b' '; ENCODED];
        a85.extend_from_slice(b"~>");
        let mut a85_stream = Stream::new(dictionary! {}, a85);
        a85_stream.dict.set("Filter", "ASCII85Decode");
        let real = doc.add_object(a85_stream);
        // Every /Contents entry is its own alias object pointing at `real`.
        let contents: Vec<Object> = (0..ALIASES)
            .map(|_| Object::Reference(doc.add_object(Object::Reference(real))))
            .collect();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => contents,
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        let loaded = Document::load_mem(&buf).unwrap();
        let page = *loaded.get_pages().values().next().unwrap();

        let mut budget = ENCODED + ALIASES * PDF_CONTENT_STREAM_COST_FLOOR_BYTES * 3;
        assert!(
            budget < ENCODED * ALIASES,
            "budget must forbid one decode per alias"
        );
        let mut cache = DecodedStreams::new();
        assert!(matches!(
            gather_page_content(&loaded, page, &mut budget, &mut cache),
            PageContent::Ready(_)
        ));
        assert_eq!(cache.len(), 1, "all aliases must share one cache entry");
    }

    /// Metadata is decoded from a bounded prefix, so an oversized value cannot
    /// force a large allocation that the clamp then discards.
    #[test]
    fn metadata_prefix_is_bounded_before_decoding() {
        // Latin-1 high bytes widen to two UTF-8 bytes each, so the prefix bound
        // is what keeps the intermediate allocation small.
        let wide = vec![0xFFu8; MAX_PDF_METADATA_FIELD_BYTES * 8];
        let prefix = metadata_prefix(&wide);
        assert_eq!(prefix.len(), MAX_PDF_METADATA_FIELD_BYTES);
        assert!(decode_pdf_string(prefix).len() <= MAX_PDF_METADATA_FIELD_BYTES * 2);

        // UTF-16BE: cut after the BOM on an even boundary so no code unit splits.
        let mut utf16 = vec![0xFE, 0xFF];
        utf16.extend(std::iter::repeat_n([0x00, 0x41], MAX_PDF_METADATA_FIELD_BYTES).flatten());
        let prefix = metadata_prefix(&utf16);
        assert_eq!((prefix.len() - 2) % 2, 0, "UTF-16 code unit split");
        assert!(!decode_pdf_string(prefix).contains('\u{FFFD}'));

        // Short values are untouched.
        assert_eq!(metadata_prefix(b"Title"), b"Title");
    }

    /// The image XObject scan is charged per entry examined, which bounds both
    /// the scanning work and the size of the retained cache.
    ///
    /// Without this, a page declaring many XObjects but no content emitted no
    /// records and decoded nothing, so it advanced no budget while its scan
    /// result stayed cached for the rest of the pass.
    #[test]
    fn xobject_scans_are_charged_per_entry() {
        use lopdf::{Document, Object, dictionary};

        const ENTRIES: usize = 500;
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut xobjects = dictionary! {};
        for i in 0..ENTRIES {
            // Non-image, so the scan collects nothing and content is skipped.
            let form = doc.add_object(lopdf::Stream::new(
                dictionary! { "Type" => "XObject", "Subtype" => "Form" },
                Vec::new(),
            ));
            xobjects.set(format!("X{i}"), Object::Reference(form));
        }
        let resources_id = doc.add_object(dictionary! { "XObject" => xobjects });
        let mut kids = Vec::new();
        for _ in 0..8 {
            kids.push(Object::Reference(doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Resources" => Object::Reference(resources_id),
                "MediaBox" => vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ],
            })));
        }
        let count = kids.len() as i64;
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => Object::Integer(count),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        // A budget smaller than one full scan: the pass must stop rather than
        // scan and retain regardless.
        let starved = InspectLimits {
            content_total_bytes: ENTRIES * PDF_CONTENT_STREAM_COST_FLOOR_BYTES / 2,
            ..InspectLimits::default()
        };
        let (_, images) = inspect_bytes_with_limits(&buf, &starved);
        assert!(images.is_empty());

        // The scan itself must stop partway rather than complete and then be
        // billed: with a budget for a quarter of the entries, at most that many
        // are examined. Asserted directly on `collect_image_xobjects`, since a
        // completed-then-charged scan is indistinguishable from this one by its
        // effect on `images` alone.
        let doc = lopdf::Document::load_mem(&buf).unwrap();
        let page = *doc.get_pages().values().next().unwrap();
        let (_, resources) = resolve_page_resources(&doc, page).expect("resources");
        let mut budget = ENTRIES / 4 * PDF_CONTENT_STREAM_COST_FLOOR_BYTES;
        let (_, within_budget) = collect_image_xobjects(&doc, resources, &mut budget);
        assert!(!within_budget, "an over-budget scan must report exhaustion");
        assert_eq!(budget, 0);

        // With the default budget the same document is walked without trouble,
        // and the shared dictionary is scanned once rather than once per page.
        let (_, images) = inspect_bytes_with_limits(&buf, &InspectLimits::default());
        assert!(images.is_empty(), "non-image XObjects yield no placements");
    }

    /// The item budget stops extraction in both passes.
    #[test]
    fn limits_item_budget_stops_extraction() {
        const PAGES: usize = 40;
        let pdf = limits_fixture(PAGES, 0);

        let limits = InspectLimits {
            items: 5,
            ..InspectLimits::default()
        };
        let (text, images) = inspect_bytes_with_limits(&pdf, &limits);
        assert_eq!(text.len(), 5);
        assert_eq!(images.len(), 5);
    }

    /// The whole-document content budget is charged for pages that are *skipped*
    /// for exceeding the per-page bound, not only for usable ones.
    ///
    /// Otherwise the aggregate stays unbounded: a page can be made to decode a
    /// full per-page allowance and then be rejected, paying nothing.
    #[test]
    fn skipped_pages_are_charged_against_the_content_budget() {
        let mut budget = 4096;
        let over_page = {
            let mut p = Vec::new();
            while p.len() <= MAX_PDF_CONTENT_BYTES {
                p.extend_from_slice(b"(A) Tj\n");
            }
            p
        };
        let pdf = make_pdf_with_shared_content_stream(1, over_page, false);
        let doc = lopdf::Document::load_mem(&pdf).unwrap();
        let page_id = *doc.get_pages().values().next().unwrap();

        // The page is over the per-page bound, so it cannot be used — but the
        // decoding it cost must still have drawn the budget down to zero, which
        // is reported as exhaustion rather than a plain skip.
        assert!(matches!(
            gather_page_content(&doc, page_id, &mut budget, &mut DecodedStreams::new()),
            PageContent::Exhausted
        ));
        assert_eq!(budget, 0);
    }

    /// A `/Contents` entry that does not resolve to a stream object is skipped
    /// individually; it must not abandon the rest of the page, matching how
    /// `lopdf::Document::get_page_content` treats the same input.
    #[test]
    fn page_content_skips_entries_that_are_not_streams() {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                },
            },
        });
        // A dictionary where a content stream is expected, then a real stream.
        let not_a_stream = doc.add_object(dictionary! { "Type" => "ExampleNotAStream" });
        let real = doc.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf 1 0 0 1 10 800 Tm (Survivor) Tj ET".to_vec(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => vec![Object::Reference(not_a_stream), Object::Reference(real)],
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        let result = inspect_bytes(&buf);
        let texts: Vec<&str> = result.text_items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["Survivor"]);
    }

    /// A stream whose filter chain cannot be decoded contributes its raw,
    /// still-encoded bytes, which is what `lopdf` does — the fallback matters
    /// because those bytes sometimes still lex as operators.
    #[test]
    fn page_content_falls_back_to_raw_bytes_when_decoding_fails() {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                },
            },
        });
        // `/Crypt` is not among the filters lopdf implements, so
        // `decompressed_content` reports an error and the raw bytes are used.
        let mut stream = Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf 1 0 0 1 10 800 Tm (Undecoded) Tj ET".to_vec(),
        );
        stream.dict.set("Filter", "Crypt");
        assert!(
            stream.decompressed_content().is_err(),
            "test relies on this filter being undecodable by lopdf"
        );
        let content_id = doc.add_object(stream);
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        let result = inspect_bytes(&buf);
        let texts: Vec<&str> = result.text_items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["Undecoded"]);
    }
}
