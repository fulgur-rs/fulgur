use fulgur::{Engine, EngineBuilder, PageSize};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

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
        other => Err(PyValueError::new_err(format!(
            "unknown page size: {other}"
        ))),
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
        let size = if let Ok(ps) = value.extract::<PyPageSize>() {
            ps.inner
        } else if let Ok(s) = value.extract::<String>() {
            parse_page_size_str(&s)?
        } else {
            return Err(PyValueError::new_err("page_size must be PageSize or str"));
        };
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
    #[staticmethod]
    fn builder() -> PyEngineBuilder {
        PyEngineBuilder::new()
    }
}
