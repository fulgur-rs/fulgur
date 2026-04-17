use std::path::PathBuf;

use fulgur::{Engine, EngineBuilder, PageSize};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::asset_bundle::PyAssetBundle;
use crate::margin::PyMargin;
use crate::page_size::PyPageSize;

#[pyclass(name = "EngineBuilder", module = "pyfulgur")]
pub struct PyEngineBuilder {
    inner: Option<EngineBuilder>,
}

impl PyEngineBuilder {
    fn take(&mut self) -> PyResult<EngineBuilder> {
        self.inner
            .take()
            .ok_or_else(|| PyRuntimeError::new_err("EngineBuilder has already been built"))
    }

    fn map(&mut self, f: impl FnOnce(EngineBuilder) -> EngineBuilder) -> PyResult<()> {
        let b = self.take()?;
        self.inner = Some(f(b));
        Ok(())
    }
}

pub(crate) fn parse_page_size_str(name: &str) -> PyResult<PageSize> {
    match name.to_ascii_uppercase().as_str() {
        "A4" => Ok(PageSize::A4),
        "LETTER" => Ok(PageSize::LETTER),
        "A3" => Ok(PageSize::A3),
        other => Err(PyValueError::new_err(format!("unknown page size: {other}"))),
    }
}

/// `PageSize` オブジェクトまたは文字列名 (大文字小文字無視) を `fulgur::PageSize` に解決する。
pub(crate) fn extract_page_size(value: &Bound<'_, PyAny>) -> PyResult<PageSize> {
    if let Ok(ps) = value.extract::<PyPageSize>() {
        Ok(ps.inner)
    } else if let Ok(s) = value.extract::<String>() {
        parse_page_size_str(&s)
    } else {
        Err(PyValueError::new_err("page_size must be PageSize or str"))
    }
}

#[pymethods]
impl PyEngineBuilder {
    #[new]
    fn new() -> Self {
        Self {
            inner: Some(Engine::builder()),
        }
    }

    fn page_size(mut slf: PyRefMut<'_, Self>, value: &Bound<'_, PyAny>) -> PyResult<Py<Self>> {
        let size = extract_page_size(value)?;
        slf.map(|b| b.page_size(size))?;
        Ok(slf.into())
    }

    fn margin(mut slf: PyRefMut<'_, Self>, margin: PyMargin) -> PyResult<Py<Self>> {
        slf.map(|b| b.margin(margin.inner))?;
        Ok(slf.into())
    }

    fn landscape(mut slf: PyRefMut<'_, Self>, value: bool) -> PyResult<Py<Self>> {
        slf.map(|b| b.landscape(value))?;
        Ok(slf.into())
    }

    fn title(mut slf: PyRefMut<'_, Self>, value: String) -> PyResult<Py<Self>> {
        slf.map(|b| b.title(value))?;
        Ok(slf.into())
    }

    fn author(mut slf: PyRefMut<'_, Self>, value: String) -> PyResult<Py<Self>> {
        slf.map(|b| b.author(value))?;
        Ok(slf.into())
    }

    fn lang(mut slf: PyRefMut<'_, Self>, value: String) -> PyResult<Py<Self>> {
        slf.map(|b| b.lang(value))?;
        Ok(slf.into())
    }

    fn bookmarks(mut slf: PyRefMut<'_, Self>, value: bool) -> PyResult<Py<Self>> {
        slf.map(|b| b.bookmarks(value))?;
        Ok(slf.into())
    }

    fn assets(
        mut slf: PyRefMut<'_, Self>,
        bundle: &Bound<'_, PyAssetBundle>,
    ) -> PyResult<Py<Self>> {
        let taken = bundle.borrow_mut().take_inner();
        slf.map(|b| b.assets(taken))?;
        Ok(slf.into())
    }

    fn build(&mut self) -> PyResult<PyEngine> {
        let b = self.take()?;
        Ok(PyEngine { inner: b.build() })
    }
}

#[pyclass(name = "Engine", module = "pyfulgur")]
pub struct PyEngine {
    pub(crate) inner: Engine,
}

#[pymethods]
impl PyEngine {
    #[new]
    #[pyo3(signature = (
        *,
        page_size = None,
        margin = None,
        landscape = None,
        title = None,
        author = None,
        lang = None,
        bookmarks = None,
        assets = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        page_size: Option<&Bound<'_, PyAny>>,
        margin: Option<PyMargin>,
        landscape: Option<bool>,
        title: Option<String>,
        author: Option<String>,
        lang: Option<String>,
        bookmarks: Option<bool>,
        assets: Option<&Bound<'_, PyAssetBundle>>,
    ) -> PyResult<Self> {
        let mut b = Engine::builder();
        if let Some(v) = page_size {
            b = b.page_size(extract_page_size(v)?);
        }
        if let Some(m) = margin {
            b = b.margin(m.inner);
        }
        if let Some(v) = landscape {
            b = b.landscape(v);
        }
        if let Some(t) = title {
            b = b.title(t);
        }
        if let Some(a) = author {
            b = b.author(a);
        }
        if let Some(l) = lang {
            b = b.lang(l);
        }
        if let Some(v) = bookmarks {
            b = b.bookmarks(v);
        }
        if let Some(bundle) = assets {
            b = b.assets(bundle.borrow_mut().take_inner());
        }
        Ok(Self { inner: b.build() })
    }

    #[staticmethod]
    fn builder() -> PyEngineBuilder {
        PyEngineBuilder::new()
    }

    fn render_html<'py>(&self, py: Python<'py>, html: String) -> PyResult<Bound<'py, PyBytes>> {
        // Engine: Send + Sync は fulgur-d3r で保証済み + src/lib.rs の
        // assert_impl_all! で compile time に検査している。Python スレッドから
        // 並列で render できるよう、GIL を解放してから呼ぶ。
        let bytes = py
            .allow_threads(|| self.inner.render_html(&html))
            .map_err(crate::error::map_fulgur_error)?;
        Ok(PyBytes::new_bound(py, &bytes))
    }

    fn render_html_to_file(&self, py: Python<'_>, html: String, path: PathBuf) -> PyResult<()> {
        py.allow_threads(|| self.inner.render_html_to_file(&html, &path))
            .map_err(crate::error::map_fulgur_error)
    }
}
