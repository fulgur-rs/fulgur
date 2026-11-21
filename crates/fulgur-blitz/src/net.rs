//! `NetProvider` implementation for fulgur.
//!
//! Allows Blitz to load CSS (and other resources) via the `file://` URL
//! scheme so that `<link rel="stylesheet">` and `@import` work natively
//! through Blitz's own loader pipeline. Two design goals shape this module:
//!
//! 1. **Offline-first.** Only `file://` and `data:` URLs are accepted.
//!    Anything else (`http://`, `https://`, ...) is silently dropped. The
//!    resolved file path must canonicalise to a location *inside* the
//!    configured base directory; symlinks or relative paths that escape
//!    the base are rejected (path traversal protection).
//!
//! 2. **GCPM parity across CSS sources.** fulgur's GCPM parser
//!    ([`crate::gcpm::parser::parse_gcpm`]) operates on raw CSS text. CSS
//!    that arrives via the network/file loader (`<link>` and `@import`)
//!    used to be invisible to it because it was injected directly into
//!    Blitz's stylist. To fix that, [`FulgurNetProvider`] runs `parse_gcpm`
//!    on every CSS payload before forwarding the cleaned bytes to Blitz,
//!    accumulates the resulting [`GcpmContext`] entries in an internal
//!    buffer, and exposes them via [`FulgurNetProvider::drain_gcpm_contexts`]
//!    so [`crate::engine::Engine::render_html`] can merge them with the
//!    GCPM context derived from `--css`.
//!
//! Because GCPM constructs (`@page`, `position: running(...)`,
//! `string-set`, `counter-*`) are stripped from the CSS that Blitz
//! actually parses, Blitz never tries to apply `@page` styles to body
//! content or treat `position: running(...)` as a real property. The
//! cleaned CSS still contains every other declaration, so cascade,
//! specificity and `@import` resolution remain delegated to stylo.

use crate::blitz_adapter::net::{Bytes, NetHandler, NetProvider, Request, Url};
use crate::gcpm::GcpmContext;
use crate::gcpm::parser::parse_gcpm;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// fulgur's [`NetProvider`] for Blitz.
///
/// This type is an implementation detail of
/// [`crate::blitz_adapter::parse_html_with_local_resources`], which is
/// the only supported entry point for loading HTML with local
/// `<link rel="stylesheet">` / `@import` resolution. That function
/// owns the provider lifecycle — construction, Blitz configuration,
/// applying loaded resources (`BaseDocument::handle_messages`), and
/// folding of GCPM contexts — so the rest of fulgur never needs to touch
/// Blitz types directly (CLAUDE.md adapter-isolation rule). The `drain_*`
/// methods below exist for that internal use only.
///
/// Since blitz-dom 0.3 a [`NetHandler`] no longer reports the decoded
/// resource back to the provider: it posts a `ResourceLoadResponse` to the
/// document's own channel, and the node id that triggered the fetch stays
/// private to the handler. Per-`<link>` data (GCPM subtree, raw CSS text)
/// is therefore recorded per *top-level fetch URL* ([`TopLevelFetch`]) and
/// matched to `<link>` nodes by the caller, in document order.
pub struct FulgurNetProvider {
    // `canonical_base` is consulted only by the native
    // `resolve_local_path`; on `wasm32-unknown-unknown` the resolver is a
    // stub that returns `None`, leaving this field intentionally unread.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    canonical_base: Option<PathBuf>,
    inner: Arc<Mutex<Inner>>,
}

/// One fetch that was not reached through a parent stylesheet's `@import`
/// (in practice: a `<link>` / `<img>` fetch, or a top-level
/// `<style>@import`). `gcpm` already has the fetch's whole `@import`
/// subtree folded in, in child-before-parent post-order.
#[derive(Debug, Clone)]
pub struct TopLevelFetch {
    pub url: Url,
    pub gcpm: GcpmContext,
    /// Raw (uncleaned) payload text, when it decodes as UTF-8. Fed to
    /// `column_css`'s side-table once the fetch is matched to a `<link>`.
    pub css_text: Option<String>,
}

#[derive(Default)]
struct Inner {
    gcpm_contexts: Vec<GcpmContext>,
    /// Recursion depth through nested `@import` fetches — 0 outside any
    /// fetch, incremented on entry to `fetch()`, decremented on exit.
    /// Stylo parses a stylesheet (and so issues its `@import` fetches)
    /// synchronously inside `handler.bytes`, so a fetch that finds depth 0
    /// on entry was not reached via a parent CSS file's `@import`.
    import_depth: usize,
    top_level_fetches: Vec<TopLevelFetch>,
}

impl FulgurNetProvider {
    /// Build a provider rooted at `base_path`. If `base_path` is `None`
    /// or cannot be canonicalised, the provider rejects every fetch.
    pub fn new(base_path: Option<PathBuf>) -> Self {
        let canonical_base = base_path.and_then(|p| p.canonicalize().ok());
        Self {
            canonical_base,
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    /// Take the GCPM contexts extracted from CSS payloads. The caller
    /// is expected to merge each one into the engine-level context.
    pub fn drain_gcpm_contexts(&self) -> Vec<GcpmContext> {
        let mut inner = self.inner.lock().unwrap();
        std::mem::take(&mut inner.gcpm_contexts)
    }

    /// Take the top-level fetches recorded so far, in fetch order (which
    /// is document order for `<link>` elements: Blitz issues their fetches
    /// while parsing).
    pub fn drain_top_level_fetches(&self) -> Vec<TopLevelFetch> {
        let mut inner = self.inner.lock().unwrap();
        std::mem::take(&mut inner.top_level_fetches)
    }

    /// Resolve a request URL to a real, canonicalised path inside the
    /// configured base directory. Returns `None` if the URL is not
    /// `file://`, the file does not exist, or the resolved path escapes
    /// the base directory.
    #[cfg(not(target_arch = "wasm32"))]
    fn resolve_local_path(&self, request: &Request) -> Option<PathBuf> {
        if request.url.scheme() != "file" {
            return None;
        }
        let path = request.url.to_file_path().ok()?;
        let canonical = path.canonicalize().ok()?;
        let base = self.canonical_base.as_ref()?;
        if !canonical.starts_with(base) {
            return None;
        }
        Some(canonical)
    }

    /// WASM stub: `Url::to_file_path` and `Path::canonicalize` are gated
    /// off on `wasm32-unknown-unknown`. Browsers also forbid JS access to
    /// `file://`, so this resolver can never succeed there. Resources for
    /// WASM must be delivered as bytes through `AssetBundle`.
    #[cfg(target_arch = "wasm32")]
    fn resolve_local_path(&self, _request: &Request) -> Option<PathBuf> {
        None
    }

    fn looks_like_css(request: &Request, path: &Path) -> bool {
        // Strict MIME match — `text/cssfoo` must not be treated as CSS.
        let ct = request.content_type.as_deref().unwrap_or("");
        if ct == "text/css" || ct.starts_with("text/css;") {
            return true;
        }
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("css"))
    }
}

impl NetProvider for FulgurNetProvider {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        // `data:` URLs carry their content inline, so serving them reads
        // nothing outside the document. Blitz needs them to size `<img>`
        // elements whose intrinsic dimensions come from the image.
        let (raw_bytes, is_css) = if request.url.scheme() == "data" {
            let Some((body, mime)) = data_url::DataUrl::process(request.url.as_str())
                .ok()
                .and_then(|url| {
                    let mime = url.mime_type();
                    let is_css = mime.type_ == "text" && mime.subtype == "css";
                    url.decode_to_vec().ok().map(|(body, _)| (body, is_css))
                })
            else {
                handler.error(request.url.to_string(), "invalid data: URL".into());
                return;
            };
            (body, mime)
        } else {
            let Some(canonical_path) = self.resolve_local_path(&request) else {
                handler.error(request.url.to_string(), "blocked by fulgur".into());
                return;
            };
            let Ok(raw_bytes) = std::fs::read(&canonical_path) else {
                handler.error(request.url.to_string(), "read failed".into());
                return;
            };
            let is_css = Self::looks_like_css(&request, &canonical_path);
            (raw_bytes, is_css)
        };

        // For CSS, hand Blitz the *cleaned* body so its style engine
        // never sees `@page` / `position: running(...)` constructs.
        // `css_text` retains the raw payload — `column_css`'s tiny parser
        // only looks for a handful of properties (`break-*`, `column-*`)
        // and silently drops anything it doesn't recognise (fulgur-s5ro).
        // A stylesheet served without a `.css` extension is still
        // recorded (raw text) so a `<link>` match can use it.
        let (bytes_for_blitz, gcpm_to_push, css_text) = if is_css {
            if let Ok(text) = std::str::from_utf8(&raw_bytes) {
                let gcpm = parse_gcpm(text);
                let cleaned = gcpm.cleaned_css.clone();
                (
                    Bytes::from(cleaned.into_bytes()),
                    Some(gcpm),
                    Some(text.to_string()),
                )
            } else {
                (Bytes::from(raw_bytes), None, None)
            }
        } else {
            let text = std::str::from_utf8(&raw_bytes).ok().map(str::to_string);
            (Bytes::from(raw_bytes), None, text)
        };

        // Blitz parses any payload it cannot decode as a raster image with
        // usvg's default options, whose image resolver reads `<image href>`
        // paths from the filesystem relative to the working directory, outside
        // the base directory. Remove those references from every payload usvg
        // could parse, whatever its extension or the branch above: the same
        // file can be both a `<link>` target and an `<img src>`.
        let bytes_for_blitz = match crate::svg::sanitize_svg_payload(&bytes_for_blitz) {
            crate::svg::SvgPayload::Unchanged => bytes_for_blitz,
            crate::svg::SvgPayload::Sanitized(sanitized) => Bytes::from(sanitized),
            crate::svg::SvgPayload::Rejected => {
                handler.error(request.url.to_string(), "rejected svg".into());
                return;
            }
        };

        let is_top_level = {
            let mut inner = self.inner.lock().unwrap();
            let top = inner.import_depth == 0;
            inner.import_depth += 1;
            top
        };
        let gcpm_start = self.inner.lock().unwrap().gcpm_contexts.len();

        // `handler.bytes` parses the CSS via stylo, which synchronously
        // triggers `fetch()` again for every `@import` before returning.
        // When it does return, every imported child stylesheet has
        // already pushed its own `GcpmContext`.
        handler.bytes(request.url.to_string(), bytes_for_blitz);

        // Post-order push: the parent context goes into the buffer
        // *after* its children, so the eventual `cleaned_css`
        // concatenation is `child_rules + parent_rules`, matching CSS
        // cascade order for `@import`.
        let mut inner = self.inner.lock().unwrap();
        if let Some(gcpm) = gcpm_to_push {
            inner.gcpm_contexts.push(gcpm);
        }
        inner.import_depth -= 1;
        if is_top_level {
            let gcpm = inner.gcpm_contexts[gcpm_start..].iter().cloned().fold(
                GcpmContext::default(),
                |mut acc, ctx| {
                    acc.extend_from(ctx);
                    acc
                },
            );
            inner.top_level_fetches.push(TopLevelFetch {
                url: request.url,
                gcpm,
                css_text,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blitz_adapter::net::Url;
    use std::fs;

    fn make_request(url: &str) -> Request {
        Request::get(Url::parse(url).unwrap())
    }

    /// A `NetHandler` that records every byte payload it receives, used
    /// in tests to drive `FulgurNetProvider::fetch` end-to-end without
    /// pulling in real Blitz handler types.
    struct RecordingHandler {
        bytes: Arc<Mutex<Option<Vec<u8>>>>,
    }

    impl NetHandler for RecordingHandler {
        fn bytes(self: Box<Self>, _resolved_url: String, bytes: Bytes) {
            *self.bytes.lock().unwrap() = Some(bytes.to_vec());
        }
    }

    #[test]
    fn rejects_http_urls() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FulgurNetProvider::new(Some(dir.path().to_path_buf()));
        let req = make_request("https://example.com/style.css");
        assert!(provider.resolve_local_path(&req).is_none());
    }

    #[test]
    fn rejects_data_urls() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FulgurNetProvider::new(Some(dir.path().to_path_buf()));
        let req = make_request("data:text/css,body{color:red}");
        assert!(provider.resolve_local_path(&req).is_none());
    }

    #[test]
    fn fetch_serves_data_urls_inline() {
        // `data:` URLs are part of the document: they are served without a
        // base path and without touching the file system.
        let provider = FulgurNetProvider::new(None);
        let recorded = Arc::new(Mutex::new(None));
        provider.fetch(
            0,
            make_request("data:image/png;base64,iVBORw0K"),
            Box::new(RecordingHandler {
                bytes: recorded.clone(),
            }),
        );
        assert_eq!(
            recorded.lock().unwrap().as_deref(),
            Some(&b"\x89PNG\r\n"[..])
        );
    }

    #[test]
    fn fetch_runs_gcpm_on_data_url_stylesheets() {
        let provider = FulgurNetProvider::new(None);
        let recorded = Arc::new(Mutex::new(None));
        provider.fetch(
            0,
            make_request("data:text/css,p{color:red}@page{size:A5}"),
            Box::new(RecordingHandler {
                bytes: recorded.clone(),
            }),
        );
        let served = String::from_utf8(recorded.lock().unwrap().clone().unwrap()).unwrap();
        assert!(served.contains("color:red"), "{served}");
        assert!(!served.contains("@page"), "{served}");
    }

    #[test]
    fn allows_file_inside_base() {
        let dir = tempfile::tempdir().unwrap();
        let css_path = dir.path().join("style.css");
        fs::write(&css_path, "p { color: red; }").unwrap();
        let provider = FulgurNetProvider::new(Some(dir.path().to_path_buf()));
        let url = Url::from_file_path(&css_path).unwrap();
        let req = make_request(url.as_str());
        let resolved = provider.resolve_local_path(&req).unwrap();
        assert_eq!(resolved, css_path.canonicalize().unwrap());
    }

    #[test]
    fn rejects_path_traversal_outside_base() {
        // Create base/ and a sibling file outside it. The provider should
        // refuse to serve the sibling even though it exists on disk.
        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().join("base");
        fs::create_dir(&base).unwrap();
        let outside = parent.path().join("outside.css");
        fs::write(&outside, "body { color: blue; }").unwrap();

        let provider = FulgurNetProvider::new(Some(base.clone()));
        let url = Url::from_file_path(&outside).unwrap();
        let req = make_request(url.as_str());
        assert!(provider.resolve_local_path(&req).is_none());
    }

    #[test]
    fn rejects_when_no_base_path() {
        let provider = FulgurNetProvider::new(None);
        let dir = tempfile::tempdir().unwrap();
        let css = dir.path().join("style.css");
        fs::write(&css, "p {}").unwrap();
        let url = Url::from_file_path(&css).unwrap();
        let req = make_request(url.as_str());
        assert!(provider.resolve_local_path(&req).is_none());
    }

    #[test]
    fn detects_css_by_extension() {
        let req = make_request("file:///tmp/x.css");
        assert!(FulgurNetProvider::looks_like_css(
            &req,
            Path::new("/tmp/style.css")
        ));
    }

    #[test]
    fn detects_css_by_content_type() {
        let mut req = make_request("file:///tmp/x");
        req.content_type = Some("text/css".to_string());
        assert!(FulgurNetProvider::looks_like_css(&req, Path::new("/tmp/x")));
    }

    #[test]
    fn fetch_runs_gcpm_and_serves_cleaned_css() {
        // End-to-end test of `fetch()`: drives the real entry point
        // (URL resolution → file read → CSS detection → parse_gcpm →
        // handler.bytes) so that a regression in any of those steps
        // would fail this test rather than slip through.
        let dir = tempfile::tempdir().unwrap();
        let css_path = dir.path().join("style.css");
        let original_css = r#"
            .pageHeader { position: running(pageHeader); }
            @page { @top-center { content: element(pageHeader); } }
            body { color: red; }
        "#;
        fs::write(&css_path, original_css).unwrap();

        let provider = FulgurNetProvider::new(Some(dir.path().to_path_buf()));
        let url = Url::from_file_path(&css_path).unwrap();
        let request = Request::get(url);

        let recorded = Arc::new(Mutex::new(None));
        let handler = Box::new(RecordingHandler {
            bytes: recorded.clone(),
        });
        provider.fetch(0, request, handler);

        // The handler must have received cleaned CSS — i.e. the
        // `body { color: red; }` rule survives, but `@page` and
        // `position: running(...)` are stripped/replaced.
        let bytes = recorded
            .lock()
            .unwrap()
            .clone()
            .expect("RecordingHandler must have received bytes from fetch()");
        let cleaned = std::str::from_utf8(&bytes).unwrap();
        assert!(
            cleaned.contains("body { color: red; }"),
            "non-GCPM rules should pass through to Blitz, got: {cleaned}"
        );
        assert!(
            !cleaned.contains("@page"),
            "@page rules should be stripped from cleaned CSS, got: {cleaned}"
        );
        assert!(
            !cleaned.contains("position: running"),
            "running declarations should be replaced in cleaned CSS, got: {cleaned}"
        );

        // The GCPM context buffer should now hold one entry with the
        // running mapping and margin box extracted from the original CSS.
        let drained = provider.drain_gcpm_contexts();
        assert_eq!(drained.len(), 1);
        assert!(!drained[0].running_mappings.is_empty());
        assert!(!drained[0].margin_boxes.is_empty());

        // Subsequent drain returns empty (drain semantics).
        assert!(provider.drain_gcpm_contexts().is_empty());
    }

    #[test]
    fn fetch_drops_request_outside_base_path() {
        // A fetch for a file outside `canonical_base` must be a no-op:
        // no bytes delivered to the handler, no GCPM context recorded.
        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().join("base");
        fs::create_dir(&base).unwrap();
        let outside = parent.path().join("outside.css");
        fs::write(&outside, "body { color: blue; }").unwrap();

        let provider = FulgurNetProvider::new(Some(base));
        let request = Request::get(Url::from_file_path(&outside).unwrap());
        let recorded = Arc::new(Mutex::new(None));
        let handler = Box::new(RecordingHandler {
            bytes: recorded.clone(),
        });
        provider.fetch(0, request, handler);

        assert!(
            recorded.lock().unwrap().is_none(),
            "handler must not receive bytes for files outside the base path"
        );
        assert!(provider.drain_gcpm_contexts().is_empty());
        assert!(
            provider.drain_top_level_fetches().is_empty(),
            "a rejected fetch must not register a top-level fetch entry"
        );
    }

    #[test]
    fn fetch_strips_svg_image_refs_outside_base_path() {
        // An SVG served from inside the base directory must not be able to
        // pull in a file outside it through `<image href>`: Blitz parses
        // the payload with usvg's file-reading default resolver.
        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().join("base");
        fs::create_dir(&base).unwrap();
        let outside = parent.path().join("secret.png");
        fs::write(&outside, crate::svg::TEST_PNG_1X1).unwrap();
        let svg_path = base.join("logo.svg");
        fs::write(
            &svg_path,
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="20"><image href="{}" width="10" height="10"/><rect width="10" height="10"/></svg>"#,
                outside.display()
            ),
        )
        .unwrap();

        let provider = FulgurNetProvider::new(Some(base));
        let request = Request::get(Url::from_file_path(&svg_path).unwrap());
        let recorded = Arc::new(Mutex::new(None));
        let handler = Box::new(RecordingHandler {
            bytes: recorded.clone(),
        });
        provider.fetch(0, request, handler);

        let bytes = recorded
            .lock()
            .unwrap()
            .take()
            .expect("an SVG inside the base path is still served");
        let tree = usvg::Tree::from_data(&bytes, &usvg::Options::default()).unwrap();
        assert_eq!(crate::svg::count_images(tree.root()), 0);
        assert_eq!(tree.size().width(), 10.0);
        assert_eq!(tree.size().height(), 20.0);
    }
}
