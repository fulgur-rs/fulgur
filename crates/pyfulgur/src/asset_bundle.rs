use fulgur::AssetBundle;
use fulgur::Error as FulgurError;
use pyo3::exceptions::{PyFileNotFoundError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::path::PathBuf;

/// 一時的な inline error mapping (Task 9 で crate::error::map_fulgur_error に置き換え).
fn map_fulgur_error(err: FulgurError) -> PyErr {
    match err {
        FulgurError::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound => {
            PyFileNotFoundError::new_err(io_err.to_string())
        }
        _ => PyValueError::new_err(err.to_string()),
    }
}

#[pyclass(name = "AssetBundle", module = "pyfulgur")]
pub struct PyAssetBundle {
    pub(crate) inner: AssetBundle,
}

#[pymethods]
impl PyAssetBundle {
    #[new]
    fn new() -> Self {
        Self {
            inner: AssetBundle::new(),
        }
    }

    fn add_css(&mut self, css: &str) {
        self.inner.add_css(css);
    }

    fn add_css_file(&mut self, path: PathBuf) -> PyResult<()> {
        self.inner.add_css_file(path).map_err(map_fulgur_error)
    }

    fn add_font_file(&mut self, path: PathBuf) -> PyResult<()> {
        self.inner.add_font_file(path).map_err(map_fulgur_error)
    }

    fn add_image(&mut self, name: &str, data: &Bound<'_, PyBytes>) {
        self.inner.add_image(name, data.as_bytes().to_vec());
    }

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
