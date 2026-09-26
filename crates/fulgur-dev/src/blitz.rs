use fulgur_core::Result;
use std::path::Path;

pub(super) fn render(input: &Path) -> Result<Vec<u8>> {
    let html = std::fs::read_to_string(input)?;
    let base_path = input
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fulgur_blitz::Engine::builder()
        .base_path(base_path)
        .build()
        .render(&html)
}

#[cfg(test)]
mod tests;
