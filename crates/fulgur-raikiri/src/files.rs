//! Local file access for the resources a document references.

use fulgur_core::{Error, Result};
use raikiri_traits::net::{FetchOutcome, FetchedResource, NetworkError, NetworkProvider, Request};
use std::path::{Path, PathBuf};
use url::Url;

/// Serves `file://` URLs that resolve inside one directory.
///
/// Raikiri requests stylesheets, `@import`s, fonts, and images through this
/// provider. A path that leaves the directory once symlinks and `..` are
/// resolved is refused, as is every other URL scheme, so a document can only
/// reach files next to it.
pub(crate) struct BaseDirectoryProvider {
    root: PathBuf,
}

impl BaseDirectoryProvider {
    /// A provider rooted at the directory that contains `input`.
    pub(crate) fn for_input(input: &Path) -> Result<Self> {
        let parent = input
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        Ok(Self {
            root: parent.canonicalize()?,
        })
    }

    /// The `file://` URL of `input`, used as the document base URL.
    pub(crate) fn document_url(&self, input: &Path) -> Result<Url> {
        let name = input
            .file_name()
            .ok_or_else(|| Error::Layout(format!("{} is not a file", input.display())))?;
        Url::from_file_path(self.root.join(name))
            .map_err(|()| Error::Layout(format!("{} has no file URL", input.display())))
    }

    fn resolve(&self, url: &Url) -> std::result::Result<PathBuf, NetworkError> {
        if url.scheme() != "file" {
            return Err(NetworkError::Other(format!(
                "only file:// resources are read, got {url}"
            )));
        }
        let path = url
            .to_file_path()
            .map_err(|()| NetworkError::Other(format!("invalid file URL: {url}")))?;
        let path = path.canonicalize().map_err(NetworkError::Io)?;
        if !path.starts_with(&self.root) {
            return Err(NetworkError::Other(format!(
                "{url} is outside {}",
                self.root.display()
            )));
        }
        Ok(path)
    }
}

impl NetworkProvider for BaseDirectoryProvider {
    fn fetch_one_hop(&self, request: Request) -> std::result::Result<FetchOutcome, NetworkError> {
        let path = self.resolve(&request.url)?;
        let bytes = std::fs::read(&path).map_err(NetworkError::Io)?;
        Ok(FetchOutcome::Body(FetchedResource {
            bytes: bytes.into(),
            content_type: content_type(&path).map(str::to_string),
            final_url: request.url,
            encoding: None,
        }))
    }
}

/// The MIME type a local file would be served with, from its extension.
///
/// Files carry no Content-Type, and Raikiri only applies stylesheet responses
/// labelled `text/css`. Unknown extensions get no type and are left to the
/// consumer of the bytes to sniff.
pub(crate) fn content_type(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "css" => "text/css",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => return None,
    })
}
