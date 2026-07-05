// Shared test-support module: each integration-test binary that does
// `mod support;` pulls in the whole module but uses only a subset of these
// helpers, so unused items here are expected per-binary.
#![allow(dead_code)]

/// Counts of PDF content-stream operators.
/// Only tracks the operators we care about in border/text optimization work.
#[derive(Debug, Default, Clone)]
pub struct OpCounts {
    pub m: usize,
    pub l: usize,
    pub re: usize,
    pub s_stroke: usize,
    pub q: usize,
    pub bt: usize,
    pub rg_stroke: usize,
}

/// Decode every page's content stream with lopdf and count PDF operators.
///
/// krilla 0.8 emits object streams / compressed xref and FLATE-compresses
/// content streams (`compress_content_streams: true`) by default, so the
/// drawing operators are no longer present in the raw PDF bytes. lopdf
/// transparently decompresses both, so we decode each page's content stream
/// and tally only the operators we care about.
///
/// Returns `Option<OpCounts>` to preserve the pre-krilla-0.8 signature (call
/// sites treat `None` as "skip"). The lopdf path has no external binary
/// dependency, so this now always returns `Some`.
pub fn count_ops(pdf_bytes: &[u8]) -> Option<OpCounts> {
    let doc = lopdf::Document::load_mem(pdf_bytes).expect("load PDF for op counting");
    let mut c = OpCounts::default();
    for (_page_num, page_id) in doc.get_pages() {
        let content_bytes = doc
            .get_page_content(page_id)
            .expect("get decoded page content stream");
        let content =
            lopdf::content::Content::decode(&content_bytes).expect("parse decoded content stream");
        for op in &content.operations {
            match op.operator.as_str() {
                "m" => c.m += 1,
                "l" => c.l += 1,
                "re" => c.re += 1,
                // `S` = stroke path (uppercase); `RG` = stroke color (RGB).
                "S" => c.s_stroke += 1,
                "q" => c.q += 1,
                "BT" => c.bt += 1,
                "RG" => c.rg_stroke += 1,
                _ => {}
            }
        }
    }
    Some(c)
}

/// Extract the `f` (vertical translate) operand of every text matrix
/// (`a b c d e f Tm`) in `pdf_bytes`, in document order. Krilla emits one
/// `Tm` per text run, so each returned value is a text run's baseline y in
/// PDF user space.
///
/// Like [`count_ops`], this decodes each page's (FLATE-compressed on krilla
/// 0.8) content stream via lopdf. Returns `Option<Vec<f32>>` to preserve the
/// previous signature; the lopdf path always returns `Some`.
///
/// Used to assert vertical placement (e.g. that an end-side margin actually
/// offsets a `bottom:0` absolute element) without rasterizing.
pub fn text_matrix_ys(pdf_bytes: &[u8]) -> Option<Vec<f32>> {
    let doc = lopdf::Document::load_mem(pdf_bytes).expect("load PDF for text matrix scan");
    let mut ys = Vec::new();
    for (_page_num, page_id) in doc.get_pages() {
        let content_bytes = doc
            .get_page_content(page_id)
            .expect("get decoded page content stream");
        let content =
            lopdf::content::Content::decode(&content_bytes).expect("parse decoded content stream");
        for op in &content.operations {
            // `a b c d e f Tm` — the f operand is the 6th number. `as_float`
            // accepts both `Real` and `Integer` (a `0` operand is an integer).
            if op.operator == "Tm" && op.operands.len() == 6 {
                if let Ok(f) = op.operands[5].as_float() {
                    ys.push(f);
                }
            }
        }
    }
    Some(ys)
}
