use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct Tolerance {
    pub max_channel_diff: u8,
    pub max_diff_pixels_ratio: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Defaults {
    #[serde(default = "default_page_size")]
    pub page_size: String,
    #[serde(default = "default_dpi")]
    pub dpi: u32,
    pub tolerance_chrome: Tolerance,
}

fn default_page_size() -> String {
    "A4".to_string()
}

fn default_dpi() -> u32 {
    150
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureRow {
    pub path: String,
    pub tolerance_chrome: Option<Tolerance>,
    pub page_size: Option<String>,
    pub dpi: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawManifest {
    pub defaults: Defaults,
    #[serde(rename = "fixture", default)]
    pub fixtures: Vec<FixtureRow>,
}

/// Fully resolved fixture with defaults applied.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub path: PathBuf,
    pub page_size: String,
    pub dpi: u32,
    pub tolerance_chrome: Tolerance,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub fixtures: Vec<Fixture>,
}

impl Manifest {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::from_toml(&text)
    }

    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        let raw: RawManifest = toml::from_str(text)?;
        let fixtures = raw
            .fixtures
            .into_iter()
            .map(|row| Fixture {
                path: PathBuf::from(&row.path),
                page_size: row
                    .page_size
                    .unwrap_or_else(|| raw.defaults.page_size.clone()),
                dpi: row.dpi.unwrap_or(raw.defaults.dpi),
                tolerance_chrome: row
                    .tolerance_chrome
                    .unwrap_or(raw.defaults.tolerance_chrome),
            })
            .collect();
        Ok(Self { fixtures })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[defaults]
page_size = "A4"
dpi = 150
tolerance_chrome = { max_channel_diff = 16, max_diff_pixels_ratio = 0.02 }

[[fixture]]
path = "basic/solid-box.html"

[[fixture]]
path = "layout/grid-simple.html"
tolerance_chrome = { max_channel_diff = 24, max_diff_pixels_ratio = 0.03 }
"#;

    #[test]
    fn parses_defaults_and_inherits() {
        let m = Manifest::from_toml(SAMPLE).expect("parse");
        assert_eq!(m.fixtures.len(), 2);
        let solid = &m.fixtures[0];
        assert_eq!(solid.path, PathBuf::from("basic/solid-box.html"));
        assert_eq!(solid.dpi, 150);
        assert_eq!(solid.page_size, "A4");
        assert_eq!(solid.tolerance_chrome.max_channel_diff, 16);
    }

    #[test]
    fn fixture_override_wins_over_defaults() {
        let m = Manifest::from_toml(SAMPLE).expect("parse");
        let grid = &m.fixtures[1];
        assert_eq!(grid.tolerance_chrome.max_channel_diff, 24);
    }

    #[test]
    fn rejects_missing_defaults_section() {
        let bad = "[[fixture]]\npath = \"a.html\"\n";
        assert!(Manifest::from_toml(bad).is_err());
    }
}
