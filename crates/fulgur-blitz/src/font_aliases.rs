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
//!
//! An application can add its own substitutes for the whole process with
//! [`set_font_substitutes`]: families that are not metric-compatible, and
//! the fonts of generic families (to resolve `serif` and `sans-serif` to
//! Times New Roman and Arial as browsers do, rather than to fontconfig's
//! choice).

use parley::fontique::{Blob, Collection, FontInfoOverride, GenericFamily, SourceKind};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// Font substitutes an application sets for every document it renders (see
/// [`set_font_substitutes`]).
#[derive(Debug, Clone, Default)]
pub struct FontSubstitutes {
    /// Each family and the installed families to use under its name when it
    /// is missing, in preference order. Applied after the metric-compatible
    /// aliases, so a substitute may itself be an alias (`"Arial"`).
    pub families: Vec<(String, Vec<String>)>,
    /// Each generic family (`"cursive"`, `"fantasy"`, ...) and the families
    /// to use for it instead of fontconfig's choice, in preference order.
    pub generic_families: Vec<(String, Vec<String>)>,
}

static SUBSTITUTES: OnceLock<FontSubstitutes> = OnceLock::new();

/// Sets the font substitutes for every document rendered afterwards in this
/// process. They can be set once; a second call returns the rejected value.
pub fn set_font_substitutes(substitutes: FontSubstitutes) -> Result<(), FontSubstitutes> {
    SUBSTITUTES.set(substitutes)
}

/// Each family and its metric-compatible substitutes, in preference order:
/// the pairs of fontconfig's `30-metric-aliases.conf`, in the order Chromium
/// prefers them. Chromium accepts only the Liberation and Croscore families
/// (Arimo, Tinos, Cousine) for Helvetica, Times and Courier as well, so a page
/// asking for Helvetica gets Liberation Sans there rather than fontconfig's
/// Nimbus Sans, whose taller lines move page breaks.
const METRIC_ALIASES: &[(&str, &[&str])] = &[
    (
        "Arial",
        &["Liberation Sans", "Arimo", "Albany AMT", "Nimbus Sans"],
    ),
    (
        "Helvetica",
        &["Liberation Sans", "Arimo", "Nimbus Sans", "TeX Gyre Heros"],
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
            "Liberation Serif",
            "Tinos",
            "Nimbus Roman",
            "TeX Gyre Termes",
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
            "Liberation Mono",
            "Cousine",
            "Nimbus Mono PS",
            "TeX Gyre Cursor",
        ],
    ),
    ("Calibri", &["Carlito"]),
    ("Cambria", &["Caladea"]),
    ("Georgia", &["Gelasio"]),
];

/// Register an installed metric-compatible substitute under the name of each
/// family in [`METRIC_ALIASES`] that `collection` does not have, then apply
/// the substitutes set with [`set_font_substitutes`]. Font files are
/// memory-mapped, as fontique maps system fonts, so an unused alias costs no
/// memory beyond the mapping.
pub(crate) fn register_metric_aliases(collection: &mut Collection) {
    for &(family, substitutes) in METRIC_ALIASES {
        alias(collection, family, substitutes.iter().copied());
    }
    if let Some(extra) = SUBSTITUTES.get() {
        apply_substitutes(collection, extra);
    }
}

fn apply_substitutes(collection: &mut Collection, substitutes: &FontSubstitutes) {
    for (family, list) in &substitutes.families {
        alias(collection, family, list.iter().map(String::as_str));
    }
    for (generic, list) in &substitutes.generic_families {
        let Some(generic) = GenericFamily::parse(generic) else {
            continue;
        };
        if let Some(id) = list.iter().find_map(|name| collection.family_id(name)) {
            collection.set_generic_families(generic, std::iter::once(id));
        }
    }
}

/// Registers the fonts of the first installed family of `substitutes` under
/// the name `family`, unless `collection` has `family` itself.
fn alias<'a>(
    collection: &mut Collection,
    family: &str,
    mut substitutes: impl Iterator<Item = &'a str>,
) {
    if collection.family_id(family).is_some() {
        return;
    }
    let Some(substitute) = substitutes.find_map(|name| collection.family_by_name(name)) else {
        return;
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

    /// Application substitutes: a missing family takes an installed one (an
    /// alias included), and a generic family is pointed at a named one.
    #[test]
    fn application_substitutes_apply_after_the_metric_aliases() {
        let mut collection = Collection::new(CollectionOptions {
            system_fonts: false,
            ..Default::default()
        });
        collection.register_fonts(
            Blob::new(crate::test_fonts::noto_sans_regular_ttf()),
            Some(FontInfoOverride {
                family_name: Some("Liberation Sans"),
                ..Default::default()
            }),
        );
        register_metric_aliases(&mut collection);
        apply_substitutes(
            &mut collection,
            &FontSubstitutes {
                families: vec![("Tahoma".into(), vec!["Missing".into(), "Arial".into()])],
                generic_families: vec![
                    ("cursive".into(), vec!["Arial".into()]),
                    ("no-such-generic".into(), vec!["Arial".into()]),
                ],
            },
        );
        let tahoma = collection.family_by_name("Tahoma").expect("Tahoma aliased");
        assert_eq!(tahoma.fonts().len(), 1);
        let arial = collection.family_id("Arial").expect("Arial aliased");
        assert_eq!(
            collection
                .generic_families(GenericFamily::Cursive)
                .collect::<Vec<_>>(),
            vec![arial]
        );
    }
}
