/// Maximum DOM tree depth before recursion is cut off. Prevents stack overflow
/// from pathologically deep HTML input.
pub(crate) const MAX_DOM_DEPTH: usize = 512;

/// Per-element page-amplification bound (NOT a strict total-page ceiling —
/// see the note below). Bounds the per-page-strip slicing of an oversized
/// block in `pagination_layout` (and the absolute-positioning page-extension
/// path) so a tiny input with a pathologically tall CSS height / offset
/// (e.g. `height: 99999999px`, `position:absolute; top:99999999px`) cannot
/// force unbounded fragment / page generation — which downstream inflates a
/// `vec![Vec::new(); page_count]` allocation and a per-page render loop into
/// a CPU/memory-exhaustion DoS.
///
/// The cap is keyed off the running `page_index`, not a per-element budget.
/// That is deliberate: it makes many oversized siblings **additive** rather
/// than **multiplicative** — each extra oversized block contributes only its
/// own first fragment once the slice loop is capped, so the document total
/// settles at `MAX_PAGES + N` for `N` body children instead of
/// `N * MAX_PAGES`. A per-element budget would re-open that multi-element
/// amplification, so the cap must stay keyed off the absolute page index
/// (Codex review on PR #501).
///
/// Consequence: the total page count can slightly **exceed** `MAX_PAGES`
/// (it is `MAX_PAGES + N`, still input-proportional and rendered in bounded
/// time). This constant therefore caps single-element amplification, not the
/// absolute page count — code must not assume `page_count <= MAX_PAGES`.
///
/// Kept deliberately conservative for untrusted server-side rendering (a
/// shared multi-tenant renderer is the primary deployment): a tiny
/// pathological input must not amplify into one render iteration and PDF
/// page per fragment. The most common amplifier — a childless block with a
/// huge height, which prints only blank pages — is additionally collapsed to
/// a single page in `pagination_layout` (fulgur-ezst), so this ceiling only
/// bounds the residual content-bearing case. Content past the cap is
/// truncated (clamp-and-warn). Sibling of the `MAX_DOM_DEPTH` / background
/// `MAX_TILES` defensive bounds.
pub(crate) const MAX_PAGES: u32 = 10_000;

/// Maximum number of columns a single multi-column container is laid out
/// across, regardless of the CSS `column-count` / `column-width` the input
/// requests. CSS accepts `column-count` up to `i32::MAX`, and a small
/// `column-width` on a wide container derives an equally large used count —
/// neither is bounded by the spec.
///
/// The used count `n` is a **multiplier on retained layout metadata**: the
/// multicol hook allocates one `vec![0.0; n]` per column group (`col_heights`)
/// and, for every paragraph that splits across columns, an `n`-length
/// `column_slices` vector padded with placeholders for unused columns (its
/// length equals `n` by the `ParagraphSplitEntry` contract). A single
/// `<div style="column-count:2000000000">x</div>` therefore forces one ~8 GB
/// `col_heights` allocation, and the split path amplifies to
/// O(`column_count` × paragraph_count) — an unbounded, attacker-controllable
/// memory-exhaustion DoS on a server-side renderer of untrusted HTML/CSS.
///
/// Clamping the resolved count at the single choke point
/// ([`multicol_layout::resolve_column_layout`]) bounds every downstream `n`
/// sink at once (`col_heights`, `column_slices`, `slice_lines_by_budget`) and
/// keeps the total metadata input-proportional (`n` is a small constant, so
/// the split path stays O(paragraph_count)). 64 is far above any legitimate
/// print layout — real multi-column text rarely exceeds a handful of columns —
/// so no realistic document is affected; a pathological explicit or
/// width-derived count is merely clamped (the columns collapse to zero width,
/// as they already would past the container's capacity). Sibling of the
/// [`MAX_DOM_DEPTH`] / [`MAX_PAGES`] defensive bounds.
pub(crate) const MAX_COLUMN_COUNT: u32 = 64;

/// Maximum number of CSS gradient color stops retained per background layer.
/// CSS accepts an unbounded stop list (`linear-gradient(c0, c1, …, cN)`), and
/// convert clones the resolved `Vec<GradientStop>` into every element's
/// [`crate::draw_primitives::BackgroundLayer`] — so a rule like
/// `* { background: linear-gradient(<N stops>) }` retains O(elements × N).
/// For repeating gradients the draw-time period expansion in
/// `background::expand_repeating_stops` further materializes
/// `total_copies × stops.len()` entries (`total_copies` is already bounded by
/// its own `MAX_PERIODS`), so bounding `stops.len()` here keeps that product
/// bounded too.
///
/// Clamping the stop count at the convert-side choke points that build the
/// vector (`resolve_color_stops` for linear/radial, plus the conic loop)
/// bounds both the retained per-layer vector and the downstream repeating
/// expansion at once. 256 stops is far beyond any legitimate gradient — real
/// designs use a handful — so no realistic document changes output; excess
/// stops are truncated (clamp-and-warn). Sibling of the [`MAX_COLUMN_COUNT`]
/// multicol bound.
pub(crate) const MAX_GRADIENT_STOPS: usize = 256;

/// Minimum tile size (in PDF pt) for CSS `background-image` gradients — a
/// defensive floor applied to the resolved tile size (explicit
/// `background-size`, origin-derived `Auto`/`Cover`/`Contain`, and the
/// `background-repeat: round`–adjusted axis). Strictly-positive values
/// below this are clamped up to the constant; zero remains zero.
///
/// `background.rs::try_uniform_grid` uses a `1e-3` pt epsilon to identify
/// tile position clusters as "the same column/row"; tiles narrower than
/// that epsilon collapse in the dedup and defeat the Pattern fast path,
/// forcing per-tile fallback with up to `MAX_TILES × MAX_GRADIENT_STOPS`
/// stop-copies of PDF output (~425 MB observed for the Codex `01f477e4`
/// subpixel residual). Clamping to `10 ×` the epsilon keeps the FP
/// safety margin.
///
/// Resolution context: `0.01 pt = 72 pt/inch / 7200 dpi`, i.e. **exactly
/// one physical dot at commercial-max 7200 dpi printing** — the clamp
/// equals a single dot at that ceiling, not below it. At more common
/// resolutions (300–1200 dpi, `1 dot ≈ 0.06–0.24 pt`) the clamp is
/// 6–24× smaller than a printed dot; content authored at those
/// resolutions is unaffected because no legitimate value falls between
/// the min and the printer dot.
///
/// **Impact on values below the clamp**: the lift factor depends on
/// how far below the min the raw tile is (`0.005 pt → 0.01 pt = 2×`,
/// `0.001 pt → 0.01 pt = 10×`, `0.0001 pt → 0.01 pt = 100×`; the ratio
/// grows unbounded as the raw value approaches zero). Sub-min values
/// are still not guaranteed invisible — antialiasing and sub-pixel
/// positioning can make them contribute to rendered output — but a
/// gradient authored with a tile of that scale cannot itself be read
/// as a gradient (a single physical dot or less is either one flat
/// color or an AA artifact of one), so the design intent is not
/// preserved either way. Lifting to the min is accepted as a
/// defense-in-depth trade-off against the exploit path above.
///
/// Sibling of the [`MAX_GRADIENT_STOPS`] gradient bound. Also applies
/// to raster/SVG tiles that transit `resolve_repeat_axis::Round` since
/// that helper is shared; the same "visibility not guaranteed, design
/// intent not preserved either way" caveat applies to raster tiles at
/// that scale.
pub(crate) const MIN_GRADIENT_TILE_PT: f32 = 0.01;

/// Maximum byte length of a CSS named page (`page: <custom-ident>` /
/// `@page <name>`). A CSS `<custom-ident>` is length-unbounded, and the name
/// `String` is retained in the parsed `ColumnProps`, cloned into the column
/// style table, and cloned again per node while walking used page names
/// (`blitz_adapter::walk_used_page_names`) — so a huge name amplifies to
/// O(nodes × name_len) retained bytes on untrusted CSS.
///
/// Capping the name at its single construction site (`parse_page_value`)
/// bounds every downstream clone at once. 256 bytes is far beyond any real
/// page name (`cover`, `landscape-table`, …); a longer ident is truncated at
/// a UTF-8 char boundary (clamp-and-warn). Sibling of the [`MAX_GRADIENT_STOPS`]
/// bound.
pub(crate) const MAX_PAGE_NAME_BYTES: usize = 256;

/// Aggregate upper bound on per-page subtree fragments emitted by the two
/// out-of-flow pagination passes that record one `Fragment` per page per node
/// of a subtree:
///
/// - `pagination_layout::append_position_fixed_fragments` — a `position: fixed`
///   root and every in-flow descendant repeat on **every** page.
/// - `pagination_layout::append_position_absolute_body_direct_fragments` — each
///   node of a body-direct `position: absolute` subtree is recorded once per
///   page it intersects (`first_page..=last_page`).
///
/// In both, the page axis is already bounded by [`MAX_PAGES`], but the node
/// axis is not: a subtree with many descendants — or many page-spanning
/// absolute / fixed elements — amplifies to O(nodes × pages) retained
/// `Fragment`s plus a matching per-page render loop on untrusted input.
///
/// Each pass threads a single running counter and stops emitting once it hits
/// this budget (excess fragments are dropped — the content simply stops
/// repeating past the cap). Combined with skipping zero-area fixed fragments
/// (which draw nothing, so the skip is output-preserving), this bounds both
/// passes in memory and time. 1M fragments (~tens of MiB) is far above any
/// realistic running header/footer or absolute layout over a long document.
/// Sibling of the [`MAX_PAGE_NAME_BYTES`] bound.
pub(crate) const MAX_SUBTREE_PAGE_FRAGMENTS: usize = 1_000_000;

/// Upper bound on the **output bytes** materialized for a single resolved
/// counter chain (`counters(name, sep, style)`), applied inside
/// [`gcpm::counter::format_counter_chain`] so it covers every call site
/// (`::before` / `::after` resolution, margin-box, and `target-counters()`).
///
/// `counters()` joins the active counter chain with an attacker-controlled
/// separator, so a single resolution is `separator_len * chain_len`. This caps
/// the *separator* axis: even a multi-kilobyte separator cannot push one
/// `counters()` output past this ceiling. The orthogonal *length* axis is
/// bounded by [`MAX_COUNTER_CHAIN_ENTRIES`]. Real documents (deepest legitimate
/// `counters()`, e.g. nested `<ol>`) stay in the tens of bytes, far below this.
/// Sibling of the total-output [`MAX_GENERATED_CSS_BYTES`] budget.
pub(crate) const MAX_COUNTER_CHAIN_BYTES: usize = 4 * 1024;

/// Upper bound on the **number of entries** kept for a single counter chain
/// (its length), applied when a chain is cloned for later resolution
/// (`CounterState::chain` / `chain_snapshot`).
///
/// A chain grows one entry per in-scope `counter-reset`. The *nesting*
/// contribution is already bounded by [`MAX_DOM_DEPTH`] — the counter walk
/// stops recursing there, so no reset below that depth is even processed —
/// which is why this cap is tied to it: any chain longer than
/// `MAX_DOM_DEPTH` can only come from following-sibling `counter-reset`
/// accumulation (unbounded breadth, i.e. adversarial), and that surplus cannot
/// affect a realistic document. Bounding the stored length keeps the retained
/// per-node snapshots from being O(N²) and is the *length* companion to the
/// *separator*-axis [`MAX_COUNTER_CHAIN_BYTES`]; the two are independent knobs
/// on `separator_len * chain_len`.
pub(crate) const MAX_COUNTER_CHAIN_ENTRIES: usize = MAX_DOM_DEPTH;

/// Upper bound on how many `/Parent` references [`inspect::resolve_page_resources`]
/// will follow when looking up an inherited `Resources` dictionary on a PDF page.
///
/// Walking the parent chain unconditionally is unsafe on attacker-controlled PDFs
/// because a cyclic chain (`A -> B -> A`, or any longer cycle) would loop forever;
/// even a genuinely long non-cyclic chain would consume unbounded work. Real page
/// trees stay very shallow — Adobe's implementation notes recommend at most 7
/// levels, and typical documents nest 1–3 deep — so a fixed depth cap terminates
/// pathological inputs without ever rejecting a legitimate one. Sibling of the
/// [`MAX_DOM_DEPTH`] defensive traversal bound.
pub(crate) const MAX_PDF_PARENT_DEPTH: usize = 128;

/// Upper bound on the total size of a page's *decoded* content that
/// [`inspect::inspect`] will hand to `lopdf::content::Content::decode`.
///
/// `Content::decode` materialises the entire operation list up front, at a
/// measured ~570 bytes of heap per operation for bare `q` operators — which
/// occupy 2 input bytes each, a ~285× expansion. Page content streams are also
/// Flate-compressed, so a few kilobytes on disk can decompress into megabytes
/// of operators, an expansion the caller never sees in the file size. Pages
/// above this bound are skipped rather than parsed.
///
/// 1 MiB is ~22× the largest content stream produced by any document in the
/// repository's own corpus (`examples/**` and `crates/fulgur-vrt/goldens/**`
/// peak at 47 KB), and bounds the worst-case operation list at roughly 285 MB
/// — the same order as an extra-large fulgur render. Peak memory stays bounded
/// *per page* because the decoded operation list is dropped before the next
/// page is read.
///
/// Enforced *incrementally*, by [`inspect::gather_page_content`], as the page's
/// content streams are decoded and concatenated. `lopdf`'s own
/// `Document::get_page_content` inflates every stream of the page and joins
/// them before returning, so testing the length of its result would bound only
/// what gets parsed, not what gets allocated: a page carrying many small
/// compressed streams would still inflate all of them first.
///
/// Three residual amplifications belong to `lopdf` rather than to fulgur, and
/// this bound is the only lever over any of them, because each has already
/// allocated by the time fulgur could observe a size. Reimplementing the
/// surrounding `lopdf` logic to bound them would risk diverging from it, and the
/// property being protected here is that `inspect` output stays byte-identical
/// across the repository corpus:
///
/// - `Content::decode`'s ~570 bytes per operation, above.
/// - A *single* content stream whose decoded size exceeds this bound is still
///   inflated in full by `Stream::decompressed_content` before its length can
///   be measured. Bounding that from fulgur would mean reimplementing the
///   filter chain (Flate/LZW/ASCII85 plus predictors, and `lopdf`'s raw-deflate
///   retry for streams with a corrupt adler32 checksum) against a size-limited
///   reader — which would silently stop inspecting PDFs that decode today.
///   Note that [`inspect::gather_page_content`]'s decoded-stream cache reduces
///   this to *once per document* rather than once per referring page, including
///   for a filter chain whose intermediate stage is large while both its encoded
///   and decoded lengths are small.
/// - `Document::get_page_contents` materialises a page's whole `/Contents` array
///   into a `Vec<ObjectId>` before any of it can be charged, so one such build
///   per pass is unavoidable from here. Measured at ~4 MB for a 500k-reference
///   array — half of what `lopdf` already holds for the same array — and it does
///   not scale with page count, because the per-reference charge exhausts the
///   budget on the first page that walks it.
pub(crate) const MAX_PDF_CONTENT_BYTES: usize = 1024 * 1024;

/// Whole-document budget on how many bytes of *decoded* page content
/// [`inspect::inspect`] will parse, across all pages, in each of its two passes.
///
/// [`MAX_PDF_CONTENT_BYTES`] is per page, and a page's cost is only ever paid
/// per page — but the number of pages is bounded by nothing except input size,
/// and several page objects may share one `/Contents` stream. So a small file
/// can define a great many pages that each re-pay the full per-page cost:
/// measured, 200 pages sharing one 256 KB stream of bare `q` operators is 27 KB
/// on disk and 8.8 s of CPU, scaling linearly with the page count while
/// [`MAX_PDF_INSPECT_ITEMS`] never fires because such a page emits no records.
/// Peak memory stays bounded there — it is the repeated work that is not.
///
/// Charged as content is decoded, *including* for a page that is then skipped
/// for exceeding the per-page bound: the decoding work has been done either
/// way, so charging only usable pages would leave the same hole open. Once the
/// budget is spent the page walk stops rather than continuing to the next page.
///
/// This also bounds retained output that a record *count* cannot: a `Tj`
/// operand is retained as [`inspect::TextItem::text`], and its bytes appear
/// literally in the content stream, so total retained text is at most this
/// budget times the worst-case widening of `decode_pdf_string` (2×, one Latin-1
/// byte becoming two UTF-8 bytes). [`MAX_PDF_INSPECT_TEXT_BYTES`] bounds that
/// directly and more tightly; this bound is what makes it unnecessary to trust
/// that derivation.
///
/// 128 MiB is ~2,500 pages at the corpus's largest content stream (47 KB),
/// matching the ~2,500-page scale [`MAX_PDF_INSPECT_ITEMS`] is calibrated for,
/// so for realistic documents the item cap is reached at about the same point
/// and this bound never binds first.
pub(crate) const MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES: usize = 128 * 1024 * 1024;

/// Minimum charge against [`MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES`] for touching
/// one content stream, regardless of how few bytes it decodes to.
///
/// Charging a stream only for its decoded length undercounts it: resolving the
/// object and calling `decompressed_content` costs the same whether the result
/// is one byte or none. A `/Contents` array of many *zero-length* streams —
/// itself shareable across pages by one indirect reference — would otherwise
/// draw the budget down by ~1 byte per stream, so a 1.8 MB file could keep
/// `inspect` busy for tens of seconds: measured, 300k such refs across 320 pages
/// took 9.9 s and grew linearly with the page count.
///
/// 512 bytes puts the document-wide ceiling at ~262k streams, which real
/// documents are nowhere near — a page carries one content stream, or a few
/// dozen at worst — while keeping the pathological case in the tens of
/// milliseconds.
pub(crate) const PDF_CONTENT_STREAM_COST_FLOOR_BYTES: usize = 512;

/// Whole-document budget on how many decoded content-stream *operations*
/// [`inspect::inspect`] will walk, across all pages, in each of its two passes.
///
/// Sibling of [`MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES`], which bounds decoding by
/// input bytes. Both are needed because they bound different work and neither
/// implies the other:
///
/// - Bytes alone cannot bound operator work, because work per byte varies by
///   ~285×: a `q` is 2 bytes and ~570 bytes of operation list. 128 MiB of bare
///   `q` is ~67M operations, measured at ~26 s — bounded, but a poor bound.
/// - Operations alone cannot bound decompression work, because a page can
///   decompress a full [`MAX_PDF_CONTENT_BYTES`] of *whitespace* and decode to
///   zero operations, paying nothing, and repeat that per page.
///
/// Charged after a page's operations are decoded, so the page whose decode was
/// already paid for is still extracted and the next page is refused. Overshoot
/// is therefore bounded by one page's operation count.
///
/// 20M operations is far above realistic content — a text-heavy page runs to a
/// few thousand — so the ~2,500-page scale the other bounds target is roughly an
/// order of magnitude below this. It caps adversarial operator work at ~8 s.
pub(crate) const MAX_PDF_INSPECT_OPERATIONS: usize = 20_000_000;

/// Whole-document budget on the total bytes of extracted text that
/// [`inspect::inspect`] will retain in [`inspect::InspectResult::text_items`].
///
/// [`MAX_PDF_INSPECT_ITEMS`] counts records but not their payload, and a single
/// `Tj` operand can be nearly as large as a whole page's content allowance. One
/// content stream holding one ~900 KB string, shared by many page objects,
/// therefore yields one very large record per page: measured, 500 pages is
/// 67 KB on disk, 869 MB of peak RSS and 450 MB of JSON, and it scales with the
/// page count up to the item cap.
///
/// Extraction stops once the budget is spent. The overshoot is bounded by the
/// last record pushed, itself bounded by [`MAX_PDF_CONTENT_BYTES`].
///
/// 64 MiB is far above realistic content — the corpus averages ~80 records per
/// page of a few dozen bytes each, so the ~2,500-page document
/// [`MAX_PDF_INSPECT_ITEMS`] targets carries single-digit MB of text.
pub(crate) const MAX_PDF_INSPECT_TEXT_BYTES: usize = 64 * 1024 * 1024;

/// Upper bound on the length of each `/Info` metadata string
/// ([`inspect::Metadata::title`] and its siblings) that [`inspect::inspect`]
/// will retain and report.
///
/// The metadata pass has no per-page or per-record structure, so neither
/// [`MAX_PDF_INSPECT_TEXT_BYTES`] nor [`MAX_PDF_INSPECT_ITEMS`] constrains it —
/// they bound the text and image passes only. An `/Info` dictionary can live
/// inside a Flate-compressed object stream, so a small file can carry a `/Title`
/// of arbitrary size, and a metadata-only document would then produce output
/// proportional to the decompressed payload.
///
/// 8 KiB per field is far above anything a producer writes into a title, author
/// or date — the fields are a line of text each — and bounds all five at 40 KiB.
/// Longer values are truncated on a UTF-8 character boundary rather than
/// dropped, so the field stays usable.
pub(crate) const MAX_PDF_METADATA_FIELD_BYTES: usize = 8 * 1024;

/// Upper bound on the length of a `/Tf` font resource name that
/// [`inspect::inspect`] will retain and report.
///
/// Every extracted [`inspect::TextItem`] owns a copy of the font name in
/// effect, so the retained and serialised size of a result is
/// `items × name_len`, not `items` alone. [`MAX_PDF_CONTENT_BYTES`] does not
/// bound that product: it is a *per-page* budget, so a document can spend one
/// page's budget on a wide `/Tf` name followed by a run of 5-byte `(A) Tj`
/// operators and repeat that across pages, up to
/// [`MAX_PDF_INSPECT_ITEMS`] records each pairing with a name of nearly a
/// megabyte. Clamping the name bounds the product at ~25 MB.
///
/// 127 bytes is the architectural limit the PDF specification's implementation
/// notes give for a name object (ISO 32000-1 Annex C.2), so no name a
/// conforming producer emits is affected — fulgur's own are `F0`, `F1`, ….
/// Longer names are truncated on a UTF-8 character boundary rather than
/// rejected, which keeps a record's other fields usable.
pub(crate) const MAX_PDF_FONT_NAME_BYTES: usize = 127;

/// Upper bound on graphics-state (`q` / `Q`) nesting depth tracked by
/// [`inspect::inspect`] while walking a page content stream.
///
/// Each `q` saves a copy of the current graphics state, so an unbounded run of
/// `q` operators grows a stack that is proportional to the operator count. The
/// PDF specification's implementation notes put the historical limit at 28
/// levels and real generators nest a handful, so this bound is orders of
/// magnitude above any legitimate document. Sibling of
/// [`MAX_PDF_PARENT_DEPTH`].
///
/// A `q` beyond the bound is dropped and counted, so the matching `Q` unwinds
/// it instead of popping a real entry. The dropped frame's *contents* are
/// discarded with it: because the frame has no stack entry of its own, a state
/// change inside it would otherwise mutate the deepest retained entry and
/// survive the matching `Q`, leaking into the enclosing scope. Discarding is
/// what makes the outer state exactly restorable.
pub(crate) const MAX_PDF_GS_STACK_DEPTH: usize = 1024;

/// Upper bound on how many records [`inspect::inspect`] accumulates in each of
/// [`inspect::InspectResult::text_items`] and [`inspect::InspectResult::images`].
///
/// Page count is bounded only by input size, so a per-page cap would still
/// leave the aggregate result unbounded; this is a whole-document total. The
/// repository corpus averages ~80 text items per page, which puts this bound at
/// roughly 2,500 pages of realistic content, for ~70 MB of retained records.
/// Extraction stops once the bound is reached — the result is silently
/// truncated rather than erroring, matching the module's existing behaviour for
/// malformed structures.
pub(crate) const MAX_PDF_INSPECT_ITEMS: usize = 200_000;

/// Total-output budget for the generated CSS that
/// [`blitz_adapter::CounterPass`] injects for resolved pseudo-element
/// content. Once the accumulated generated CSS reaches this size, further
/// per-element rules are skipped (the pseudo-element simply does not render).
///
/// Each element with matching `::before` / `::after` counter content emits
/// its own generated rule, and the element (sibling) count is bounded only by
/// input size — so without a total cap the aggregate is input-proportional in
/// N with a per-rule constant, still a resource-exhaustion vector even after
/// [`MAX_COUNTER_CHAIN_BYTES`] bounds each single rule. This budget makes the
/// aggregate output absolutely bounded regardless of N, mirroring the
/// additive-not-multiplicative rationale of [`MAX_PAGES`]. Kept generous so no
/// realistic document is clipped (a document would need on the order of 10^5
/// counter-bearing pseudo-elements to reach it).
pub(crate) const MAX_GENERATED_CSS_BYTES: usize = 8 * 1024 * 1024;

/// Total-output budget for the resolved `bookmark-label` strings that
/// [`blitz_adapter::BookmarkPass`] accumulates into the PDF outline. Once the
/// aggregate resolved-label size reaches this bound, further outline entries
/// are skipped.
///
/// `bookmark-label: counters(name, sep)` resolves through the same
/// [`MAX_COUNTER_CHAIN_BYTES`]-capped path, so a single label can be up to
/// that cap while its originating element is only a few bytes — a per-element
/// amplification that plain text labels (whose bytes come from the input)
/// cannot produce. The outline is a separate sink from the generated CSS, so
/// [`MAX_GENERATED_CSS_BYTES`] does not bound it; this budget caps the
/// N-element accumulation there, mirroring that guard. Kept generous so no
/// realistic document (headings / figures with short labels) is clipped.
pub(crate) const MAX_OUTLINE_LABEL_BYTES: usize = 8 * 1024 * 1024;

/// Total-storage budget (in counter values) for the per-node counter-chain
/// snapshots that [`blitz_adapter::CounterPass`] harvests for `BookmarkPass` /
/// anchor resolution. Once the accumulated stored-value count reaches this
/// bound, no further node snapshots are recorded.
///
/// Snapshot recording clones the *full* active counter chain
/// (`chain_snapshot`) at every visited element. Sibling `counter-reset`s make
/// the chain length ≈ the sibling index, so storing it at N nodes is O(N²)
/// memory — a resource-exhaustion sink independent of any output budget
/// (`MAX_GENERATED_CSS_BYTES` / `MAX_OUTLINE_LABEL_BYTES` bound output bytes,
/// not this retained storage). Per-node length is separately clamped to
/// [`MAX_COUNTER_CHAIN_ENTRIES`] inside `chain_snapshot` (lossless for realistic
/// counter state — nesting depth is bounded by [`MAX_DOM_DEPTH`], and anything
/// longer is adversarial sibling accumulation); this budget then bounds the
/// aggregate
/// across all nodes, mirroring the additive-not-multiplicative rationale of
/// [`MAX_PAGES`]. ~8M values ≈ 32 MiB — far above any realistic document.
pub(crate) const MAX_COUNTER_SNAPSHOT_ENTRIES: usize = 8 * 1024 * 1024;

/// Total-storage budget (in bytes) for the per-node named-string snapshots that
/// [`blitz_adapter::StringSetPass`] harvests for `BookmarkPass` `string(name)`
/// resolution inside `bookmark-label`. Once the accumulated stored string bytes
/// reach this bound, no further node snapshots are recorded.
///
/// Snapshot recording clones the *full* running `name -> value` map at every
/// visited element, and the values are attacker-controlled strings resolved
/// from element text / attributes (`string-set: title content(text)`). A single
/// large value snapshotted at N nodes is O(N × value_size) retained memory — a
/// resource-exhaustion sink independent of any output budget, and the string
/// twin of the counter-chain [`MAX_COUNTER_SNAPSHOT_ENTRIES`] guard. This budget
/// bounds the aggregate across all nodes regardless of N, mirroring the
/// additive-not-multiplicative rationale of [`MAX_PAGES`]. Kept generous (8 MiB)
/// so no realistic bookmark-bearing document (short named strings × headings) is
/// clipped; adversarial input degrades `string()` to the CSS default ("") for
/// nodes past the cap, which is acceptable.
pub(crate) const MAX_STRING_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

/// Approximate per-record heap + struct overhead charged against the
/// [`MAX_STRING_SNAPSHOT_BYTES`] / [`MAX_STRING_SET_STORE_BYTES`] budgets in
/// addition to each record's `name` + `value` payload bytes.
///
/// Both budgets otherwise counted only string payload, so empty / tiny named
/// strings (`p { string-set: x "" }` matched by N elements, or a per-node
/// snapshot recorded at every visited element while the running map is still
/// empty) accumulated near-zero *counted* bytes while retaining millions of
/// `StringSetEntry` / `BTreeMap` records — each of which costs `String` headers,
/// a node id, and B-tree / `Vec` slot overhead that dwarfs a zero-length value.
/// Charging this per-record constant makes the byte budget bound the record
/// *count* as well (ceiling ≈ `budget / this`, ~131 K records at 8 MiB), matching
/// the entry-count guard of the sibling [`MAX_COUNTER_SNAPSHOT_ENTRIES`]. Sized
/// to conservatively cover two `String` structs (24 B each) plus a node id and
/// container slot; realistic bookmark documents stay far below the ceiling.
pub(crate) const STRING_ENTRY_OVERHEAD_BYTES: usize = 64;

/// Total-storage budget (in bytes) for the resolved `string-set` values that
/// [`gcpm::string_set::StringSetStore`] accumulates during
/// [`blitz_adapter::StringSetPass`]. Once the accumulated stored value bytes
/// would exceed this bound, further assignments are dropped.
///
/// Unlike the [`MAX_STRING_SNAPSHOT_BYTES`] snapshot (a bookmark-only aux gated
/// on `record_node_snapshots`), the store is populated *unconditionally* on the
/// default render path — one entry per (element, matching `string-set` rule) —
/// and its values are cloned again downstream into the per-node render map
/// (`Engine::render_html`'s `string_set_by_node` / `string_set_for_render`). A
/// single repeated literal (`p { string-set: x "BIG" }` matched by N sibling
/// elements) pushes N copies of `BIG` from a single-copy input — an
/// O(N × value_size) breadth-unbounded amplification independent of any output
/// budget. Capping the aggregate at the source push bounds the downstream clones
/// dependently (peak ≈ 3× this budget). Kept generous (8 MiB) so no realistic
/// document (headings / figures with short named strings, even thousands of
/// them) is clipped; past the cap, `string()` reads the last recorded value —
/// a stale/absent-value degradation acceptable under adversarial input. Store
/// sibling of [`MAX_STRING_SNAPSHOT_BYTES`].
pub(crate) const MAX_STRING_SET_STORE_BYTES: usize = 8 * 1024 * 1024;

/// Per-call cap on the bytes materialized while resolving a single content /
/// label list (`::before` / `::after`, `bookmark-label`, margin-box running
/// content) into one string.
///
/// [`MAX_COUNTER_CHAIN_BYTES`] bounds a *single* `counters()` / `counter()` /
/// `target-counters()` item, but a content list may hold an unbounded number
/// of such items (`content: counters(x) counters(x) …` — the parser caps
/// neither item count nor list length). The aggregate output budgets
/// ([`MAX_GENERATED_CSS_BYTES`], [`MAX_OUTLINE_LABEL_BYTES`]) are checked once
/// per element *before* the list is resolved, so a single element could
/// otherwise materialize `item_count × MAX_COUNTER_CHAIN_BYTES` in one
/// allocation and overshoot them. This bounds that per-call materialization.
/// Kept generous — no realistic single element resolves anywhere near this —
/// so only adversarial multi-item lists are clipped.
pub(crate) const MAX_RESOLVED_CONTENT_BYTES: usize = 1024 * 1024;

pub mod asset;
pub mod background;
pub mod blitz_adapter;
pub mod column_css;
pub mod config;
pub mod convert;
pub mod draw_primitives;
#[doc(hidden)]
pub mod drawables;
pub mod engine;
pub mod error;
pub mod gcpm;
pub mod image;
pub mod inspect;
pub(crate) mod link;
#[doc(hidden)]
pub mod multicol_layout;
pub(crate) mod net;
pub mod outline;
#[doc(hidden)]
pub mod pagination_layout;
pub mod paragraph;
pub mod render;
pub mod schema;
pub mod svg;
pub mod tagging;
pub mod template;
pub mod units;

pub use asset::AssetBundle;
pub use config::{Config, ConfigBuilder, Margin, PageSize};
pub use engine::{Engine, EngineBuilder, LayoutOutput};
pub use error::{Error, Result};
pub use outline::build_outline;

/// Convert HTML to PDF with default settings.
pub fn convert_html(html: &str) -> Result<Vec<u8>> {
    let engine = Engine::builder().build();
    engine.render(html)
}

#[cfg(test)]
mod tests {
    use super::convert_html;

    /// `convert_html` is a thin public-API wrapper over `Engine::builder().build().render()`.
    /// These smoke tests ensure the function is reachable from lib-mode coverage and that
    /// the default-settings path produces valid PDF output for representative inputs.

    #[test]
    fn convert_html_basic_html_returns_pdf() {
        let pdf = convert_html("<html><body><h1>Hello, fulgur</h1><p>World</p></body></html>")
            .expect("convert_html failed on basic HTML");
        assert!(pdf.starts_with(b"%PDF"), "expected PDF header");
    }

    #[test]
    fn convert_html_empty_body_returns_pdf() {
        let pdf =
            convert_html("<html><body></body></html>").expect("convert_html failed on empty body");
        assert!(!pdf.is_empty(), "expected non-empty PDF for empty body");
    }

    #[test]
    fn convert_html_minimal_fragment_returns_pdf() {
        // A bare fragment without explicit <html>/<body> tags is valid HTML5.
        let pdf =
            convert_html("<h2>Section</h2><ul><li>item</li></ul>").expect("convert_html failed");
        assert!(pdf.starts_with(b"%PDF"));
    }
}
