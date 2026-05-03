use crate::error::map_fulgur_error;
use fulgur::AssetBundle;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::path::PathBuf;

/// Bundle of CSS, fonts, and images passed to :class:`Engine`.
///
/// fulgur is offline-first: every asset must be explicitly registered before
/// rendering. The engine never performs network fetches.
///
/// Example:
///     >>> from pyfulgur import AssetBundle, Engine
///     >>> bundle = AssetBundle()
///     >>> bundle.add_css("body { font-family: sans-serif; }")
///     >>> engine = Engine(assets=bundle)
#[pyclass(name = "AssetBundle", module = "pyfulgur")]
pub struct PyAssetBundle {
    pub(crate) inner: AssetBundle,
}

#[pymethods]
impl PyAssetBundle {
    /// Create an empty asset bundle.
    #[new]
    fn new() -> Self {
        Self {
            inner: AssetBundle::new(),
        }
    }

    /// Add an inline CSS string to the bundle.
    ///
    /// Args:
    ///     css: CSS source text.
    fn add_css(&mut self, css: &str) {
        self.inner.add_css(css);
    }

    /// Add a CSS file from disk.
    ///
    /// Args:
    ///     path: Filesystem path to the CSS file.
    ///
    /// Raises:
    ///     FileNotFoundError: When ``path`` does not exist.
    ///     ValueError: When the CSS file cannot be read or decoded.
    fn add_css_file(&mut self, path: PathBuf) -> PyResult<()> {
        self.inner.add_css_file(path).map_err(map_fulgur_error)
    }

    /// Add a font file (TTF, OTF, WOFF, or WOFF2) from disk.
    ///
    /// Args:
    ///     path: Filesystem path to the font file.
    ///
    /// Raises:
    ///     FileNotFoundError: When ``path`` does not exist.
    ///     ValueError: When the font format is unsupported.
    fn add_font_file(&mut self, path: PathBuf) -> PyResult<()> {
        self.inner.add_font_file(path).map_err(map_fulgur_error)
    }

    /// Add an image with an explicit logical name and raw bytes.
    ///
    /// Args:
    ///     name: Logical name to reference from CSS or HTML
    ///         (e.g. ``"logo.png"``).
    ///     data: Raw image bytes (PNG, JPEG, etc.).
    fn add_image(&mut self, name: &str, data: &Bound<'_, PyBytes>) {
        self.inner.add_image(name, data.as_bytes().to_vec());
    }

    /// Add an image file from disk.
    ///
    /// Args:
    ///     name: Logical name to reference from CSS or HTML.
    ///     path: Filesystem path to the image file.
    ///
    /// Raises:
    ///     FileNotFoundError: When ``path`` does not exist.
    fn add_image_file(&mut self, name: &str, path: PathBuf) -> PyResult<()> {
        self.inner
            .add_image_file(name, path)
            .map_err(map_fulgur_error)
    }
}

impl PyAssetBundle {
    /// Engine builder/constructor に渡すために内部の `AssetBundle` を取り出す。
    /// 呼び出し後 inner は空の `AssetBundle::new()` にリセットされる。
    pub(crate) fn take_inner(&mut self) -> AssetBundle {
        std::mem::replace(&mut self.inner, AssetBundle::new())
    }
}
