//! Backend-independent building blocks shared by Fulgur's layout backends.
//!
//! This crate holds the parts of Fulgur that do not depend on a layout engine:
//! page/PDF configuration, the asset bundle, unit newtypes, the error type,
//! image format handling, and PDF inspection. Layout backends (currently
//! `fulgur-blitz`) build on it, and the `fulgur` facade crate re-exports it.
//!
//! It must not depend on Blitz, Stylo, Taffy, Parley, or Raikiri.

// MSRV 1.89 newly enables clippy's MSRV-gated `collapsible_if` suggestion
// for let-chains (stabilized in Rust 1.88). The modules moved here from the
// `fulgur` crate carry pre-existing nested if-let sites that were suppressed
// there crate-wide; keep the same suppression until the follow-up migration
// in fulgur-pt70 (see the matching note in fulgur-blitz's lib.rs).
#![allow(clippy::collapsible_if)]

pub mod asset;
pub mod config;
pub mod error;
pub mod image;
pub mod inspect;
pub mod units;

pub use asset::AssetBundle;
pub use config::{Config, ConfigBuilder, Margin, PageSize};
pub use error::{Error, Result};

/// Upper bound on how many `/Parent` references [`inspect::resolve_page_resources`]
/// will follow when looking up an inherited `Resources` dictionary on a PDF page.
///
/// Walking the parent chain unconditionally is unsafe on attacker-controlled PDFs
/// because a cyclic chain (`A -> B -> A`, or any longer cycle) would loop forever;
/// even a genuinely long non-cyclic chain would consume unbounded work. Real page
/// trees stay very shallow — Adobe's implementation notes recommend at most 7
/// levels, and typical documents nest 1–3 deep — so a fixed depth cap terminates
/// pathological inputs without ever rejecting a legitimate one. Sibling of the
/// `fulgur_blitz::MAX_DOM_DEPTH` defensive traversal bound.
#[doc(hidden)]
pub const MAX_PDF_PARENT_DEPTH: usize = 128;

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
#[doc(hidden)]
pub const MAX_PDF_CONTENT_BYTES: usize = 1024 * 1024;

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
#[doc(hidden)]
pub const MAX_PDF_INSPECT_CONTENT_TOTAL_BYTES: usize = 128 * 1024 * 1024;

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
#[doc(hidden)]
pub const PDF_CONTENT_STREAM_COST_FLOOR_BYTES: usize = 512;

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
#[doc(hidden)]
pub const MAX_PDF_INSPECT_OPERATIONS: usize = 20_000_000;

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
#[doc(hidden)]
pub const MAX_PDF_INSPECT_TEXT_BYTES: usize = 64 * 1024 * 1024;

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
#[doc(hidden)]
pub const MAX_PDF_METADATA_FIELD_BYTES: usize = 8 * 1024;

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
#[doc(hidden)]
pub const MAX_PDF_FONT_NAME_BYTES: usize = 127;

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
#[doc(hidden)]
pub const MAX_PDF_GS_STACK_DEPTH: usize = 1024;

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
#[doc(hidden)]
pub const MAX_PDF_INSPECT_ITEMS: usize = 200_000;

/// Approximate per-record heap + struct overhead charged against the
/// `MAX_STRING_SNAPSHOT_BYTES` / `MAX_STRING_SET_STORE_BYTES` budgets (in `fulgur-blitz`) in
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
/// the entry-count guard of the sibling `MAX_COUNTER_SNAPSHOT_ENTRIES`. Sized
/// to conservatively cover two `String` structs (24 B each) plus a node id and
/// container slot; realistic bookmark documents stay far below the ceiling.
#[doc(hidden)]
pub const STRING_ENTRY_OVERHEAD_BYTES: usize = 64;
