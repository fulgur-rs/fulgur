//! Bundled images as files of the document's sandbox.

use crate::files::{BaseDirectoryProvider, content_type};
use fulgur_core::AssetBundle;
use percent_encoding::percent_decode_str;
use raikiri_traits::net::{FetchOutcome, FetchedResource, NetworkError, NetworkProvider, Request};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use url::Url;

/// The resources a document can fetch: the bundle's images, then the files
/// in the document's base directory.
///
/// Every bundle image is a file at a URL, like a file served next to the
/// document. A name is a URL reference resolved against the base directory
/// (`img/logo.png` is `file:///…/dir/img/logo.png`), or an absolute URL
/// (`https://example.com/logo.png`). HTML without a base directory is
/// `about:blank`, so only absolute-URL names apply to it. Raikiri resolves each
/// reference in a document by the usual rules (an `<img src>` against the
/// document base URL, a stylesheet's `url()` against the stylesheet URL)
/// and fetches the result; a request whose URL is a bundle image gets its
/// bytes, whatever the resource kind, and any other request goes to the
/// directory, which refuses every file outside it (after symlinks and `..`
/// are resolved). A bundle image only adds bytes the caller supplied; it
/// never opens a path outside the directory.
#[derive(Clone)]
pub(crate) struct Sandbox {
    images: Arc<HashMap<Url, BundledFile>>,
    files: BaseDirectoryProvider,
}

struct BundledFile {
    data: Arc<Vec<u8>>,
    /// Served like a local file: by the extension of the bundle name.
    content_type: Option<&'static str>,
}

impl Sandbox {
    pub(crate) fn new(bundle: Option<&AssetBundle>, files: BaseDirectoryProvider) -> Self {
        // Without a base directory the document is `about:blank`, against
        // which no relative name resolves: only absolute-URL names apply.
        let root = files.directory_url().ok();
        // Names visited in sorted order, so that when two names resolve to
        // the same URL (`a b.png` and `a%20b.png`) the same one always wins.
        let mut names: Vec<_> = bundle
            .into_iter()
            .flat_map(|bundle| bundle.images.iter())
            .collect();
        names.sort_unstable_by(|a, b| a.0.cmp(b.0));
        let mut images = HashMap::new();
        for (name, data) in names {
            let Some(url) = Url::parse(name)
                .ok()
                .or_else(|| root.as_ref()?.join(name).ok())
            else {
                continue;
            };
            // Typed by the decoded URL path, as a file on disk would be:
            // `icon%2Esvg` is `icon.svg`.
            let path = percent_decode_str(url.path()).decode_utf8_lossy();
            let content_type = content_type(Path::new(path.as_ref()));
            images.entry(key(url)).or_insert_with(|| BundledFile {
                data: Arc::clone(data),
                content_type,
            });
        }
        Self {
            images: Arc::new(images),
            files,
        }
    }
}

/// The identity of the resource at `url`. A fragment never names a separate
/// resource. A query does not either for a `file:` URL: it reaches the same
/// file on disk, so `logo.png?v=1` is the bundled `logo.png`.
fn key(mut url: Url) -> Url {
    url.set_fragment(None);
    if url.scheme() == "file" {
        url.set_query(None);
    }
    url
}

impl NetworkProvider for Sandbox {
    fn fetch_one_hop(&self, request: Request) -> std::result::Result<FetchOutcome, NetworkError> {
        if let Some(file) = self.images.get(&key(request.url.clone())) {
            return Ok(FetchOutcome::Body(FetchedResource {
                bytes: file.data.as_slice().to_vec().into(),
                content_type: file.content_type.map(str::to_string),
                final_url: request.url,
                encoding: None,
            }));
        }
        self.files.fetch_one_hop(request)
    }
}
