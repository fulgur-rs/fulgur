//! Local file access for the resources a document references.

use fulgur_core::{Error, Result};
use raikiri_html::DEFAULT_MAX_RESOURCE_BYTES;
use raikiri_traits::net::{FetchOutcome, FetchedResource, NetworkError, NetworkProvider, Request};
use std::io::Read;
use std::path::{Path, PathBuf};
use url::Url;

/// Serves `file://` URLs that resolve inside one directory.
///
/// Raikiri requests stylesheets, `@import`s, fonts, and images through this
/// provider. A path that leaves the directory once symlinks and `..` are
/// resolved is refused, as is every other URL scheme, so a document can only
/// reach files next to it.
#[derive(Clone)]
pub(crate) struct BaseDirectoryProvider {
    root: PathBuf,
    max_bytes: u64,
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
            max_bytes: DEFAULT_MAX_RESOURCE_BYTES,
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

    /// The `file://` URL of the directory, ending in a slash.
    pub(crate) fn root_url(&self) -> Url {
        Url::from_directory_path(&self.root).expect("the canonical root is absolute")
    }

    fn resolve(&self, url: &Url) -> std::result::Result<PathBuf, NetworkError> {
        if url.scheme() != "file" {
            return Err(NetworkError::Other(format!(
                "only file:// resources are read, got {url}"
            )));
        }
        // A host names another machine (a UNC share on Windows); only
        // `file:///path` and `file://localhost/path` are local.
        let local = matches!(url.host_str(), None | Some("") | Some("localhost"));
        let path = url
            .to_file_path()
            .ok()
            .filter(|_| local)
            .ok_or_else(|| NetworkError::Other(format!("invalid file URL: {url}")))?;
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
        let bytes = read_capped(&path, self.max_bytes)?;
        Ok(FetchOutcome::Body(FetchedResource {
            bytes: bytes.into(),
            content_type: content_type(&path).map(str::to_string),
            final_url: request.url,
            encoding: None,
        }))
    }
}

/// Read at most `max_bytes` of `path`, failing if the file is larger.
///
/// Raikiri also rejects oversized responses, but only after the provider
/// has returned them; reading through a cap keeps a huge file from being
/// buffered in full first.
fn read_capped(path: &Path, max_bytes: u64) -> std::result::Result<Vec<u8>, NetworkError> {
    let file = std::fs::File::open(path).map_err(NetworkError::Io)?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(NetworkError::Io)?;
    if bytes.len() as u64 > max_bytes {
        return Err(NetworkError::Other(format!(
            "{} exceeds {max_bytes} bytes",
            path.display()
        )));
    }
    Ok(bytes)
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

#[cfg(test)]
impl BaseDirectoryProvider {
    pub(crate) fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}
