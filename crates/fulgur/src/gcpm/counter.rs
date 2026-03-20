use super::{ContentItem, CounterType};

/// Resolve content items to a plain string.
///
/// `Element` references are skipped in plain string mode.
pub fn resolve_content_to_string(items: &[ContentItem], page: usize, total_pages: usize) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => out.push_str(s),
            ContentItem::Counter(CounterType::Page) => {
                out.push_str(&page.to_string());
            }
            ContentItem::Counter(CounterType::Pages) => {
                out.push_str(&total_pages.to_string());
            }
            ContentItem::Element(_) => {}
        }
    }
    out
}

/// Resolve content items to an HTML string.
///
/// `Element(name)` references are looked up in `running_elements` (a `&[(name, html)]` slice)
/// and the matching HTML is appended.
pub fn resolve_content_to_html(
    items: &[ContentItem],
    running_elements: &[(String, String)],
    page: usize,
    total_pages: usize,
) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => out.push_str(s),
            ContentItem::Counter(CounterType::Page) => {
                out.push_str(&page.to_string());
            }
            ContentItem::Counter(CounterType::Pages) => {
                out.push_str(&total_pages.to_string());
            }
            ContentItem::Element(name) => {
                if let Some((_, html)) = running_elements.iter().find(|(n, _)| n == name) {
                    out.push_str(html);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_counters() {
        let items = vec![
            ContentItem::String("Page ".into()),
            ContentItem::Counter(CounterType::Page),
            ContentItem::String(" of ".into()),
            ContentItem::Counter(CounterType::Pages),
        ];
        assert_eq!(resolve_content_to_string(&items, 3, 10), "Page 3 of 10");
    }

    #[test]
    fn test_element_becomes_empty() {
        let items = vec![
            ContentItem::String("Before".into()),
            ContentItem::Element("hdr".into()),
            ContentItem::String("After".into()),
        ];
        assert_eq!(resolve_content_to_string(&items, 1, 5), "BeforeAfter");
    }

    #[test]
    fn test_resolve_html_with_running_element() {
        let items = vec![ContentItem::Element("hdr".into())];
        let running = vec![("hdr".to_string(), "<b>Header</b>".to_string())];
        assert_eq!(
            resolve_content_to_html(&items, &running, 1, 1),
            "<b>Header</b>"
        );
    }

    #[test]
    fn test_resolve_html_mixed() {
        let items = vec![
            ContentItem::Element("hdr".into()),
            ContentItem::String(" - Page ".into()),
            ContentItem::Counter(CounterType::Page),
            ContentItem::String("/".into()),
            ContentItem::Counter(CounterType::Pages),
        ];
        let running = vec![("hdr".to_string(), "<span>Title</span>".to_string())];
        assert_eq!(
            resolve_content_to_html(&items, &running, 2, 8),
            "<span>Title</span> - Page 2/8"
        );
    }
}
