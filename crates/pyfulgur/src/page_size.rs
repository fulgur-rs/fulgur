use fulgur::PageSize;
use pyo3::prelude::*;

/// Page size with dimensions in millimeters.
///
/// Use the predefined class attributes ``A4``, ``LETTER``, or ``A3``, or
/// :meth:`custom` for arbitrary sizes. ``PageSize`` is immutable.
///
/// Example:
///     >>> from pyfulgur import PageSize
///     >>> a4 = PageSize.A4
///     >>> custom = PageSize.custom(210.0, 297.0)
///     >>> landscape = a4.landscape()
#[pyclass(name = "PageSize", module = "pyfulgur", frozen, from_py_object)]
#[derive(Clone, Copy)]
pub struct PyPageSize {
    pub(crate) inner: PageSize,
}

#[pymethods]
impl PyPageSize {
    #[classattr]
    const A4: PyPageSize = PyPageSize {
        inner: PageSize::A4,
    };

    #[classattr]
    const LETTER: PyPageSize = PyPageSize {
        inner: PageSize::LETTER,
    };

    #[classattr]
    const A3: PyPageSize = PyPageSize {
        inner: PageSize::A3,
    };

    /// Create a page size with arbitrary dimensions.
    ///
    /// Args:
    ///     width_mm: Page width in millimeters.
    ///     height_mm: Page height in millimeters.
    ///
    /// Returns:
    ///     A new ``PageSize`` instance.
    #[staticmethod]
    fn custom(width_mm: f32, height_mm: f32) -> Self {
        Self {
            inner: PageSize::custom(width_mm, height_mm),
        }
    }

    /// Return a new ``PageSize`` with width and height swapped.
    ///
    /// Returns:
    ///     A new ``PageSize`` rotated 90 degrees from this one.
    fn landscape(&self) -> Self {
        Self {
            inner: self.inner.landscape(),
        }
    }

    /// Page width in millimeters.
    #[getter]
    fn width(&self) -> f32 {
        self.inner.width
    }

    /// Page height in millimeters.
    #[getter]
    fn height(&self) -> f32 {
        self.inner.height
    }

    fn __repr__(&self) -> String {
        format!(
            "PageSize(width={:.2}, height={:.2})",
            self.inner.width, self.inner.height
        )
    }
}
