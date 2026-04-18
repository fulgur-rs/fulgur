use fulgur::PageSize;
use pyo3::prelude::*;

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

    #[staticmethod]
    fn custom(width_mm: f32, height_mm: f32) -> Self {
        Self {
            inner: PageSize::custom(width_mm, height_mm),
        }
    }

    fn landscape(&self) -> Self {
        Self {
            inner: self.inner.landscape(),
        }
    }

    #[getter]
    fn width(&self) -> f32 {
        self.inner.width
    }

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
