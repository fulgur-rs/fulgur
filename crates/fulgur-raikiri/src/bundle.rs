//! Images registered in an [`AssetBundle`], served ahead of local files.

use crate::files::{BaseDirectoryProvider, content_type};
use fulgur_core::AssetBundle;
use percent_encoding::percent_decode_str;
use raikiri_traits::ResourceKind;
use raikiri_traits::net::{FetchOutcome, FetchedResource, NetworkError, NetworkProvider, Request};
use std::path::Path;
use url::Url;

/// Answers image requests from the bundle, and every other request (and
/// every image the bundle lacks) from the input file's directory.
///
/// A bundle image is named the way the Blitz backend names it: by its path
/// relative to the input file's directory, so `<img src="logo.png">`,
/// `url("./logo.png")` and `url("img/a%20b.png")` find the images registered
/// as `logo.png` and `img/a b.png`. A URL outside that directory, such as
/// `https://example.com/logo.png`, is looked up by the whole URL.
#[derive(Clone)]
pub(crate) struct BundledImages<'a> {
    bundle: Option<&'a AssetBundle>,
    root: Url,
    files: BaseDirectoryProvider,
}

impl<'a> BundledImages<'a> {
    pub(crate) fn new(bundle: Option<&'a AssetBundle>, files: BaseDirectoryProvider) -> Self {
        let bundle = bundle.filter(|bundle| !bundle.images.is_empty());
        Self {
            bundle,
            root: files.root_url(),
            files,
        }
    }

    /// The bundle name `url` resolves to, if the bundle has that image.
    fn lookup(&self, url: &Url) -> Option<(&'a [u8], String)> {
        let bundle = self.bundle?;
        let relative = url
            .path()
            .strip_prefix(self.root.path())
            .filter(|_| url.scheme() == "file" && url.host_str() == self.root.host_str())
            .and_then(|path| percent_decode_str(path).decode_utf8().ok())
            .map(|path| match url.query() {
                Some(query) => format!("{path}?{query}"),
                None => path.into_owned(),
            });
        [relative, Some(url.as_str().to_string())]
            .into_iter()
            .flatten()
            .find_map(|name| bundle.get_image(&name).map(|data| (data.as_slice(), name)))
    }
}

impl NetworkProvider for BundledImages<'_> {
    fn fetch_one_hop(&self, request: Request) -> std::result::Result<FetchOutcome, NetworkError> {
        if matches!(request.kind, ResourceKind::Image | ResourceKind::Svg)
            && let Some((data, name)) = self.lookup(&request.url)
        {
            return Ok(FetchOutcome::Body(FetchedResource {
                bytes: data.to_vec().into(),
                content_type: content_type(Path::new(&name)).map(str::to_string),
                final_url: request.url,
                encoding: None,
            }));
        }
        self.files.fetch_one_hop(request)
    }
}
