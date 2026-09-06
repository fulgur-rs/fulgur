//! AssetBundle for managing CSS, fonts, and images.

use crate::error::Error;
use crate::error::Result;
use std::collections::HashMap;
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

/// Collection of external assets (CSS, fonts, images) for PDF generation.
#[derive(Clone)]
pub struct AssetBundle {
    pub css: Vec<String>,
    pub fonts: Vec<Arc<Vec<u8>>>,
    pub images: HashMap<String, Arc<Vec<u8>>>,
    base_path_str: Option<String>,
    /// Running total of bytes accepted via [`Self::add_css`] /
    /// [`Self::add_css_file`], enforcing [`MAX_TOTAL_CSS_BYTES`]. `css` is
    /// `pub` for caller convenience, so a caller can mutate it directly
    /// (`push`, `remove`, `clear`) bypassing those two methods; see
    /// `css_synced_len` for how `try_push_css` detects and repairs that.
    css_total_bytes: usize,
    /// `self.css.len()` as of the last time `css_total_bytes` was known
    /// accurate. `try_push_css` compares this against the vec's current
    /// length on every call: a mismatch means the `pub` field was mutated
    /// directly since, so it recomputes `css_total_bytes` from `self.css`
    /// before charging the new entry. An attacker reachable only through
    /// `add_css`/`add_image` (not the pub fields themselves — e.g. the
    /// fulgur-wasm/binding path, and confirmed none of fulgur-wasm/pyfulgur/
    /// fulgur-ruby expose `css`/`images` to their callers at all) can never
    /// cause this mismatch, since every accepted push here updates both
    /// fields in lockstep; only trusted native-Rust embedder code with
    /// direct struct access, choosing to bypass the accessor methods, can
    /// trigger it — and each trigger costs O(current length) once rather
    /// than O(n) per call, so this doesn't reopen the entry-count-driven
    /// CPU sink the budget itself exists to close.
    ///
    /// Known residual gap (Codex review, PR #688): a length-preserving
    /// in-place edit through the `pub` field — `bundle.css[i] = new_value`,
    /// or mutating the `String` at that index directly — changes retained
    /// bytes without changing `self.css.len()`, so this check can't detect
    /// it and `css_total_bytes` goes stale until some *other* mutation
    /// changes the length. Not fixed: doing so would mean either giving up
    /// the length check's O(1)-amortized property (recomputing every call,
    /// reopening the CPU sink above) or making `css` non-`pub` (a breaking
    /// API change for a path that, per the confirmation above, no shipped
    /// binding can even reach). Direct index-assignment into `AssetBundle`
    /// fields is not a documented usage pattern; use `add_css`/`add_css_file`.
    css_synced_len: usize,
    /// Running total of `STRING_ENTRY_OVERHEAD_BYTES + key.len() +
    /// data.len()` for entries accepted via [`Self::add_image`] /
    /// [`Self::add_image_file`], enforcing [`MAX_TOTAL_IMAGE_BYTES`]. Same
    /// `pub`-field mutation caveat as `css_total_bytes`.
    image_total_bytes: usize,
    /// `self.images.len()` counterpart to `css_synced_len`, checked by
    /// `try_insert_image`. Same length-preserving-replacement residual gap:
    /// `bundle.images.insert(existing_key, bigger_value)` overwrites in
    /// place without changing `self.images.len()`, so it isn't detected
    /// either. Use `add_image`/`add_image_file` instead of mutating the map
    /// directly.
    image_synced_len: usize,
    /// Running total of `STRING_ENTRY_OVERHEAD_BYTES + font.len()` for
    /// entries accepted via [`Self::add_font_bytes`] / [`Self::add_font_file`],
    /// enforcing [`MAX_TOTAL_FONT_BYTES`]. Same `pub`-field mutation caveat
    /// as `css_total_bytes` (`fonts` is `pub`).
    font_total_bytes: usize,
    /// `self.fonts.len()` counterpart to `css_synced_len`, checked by
    /// `try_push_font`. Same length-preserving-replacement residual gap as
    /// `css_synced_len`/`image_synced_len`: `bundle.fonts[i] = bigger_value`
    /// changes retained bytes without changing `self.fonts.len()`. Use
    /// `add_font_bytes`/`add_font_file` instead of mutating the vec directly.
    font_synced_len: usize,
}

impl AssetBundle {
    pub fn new() -> Self {
        Self {
            css: Vec::new(),
            fonts: Vec::new(),
            images: HashMap::new(),
            base_path_str: None,
            css_total_bytes: 0,
            css_synced_len: 0,
            image_total_bytes: 0,
            image_synced_len: 0,
            font_total_bytes: 0,
            font_synced_len: 0,
        }
    }

    /// Register a stylesheet.
    ///
    /// `css` is attacker-controlled in any embedding that forwards
    /// tenant-supplied stylesheets (WASM/binding callers in particular —
    /// see fulgur-wasm's `Engine.add_css`). A single call exceeding
    /// [`MAX_CSS_BYTES`], or one that would push the bundle's aggregate
    /// registered CSS past [`MAX_TOTAL_CSS_BYTES`] (`combined_css`
    /// allocates a fresh `String` this large on every render), is dropped
    /// with a `log::warn!` rather than silently retained without bound.
    /// Realistic stylesheets are orders of magnitude below either cap.
    pub fn add_css(&mut self, css: impl Into<String>) {
        let css = css.into();
        if let Err(msg) = self.try_push_css(css) {
            log::warn!("add_css: {msg}");
        }
    }

    /// File-backed sibling of [`Self::add_css`]. Bounds the actual bytes
    /// read from disk (see [`read_file_capped`]), mirroring
    /// [`Self::add_font_file`].
    pub fn add_css_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let data = read_file_capped(path, MAX_CSS_BYTES, "CSS file")?;
        let css = String::from_utf8(data)
            .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        self.try_push_css(css).map_err(Error::Asset)
    }

    /// Shared cap-enforcing push used by [`Self::add_css`] and
    /// [`Self::add_css_file`]. `Err` carries a human-readable reason; the
    /// CSS is dropped in both cases (never partially applied).
    fn try_push_css(&mut self, mut css: String) -> std::result::Result<(), String> {
        if css.len() > MAX_CSS_BYTES {
            return Err(format!(
                "stylesheet exceeds {MAX_CSS_BYTES} byte limit ({} bytes); dropping",
                css.len()
            ));
        }
        // `css` is `pub`, so a caller can bypass this method and mutate it
        // directly (push / remove / clear). See `css_synced_len`'s doc for
        // why comparing lengths here is safe from an attacker-facing cost
        // standpoint: only external mutation of the pub field can trigger
        // the O(current length) recompute, not call volume through this
        // method alone.
        if self.css.len() != self.css_synced_len {
            self.css_total_bytes = self
                .css
                .iter()
                .map(|s| crate::STRING_ENTRY_OVERHEAD_BYTES + s.len())
                .sum();
            self.css_synced_len = self.css.len();
        }
        // Charging only `css.len()` let empty/tiny stylesheets accumulate
        // near-zero counted bytes while each still grows `self.css` by one
        // `String` entry (heap header + struct) and, at render time,
        // `combined_css()` inserts a `\n` separator between every pair of
        // entries — an uncounted O(entry count) cost. `STRING_ENTRY_OVERHEAD_BYTES`
        // (shared with the string-set store's identical gap, gcpm/string_set.rs)
        // makes the byte budget bound the entry count too.
        let charge = crate::STRING_ENTRY_OVERHEAD_BYTES + css.len();
        if self.css_total_bytes.saturating_add(charge) > MAX_TOTAL_CSS_BYTES {
            return Err(format!(
                "aggregate registered CSS would exceed {MAX_TOTAL_CSS_BYTES} byte budget; \
                 dropping {} bytes",
                css.len()
            ));
        }
        // Native Rust callers can hand in a `String` whose retained
        // allocation capacity is much larger than its length (e.g. built
        // with `String::with_capacity`); charging by `.len()` alone would
        // under-count the actual retained memory. `shrink_to_fit` is
        // best-effort (the allocator may keep a little slack) but brings
        // capacity back in line with what the budget is counting.
        css.shrink_to_fit();
        self.css_total_bytes += charge;
        self.css.push(css);
        self.css_synced_len = self.css.len();
        Ok(())
    }

    pub fn add_font_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        // Bounds the actual bytes read from disk (see `read_file_capped`) so
        // a malicious multi-gigabyte font cannot drive the process into OOM.
        // `MAX_DECODED_FONT_BYTES` is the generous upper bound used across
        // the font pipeline; WOFF2-specific input capping happens later in
        // `decode_woff2`.
        let data = read_file_capped(path, MAX_DECODED_FONT_BYTES, "font file")?;
        self.add_font_bytes(data)
    }

    /// バイト列からフォントを登録する。
    ///
    /// マジックバイトで形式を自動判定する:
    /// - TTF / OTF / TTC: そのまま登録
    /// - WOFF2: `woff2_patched` で TTF にデコードしてから登録
    /// - WOFF1: `Error::UnsupportedFontFormat` を返す（未対応）
    /// - その他: 警告ログを出してそのまま登録（caller が正しい形式を渡している可能性）
    ///
    /// `data` は attacker-controlled（fulgur-wasm の `Engine.add_font` が
    /// JS 呼び出し元から渡された `Uint8Array` をそのまま転送する）。WOFF2 は
    /// 既存の `decode_woff2` が入力/デコード後サイズ双方を
    /// [`MAX_WOFF2_INPUT_BYTES`]/[`MAX_DECODED_FONT_BYTES`] で bound するが、
    /// TTF/OTF/TTC/Unknown の生バイト列にはこの経路が通らず、以前は無制限に
    /// 保持されていた。単一エントリが [`MAX_DECODED_FONT_BYTES`] を超える、
    /// または bundle 全体の累積が [`MAX_TOTAL_FONT_BYTES`] を超える場合は
    /// `Err` を返しフォントを登録しない（CSS/image の silent-drop とは異なり、
    /// 既存の WOFF2 系エラーと同じ `Result` 契約に揃える）。
    pub fn add_font_bytes(&mut self, data: Vec<u8>) -> Result<()> {
        let decoded = match detect_font_format(&data) {
            FontFormat::Woff2 => {
                // Preflight the aggregate budget against the header's
                // declared `totalSfntSize` *before* paying `decode_woff2`'s
                // real cost (up to 64 MiB of brotli decompression). Without
                // this, a bundle already at/near MAX_TOTAL_FONT_BYTES still
                // pays the full decode for every rejected request — the
                // final size-accurate charge in `try_push_font` only runs
                // *after* decoding (coderabbit review, PR #697).
                // `declared_size` is a raw, unvalidated header field (up to
                // `u32::MAX`) at this point — `decode_woff2`'s own
                // MAX_DECODED_FONT_BYTES check hasn't run yet. On a 32-bit
                // `usize` target (wasm32, exactly what `Engine.add_font`
                // runs on) a bare `+` here can overflow and panic with
                // overflow checks enabled, turning an expected `Result::Err`
                // into a wasm trap for JS callers (Codex Review, PR #697).
                let declared_size = woff2_declared_sfnt_size(&data)?;
                self.check_font_budget(
                    crate::STRING_ENTRY_OVERHEAD_BYTES.saturating_add(declared_size),
                )?;
                decode_woff2(&data)?
            }
            FontFormat::Woff1 => {
                return Err(Error::UnsupportedFontFormat(
                    "WOFF1 is not supported; convert to WOFF2 or TTF/OTF".into(),
                ));
            }
            FontFormat::Unknown => {
                log::warn!("add_font_bytes: unknown font magic bytes; passing through as-is");
                data
            }
            FontFormat::Ttf | FontFormat::Otf | FontFormat::Ttc => data,
        };
        self.try_push_font(decoded)
    }

    /// Recompute-if-desynced-then-check against the aggregate font budget.
    /// Shared by [`Self::add_font_bytes`]'s pre-decode WOFF2 preflight
    /// (charged with the header's *declared* size, to fail fast before
    /// `decode_woff2`'s real CPU/temp-buffer cost — coderabbit review, PR
    /// #697) and [`Self::try_push_font`]'s final charge on the *actual*
    /// decoded size. Does not mutate `font_total_bytes` by `charge` itself —
    /// callers that accept the entry apply that separately once they're
    /// committed to pushing it.
    ///
    /// `fonts` is `pub`, so a caller can bypass the `add_font_*` methods and
    /// mutate it directly (push / remove / clear). Same length-mismatch
    /// recompute as `try_push_css`/`try_insert_image` — see
    /// `css_synced_len`'s doc for why this is safe from an attacker-facing
    /// cost standpoint.
    fn check_font_budget(&mut self, charge: usize) -> Result<()> {
        if self.fonts.len() != self.font_synced_len {
            self.font_total_bytes = self
                .fonts
                .iter()
                .map(|f| crate::STRING_ENTRY_OVERHEAD_BYTES + f.len())
                .sum();
            self.font_synced_len = self.fonts.len();
        }
        if self.font_total_bytes.saturating_add(charge) > MAX_TOTAL_FONT_BYTES {
            return Err(Error::Asset(format!(
                "aggregate registered font bytes would exceed {MAX_TOTAL_FONT_BYTES} byte budget"
            )));
        }
        Ok(())
    }

    /// Shared cap-enforcing push used by [`Self::add_font_bytes`] (and, via
    /// it, [`Self::add_font_file`]). A single choke point so the WOFF2
    /// decoded output and the raw TTF/OTF/TTC/Unknown passthrough are both
    /// charged against the same per-entry and aggregate budgets.
    fn try_push_font(&mut self, mut decoded: Vec<u8>) -> Result<()> {
        // Redundant for the WOFF2 branch (`decode_woff2` already enforces
        // this internally) but the only size gate for the raw
        // TTF/OTF/TTC/Unknown passthrough, which previously had none.
        if decoded.len() > MAX_DECODED_FONT_BYTES {
            return Err(Error::Asset(format!(
                "font exceeds {MAX_DECODED_FONT_BYTES} byte limit ({} bytes)",
                decoded.len()
            )));
        }
        // Fixed per-entry overhead bounds the *count* too (repeatedly
        // calling `add_font` with small/near-empty payloads), not just
        // cumulative content bytes — same reasoning as `try_push_css`.
        let charge = crate::STRING_ENTRY_OVERHEAD_BYTES + decoded.len();
        self.check_font_budget(charge)?;
        // See `try_push_css`'s `shrink_to_fit` comment — same rationale for
        // native Rust callers passing an over-capacity `Vec<u8>`.
        decoded.shrink_to_fit();
        self.font_total_bytes += charge;
        self.fonts.push(Arc::new(decoded));
        self.font_synced_len = self.fonts.len();
        Ok(())
    }

    /// Set the base URL used by Blitz to resolve relative asset URLs.
    ///
    /// When Stylo resolves `content: url("icon.png")` against a real base URL
    /// like `file:///path/to/project/`, the computed value is an absolute URL.
    /// `extract_asset_name` strips the `file:///` prefix, leaving an absolute
    /// path (`path/to/project/icon.png`). This method records the stripped
    /// base path so `get_image` can strip it too and look up the relative name.
    ///
    /// Call this with the `file://` directory URL (trailing slash required).
    /// Example: `"file:///home/user/project/examples/"`.
    pub fn set_base_url(&mut self, url: &str) {
        let stripped = url.strip_prefix("file:///").unwrap_or(url);
        self.base_path_str = if stripped.is_empty() {
            None
        } else {
            Some(stripped.to_string())
        };
    }

    /// Strip a leading `./` from an image key. Single source of truth for
    /// the rule used by `add_image`, `add_image_file`, and `get_image`.
    fn normalize_key(key: &str) -> &str {
        key.strip_prefix("./").unwrap_or(key)
    }

    /// Register an image asset.
    ///
    /// `name` and `data` are both attacker-controlled in any embedding that
    /// forwards tenant-supplied images (WASM/binding callers in particular —
    /// see fulgur-wasm's `Engine.add_image`). Raster decode itself is bounded
    /// by the downstream decoders' own defaults (png: 64 MiB allocation
    /// limit, gif: 50 MB memory limit, krilla's zune-jpeg: 16384×16384 max
    /// dimensions), but the *raw, undecoded* bytes handed to this method
    /// were retained with no bound at all. A single call with a `name`
    /// exceeding [`MAX_IMAGE_KEY_BYTES`] or `data` exceeding
    /// [`MAX_IMAGE_BYTES`], or one that would push the bundle's aggregate
    /// registered image bytes past [`MAX_TOTAL_IMAGE_BYTES`], is dropped
    /// with a `log::warn!` instead. Realistic images (and their names) are
    /// orders of magnitude below any of these caps.
    pub fn add_image(&mut self, name: impl Into<String>, data: Vec<u8>) {
        let name = name.into();
        // Check the *borrowed* normalized key against MAX_IMAGE_KEY_BYTES
        // before allocating an owned copy of it (`to_string()` below) — a
        // deliberately huge `name` would otherwise cost that allocation
        // even though it's about to be rejected anyway.
        let key = Self::normalize_key(&name);
        if key.len() > MAX_IMAGE_KEY_BYTES {
            log::warn!(
                "add_image: image name exceeds {MAX_IMAGE_KEY_BYTES} byte limit \
                 ({} bytes); dropping",
                key.len()
            );
            return;
        }
        let key = key.to_string();
        if let Err(msg) = self.try_insert_image(key, data) {
            log::warn!("add_image: {msg}");
        }
    }

    /// File-backed sibling of [`Self::add_image`]. Bounds the actual bytes
    /// read from disk (see [`read_file_capped`]), mirroring
    /// [`Self::add_font_file`].
    pub fn add_image_file(
        &mut self,
        name: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> Result<()> {
        let name = name.into();
        // Same borrowed-key-first check as `add_image`, and checked before
        // the file read too: a huge `name` shouldn't cost either the
        // to-owned allocation or the (now-bounded, but still real) I/O of
        // reading the file, when the request is going to be rejected on the
        // name alone regardless of what the file contains.
        let key = Self::normalize_key(&name);
        if key.len() > MAX_IMAGE_KEY_BYTES {
            return Err(Error::Asset(format!(
                "image name exceeds {MAX_IMAGE_KEY_BYTES} byte limit ({} bytes); dropping",
                key.len()
            )));
        }
        let key = key.to_string();
        let path = path.as_ref();
        let data = read_file_capped(path, MAX_IMAGE_BYTES, "image file")?;
        self.try_insert_image(key, data).map_err(Error::Asset)
    }

    /// Shared cap-enforcing insert used by [`Self::add_image`] and
    /// [`Self::add_image_file`]. Charges `STRING_ENTRY_OVERHEAD_BYTES +
    /// key.len() + data.len()` against the aggregate budget: the fixed
    /// overhead bounds the *entry count* (many distinct tiny/empty-payload
    /// images each also retain a `String` key struct, an `Arc<Vec<u8>>`
    /// allocation, and `HashMap` bucket overhead that a content-only charge
    /// misses entirely), while `key.len() + data.len()` bounds the *content*.
    /// Overwriting an existing key first credits back that entry's charge,
    /// so repeated re-registration of the same key (a legitimate "replace
    /// this image" use) can't leak budget over time.
    ///
    /// Precondition: `key.len() <= MAX_IMAGE_KEY_BYTES`. Both callers check
    /// this against the *borrowed* key before constructing the owned `key`
    /// passed in here (so a huge name is rejected without the extra
    /// allocation), which is also why this function itself never
    /// `Debug`-formats an unbounded `key` into a rejection message.
    fn try_insert_image(
        &mut self,
        key: String,
        mut data: Vec<u8>,
    ) -> std::result::Result<(), String> {
        debug_assert!(key.len() <= MAX_IMAGE_KEY_BYTES);
        if data.len() > MAX_IMAGE_BYTES {
            return Err(format!(
                "image {key:?} exceeds {MAX_IMAGE_BYTES} byte limit ({} bytes); dropping",
                data.len()
            ));
        }
        // `images` is `pub`, so a caller can bypass this method and mutate
        // it directly (insert / remove / clear). Same length-mismatch
        // recompute as `try_push_css` — see `css_synced_len`'s doc for why
        // this is safe from an attacker-facing cost standpoint.
        if self.images.len() != self.image_synced_len {
            self.image_total_bytes = self
                .images
                .iter()
                .map(|(k, v)| crate::STRING_ENTRY_OVERHEAD_BYTES + k.len() + v.len())
                .sum();
            self.image_synced_len = self.images.len();
        }
        let charge = crate::STRING_ENTRY_OVERHEAD_BYTES + key.len() + data.len();
        let existing_charge = self
            .images
            .get(&key)
            .map(|old| crate::STRING_ENTRY_OVERHEAD_BYTES + key.len() + old.len())
            .unwrap_or(0);
        let projected = self
            .image_total_bytes
            .saturating_sub(existing_charge)
            .saturating_add(charge);
        if projected > MAX_TOTAL_IMAGE_BYTES {
            return Err(format!(
                "aggregate registered image bytes would exceed {MAX_TOTAL_IMAGE_BYTES} byte \
                 budget; dropping {key:?}"
            ));
        }
        // See `try_push_css`'s `shrink_to_fit` comment — same rationale for
        // native Rust callers passing an over-capacity `Vec<u8>`.
        data.shrink_to_fit();
        self.image_total_bytes = projected;
        self.images.insert(key, Arc::new(data));
        self.image_synced_len = self.images.len();
        Ok(())
    }

    pub fn get_image(&self, name: &str) -> Option<&Arc<Vec<u8>>> {
        let key = Self::normalize_key(name);
        if let result @ Some(_) = self.images.get(key) {
            return result;
        }
        // When the URL resolver hands `get_image` an absolute file path
        // (Stylo resolves `content: url("icon.png")` against the engine's
        // base URL), strip the recorded base prefix to recover the relative
        // asset name.
        if let Some(base) = &self.base_path_str
            && let Some(rel) = key.strip_prefix(base.as_str())
        {
            return self.images.get(Self::normalize_key(rel));
        }
        None
    }

    /// Build combined CSS from all added stylesheets.
    pub fn combined_css(&self) -> String {
        self.css.join("\n")
    }
}

impl Default for AssetBundle {
    fn default() -> Self {
        Self::new()
    }
}

/// Font container format detected from magic bytes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FontFormat {
    Ttf,
    Otf,
    Ttc,
    Woff1,
    Woff2,
    Unknown,
}

/// Detect a font container format from the first four bytes.
///
/// Recognizes TrueType (`0x00010000`, `true`, `typ1`), OpenType (`OTTO`),
/// TrueType Collection (`ttcf`), WOFF (`wOFF`), and WOFF2 (`wOF2`) magic
/// sequences. Returns `FontFormat::Unknown` for anything else, including
/// inputs shorter than four bytes.
pub(crate) fn detect_font_format(bytes: &[u8]) -> FontFormat {
    match bytes.get(0..4) {
        Some(b"wOF2") => FontFormat::Woff2,
        Some(b"wOFF") => FontFormat::Woff1,
        Some(b"OTTO") => FontFormat::Otf,
        Some(b"ttcf") => FontFormat::Ttc,
        Some([0x00, 0x01, 0x00, 0x00]) => FontFormat::Ttf,
        Some(b"true") | Some(b"typ1") => FontFormat::Ttf,
        _ => FontFormat::Unknown,
    }
}

/// Read `path` up to `max_bytes` (+1, to distinguish "exactly at the cap"
/// from "over it") through a capped reader, shared by
/// [`AssetBundle::add_css_file`], [`AssetBundle::add_image_file`], and
/// [`AssetBundle::add_font_file`].
///
/// The prior implementation checked `fs::metadata(path)?.len()` and then
/// read the file separately — a TOCTOU/FIFO gap. `stat` commonly reports
/// length 0 for a named pipe (whose writer can then stream unbounded data
/// through the later read), and a regular file can grow or be replaced
/// between the check and the read. Reading through `Read::take` bounds the
/// bytes actually pulled off the file descriptor regardless of what
/// metadata claimed, so the cap holds even for changing or non-regular
/// files. Trade-off: an oversized *regular* file now costs up to
/// `max_bytes + 1` bytes of read (vs. a free `stat`-only rejection before)
/// to close that gap — bounded, not the unbounded read it replaces.
fn read_file_capped(path: &Path, max_bytes: usize, kind: &str) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut data = Vec::new();
    file.take(max_bytes as u64 + 1).read_to_end(&mut data)?;
    if data.len() > max_bytes {
        return Err(Error::Asset(format!(
            "{kind} {} exceeds {max_bytes} byte limit",
            path.display()
        )));
    }
    Ok(data)
}

/// Upper bound on accepted WOFF2 input size (32 MiB). Rejects obviously
/// oversized or adversarial payloads before invoking the brotli decoder,
/// limiting decompression-bomb exposure.
const MAX_WOFF2_INPUT_BYTES: usize = 32 * 1024 * 1024;

/// Upper bound on accepted decompressed TTF/OTF output size (64 MiB).
/// A single real-world font family tops out well below this; anything larger
/// is likely a decompression bomb. Also enforced, via [`AssetBundle::try_push_font`],
/// as the per-call cap on raw TTF/OTF/TTC/Unknown bytes passed to
/// [`AssetBundle::add_font_bytes`] (that path has no decode step of its own).
const MAX_DECODED_FONT_BYTES: usize = 64 * 1024 * 1024;

/// Aggregate ceiling across every font registered on one bundle (256 MiB),
/// matching [`MAX_TOTAL_IMAGE_BYTES`]. Generous enough for a report embedding
/// several full font families with multiple weights/styles, but bounds the
/// case of many separately-reasonable `add_font_bytes`/`add_font_file` calls
/// (or a compromised tenant issuing many of them via fulgur-wasm's
/// `Engine.add_font`) accumulating without limit. Render time clones every
/// registered font into a `fontique::Blob` (`blitz_adapter::apply_passes`),
/// so this also bounds that per-render amplification.
const MAX_TOTAL_FONT_BYTES: usize = 256 * 1024 * 1024;

/// Upper bound on a single [`AssetBundle::add_css`] / [`AssetBundle::add_css_file`]
/// call (16 MiB). A real-world stylesheet is KBs to low MBs; this is
/// generous headroom, not a realistic ceiling.
const MAX_CSS_BYTES: usize = 16 * 1024 * 1024;

/// Aggregate ceiling across every stylesheet registered on one bundle
/// (64 MiB). `AssetBundle::combined_css` allocates a fresh `String` this
/// large on every render, so this also bounds that per-render allocation.
const MAX_TOTAL_CSS_BYTES: usize = 64 * 1024 * 1024;

/// Upper bound on a single [`AssetBundle::add_image`] / [`AssetBundle::add_image_file`]
/// call (64 MiB), matching [`MAX_DECODED_FONT_BYTES`]. This bounds the raw,
/// undecoded bytes retained by the bundle — decode-time memory is separately
/// bounded by the downstream decoders' own defaults (see `add_image`'s doc
/// comment).
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// Upper bound on an image's registration key/name (4 KiB), checked before
/// any `data` size or budget logic. `name` in [`AssetBundle::add_image`] /
/// [`AssetBundle::add_image_file`] is attacker-controlled with no length
/// limit of its own — the WASM entry point forwards it straight from the JS
/// caller's string — and, unlike `data`, the raw key length was previously
/// unbounded even though rejection messages elsewhere `Debug`-format it.
/// Generous: real asset keys are short relative paths/filenames.
const MAX_IMAGE_KEY_BYTES: usize = 4 * 1024;

/// Aggregate ceiling across every image registered on one bundle (256 MiB).
/// Generous enough for a report embedding many photos/logos, but bounds the
/// case of many separately-reasonable `add_image` calls (or a compromised
/// tenant issuing many of them) accumulating without limit.
const MAX_TOTAL_IMAGE_BYTES: usize = 256 * 1024 * 1024;

/// Validate a WOFF2 byte stream's input-size cap and header, returning the
/// header's declared `totalSfntSize` (bytes 16-19, big-endian u32; see
/// <https://www.w3.org/TR/WOFF2/#woff20Header>) without running the decoder.
/// Shared by [`decode_woff2`] and [`AssetBundle::add_font_bytes`]'s
/// pre-decode aggregate-budget preflight, so the cheap header check runs
/// before `decode_woff2`'s real (up to 64 MiB brotli) decode cost.
fn woff2_declared_sfnt_size(data: &[u8]) -> Result<usize> {
    if data.len() > MAX_WOFF2_INPUT_BYTES {
        return Err(Error::WoffDecode(format!(
            "WOFF2 input exceeds {MAX_WOFF2_INPUT_BYTES} byte limit (got {} bytes)",
            data.len()
        )));
    }
    if data.len() < 20 {
        return Err(Error::WoffDecode(format!(
            "WOFF2 header truncated: expected >= 20 bytes, got {}",
            data.len()
        )));
    }
    Ok(u32::from_be_bytes([data[16], data[17], data[18], data[19]]) as usize)
}

/// Decode a WOFF2 byte stream into an uncompressed TTF/OTF font.
///
/// Three layered defenses against decompression-bomb inputs, since
/// `woff2_patched` itself caps neither input nor output:
///
/// 1. `MAX_WOFF2_INPUT_BYTES` rejects oversized compressed inputs up front
///    (via [`woff2_declared_sfnt_size`]).
/// 2. The WOFF2 header's declared `totalSfntSize` is checked *before*
///    invoking brotli so an adversarial header declaring a huge output
///    cannot drive the decoder into OOM.
/// 3. `MAX_DECODED_FONT_BYTES` is re-checked after decode as a belt-and-
///    suspenders guard against a liar header.
fn decode_woff2(data: &[u8]) -> Result<Vec<u8>> {
    let total_sfnt_size = woff2_declared_sfnt_size(data)?;
    if total_sfnt_size > MAX_DECODED_FONT_BYTES {
        return Err(Error::WoffDecode(format!(
            "WOFF2 header declares uncompressed size {total_sfnt_size} which exceeds {MAX_DECODED_FONT_BYTES} byte limit"
        )));
    }
    let mut buf: &[u8] = data;
    let decoded = woff2_patched::decode::convert_woff2_to_ttf(&mut buf)
        .map_err(|e| Error::WoffDecode(format!("WOFF2 decode failed: {e:?}")))?;
    if decoded.len() > MAX_DECODED_FONT_BYTES {
        return Err(Error::WoffDecode(format!(
            "WOFF2 decoded output exceeds {MAX_DECODED_FONT_BYTES} byte limit (got {} bytes)",
            decoded.len()
        )));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards *every* test below that allocates or reads a production-sized
    /// (tens to ~320 MiB transiently) CSS/image/font buffer — both the
    /// aggregate-budget-boundary tests and the individual oversized-single-
    /// entry / oversized-file tests (the latter now genuinely read that
    /// many bytes off disk too, since `read_file_capped` no longer trusts
    /// `fs::metadata` and actually reads up to the cap). Rust's default
    /// test harness runs tests in parallel, so without a *complete* set of
    /// callers here, an unguarded test can still run alongside a guarded
    /// one and the lock doesn't actually bound suite-wide peak memory —
    /// exactly the gap a follow-up Codex review found in an earlier,
    /// partial version of this lock (PR #688). Serializing all of them
    /// against this one caps the peak at roughly the single largest test
    /// instead of their sum, without touching production code or adding a
    /// dependency. When adding a new test that builds a `MAX_*_BYTES`-sized
    /// value (in memory or via a file read), take this lock too.
    static HEAVY_BUDGET_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_get_image_normalizes_dot_slash() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("logo.png", vec![1, 2, 3]);
        assert!(bundle.get_image("./logo.png").is_some());
        assert!(bundle.get_image("logo.png").is_some());
    }

    #[test]
    fn test_add_image_normalizes_dot_slash() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("./logo.png", vec![1, 2, 3]);
        assert!(bundle.get_image("logo.png").is_some());
    }

    #[test]
    fn test_nested_dot_slash_preserved() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("images/./logo.png", vec![1, 2, 3]);
        assert!(bundle.get_image("images/./logo.png").is_some());
        assert!(bundle.get_image("logo.png").is_none());
    }

    #[test]
    fn test_detect_font_format_ttf() {
        assert_eq!(
            detect_font_format(&[0x00, 0x01, 0x00, 0x00, 0xFF]),
            FontFormat::Ttf
        );
    }

    #[test]
    fn test_detect_font_format_otf() {
        assert_eq!(detect_font_format(b"OTTO\x00\x00"), FontFormat::Otf);
    }

    #[test]
    fn test_detect_font_format_ttc() {
        assert_eq!(detect_font_format(b"ttcf\x00\x00"), FontFormat::Ttc);
    }

    #[test]
    fn test_detect_font_format_woff2() {
        assert_eq!(detect_font_format(b"wOF2\x00\x00"), FontFormat::Woff2);
    }

    #[test]
    fn test_detect_font_format_woff1() {
        assert_eq!(detect_font_format(b"wOFF\x00\x00"), FontFormat::Woff1);
    }

    #[test]
    fn test_detect_font_format_unknown() {
        assert_eq!(detect_font_format(b"XXXX"), FontFormat::Unknown);
        assert_eq!(detect_font_format(&[0x00]), FontFormat::Unknown);
        assert_eq!(detect_font_format(&[]), FontFormat::Unknown);
    }

    #[test]
    fn test_detect_font_format_old_mac_ttf() {
        assert_eq!(detect_font_format(b"true\x00\x00"), FontFormat::Ttf);
        assert_eq!(detect_font_format(b"typ1\x00\x00"), FontFormat::Ttf);
    }

    #[test]
    fn test_add_font_bytes_ttf_passthrough() {
        let mut bundle = AssetBundle::new();
        let mut data = vec![0x00, 0x01, 0x00, 0x00];
        data.extend_from_slice(&[0xAA; 100]);
        bundle
            .add_font_bytes(data.clone())
            .expect("should accept TTF");
        assert_eq!(bundle.fonts.len(), 1);
        assert_eq!(&bundle.fonts[0][..], &data[..]);
    }

    #[test]
    fn test_add_font_bytes_unknown_passthrough() {
        let mut bundle = AssetBundle::new();
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00];
        bundle
            .add_font_bytes(data.clone())
            .expect("unknown format should pass through");
        assert_eq!(bundle.fonts.len(), 1);
        assert_eq!(&bundle.fonts[0][..], &data[..]);
    }

    #[test]
    fn test_add_font_bytes_woff1_rejected() {
        use crate::error::Error;
        let mut bundle = AssetBundle::new();
        let data = b"wOFF\x00\x01\x00\x00".to_vec();
        let err = bundle
            .add_font_bytes(data)
            .expect_err("WOFF1 must be rejected");
        match err {
            Error::UnsupportedFontFormat(s) => assert!(s.contains("WOFF1"), "msg: {s}"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn test_add_font_bytes_woff2_decodes_to_ttf_or_otf() {
        let data = std::fs::read("tests/fixtures/fonts/NotoSans-Regular.woff2")
            .expect("fixture must exist");
        assert_eq!(detect_font_format(&data), FontFormat::Woff2);

        let mut bundle = AssetBundle::new();
        bundle.add_font_bytes(data).expect("WOFF2 should decode");
        assert_eq!(bundle.fonts.len(), 1);

        let decoded = &bundle.fonts[0];
        let magic = &decoded[0..4];
        assert!(
            magic == [0x00, 0x01, 0x00, 0x00] || magic == b"OTTO",
            "decoded magic should be TTF or OTF, got {magic:?}"
        );
    }

    #[test]
    fn test_add_font_bytes_woff2_invalid_returns_error() {
        use crate::error::Error;
        let mut bundle = AssetBundle::new();
        // bytes 16..20 (totalSfntSize) must declare a small, budget-passing
        // size so add_font_bytes's pre-decode aggregate-budget preflight
        // (PR #697) doesn't short-circuit before decode_woff2 ever runs —
        // this test's intent is specifically to exercise decode_woff2's own
        // WoffDecode rejection of invalid brotli data, not the preflight.
        let mut fake = b"wOF2".to_vec();
        fake.extend_from_slice(&[0u8; 12]); // bytes 4..16, unused by the header check
        fake.extend_from_slice(&100u32.to_be_bytes()); // bytes 16..20: totalSfntSize
        fake.extend_from_slice(b"garbagegarbagegarbage");
        let err = bundle
            .add_font_bytes(fake)
            .expect_err("bad WOFF2 must error");
        match err {
            Error::WoffDecode(_) => {}
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn test_add_font_bytes_woff2_input_size_cap() {
        let mut bundle = AssetBundle::new();
        let mut oversized = b"wOF2".to_vec();
        oversized.resize(MAX_WOFF2_INPUT_BYTES + 1, 0);
        let err = bundle
            .add_font_bytes(oversized)
            .expect_err("oversized WOFF2 must error before decoding");
        match err {
            Error::WoffDecode(msg) => assert!(msg.contains("limit"), "msg: {msg}"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn test_add_font_bytes_woff2_header_declares_oversized_output() {
        // Craft a minimal 20-byte WOFF2 header where totalSfntSize
        // (bytes 16..20, big-endian u32) declares an uncompressed size
        // that exceeds MAX_DECODED_FONT_BYTES. Must be rejected before
        // the decoder runs.
        let mut header = vec![0u8; 20];
        header[0..4].copy_from_slice(b"wOF2");
        let declared = (MAX_DECODED_FONT_BYTES as u64 + 1) as u32;
        header[16..20].copy_from_slice(&declared.to_be_bytes());
        let mut bundle = AssetBundle::new();
        let err = bundle
            .add_font_bytes(header)
            .expect_err("declared-oversized WOFF2 must be rejected");
        match err {
            Error::WoffDecode(msg) => {
                assert!(msg.contains("declares uncompressed size"), "msg: {msg}")
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn test_add_font_file_rejects_oversized_before_reading() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        use std::io::Seek;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        // Create a sparse file larger than the cap. `set_len` extends the
        // file on disk without actually allocating blocks, but
        // `read_file_capped` reads real (zeroed) bytes off it up to the
        // cap — this test now genuinely allocates ~MAX_DECODED_FONT_BYTES,
        // hence the lock above (Codex review, PR #688).
        let oversized = (MAX_DECODED_FONT_BYTES as u64) + 1;
        tmp.as_file_mut()
            .set_len(oversized)
            .expect("extend tempfile");
        tmp.as_file_mut().rewind().expect("rewind");
        let mut bundle = AssetBundle::new();
        let err = bundle
            .add_font_file(tmp.path())
            .expect_err("oversized font file must be rejected");
        match err {
            Error::Asset(msg) => assert!(msg.contains("limit"), "msg: {msg}"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }

    // --- Codex review on PR #688: read_file_capped TOCTOU/FIFO fix ---
    // The old `_file` methods checked `fs::metadata(path)?.len()` and then
    // read the file separately, which is a TOCTOU gap: a named pipe reports
    // length 0 via `stat` (its writer can then stream unbounded data through
    // the later read), and a regular file can grow or be replaced between
    // the check and the read. `read_file_capped` never calls `metadata` —
    // it bounds the actual bytes pulled off the file descriptor via
    // `Read::take`. These tests exercise that function directly with a
    // small `max_bytes` so the boundary is exact and the test is cheap; a
    // literal FIFO repro isn't used because named pipes aren't portable
    // across the Windows/macOS/Linux CI matrix, and the fix holds
    // structurally (no `metadata` call at all) rather than by demonstrating
    // one specific non-regular-file case.

    #[test]
    fn read_file_capped_accepts_data_exactly_at_the_cap() {
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.write_all(&[b'x'; 10]).expect("write");
        let data =
            read_file_capped(tmp.path(), 10, "test file").expect("exactly-at-cap must be accepted");
        assert_eq!(data.len(), 10);
    }

    #[test]
    fn read_file_capped_rejects_data_one_byte_over_the_cap() {
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.write_all(&[b'x'; 11]).expect("write");
        let err =
            read_file_capped(tmp.path(), 10, "test file").expect_err("over-cap must be rejected");
        match err {
            Error::Asset(msg) => assert!(msg.contains("limit"), "msg: {msg}"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn test_add_font_bytes_woff2_truncated_header() {
        let mut bundle = AssetBundle::new();
        // Only 8 bytes: long enough to pass the 4-byte magic detection
        // (FontFormat::Woff2) but too short to read totalSfntSize.
        let truncated = b"wOF2\x00\x00\x00\x00".to_vec();
        let err = bundle
            .add_font_bytes(truncated)
            .expect_err("truncated WOFF2 header must be rejected");
        match err {
            Error::WoffDecode(msg) => {
                assert!(msg.contains("header truncated"), "msg: {msg}")
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn clone_shares_font_arc() {
        use std::sync::Arc;
        let mut bundle = AssetBundle::new();
        let data = vec![0u8; 64];
        bundle.fonts.push(Arc::new(data));

        let cloned = bundle.clone();
        assert_eq!(bundle.fonts.len(), 1);
        assert_eq!(cloned.fonts.len(), 1);
        // Arc の共有を確認（同じヒープ上の Vec を指している）
        assert!(Arc::ptr_eq(&bundle.fonts[0], &cloned.fonts[0]));
    }

    #[test]
    fn get_image_resolves_absolute_file_url_when_base_url_set() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", vec![1, 2, 3]);
        bundle.set_base_url("file:///home/user/project/examples/demo/");
        // Stylo resolves url("icon.png") to an absolute file:// path when a real base URL is configured.
        // get_image must find the image by stripping the base prefix.
        let resolved = "home/user/project/examples/demo/icon.png";
        assert!(
            bundle.get_image(resolved).is_some(),
            "get_image must resolve the absolute path to icon.png when base_url is set"
        );
    }

    #[test]
    fn get_image_resolves_subdir_image_with_base_url() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("images/logo.png", vec![4, 5, 6]);
        bundle.set_base_url("file:///base/");
        // "images/logo.png" registered → lookup "base/images/logo.png" should work
        assert!(
            bundle.get_image("base/images/logo.png").is_some(),
            "get_image must find images/logo.png via base_url prefix stripping"
        );
    }

    #[test]
    fn get_image_direct_lookup_still_works_without_base_url() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", vec![1, 2, 3]);
        // Regression guard: short-name lookup must not break when set_base_url is not called.
        assert!(bundle.get_image("icon.png").is_some());
    }

    #[test]
    fn get_image_returns_none_when_base_url_mismatch() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", vec![1, 2, 3]);
        bundle.set_base_url("file:///other/path/");
        // Long path does NOT match the base → None
        assert!(bundle.get_image("home/user/project/icon.png").is_none());
    }

    #[test]
    fn add_css_file_reads_content_from_disk() {
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        use std::io::Write as _;
        write!(tmp, "body {{ color: red; }}").expect("write");
        let mut bundle = AssetBundle::new();
        bundle.add_css_file(tmp.path()).expect("add_css_file");
        assert_eq!(bundle.css, vec!["body { color: red; }"]);
    }

    #[test]
    fn add_font_file_accepts_valid_ttf_on_disk() {
        // Write a file whose first 4 bytes are TTF magic (0x00010000);
        // add_font_file reads it and passes it to add_font_bytes unchanged.
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        use std::io::Write as _;
        let mut data = vec![0x00u8, 0x01, 0x00, 0x00];
        data.extend_from_slice(&[0xAB; 32]);
        tmp.write_all(&data).expect("write");
        let mut bundle = AssetBundle::new();
        bundle
            .add_font_file(tmp.path())
            .expect("add_font_file should accept TTF");
        assert_eq!(bundle.fonts.len(), 1);
        assert_eq!(&bundle.fonts[0][..], &data[..]);
    }

    // --- Codex finding: unbounded raw font-byte registration ---
    // fulgur-wasm's Engine.add_font forwards JS-caller-controlled bytes
    // straight into AssetBundle::add_font_bytes. WOFF2 input/output was
    // already bounded by decode_woff2's own checks, but the raw
    // TTF/OTF/TTC/Unknown passthrough had no size cap of its own, and no
    // registration was charged against a bundle-wide aggregate — so a
    // single oversized raw font, or many small ones, could grow the bundle
    // (and the render-time clone of every entry, blitz_adapter.rs) without
    // bound. These tests assert the caps engage under adversarial input and
    // that ordinary fonts are unaffected.

    #[test]
    fn add_font_bytes_rejects_oversized_raw_ttf() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        let mut huge = vec![0x00u8, 0x01, 0x00, 0x00]; // TTF magic
        huge.resize(MAX_DECODED_FONT_BYTES + 1, 0xAA);
        let err = bundle
            .add_font_bytes(huge)
            .expect_err("oversized raw TTF must be rejected");
        match err {
            Error::Asset(msg) => assert!(msg.contains("limit"), "msg: {msg}"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert!(bundle.fonts.is_empty());
    }

    #[test]
    fn add_font_bytes_rejects_oversized_unknown_format() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        let huge = vec![0xDEu8; MAX_DECODED_FONT_BYTES + 1]; // no recognized magic
        assert_eq!(detect_font_format(&huge), FontFormat::Unknown);
        let err = bundle
            .add_font_bytes(huge)
            .expect_err("oversized unknown-format payload must be rejected");
        match err {
            Error::Asset(msg) => assert!(msg.contains("limit"), "msg: {msg}"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert!(bundle.fonts.is_empty());
    }

    #[test]
    fn add_font_bytes_normal_font_is_unaffected() {
        let mut bundle = AssetBundle::new();
        let mut data = vec![0x00u8, 0x01, 0x00, 0x00];
        data.extend_from_slice(&[0xAB; 128]);
        bundle
            .add_font_bytes(data.clone())
            .expect("ordinary TTF should be accepted");
        assert_eq!(bundle.fonts.len(), 1);
        assert_eq!(&bundle.fonts[0][..], &data[..]);
    }

    #[test]
    fn add_font_bytes_drops_once_aggregate_budget_exceeded() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        // Largest single font that still fits the per-call cap.
        let mut chunk = vec![0x00u8, 0x01, 0x00, 0x00];
        chunk.resize(MAX_DECODED_FONT_BYTES, 0xAA);
        let max_possible_fits = MAX_TOTAL_FONT_BYTES / MAX_DECODED_FONT_BYTES + 1;
        let mut accepted = 0;
        for _ in 0..=max_possible_fits {
            match bundle.add_font_bytes(chunk.clone()) {
                Ok(()) => accepted += 1,
                Err(_) => {
                    assert!(
                        accepted >= max_possible_fits.saturating_sub(2),
                        "aggregate budget must accept close to MAX_TOTAL_FONT_BYTES worth of \
                         fonts before rejecting (accepted {accepted}, expected near \
                         {max_possible_fits})"
                    );
                    return;
                }
            }
        }
        panic!(
            "aggregate font budget never rejected a push after {} fonts \
             ({} bytes each) — the cap did not engage",
            max_possible_fits + 1,
            MAX_DECODED_FONT_BYTES
        );
    }

    #[test]
    fn add_font_bytes_rejects_woff2_by_declared_size_before_decoding() {
        // coderabbit review (PR #697): decode_woff2 used to run *before* the
        // aggregate-budget check, so a bundle already at/near
        // MAX_TOTAL_FONT_BYTES still paid the full brotli decode cost for
        // every rejected request. add_font_bytes now preflights the budget
        // against the WOFF2 header's declared totalSfntSize first.
        //
        // To prove the preflight fires *before* decode_woff2 runs (rather
        // than just checking the end-to-end Err), the payload's post-header
        // bytes are garbage — not valid brotli. If decode_woff2 actually
        // ran, `woff2_patched::decode::convert_woff2_to_ttf` would fail and
        // surface as `Error::WoffDecode("WOFF2 decode failed...")`. Getting
        // `Error::Asset` (the aggregate-budget message) instead is only
        // possible if the preflight rejected the request first.
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        let mut chunk = vec![0x00u8, 0x01, 0x00, 0x00];
        chunk.resize(MAX_DECODED_FONT_BYTES, 0xAA);
        let max_attempts = MAX_TOTAL_FONT_BYTES / MAX_DECODED_FONT_BYTES + 1;
        for _ in 0..max_attempts {
            // Fill until the aggregate budget has little headroom left (see
            // add_font_bytes_drops_once_aggregate_budget_exceeded — this
            // loop count always leaves remaining budget < MAX_DECODED_FONT_BYTES).
            let _ = bundle.add_font_bytes(chunk.clone());
        }

        let len_before = bundle.fonts.len();

        let mut fake_woff2 = b"wOF2".to_vec();
        fake_woff2.extend_from_slice(&[0u8; 12]); // bytes 4..16, unused by the header check
        let declared = MAX_DECODED_FONT_BYTES as u32; // remaining headroom is well under this
        fake_woff2.extend_from_slice(&declared.to_be_bytes()); // bytes 16..20: totalSfntSize
        fake_woff2.extend_from_slice(b"not valid brotli data at all, would fail to decode");

        let err = bundle
            .add_font_bytes(fake_woff2)
            .expect_err("declared size over remaining budget must be rejected");
        match err {
            Error::Asset(msg) => assert!(
                msg.contains("budget"),
                "must be the aggregate-budget preflight error: {msg}"
            ),
            other => panic!(
                "expected Error::Asset from the pre-decode budget preflight, got {other:?} — \
                 a WoffDecode error here would mean decode_woff2 ran on garbage bytes, i.e. \
                 the preflight didn't fire before the expensive decode"
            ),
        }
        assert_eq!(
            bundle.fonts.len(),
            len_before,
            "the garbage WOFF2 payload must not have been registered"
        );
    }

    #[test]
    fn add_font_bytes_woff2_near_u32_max_declared_size_does_not_panic() {
        // Codex review (PR #697): the preflight charge computation
        // (`STRING_ENTRY_OVERHEAD_BYTES + declared_size`) used a bare `+`
        // against a raw, unvalidated `u32` header field. On a 32-bit `usize`
        // target (wasm32 — exactly what `Engine.add_font` runs on) a
        // declared_size near `u32::MAX` overflows that addition, which
        // panics with overflow checks enabled instead of returning a clean
        // `Result::Err`. This test can't reproduce the overflow itself on a
        // 64-bit host (`usize` is 64-bit here, so the sum never overflows),
        // but locks in that `saturating_add` — not a bare `+` — is used at
        // this call site, and that the near-`u32::MAX` boundary is rejected
        // cleanly rather than left to a future edit to regress silently.
        let mut bundle = AssetBundle::new();
        let mut fake = b"wOF2".to_vec();
        fake.extend_from_slice(&[0u8; 12]); // bytes 4..16, unused by the header check
        fake.extend_from_slice(&u32::MAX.to_be_bytes()); // bytes 16..20: totalSfntSize
        fake.extend_from_slice(b"irrelevant tail bytes");
        let err = bundle
            .add_font_bytes(fake)
            .expect_err("near-u32::MAX declared size must be rejected, not panic");
        match err {
            Error::Asset(msg) => assert!(msg.contains("budget"), "msg: {msg}"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn add_font_bytes_charges_overhead_for_tiny_fonts() {
        let mut bundle = AssetBundle::new();
        // Unknown-format tiny payloads (<4 bytes, no magic to match) also
        // cover the "repeatedly calling add_font" count vector — a fixed
        // per-entry overhead bounds count even when content is ~0 bytes each.
        bundle.add_font_bytes(vec![0xAAu8]).unwrap();
        bundle.add_font_bytes(vec![0xBBu8]).unwrap();
        assert_eq!(bundle.fonts.len(), 2);
        assert_eq!(
            bundle.font_total_bytes,
            2 * crate::STRING_ENTRY_OVERHEAD_BYTES + 1 + 1,
            "each tiny font must still be charged a fixed per-entry overhead \
             in addition to content bytes — otherwise entry count is unbounded"
        );
    }

    #[test]
    fn add_font_bytes_shrinks_retained_capacity_to_length() {
        let mut oversized_capacity: Vec<u8> = Vec::with_capacity(1024 * 1024);
        oversized_capacity.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        assert!(oversized_capacity.capacity() >= 1024 * 1024);
        let mut bundle = AssetBundle::new();
        bundle
            .add_font_bytes(oversized_capacity)
            .expect("should accept");
        assert!(
            bundle.fonts[0].capacity() < 1024 * 1024,
            "retained capacity must be shrunk toward the actual content length \
             (got {})",
            bundle.fonts[0].capacity()
        );
    }

    #[test]
    fn add_font_bytes_budget_resets_after_bundle_is_cleared_directly() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        let mut chunk = vec![0x00u8, 0x01, 0x00, 0x00];
        chunk.resize(MAX_DECODED_FONT_BYTES, 0xAA);
        let max_attempts = MAX_TOTAL_FONT_BYTES / MAX_DECODED_FONT_BYTES + 1;
        for _ in 0..max_attempts {
            let _ = bundle.add_font_bytes(chunk.clone());
        }
        let filled_len = bundle.fonts.len();
        assert!(
            filled_len < max_attempts,
            "at least one fill attempt must have been rejected"
        );

        assert!(bundle.add_font_bytes(chunk.clone()).is_err());
        assert_eq!(
            bundle.fonts.len(),
            filled_len,
            "budget must still be exhausted before the direct clear"
        );

        bundle.fonts.clear();
        bundle.add_font_bytes(vec![0x00, 0x01, 0x00, 0x00]).expect(
            "clearing the pub field directly must reset the tracked budget, \
                 not leave the bundle permanently stuck rejecting",
        );
        assert_eq!(bundle.fonts.len(), 1);
    }

    #[test]
    fn add_font_bytes_budget_recomputes_after_partial_removal() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        let mut chunk = vec![0x00u8, 0x01, 0x00, 0x00];
        chunk.resize(MAX_DECODED_FONT_BYTES, 0xAA);
        let max_attempts = MAX_TOTAL_FONT_BYTES / MAX_DECODED_FONT_BYTES + 1;
        for _ in 0..max_attempts {
            let _ = bundle.add_font_bytes(chunk.clone());
        }
        let filled_len = bundle.fonts.len();
        assert!(filled_len >= 1, "fill loop must accept at least one font");

        bundle.fonts.remove(0);
        assert_eq!(bundle.fonts.len(), filled_len - 1);

        bundle.add_font_bytes(chunk).unwrap_or_else(|e| {
            panic!(
                "removing one entry through the pub field must free budget for a new \
                 entry of the same size, proving the stale total was recomputed \
                 rather than staying stuck at the pre-removal ceiling: {e}"
            )
        });
        assert_eq!(bundle.fonts.len(), filled_len);
    }

    #[test]
    fn set_base_url_with_root_url_clears_base_path() {
        // "file:///" stripped of the prefix yields "" → base_path_str becomes None
        let mut bundle = AssetBundle::new();
        bundle.add_image("logo.png", vec![7, 8, 9]);
        bundle.set_base_url("file:///");
        // No prefix to strip, so direct lookup still works.
        assert!(bundle.get_image("logo.png").is_some());
        // A path that would only match via prefix stripping must NOT match.
        assert!(bundle.get_image("logo.png/extra").is_none());
    }

    #[test]
    fn add_image_file_reads_bytes_from_disk() {
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        use std::io::Write as _;
        let pixels = vec![0xFFu8, 0xD8, 0xFF, 0xE0]; // JPEG magic
        tmp.write_all(&pixels).expect("write");
        let mut bundle = AssetBundle::new();
        bundle
            .add_image_file("photo.jpg", tmp.path())
            .expect("add_image_file");
        let stored = bundle.get_image("photo.jpg").expect("should be present");
        assert_eq!(&stored[..], &pixels[..]);
    }

    // --- Codex finding: unbounded CSS/image asset registration ---
    // fulgur-wasm's Engine.add_css/add_image forward JS-caller-controlled
    // strings/bytes straight into AssetBundle with no cap. The same is true
    // of the native add_css/add_image API used directly (and via
    // pyfulgur/fulgur-ruby), so the fix lives here rather than in the WASM
    // shim. These tests assert the caps actually engage under adversarial
    // input, and that ordinary small assets are unaffected.

    #[test]
    fn add_css_drops_oversized_single_stylesheet() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        let huge = "a".repeat(MAX_CSS_BYTES + 1);
        bundle.add_css(huge);
        assert!(
            bundle.css.is_empty(),
            "oversized single stylesheet must be dropped, not stored"
        );
    }

    #[test]
    fn add_css_drops_once_aggregate_budget_exceeded() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        // Largest single chunk that still fits the per-call cap. Each
        // accepted entry also charges STRING_ENTRY_OVERHEAD_BYTES, so one
        // fewer full-size chunk fits than a naive total/per-item division
        // would suggest (same reasoning as the image aggregate test) —
        // push until one is rejected rather than assuming an exact count.
        let chunk = "a".repeat(MAX_CSS_BYTES);
        let max_possible_fits = MAX_TOTAL_CSS_BYTES / MAX_CSS_BYTES + 1;
        let mut accepted = 0;
        for _ in 0..=max_possible_fits {
            bundle.add_css(chunk.clone());
            if bundle.css.len() > accepted {
                accepted += 1;
            } else {
                assert!(
                    accepted >= max_possible_fits.saturating_sub(2),
                    "aggregate budget must accept close to MAX_TOTAL_CSS_BYTES worth of \
                     stylesheets before rejecting (accepted {accepted}, expected near \
                     {max_possible_fits})"
                );
                return;
            }
        }
        panic!(
            "aggregate CSS budget never rejected a push after {} chunks \
             ({} bytes each) — the cap did not engage",
            max_possible_fits + 1,
            MAX_CSS_BYTES
        );
    }

    #[test]
    fn add_css_normal_stylesheet_is_unaffected() {
        let mut bundle = AssetBundle::new();
        bundle.add_css("body { color: red; }");
        assert_eq!(bundle.css, vec!["body { color: red; }"]);
    }

    #[test]
    fn add_css_file_rejects_oversized_before_reading() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        use std::io::Seek;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let oversized = (MAX_CSS_BYTES as u64) + 1;
        tmp.as_file_mut()
            .set_len(oversized)
            .expect("extend tempfile");
        tmp.as_file_mut().rewind().expect("rewind");
        let mut bundle = AssetBundle::new();
        let err = bundle
            .add_css_file(tmp.path())
            .expect_err("oversized CSS file must be rejected");
        match err {
            Error::Asset(msg) => assert!(msg.contains("limit"), "msg: {msg}"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert!(bundle.css.is_empty());
    }

    #[test]
    fn add_image_drops_oversized_single_image() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        let huge = vec![0u8; MAX_IMAGE_BYTES + 1];
        bundle.add_image("big.png", huge);
        assert!(
            bundle.get_image("big.png").is_none(),
            "oversized single image must be dropped, not stored"
        );
    }

    #[test]
    fn add_image_drops_oversized_key_without_formatting_it_in_full() {
        // Codex review follow-up: `name` had no length cap of its own, so a
        // deliberately huge key (independent of `data` size) reached the
        // rejection paths below, which `Debug`-format `key` into the error
        // message — Debug-formatting an unbounded attacker-controlled
        // string is itself an unbounded allocation, happening precisely
        // while rejecting the request for being oversized. Use a key large
        // enough that formatting it in full would be a clear test failure
        // (via timeout/OOM in CI) if the cap regressed, while staying small
        // enough (~16 MiB) to run fast today.
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        let huge_key = "k".repeat(MAX_IMAGE_KEY_BYTES + (16 * 1024 * 1024));
        bundle.add_image(huge_key, vec![1, 2, 3]);
        assert!(
            bundle.images.is_empty(),
            "oversized key must be dropped, not stored"
        );
    }

    #[test]
    fn add_image_accepts_key_exactly_at_the_cap() {
        let mut bundle = AssetBundle::new();
        let key = "k".repeat(MAX_IMAGE_KEY_BYTES);
        bundle.add_image(key.clone(), vec![1, 2, 3]);
        assert!(bundle.get_image(&key).is_some());
    }

    #[test]
    fn add_image_file_rejects_oversized_key_before_touching_the_file() {
        // coderabbit review follow-up (PR #688): the key-length check must
        // run before add_image_file reads the file, not just before
        // try_insert_image is called. If it happened after the read, a
        // nonexistent path would surface as `Error::Io` (file not found)
        // before ever reaching the name check — using one here proves the
        // name is validated first, without needing to inspect I/O directly.
        let mut bundle = AssetBundle::new();
        let huge_key = "k".repeat(MAX_IMAGE_KEY_BYTES + 1);
        let err = bundle
            .add_image_file(huge_key, "/nonexistent/path/does/not/exist.png")
            .expect_err("oversized name must be rejected");
        match err {
            Error::Asset(msg) => assert!(msg.contains("limit"), "msg: {msg}"),
            other => {
                panic!("expected Error::Asset (name checked before file I/O), got {other:?}")
            }
        }
    }

    #[test]
    fn add_image_drops_once_aggregate_budget_exceeded() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        // Largest single image that still fits the per-call cap; register
        // distinct-keyed images (charge = key + data bytes) until the
        // aggregate budget rejects one, which must happen well before an
        // unbounded amount of data has been accepted.
        let chunk = vec![0u8; MAX_IMAGE_BYTES];
        let max_possible_fits = MAX_TOTAL_IMAGE_BYTES / MAX_IMAGE_BYTES + 1;
        let mut accepted = 0;
        for i in 0..=max_possible_fits {
            let key = format!("img{i}.bin");
            bundle.add_image(key.clone(), chunk.clone());
            if bundle.get_image(&key).is_some() {
                accepted += 1;
            } else {
                // Each accepted entry also charges its (small) key length, so
                // one fewer full-size chunk fits than a naive
                // total/per-item division would suggest — allow that slack
                // without allowing the cap to engage drastically early.
                assert!(
                    accepted >= max_possible_fits.saturating_sub(2),
                    "aggregate budget must accept close to MAX_TOTAL_IMAGE_BYTES worth of \
                     images before rejecting (accepted {accepted}, expected near \
                     {max_possible_fits})"
                );
                return;
            }
        }
        panic!(
            "aggregate image budget never rejected a push after {} images \
             ({} bytes each) — the cap did not engage",
            max_possible_fits + 1,
            MAX_IMAGE_BYTES
        );
    }

    #[test]
    fn add_image_overwriting_same_key_does_not_leak_budget() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        // Repeated re-registration of the *same* key is a legitimate
        // "replace this image" use. If the budget didn't credit back the
        // superseded entry, enough overwrites would eventually trip the
        // aggregate cap even though the bundle only ever holds one entry.
        let mut bundle = AssetBundle::new();
        let chunk = vec![0u8; MAX_IMAGE_BYTES / 2];
        for _ in 0..10 {
            bundle.add_image("logo.png", chunk.clone());
        }
        assert!(
            bundle.get_image("logo.png").is_some(),
            "repeated overwrite of one key must not exhaust the aggregate budget"
        );
    }

    #[test]
    fn add_image_normal_image_is_unaffected() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", vec![1, 2, 3]);
        assert_eq!(
            bundle.get_image("icon.png").map(|d| d.as_slice()),
            Some([1, 2, 3].as_slice())
        );
    }

    #[test]
    fn add_image_file_rejects_oversized_before_reading() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        use std::io::Seek;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let oversized = (MAX_IMAGE_BYTES as u64) + 1;
        tmp.as_file_mut()
            .set_len(oversized)
            .expect("extend tempfile");
        tmp.as_file_mut().rewind().expect("rewind");
        let mut bundle = AssetBundle::new();
        let err = bundle
            .add_image_file("big.png", tmp.path())
            .expect_err("oversized image file must be rejected");
        match err {
            Error::Asset(msg) => assert!(msg.contains("limit"), "msg: {msg}"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert!(bundle.get_image("big.png").is_none());
    }

    // --- Codex review on PR #688: budget desync + retained capacity ---

    #[test]
    fn add_css_budget_resets_after_bundle_is_cleared_directly() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        // `css` is `pub`, so a caller can reuse a bundle by clearing it
        // directly instead of going through the constructor methods. Before
        // this fix, `css_total_bytes` never noticed the clear and stayed at
        // the budget ceiling, so every following `add_css` was silently
        // dropped even though the bundle held nothing.
        let mut bundle = AssetBundle::new();
        let chunk = "a".repeat(MAX_CSS_BYTES);
        // Fill until one full-size chunk is rejected (per-entry overhead
        // means fewer than a naive total/per-item division fit — see
        // `add_css_drops_once_aggregate_budget_exceeded`), so a further
        // same-size chunk is a reliable "budget exhausted" probe. A tiny
        // probe wouldn't be: the coarse fill can leave nearly one whole
        // chunk of slack that a tiny item would still fit into.
        let max_attempts = MAX_TOTAL_CSS_BYTES / MAX_CSS_BYTES + 1;
        for _ in 0..max_attempts {
            bundle.add_css(chunk.clone());
        }
        let filled_len = bundle.css.len();
        assert!(
            filled_len < max_attempts,
            "at least one fill attempt must have been rejected"
        );

        bundle.add_css(chunk.clone());
        assert_eq!(
            bundle.css.len(),
            filled_len,
            "budget must still be exhausted before the direct clear"
        );

        bundle.css.clear();
        bundle.add_css("body { color: red; }");
        assert_eq!(
            bundle.css,
            vec!["body { color: red; }"],
            "clearing the pub field directly must reset the tracked budget, \
             not leave the bundle permanently stuck rejecting"
        );
    }

    #[test]
    fn add_image_budget_resets_after_bundle_is_cleared_directly() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        let chunk = vec![0u8; MAX_IMAGE_BYTES];
        // Fill until one full-size chunk is rejected (per-entry key
        // overhead means fewer than a naive total/per-item division fit —
        // see `add_image_drops_once_aggregate_budget_exceeded`), so a
        // further same-size chunk is a reliable "budget exhausted" probe.
        // A tiny probe wouldn't be: the coarse fill can leave nearly one
        // whole chunk of slack that a tiny item would still fit into.
        let max_attempts = MAX_TOTAL_IMAGE_BYTES / MAX_IMAGE_BYTES + 1;
        for i in 0..max_attempts {
            bundle.add_image(format!("img{i}.bin"), chunk.clone());
        }
        let probe_key = format!("img{}.bin", max_attempts - 1);
        assert!(
            bundle.get_image(&probe_key).is_none(),
            "the last fill attempt must have been rejected"
        );

        bundle.add_image(probe_key.clone(), chunk.clone());
        assert!(
            bundle.get_image(&probe_key).is_none(),
            "budget must still be exhausted before the direct clear"
        );

        bundle.images.clear();
        bundle.add_image(probe_key.clone(), chunk);
        assert!(
            bundle.get_image(&probe_key).is_some(),
            "clearing the pub field directly must reset the tracked budget"
        );
    }

    #[test]
    fn add_css_budget_recomputes_after_partial_removal() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        // Codex review follow-up: the earlier is_empty()-only reset only
        // caught a caller clearing the *entire* pub `css` field. Removing
        // just one entry (e.g. `bundle.css.remove(0)`) left the tracked
        // total stale and too high, permanently over-rejecting legitimate
        // reuse even though real remaining usage was well under budget.
        let mut bundle = AssetBundle::new();
        let chunk = "a".repeat(MAX_CSS_BYTES);
        let max_attempts = MAX_TOTAL_CSS_BYTES / MAX_CSS_BYTES + 1;
        for _ in 0..max_attempts {
            bundle.add_css(chunk.clone());
        }
        let filled_len = bundle.css.len();
        assert!(filled_len >= 1, "fill loop must accept at least one chunk");

        bundle.css.remove(0);
        assert_eq!(bundle.css.len(), filled_len - 1);

        bundle.add_css(chunk);
        assert_eq!(
            bundle.css.len(),
            filled_len,
            "removing one entry through the pub field must free budget for a new \
             entry of the same size, proving the stale total was recomputed \
             rather than staying stuck at the pre-removal ceiling"
        );
    }

    #[test]
    fn add_image_budget_recomputes_after_partial_removal() {
        let _guard = HEAVY_BUDGET_TEST_LOCK.lock().unwrap();
        let mut bundle = AssetBundle::new();
        let chunk = vec![0u8; MAX_IMAGE_BYTES];
        let max_attempts = MAX_TOTAL_IMAGE_BYTES / MAX_IMAGE_BYTES + 1;
        for i in 0..max_attempts {
            bundle.add_image(format!("img{i}.bin"), chunk.clone());
        }
        let filled_len = bundle.images.len();
        assert!(filled_len >= 1, "fill loop must accept at least one image");

        bundle.images.remove("img0.bin");
        assert_eq!(bundle.images.len(), filled_len - 1);

        bundle.add_image("new.bin", chunk);
        assert_eq!(
            bundle.images.len(),
            filled_len,
            "removing one entry through the pub field must free budget for a new \
             entry of the same size, proving the stale total was recomputed \
             rather than staying stuck at the pre-removal ceiling"
        );
    }

    #[test]
    fn add_css_shrinks_retained_capacity_to_length() {
        // A native Rust caller can hand in a `String` whose capacity is far
        // larger than its content (e.g. built with `String::with_capacity`
        // then only partially filled). Charging the budget by `.len()`
        // alone would under-count retained memory if that excess capacity
        // is kept; `add_css` must shrink it down before storing.
        let mut oversized_capacity = String::with_capacity(1024 * 1024);
        oversized_capacity.push_str("small");
        assert!(oversized_capacity.capacity() >= 1024 * 1024);

        let mut bundle = AssetBundle::new();
        bundle.add_css(oversized_capacity);
        assert_eq!(bundle.css.len(), 1);
        assert!(
            bundle.css[0].capacity() < 1024 * 1024,
            "retained capacity must be shrunk toward the actual content length \
             (got {})",
            bundle.css[0].capacity()
        );
    }

    #[test]
    fn add_image_shrinks_retained_capacity_to_length() {
        let mut oversized_capacity: Vec<u8> = Vec::with_capacity(1024 * 1024);
        oversized_capacity.extend_from_slice(&[1, 2, 3]);
        assert!(oversized_capacity.capacity() >= 1024 * 1024);

        let mut bundle = AssetBundle::new();
        bundle.add_image("small.bin", oversized_capacity);
        let stored = bundle.get_image("small.bin").expect("must be stored");
        assert!(
            stored.capacity() < 1024 * 1024,
            "retained capacity must be shrunk toward the actual content length \
             (got {})",
            stored.capacity()
        );
    }

    #[test]
    fn add_css_charges_overhead_for_empty_stylesheets() {
        // Before this fix, `css.len()` was the only charge, so an empty (or
        // tiny) stylesheet cost ~0 counted bytes each — a caller could push
        // an unbounded number of entries (each still a `Vec<String>` slot,
        // plus a `\n` separator per pair at `combined_css()` time) without
        // ever tripping the budget. Assert the fixed overhead is actually
        // charged by inspecting the running total directly, rather than
        // looping to the (~1M-entry) ceiling, which would make this test
        // slow without proving anything the arithmetic check doesn't.
        let mut bundle = AssetBundle::new();
        bundle.add_css("");
        bundle.add_css("");
        bundle.add_css("");
        assert_eq!(bundle.css.len(), 3);
        assert_eq!(
            bundle.css_total_bytes,
            3 * crate::STRING_ENTRY_OVERHEAD_BYTES,
            "each empty stylesheet must still be charged a fixed per-entry \
             overhead, not zero — otherwise entry count is unbounded"
        );
    }

    #[test]
    fn add_image_charges_overhead_for_tiny_images() {
        // Same gap as `add_css_charges_overhead_for_empty_stylesheets`, for
        // images: `key.len() + data.len()` alone doesn't cover the `String`
        // key struct, `Arc<Vec<u8>>` allocation, and `HashMap` bucket that
        // every accepted entry retains regardless of content size.
        let mut bundle = AssetBundle::new();
        bundle.add_image("a", vec![]);
        bundle.add_image("b", vec![]);
        assert_eq!(bundle.images.len(), 2);
        assert_eq!(
            bundle.image_total_bytes,
            2 * crate::STRING_ENTRY_OVERHEAD_BYTES + "a".len() + "b".len(),
            "each tiny image must still be charged a fixed per-entry overhead \
             in addition to key+data bytes — otherwise entry count is unbounded"
        );
    }
}
