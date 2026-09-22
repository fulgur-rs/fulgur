use fulgur::PageSize;
use pyo3::prelude::*;

/// A page size.
///
/// Use one of the predefined class attributes (``A3``, ``A4``, ``A5``,
/// ``B4``, ``B5``, ``JIS_B4``, ``JIS_B5``, ``LETTER``, ``LEGAL``,
/// ``LEDGER``) covering the CSS Paged Media Level 3 page-size keyword set,
/// or `custom` for arbitrary sizes (given in millimeters). ``width`` and
/// ``height`` return the resolved size in PDF points (1 pt = 1/72 inch),
/// not millimeters. ``PageSize`` is immutable.
///
/// Example:
///     ```python
///     from pyfulgur import PageSize
///     a4 = PageSize.A4
///     custom = PageSize.custom(210.0, 297.0)
///     landscape = a4.landscape()
///     ```
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

    #[classattr]
    const A5: PyPageSize = PyPageSize {
        inner: PageSize::A5,
    };

    #[classattr]
    const B4: PyPageSize = PyPageSize {
        inner: PageSize::B4,
    };

    #[classattr]
    const B5: PyPageSize = PyPageSize {
        inner: PageSize::B5,
    };

    #[classattr]
    const JIS_B4: PyPageSize = PyPageSize {
        inner: PageSize::JIS_B4,
    };

    #[classattr]
    const JIS_B5: PyPageSize = PyPageSize {
        inner: PageSize::JIS_B5,
    };

    #[classattr]
    const LEGAL: PyPageSize = PyPageSize {
        inner: PageSize::LEGAL,
    };

    #[classattr]
    const LEDGER: PyPageSize = PyPageSize {
        inner: PageSize::LEDGER,
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

    /// Page width in PDF points (1 pt = 1/72 inch).
    #[getter]
    fn width(&self) -> f32 {
        self.inner.width
    }

    /// Page height in PDF points (1 pt = 1/72 inch).
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
