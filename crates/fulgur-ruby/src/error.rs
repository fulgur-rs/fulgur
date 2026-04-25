//! Fulgur::Error → Ruby 例外 への写像。
//!
//! Ruby 側の `Fulgur::Error` / `Fulgur::RenderError` / `Fulgur::AssetError` クラスは
//! `lib/fulgur.rb` で既に定義済み。このモジュールでは lookup のみ行い、
//! `fulgur::Error` の variant に応じて適切な Ruby 例外に変換する。

use fulgur::Error as FulgurError;
use magnus::{Error, ExceptionClass, Module, RModule, Ruby, exception};

/// Ruby 側の `Fulgur::<name>` 例外クラスを lookup する。
fn class(ruby: &Ruby, name: &str) -> Result<ExceptionClass, Error> {
    let fulgur = ruby.class_object().const_get::<_, RModule>("Fulgur")?;
    fulgur.const_get::<_, ExceptionClass>(name)
}

/// `fulgur::Error` を Ruby の `magnus::Error` に変換する。
///
/// - `Io(NotFound)` → `Errno::ENOENT`
/// - `Io(_)` / `WoffDecode` / `HtmlParse` / `Layout` / `PdfGeneration` / `Template` → `Fulgur::RenderError`
/// - `Asset` / `UnsupportedFontFormat` → `Fulgur::AssetError`
///
/// lookup に失敗した場合は `RuntimeError` にフォールバックする。
pub fn map_fulgur_error(ruby: &Ruby, err: FulgurError) -> Error {
    match err {
        FulgurError::Io(io_err) => match io_err.kind() {
            std::io::ErrorKind::NotFound => {
                let errno = ruby
                    .class_object()
                    .const_get::<_, RModule>("Errno")
                    .and_then(|m| m.const_get::<_, ExceptionClass>("ENOENT"))
                    .unwrap_or_else(|_| exception::runtime_error());
                Error::new(errno, io_err.to_string())
            }
            _ => render_error(ruby, io_err.to_string()),
        },
        FulgurError::Asset(msg) | FulgurError::UnsupportedFontFormat(msg) => asset_error(ruby, msg),
        FulgurError::WoffDecode(msg)
        | FulgurError::HtmlParse(msg)
        | FulgurError::Layout(msg)
        | FulgurError::PdfGeneration(msg)
        | FulgurError::Template(msg)
        | FulgurError::Other(msg) => render_error(ruby, msg),
    }
}

fn render_error(ruby: &Ruby, msg: String) -> Error {
    match class(ruby, "RenderError") {
        Ok(c) => Error::new(c, msg),
        Err(_) => Error::new(exception::runtime_error(), msg),
    }
}

fn asset_error(ruby: &Ruby, msg: String) -> Error {
    match class(ruby, "AssetError") {
        Ok(c) => Error::new(c, msg),
        Err(_) => Error::new(exception::runtime_error(), msg),
    }
}
