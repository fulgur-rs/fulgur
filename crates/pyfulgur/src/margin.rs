use fulgur::Margin;
use pyo3::prelude::*;

/// Page margin in points (PDF pt).
///
/// Margins apply uniformly to every generated page. Use the constructor for
/// explicit per-side values, or one of the helper factories
/// (`uniform`, `symmetric`, `uniform_mm`).
///
/// Example:
///     ```python
///     from pyfulgur import Margin
///     Margin(36.0, 36.0, 36.0, 36.0)
///     Margin.uniform_mm(20.0)
///     ```
#[pyclass(name = "Margin", module = "pyfulgur", frozen, from_py_object)]
#[derive(Clone, Copy)]
pub struct PyMargin {
    pub(crate) inner: Margin,
}

#[pymethods]
impl PyMargin {
    /// Args:
    ///     top: Top margin in points.
    ///     right: Right margin in points.
    ///     bottom: Bottom margin in points.
    ///     left: Left margin in points.
    #[new]
    #[pyo3(signature = (top, right, bottom, left))]
    fn new(top: f32, right: f32, bottom: f32, left: f32) -> Self {
        Self {
            inner: Margin {
                top,
                right,
                bottom,
                left,
            },
        }
    }

    /// Create a uniform margin (the same value on all four sides).
    ///
    /// Args:
    ///     pt: Margin value in points (PDF pt).
    ///
    /// Returns:
    ///     A new ``Margin`` with all sides set to ``pt``.
    #[staticmethod]
    fn uniform(pt: f32) -> Self {
        Self {
            inner: Margin::uniform(pt),
        }
    }

    /// Create a symmetric margin (vertical and horizontal pairs).
    ///
    /// Args:
    ///     vertical: Top and bottom margin in points.
    ///     horizontal: Left and right margin in points.
    ///
    /// Returns:
    ///     A new ``Margin`` with the given symmetric values.
    #[staticmethod]
    #[pyo3(signature = (vertical, horizontal))]
    fn symmetric(vertical: f32, horizontal: f32) -> Self {
        Self {
            inner: Margin::symmetric(vertical, horizontal),
        }
    }

    /// Create a uniform margin specified in millimeters.
    ///
    /// Args:
    ///     mm: Margin value in millimeters.
    ///
    /// Returns:
    ///     A new ``Margin`` with all sides set to the equivalent point value.
    #[staticmethod]
    fn uniform_mm(mm: f32) -> Self {
        Self {
            inner: Margin::uniform_mm(mm),
        }
    }

    /// Top margin in points.
    #[getter]
    fn top(&self) -> f32 {
        self.inner.top
    }

    /// Right margin in points.
    #[getter]
    fn right(&self) -> f32 {
        self.inner.right
    }

    /// Bottom margin in points.
    #[getter]
    fn bottom(&self) -> f32 {
        self.inner.bottom
    }

    /// Left margin in points.
    #[getter]
    fn left(&self) -> f32 {
        self.inner.left
    }

    fn __repr__(&self) -> String {
        format!(
            "Margin(top={:.2}, right={:.2}, bottom={:.2}, left={:.2})",
            self.inner.top, self.inner.right, self.inner.bottom, self.inner.left
        )
    }
}
