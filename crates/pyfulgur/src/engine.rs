use std::path::PathBuf;

use fulgur::{Engine, EngineBuilder, PageSize};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::asset_bundle::PyAssetBundle;
use crate::margin::PyMargin;
use crate::page_size::PyPageSize;

/// Builder for configuring and constructing an :class:`Engine`.
///
/// All setters return ``self`` to support method chaining. Each builder
/// instance can be ``build()`` ed only once; subsequent calls raise
/// ``RuntimeError``.
///
/// Example:
///     >>> from pyfulgur import Engine, PageSize, Margin
///     >>> engine = (
///     ...     Engine.builder()
///     ...     .page_size(PageSize.A4)
///     ...     .margin(Margin.uniform_mm(20.0))
///     ...     .title("Doc")
///     ...     .build()
///     ... )
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
    /// Create a new builder seeded with default configuration.
    #[new]
    fn new() -> Self {
        Self {
            inner: Some(Engine::builder()),
        }
    }

    /// Set the page size.
    ///
    /// Args:
    ///     value: Either a :class:`PageSize` instance or a string name
    ///         (``"A4"``, ``"LETTER"``, ``"A3"``; case-insensitive).
    ///
    /// Returns:
    ///     ``self`` for method chaining.
    ///
    /// Raises:
    ///     ValueError: When ``value`` is an unknown size name or wrong type.
    fn page_size(mut slf: PyRefMut<'_, Self>, value: &Bound<'_, PyAny>) -> PyResult<Py<Self>> {
        let size = extract_page_size(value)?;
        slf.map(|b| b.page_size(size))?;
        Ok(slf.into())
    }

    /// Set the page margins.
    ///
    /// Args:
    ///     margin: A :class:`Margin` instance.
    ///
    /// Returns:
    ///     ``self`` for method chaining.
    fn margin(mut slf: PyRefMut<'_, Self>, margin: PyMargin) -> PyResult<Py<Self>> {
        slf.map(|b| b.margin(margin.inner))?;
        Ok(slf.into())
    }

    /// Toggle landscape orientation.
    ///
    /// Args:
    ///     value: ``True`` to render in landscape, ``False`` for portrait.
    ///
    /// Returns:
    ///     ``self`` for method chaining.
    fn landscape(mut slf: PyRefMut<'_, Self>, value: bool) -> PyResult<Py<Self>> {
        slf.map(|b| b.landscape(value))?;
        Ok(slf.into())
    }

    /// Set the PDF document title metadata.
    ///
    /// Args:
    ///     value: Title string written to the PDF metadata.
    ///
    /// Returns:
    ///     ``self`` for method chaining.
    fn title(mut slf: PyRefMut<'_, Self>, value: String) -> PyResult<Py<Self>> {
        slf.map(|b| b.title(value))?;
        Ok(slf.into())
    }

    /// Set the PDF document author metadata.
    ///
    /// Args:
    ///     value: Author string written to the PDF metadata.
    ///
    /// Returns:
    ///     ``self`` for method chaining.
    fn author(mut slf: PyRefMut<'_, Self>, value: String) -> PyResult<Py<Self>> {
        slf.map(|b| b.author(value))?;
        Ok(slf.into())
    }

    /// Set the document language tag.
    ///
    /// Args:
    ///     value: BCP 47 language tag (e.g. ``"en"``, ``"ja"``).
    ///
    /// Returns:
    ///     ``self`` for method chaining.
    fn lang(mut slf: PyRefMut<'_, Self>, value: String) -> PyResult<Py<Self>> {
        slf.map(|b| b.lang(value))?;
        Ok(slf.into())
    }

    /// Toggle PDF bookmark generation.
    ///
    /// Args:
    ///     value: ``True`` to generate bookmarks from heading hierarchy.
    ///
    /// Returns:
    ///     ``self`` for method chaining.
    fn bookmarks(mut slf: PyRefMut<'_, Self>, value: bool) -> PyResult<Py<Self>> {
        slf.map(|b| b.bookmarks(value))?;
        Ok(slf.into())
    }

    /// Attach an :class:`AssetBundle` of CSS, fonts, and images.
    ///
    /// The bundle is consumed and reset to an empty state so it can be
    /// reused for a new build cycle.
    ///
    /// Args:
    ///     bundle: The :class:`AssetBundle` to attach.
    ///
    /// Returns:
    ///     ``self`` for method chaining.
    fn assets(
        mut slf: PyRefMut<'_, Self>,
        bundle: &Bound<'_, PyAssetBundle>,
    ) -> PyResult<Py<Self>> {
        let taken = bundle.borrow_mut().take_inner();
        slf.map(|b| b.assets(taken))?;
        Ok(slf.into())
    }

    /// Finalize the configuration and build an :class:`Engine`.
    ///
    /// Returns:
    ///     A new :class:`Engine` ready to render HTML.
    ///
    /// Raises:
    ///     RuntimeError: When the builder has already been consumed.
    fn build(&mut self) -> PyResult<PyEngine> {
        let b = self.take()?;
        Ok(PyEngine { inner: b.build() })
    }
}

/// HTML/CSS to PDF conversion engine.
///
/// fulgur converts HTML, CSS, and SVG into a PDF document deterministically
/// and offline. All assets must be explicitly bundled via
/// :class:`AssetBundle`; no network fetches are performed.
///
/// Example:
///     >>> from pyfulgur import Engine, PageSize
///     >>> engine = Engine(page_size=PageSize.A4, title="Hello")
///     >>> pdf = engine.render_html("<h1>Hi</h1>")
///     >>> assert pdf.startswith(b"%PDF")
#[pyclass(name = "Engine", module = "pyfulgur")]
pub struct PyEngine {
    pub(crate) inner: Engine,
}

#[pymethods]
impl PyEngine {
    /// Construct an engine directly with keyword arguments.
    ///
    /// All arguments are keyword-only and optional. For more granular
    /// configuration use :meth:`builder`.
    ///
    /// Args:
    ///     page_size: Page dimensions, either a :class:`PageSize` or a
    ///         case-insensitive string name (``"A4"``, ``"LETTER"``,
    ///         ``"A3"``).
    ///     margin: Page margins.
    ///     landscape: ``True`` for landscape orientation.
    ///     title: PDF title metadata.
    ///     author: PDF author metadata.
    ///     lang: BCP 47 language tag.
    ///     bookmarks: ``True`` to generate bookmarks from heading hierarchy.
    ///     assets: An :class:`AssetBundle` of CSS, fonts, and images.
    ///
    /// Raises:
    ///     ValueError: When ``page_size`` is an unknown name.
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

    /// Return a fresh :class:`EngineBuilder` for fluent configuration.
    ///
    /// Returns:
    ///     A new :class:`EngineBuilder`.
    #[staticmethod]
    fn builder() -> PyEngineBuilder {
        PyEngineBuilder::new()
    }

    /// Render HTML to PDF bytes.
    ///
    /// The Python GIL is released for the duration of the render so multiple
    /// threads can render in parallel.
    ///
    /// Args:
    ///     html: HTML source string.
    ///
    /// Returns:
    ///     PDF bytes (always start with ``b"%PDF"``).
    ///
    /// Raises:
    ///     RenderError: When parsing, layout, or PDF generation fails.
    fn render_html<'py>(&self, py: Python<'py>, html: String) -> PyResult<Bound<'py, PyBytes>> {
        // Engine: Send + Sync は fulgur-d3r で保証済み + src/lib.rs の
        // assert_impl_all! で compile time に検査している。Python スレッドから
        // 並列で render できるよう、GIL を解放してから呼ぶ。
        let bytes = py
            .detach(|| self.inner.render_html(&html))
            .map_err(crate::error::map_fulgur_error)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Render HTML and write the PDF to a file.
    ///
    /// The GIL is released for the duration of the render.
    ///
    /// Args:
    ///     html: HTML source string.
    ///     path: Filesystem path to write the PDF to.
    ///
    /// Raises:
    ///     RenderError: When rendering or writing fails.
    ///     FileNotFoundError: When the parent directory does not exist.
    fn render_html_to_file(&self, py: Python<'_>, html: String, path: PathBuf) -> PyResult<()> {
        py.detach(|| self.inner.render_html_to_file(&html, &path))
            .map_err(crate::error::map_fulgur_error)
    }
}
