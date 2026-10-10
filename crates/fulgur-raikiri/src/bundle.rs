//! Bundled images as files of the document's sandbox.

use crate::files::{BaseDirectoryProvider, content_type};
use fulgur_core::AssetBundle;
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
        let images = bundle
            .into_iter()
            .flat_map(|bundle| bundle.images.iter())
            .filter_map(|(name, data)| {
                let url = Url::parse(name)
                    .ok()
                    .or_else(|| root.as_ref()?.join(name).ok())?;
                let file = BundledFile {
                    data: Arc::clone(data),
                    content_type: content_type(Path::new(name)),
                };
                Some((without_fragment(url), file))
            })
            .collect();
        Self {
            images: Arc::new(images),
            files,
        }
    }
}

fn without_fragment(mut url: Url) -> Url {
    url.set_fragment(None);
    url
}

impl NetworkProvider for Sandbox {
    fn fetch_one_hop(&self, request: Request) -> std::result::Result<FetchOutcome, NetworkError> {
        if let Some(file) = self.images.get(&without_fragment(request.url.clone())) {
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
