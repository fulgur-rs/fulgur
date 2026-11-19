//! Metric-compatible substitutes for common proprietary font families.
//!
//! A page that asks for `font-family: Arial` on a system without Arial should
//! get Liberation Sans (or Arimo): the same glyph widths, so lines break and
//! pages end where the author expected. fontconfig does this with the
//! `binding="same"` aliases in `30-metric-aliases.conf`, and engines that
//! match fonts through fontconfig (wkhtmltopdf, Chromium) inherit them.
//! fontique asks fontconfig only for generic families and script fallbacks;
//! a named family it cannot find is skipped, and the text falls through to
//! the next family in the list or the default font.
//!
//! [`register_metric_aliases`] registers the files of an installed
//! substitute under the missing family's name. Only these metric-compatible
//! pairs are aliased, and only when the family itself is not installed: an
//! arbitrary unknown family must still fall through to the next one in the
//! CSS list, as in browsers, rather than to fontconfig's default match.

use parley::fontique::{Blob, Collection, FontInfoOverride, SourceKind};
use std::path::PathBuf;
use std::sync::Arc;

/// Each family and its metric-compatible substitutes, in preference order
/// (as in fontconfig's `30-metric-aliases.conf`).
const METRIC_ALIASES: &[(&str, &[&str])] = &[
    (
        "Arial",
        &["Liberation Sans", "Arimo", "Albany AMT", "Nimbus Sans"],
    ),
    (
        "Helvetica",
        &["Nimbus Sans", "TeX Gyre Heros", "Liberation Sans", "Arimo"],
    ),
    (
        "Arial Narrow",
        &["Liberation Sans Narrow", "Nimbus Sans Narrow"],
    ),
    (
        "Times New Roman",
        &["Liberation Serif", "Tinos", "Thorndale AMT", "Nimbus Roman"],
    ),
    (
        "Times",
        &[
            "Nimbus Roman",
            "TeX Gyre Termes",
            "Liberation Serif",
            "Tinos",
        ],
    ),
    (
        "Courier New",
        &[
            "Liberation Mono",
            "Cousine",
            "Cumberland AMT",
            "Nimbus Mono PS",
        ],
    ),
    (
        "Courier",
        &[
            "Nimbus Mono PS",
            "TeX Gyre Cursor",
            "Liberation Mono",
            "Cousine",
        ],
    ),
    ("Calibri", &["Carlito"]),
    ("Cambria", &["Caladea"]),
    ("Georgia", &["Gelasio"]),
];

/// Register an installed metric-compatible substitute under the name of each
/// family in [`METRIC_ALIASES`] that `collection` does not have. Font files
/// are memory-mapped, as fontique maps system fonts, so an unused alias costs
/// no memory beyond the mapping.
pub(crate) fn register_metric_aliases(collection: &mut Collection) {
    for &(family, substitutes) in METRIC_ALIASES {
        if collection.family_id(family).is_some() {
            continue;
        }
        let Some(substitute) = substitutes
            .iter()
            .find_map(|name| collection.family_by_name(name))
        else {
            continue;
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut blobs: Vec<Blob<u8>> = Vec::new();
        for font in substitute.fonts() {
            match font.source().kind() {
                SourceKind::Path(path) => {
                    if !paths.iter().any(|p| p.as_path() == path.as_ref()) {
                        paths.push(path.to_path_buf());
                    }
                }
                SourceKind::Memory(blob) => {
                    if !blobs.iter().any(|b| b.id() == blob.id()) {
                        blobs.push(blob.clone());
                    }
                }
            }
        }
        for path in paths {
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            // SAFETY: as in fontique's own source cache, the font file is
            // mapped read-only and assumed not to change while in use.
            let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else {
                continue;
            };
            blobs.push(Blob::new(Arc::new(map)));
        }
        let rename = FontInfoOverride {
            family_name: Some(family),
            ..Default::default()
        };
        for blob in blobs {
            collection.register_fonts(blob, Some(rename));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parley::fontique::CollectionOptions;

    /// A family registered only under its own name is reachable under every
    /// alias that lists it, and an installed family is never shadowed.
    #[test]
    fn missing_families_resolve_to_a_registered_substitute() {
        let mut collection = Collection::new(CollectionOptions {
            system_fonts: false,
            ..Default::default()
        });
        let font = crate::test_fonts::noto_sans_regular_ttf();
        collection.register_fonts(
            Blob::new(font.clone()),
            Some(FontInfoOverride {
                family_name: Some("Liberation Sans"),
                ..Default::default()
            }),
        );
        collection.register_fonts(
            Blob::new(font),
            Some(FontInfoOverride {
                family_name: Some("Helvetica"),
                ..Default::default()
            }),
        );
        let helvetica = collection.family_id("Helvetica");

        register_metric_aliases(&mut collection);

        let arial = collection.family_by_name("Arial").expect("Arial aliased");
        assert_eq!(arial.fonts().len(), 1);
        assert_eq!(collection.family_id("Helvetica"), helvetica);
        assert!(collection.family_id("Times New Roman").is_none());
    }
}
