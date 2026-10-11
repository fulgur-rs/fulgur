use fulgur_core::{Config, Error, Result};
use krilla::metadata::{DateTime, Metadata};

/// The document metadata. `html_title` and `html_lang` are the document's
/// `<title>` and root `lang`, used when the configuration sets neither.
pub(super) fn build(
    config: &Config,
    html_title: Option<String>,
    html_lang: Option<String>,
) -> Result<Metadata> {
    let mut metadata = Metadata::new()
        .authors(config.authors.clone())
        .keywords(config.keywords.clone());
    if let Some(value) = config.title.clone().or(html_title) {
        metadata = metadata.title(value);
    }
    if let Some(value) = &config.description {
        metadata = metadata.description(value.clone());
    }
    let lang = config.lang.clone().filter(|lang| !lang.trim().is_empty());
    if let Some(value) = lang.or(html_lang) {
        metadata = metadata.language(value);
    }
    if let Some(value) = &config.creator {
        metadata = metadata.creator(value.clone());
    }
    if let Some(value) = &config.producer {
        metadata = metadata.producer(value.clone());
    }
    if let Some(value) = &config.creation_date {
        metadata = metadata.creation_date(parse_date(value)?);
    }
    Ok(metadata)
}

fn parse_date(value: &str) -> Result<DateTime> {
    let invalid = || {
        Error::PdfGeneration(format!(
            "invalid creation date: {value}; expected YYYY[-MM[-DD[Thh:mm:ss[Z]]]]"
        ))
    };
    let component = |text: &str, width: usize| -> Result<u16> {
        if text.len() != width || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        text.parse().map_err(|_| invalid())
    };
    let (date, time) = value
        .split_once('T')
        .map_or((value, None), |(date, time)| (date, Some(time)));
    let parts: Vec<_> = date.split('-').collect();
    if parts.is_empty() || parts.len() > 3 || (time.is_some() && parts.len() != 3) {
        return Err(invalid());
    }
    let year = component(parts[0], 4)?;
    let mut result = DateTime::new(year);
    if let Some(month) = parts.get(1) {
        let month = component(month, 2)?;
        if !(1..=12).contains(&month) {
            return Err(invalid());
        }
        result = result.month(month as u8);
        if let Some(day) = parts.get(2) {
            let day = component(day, 2)?;
            let leap =
                year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
            let days = match month {
                2 => {
                    if leap {
                        29
                    } else {
                        28
                    }
                }
                4 | 6 | 9 | 11 => 30,
                _ => 31,
            };
            if !(1..=days).contains(&day) {
                return Err(invalid());
            }
            result = result.day(day as u8);
        }
    }
    if let Some(time) = time {
        let utc = time.ends_with('Z');
        let parts: Vec<_> = time.strip_suffix('Z').unwrap_or(time).split(':').collect();
        if parts.len() != 3 {
            return Err(invalid());
        }
        let hour = component(parts[0], 2)?;
        let minute = component(parts[1], 2)?;
        let second = component(parts[2], 2)?;
        if hour > 23 || minute > 59 || second > 59 {
            return Err(invalid());
        }
        result = result
            .hour(hour as u8)
            .minute(minute as u8)
            .second(second as u8);
        if utc {
            result = result.utc_offset_hour(0);
        }
    }
    Ok(result)
}
