//! Walk a directory tree and register every `.ttf`/`.otf`/`.woff`/`.woff2`
//! file into a fresh `AssetBundle`. Used by the WPT runner to shove
//! `target/wpt/fonts/` (Ahem, CSSTest, Lato, ...) into fulgur's Parley
//! FontContext so reftests that declare `@font-face { family: "Ahem"; ... }`
//! resolve by family name instead of falling back to system fonts.

use anyhow::{Context, Result};
use fulgur::asset::AssetBundle;
use std::path::Path;

pub fn load_fonts_dir(dir: &Path) -> Result<AssetBundle> {
    let mut bundle = AssetBundle::new();
    if !dir.is_dir() {
        log::debug!(
            "load_fonts_dir: {} not a directory, returning empty bundle",
            dir.display()
        );
        return Ok(bundle);
    }
    walk(dir, &mut bundle)?;
    Ok(bundle)
}

fn walk(dir: &Path, bundle: &mut AssetBundle) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    // 決定性のため sort（Vec<Arc<Vec<u8>>> の登録順が PDF 出力に影響する可能性）
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, bundle)?;
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        if let Some("ttf" | "otf" | "woff" | "woff2") = ext.as_deref() {
            if let Err(e) = bundle.add_font_file(&path) {
                log::warn!("load_fonts_dir: skipping {}: {e}", path.display());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    /// Minimal valid TTF header: 0x00010000 magic + zero-filled rest.
    /// `AssetBundle::add_font_file` accepts Unknown/TTF/OTF/TTC bytes as-is
    /// (magic is inspected but not validated beyond format detection), so
    /// a synthetic header is enough to exercise the walker.
    fn write_fake_ttf(dir: &Path, name: &str) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(&[0x00, 0x01, 0x00, 0x00]).unwrap();
        f.write_all(&[0u8; 64]).unwrap();
    }

    #[test]
    fn missing_dir_returns_empty_bundle() {
        let bundle = load_fonts_dir(Path::new("/definitely/does/not/exist")).unwrap();
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn empty_dir_returns_empty_bundle() {
        let tmp = tempdir().unwrap();
        let bundle = load_fonts_dir(tmp.path()).unwrap();
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn loads_ttf_files() {
        let tmp = tempdir().unwrap();
        write_fake_ttf(tmp.path(), "a.ttf");
        write_fake_ttf(tmp.path(), "b.ttf");
        let bundle = load_fonts_dir(tmp.path()).unwrap();
        assert_eq!(bundle.fonts.len(), 2);
    }

    #[test]
    fn ignores_non_font_extensions() {
        let tmp = tempdir().unwrap();
        write_fake_ttf(tmp.path(), "a.ttf");
        std::fs::write(tmp.path().join("README.md"), b"ignore me").unwrap();
        std::fs::write(tmp.path().join("notes.txt"), b"also ignore").unwrap();
        let bundle = load_fonts_dir(tmp.path()).unwrap();
        assert_eq!(bundle.fonts.len(), 1);
    }

    #[test]
    fn recurses_into_subdirs() {
        let tmp = tempdir().unwrap();
        let sub = tmp.path().join("CSSTest");
        std::fs::create_dir(&sub).unwrap();
        write_fake_ttf(tmp.path(), "Ahem.ttf");
        write_fake_ttf(&sub, "csstest-ascii.ttf");
        let bundle = load_fonts_dir(tmp.path()).unwrap();
        assert_eq!(bundle.fonts.len(), 2);
    }
}
