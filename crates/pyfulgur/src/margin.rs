use fulgur::Margin;
use pyo3::prelude::*;

#[pyclass(name = "Margin", module = "pyfulgur", frozen)]
#[derive(Clone, Copy)]
pub struct PyMargin {
    pub(crate) inner: Margin,
}

#[pymethods]
impl PyMargin {
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

    #[staticmethod]
    fn uniform(pt: f32) -> Self {
        Self {
            inner: Margin::uniform(pt),
        }
    }

    #[staticmethod]
    #[pyo3(signature = (vertical, horizontal))]
    fn symmetric(vertical: f32, horizontal: f32) -> Self {
        Self {
            inner: Margin::symmetric(vertical, horizontal),
        }
    }

    #[staticmethod]
    fn uniform_mm(mm: f32) -> Self {
        Self {
            inner: Margin::uniform_mm(mm),
        }
    }

    #[getter]
    fn top(&self) -> f32 {
        self.inner.top
    }

    #[getter]
    fn right(&self) -> f32 {
        self.inner.right
    }

    #[getter]
    fn bottom(&self) -> f32 {
        self.inner.bottom
    }

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
