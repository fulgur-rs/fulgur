use fulgur::Error as FulgurError;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyFileNotFoundError, PyValueError};
use pyo3::prelude::*;

create_exception!(pyfulgur, RenderError, PyException, "Rendering failed");

pub fn map_fulgur_error(err: FulgurError) -> PyErr {
    match err {
        FulgurError::Io(io_err) => match io_err.kind() {
            std::io::ErrorKind::NotFound => PyFileNotFoundError::new_err(io_err.to_string()),
            _ => RenderError::new_err(io_err.to_string()),
        },
        FulgurError::Asset(msg) => PyValueError::new_err(msg),
        FulgurError::UnsupportedFontFormat(msg) => PyValueError::new_err(msg),
        FulgurError::WoffDecode(msg) => RenderError::new_err(msg),
        FulgurError::HtmlParse(msg)
        | FulgurError::Layout(msg)
        | FulgurError::PdfGeneration(msg)
        | FulgurError::Template(msg)
        | FulgurError::Other(msg) => RenderError::new_err(msg),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("RenderError", m.py().get_type::<RenderError>())?;
    Ok(())
}
