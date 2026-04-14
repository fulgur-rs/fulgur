//! `NetProvider` implementation for fulgur.
//!
//! Allows Blitz to load CSS (and other resources) via the `file://` URL
//! scheme so that `<link rel="stylesheet">` and `@import` work natively
//! through Blitz's own loader pipeline. Two design goals shape this module:
//!
//! 1. **Offline-first.** Only `file://` URLs are accepted. Anything else
//!    (`http://`, `https://`, `data:`, ...) is silently dropped. The
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

use crate::gcpm::GcpmContext;
use crate::gcpm::parser::parse_gcpm;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// fulgur's [`NetProvider`] for Blitz.
///
/// This type is an implementation detail of
/// [`crate::blitz_adapter::parse_html_with_local_resources`], which is
/// the only supported entry point for loading HTML with local
/// `<link rel="stylesheet">` / `@import` resolution. That function
/// owns the provider lifecycle — construction, Blitz configuration,
/// draining of pending resources, and folding of GCPM contexts — so
/// the rest of fulgur never needs to touch Blitz types directly
/// (CLAUDE.md adapter-isolation rule). The `drain_*` methods below
/// exist for that internal use only.
pub struct FulgurNetProvider {
    canonical_base: Option<PathBuf>,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    gcpm_contexts: Vec<GcpmContext>,
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

    /// Resolve a request URL to a real, canonicalised path inside the
    /// configured base directory. Returns `None` if the URL is not
    /// `file://`, the file does not exist, or the resolved path escapes
    /// the base directory.
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

    fn looks_like_css(request: &Request, path: &Path) -> bool {
        // Strict MIME match — `text/cssfoo` must not be treated as CSS.
        let ct = request.content_type.as_str();
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
        let Some(canonical_path) = self.resolve_local_path(&request) else {
            return;
        };
        let Ok(raw_bytes) = std::fs::read(&canonical_path) else {
            return;
        };

        // For CSS, hand Blitz the *cleaned* body so its style engine
        // never sees `@page` / `position: running(...)` constructs.
        // The GCPM context is pushed into our buffer *after* the
        // handler returns (see below).
        let (bytes_for_blitz, gcpm_to_push) = if Self::looks_like_css(&request, &canonical_path) {
            if let Ok(text) = std::str::from_utf8(&raw_bytes) {
                let gcpm = parse_gcpm(text);
                let cleaned = gcpm.cleaned_css.clone();
                (Bytes::from(cleaned.into_bytes()), Some(gcpm))
            } else {
                (Bytes::from(raw_bytes), None)
            }
        } else {
            (Bytes::from(raw_bytes), None)
        };

        // `handler.bytes` parses the CSS via stylo, which synchronously
        // triggers `fetch()` again for every `@import` before returning.
        // When it does return, every imported child stylesheet has
        // already pushed its own `GcpmContext`.
        let resolved_url = request.url.to_string();
        handler.bytes(resolved_url, bytes_for_blitz);

        // Post-order push: the parent context goes into the buffer
        // *after* its children, so the eventual `cleaned_css`
        // concatenation is `child_rules + parent_rules`. This matches
        // CSS cascade ordering — `@import` is semantically equivalent
        // to inlining the child stylesheet at the top of the parent,
        // so the parent's own declarations must override the imported
        // ones in the margin-box mini-documents rendered from
        // `gcpm.cleaned_css`.
        if let Some(gcpm) = gcpm_to_push {
            self.inner.lock().unwrap().gcpm_contexts.push(gcpm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz_traits::net::{NetHandler, Url};
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
        fn bytes(self: Box<Self>, _resolved_url: String, bytes: blitz_traits::net::Bytes) {
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
        req.content_type = "text/css".to_string();
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
            provider.drain_pending_resources().is_empty(),
            "rejected fetches must not register a pending resource"
        );
    }
}
