use clap::Args;
use fulgur_core::{AssetBundle, Config, Error, Margin, PageSize, Result as CoreResult};
use std::path::PathBuf;

#[derive(Args)]
pub(super) struct RenderArgs {
    /// Page keyword or custom WxH with mm/cm/in/pt/px units.
    #[arg(long, value_parser = parse_page_size)]
    size: Option<PageSize>,
    /// One to four non-negative margins in millimetres.
    #[arg(long, value_parser = parse_margin)]
    margin: Option<Margin>,
    /// Rotate an explicitly supplied page size.
    #[arg(long, requires = "size")]
    landscape: bool,
    /// Additional user stylesheet, repeatable.
    #[arg(long)]
    css: Vec<PathBuf>,
    /// Bundled font file, repeatable (TTF/OTF/TTC/WOFF2).
    #[arg(long)]
    font: Vec<PathBuf>,
    /// Disable system fonts; requires at least one bundled font.
    #[arg(long, requires = "font")]
    no_system_fonts: bool,
    /// PDF title.
    #[arg(long)]
    title: Option<String>,
    /// PDF author, repeatable.
    #[arg(long)]
    author: Vec<String>,
    /// PDF description.
    #[arg(long)]
    description: Option<String>,
    /// PDF keyword, repeatable.
    #[arg(long, alias = "keywords")]
    keyword: Vec<String>,
    /// Document language tag.
    #[arg(long)]
    language: Option<String>,
    /// Creator tool.
    #[arg(long)]
    creator: Option<String>,
    /// PDF producer.
    #[arg(long)]
    producer: Option<String>,
    /// Creation date as YYYY, YYYY-MM, YYYY-MM-DD, or a full timestamp.
    #[arg(long)]
    creation_date: Option<String>,
}

impl RenderArgs {
    pub(super) fn assets(&self) -> CoreResult<AssetBundle> {
        let mut bundle = AssetBundle::new();
        for path in &self.css {
            bundle.add_css_file(path)?;
        }
        for path in &self.font {
            bundle.add_font_file(path)?;
        }
        for data in &bundle.fonts {
            let file = skrifa::raw::FileRef::new(data)
                .map_err(|error| Error::Asset(format!("invalid bundled font: {error}")))?;
            for font in file.fonts() {
                font.map_err(|error| Error::Asset(format!("invalid bundled font face: {error}")))?;
            }
        }
        Ok(bundle)
    }

    pub(super) fn system_fonts(&self) -> bool {
        !self.no_system_fonts
    }

    pub(super) fn config(&self) -> Config {
        let mut builder = Config::builder();
        if let Some(size) = self.size {
            builder = builder.page_size(size);
        }
        if let Some(margin) = self.margin {
            builder = builder.margin(margin);
        }
        if self.landscape {
            builder = builder.landscape(true);
        }
        if let Some(value) = &self.title {
            builder = builder.title(value);
        }
        if let Some(value) = &self.description {
            builder = builder.description(value);
        }
        if let Some(value) = &self.language {
            builder = builder.lang(value);
        }
        if let Some(value) = &self.creator {
            builder = builder.creator(value);
        }
        if let Some(value) = &self.producer {
            builder = builder.producer(value);
        }
        if let Some(value) = &self.creation_date {
            builder = builder.creation_date(value);
        }
        builder = builder
            .authors(self.author.iter())
            .keywords(self.keyword.iter());
        builder.build()
    }
}

fn parse_page_size(s: &str) -> Result<PageSize, String> {
    PageSize::from_css_keyword(s)
        .or_else(|| parse_custom_size(s))
        .ok_or_else(|| "expected a page keyword or positive WxH with mm/cm/in/pt/px units".into())
}

const PAGE_UNITS: [&str; 5] = ["mm", "cm", "in", "pt", "px"];

fn unit_to_pt(value: f32, unit: &str) -> Option<f32> {
    let factor = match () {
        _ if unit.eq_ignore_ascii_case("mm") => 72.0 / 25.4,
        _ if unit.eq_ignore_ascii_case("cm") => 72.0 / 2.54,
        _ if unit.eq_ignore_ascii_case("in") => 72.0,
        _ if unit.eq_ignore_ascii_case("pt") => 1.0,
        _ if unit.eq_ignore_ascii_case("px") => 72.0 / 96.0,
        _ => return None,
    };
    Some(value * factor)
}

fn take_number(rest: &str) -> Option<(f32, &str)> {
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let (num, tail) = rest.split_at(end);
    let v: f32 = num.parse().ok()?;
    Some((v, tail))
}

fn take_unit(rest: &str) -> (Option<&str>, &str) {
    for u in PAGE_UNITS {
        if let Some(head) = rest.get(..u.len())
            && head.eq_ignore_ascii_case(u)
        {
            return (Some(head), &rest[u.len()..]);
        }
    }
    (None, rest)
}

fn parse_custom_size(s: &str) -> Option<PageSize> {
    let s = s.trim();

    let (wv, rest) = take_number(s)?;
    let (wunit, rest) = take_unit(rest);

    // Separator: optional whitespace, then 'x'/'X', then optional whitespace;
    // or a bare run of whitespace with no 'x' (e.g. "100mm 200mm").
    let after_lead_ws = rest.trim_start();
    let rest = if let Some(after) = after_lead_ws.strip_prefix(['x', 'X']) {
        after.trim_start()
    } else if after_lead_ws.len() != rest.len() {
        after_lead_ws // whitespace-only separator
    } else {
        return None; // no separator
    };

    let (hv, rest) = take_number(rest)?;
    let (hunit, rest) = take_unit(rest);
    if !rest.trim().is_empty() {
        return None; // trailing garbage
    }

    // A unit on one side applies to both; both missing is invalid.
    let (wu, hu) = match (wunit, hunit) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => (a, a),
        (None, Some(b)) => (b, b),
        (None, None) => return None,
    };

    let width = unit_to_pt(wv, wu)?;
    let height = unit_to_pt(hv, hu)?;
    if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
        Some(PageSize { width, height })
    } else {
        None
    }
}

fn parse_margin(s: &str) -> Result<Margin, String> {
    let values = s
        .split_whitespace()
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "margins must be numbers in millimetres".to_string())?;
    if values.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Err("margins must be finite and non-negative".into());
    }
    let to_pt = |mm: f32| mm * 72.0 / 25.4;
    let margin = match values.as_slice() {
        [all] => Margin::uniform(to_pt(*all)),
        [vertical, horizontal] => Margin::symmetric(to_pt(*vertical), to_pt(*horizontal)),
        [top, horizontal, bottom] => Margin {
            top: to_pt(*top),
            right: to_pt(*horizontal),
            bottom: to_pt(*bottom),
            left: to_pt(*horizontal),
        },
        [top, right, bottom, left] => Margin {
            top: to_pt(*top),
            right: to_pt(*right),
            bottom: to_pt(*bottom),
            left: to_pt(*left),
        },
        _ => return Err("expected one to four margins in millimetres".into()),
    };
    if [margin.top, margin.right, margin.bottom, margin.left]
        .iter()
        .any(|v| !v.is_finite())
    {
        return Err("margins are too large".into());
    }
    Ok(margin)
}

#[cfg(test)]
mod tests;
