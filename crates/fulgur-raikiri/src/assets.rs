use crate::RenderOptions;
use fulgur_core::{Error, Result};
use raikiri_html::{FontCollectionBuilder, RenderFonts};
use skrifa::{
    MetadataProvider,
    raw::{FileRef, types::NameId},
};
use std::sync::Arc;

pub(super) fn fonts(options: &RenderOptions<'_>) -> Result<Option<RenderFonts>> {
    let Some(bundle) = options.assets.filter(|bundle| !bundle.fonts.is_empty()) else {
        return if options.system_fonts {
            Ok(None)
        } else {
            Err(Error::Asset(
                "disabling system fonts requires a bundled font".into(),
            ))
        };
    };
    let mut builder = FontCollectionBuilder::new().system_fonts(options.system_fonts);
    for data in &bundle.fonts {
        let bytes: Arc<[u8]> = Arc::from(data.as_slice());
        for family in family_names(&bytes)? {
            builder = builder.font_bytes(family, bytes.clone());
        }
    }
    builder
        .build()
        .map(Some)
        .map_err(|error| Error::Asset(error.to_string()))
}

fn family_names(bytes: &[u8]) -> Result<Vec<String>> {
    let file = FileRef::new(bytes)
        .map_err(|error| Error::Asset(format!("invalid bundled font: {error}")))?;
    if matches!(file, FileRef::Collection(_)) {
        return Err(Error::Asset(
            "font collections are not supported by the current Raikiri font API".into(),
        ));
    }
    let mut families = Vec::new();
    for font in file.fonts() {
        let font =
            font.map_err(|error| Error::Asset(format!("invalid bundled font face: {error}")))?;
        let family = font
            .localized_strings(NameId::TYPOGRAPHIC_FAMILY_NAME)
            .english_or_first()
            .or_else(|| {
                font.localized_strings(NameId::FAMILY_NAME)
                    .english_or_first()
            })
            .map(|value| value.to_string())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| Error::Asset("bundled font has no family name".into()))?;
        if !families.contains(&family) {
            families.push(family);
        }
    }
    if families.is_empty() {
        return Err(Error::Asset("bundled font has no faces".into()));
    }
    Ok(families)
}
