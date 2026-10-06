use fulgur_core::{AssetBundle, Config, Result};
use std::path::Path;

pub(super) fn render(
    input: &Path,
    config: &Config,
    assets: Option<&AssetBundle>,
    system_fonts: bool,
) -> Result<Vec<u8>> {
    let html = std::fs::read_to_string(input)?;
    let base_path = input
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut builder = fulgur_blitz::Engine::builder()
        .base_path(base_path)
        .system_fonts(system_fonts);
    if config.overrides.page_size {
        builder = builder.page_size(config.page_size);
    }
    if config.overrides.margin {
        builder = builder.margin(config.margin);
    }
    if config.overrides.landscape {
        builder = builder.landscape(config.landscape);
    }
    if let Some(assets) = assets {
        builder = builder.assets(assets.clone());
    }
    if let Some(value) = &config.title {
        builder = builder.title(value);
    }
    if let Some(value) = &config.description {
        builder = builder.description(value);
    }
    if let Some(value) = &config.lang {
        builder = builder.lang(value);
    }
    if let Some(value) = &config.creator {
        builder = builder.creator(value);
    }
    if let Some(value) = &config.producer {
        builder = builder.producer(value);
    }
    if let Some(value) = &config.creation_date {
        builder = builder.creation_date(value);
    }
    builder = builder
        .authors(config.authors.iter())
        .keywords(config.keywords.iter());
    builder.bookmarks(config.bookmarks).build().render(&html)
}

#[cfg(test)]
mod tests;
