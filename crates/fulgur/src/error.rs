#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTML parse error: {0}")]
    HtmlParse(String),

    #[error("Layout error: {0}")]
    Layout(String),

    #[error("PDF generation error: {0}")]
    PdfGeneration(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Asset error: {0}")]
    Asset(String),
}

pub type Result<T> = std::result::Result<T, Error>;
