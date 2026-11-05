//! Engine-neutral page layout summary.
//!
//! Every layout backend can report how a document was paginated: how many
//! pages it produced and the resolved page box of each page, after `@page`
//! rules (including `:first`, `:left` and `:right`) and the configuration
//! have been applied. This is deliberately a summary, not an intermediate
//! representation of the content: backends do not share one.

use crate::config::{Margin, PageSize};

/// The resolved page box of one page, in PDF points.
///
/// `size` is the page size with the orientation already applied, so it is
/// the size of the PDF page. `margin` is the resolved page margin.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct PageGeometry {
    /// The page size, orientation applied.
    pub size: PageSize,
    /// The resolved page margins.
    pub margin: Margin,
}

impl PageGeometry {
    /// A page of `size` with `margin`.
    pub fn new(size: PageSize, margin: Margin) -> Self {
        Self { size, margin }
    }

    /// The size of the area inside the margins. A side whose margins exceed
    /// the page size is zero.
    pub fn content_size(&self) -> PageSize {
        PageSize {
            width: (self.size.width - self.margin.left - self.margin.right).max(0.0),
            height: (self.size.height - self.margin.top - self.margin.bottom).max(0.0),
        }
    }
}

/// How a document was paginated: one [`PageGeometry`] per page, in order.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct PageLayout {
    /// The pages in document order.
    pub pages: Vec<PageGeometry>,
}

impl PageLayout {
    /// A layout of `pages`, in document order.
    pub fn new(pages: Vec<PageGeometry>) -> Self {
        Self { pages }
    }

    /// The number of pages.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn margin(v: f32) -> Margin {
        Margin {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }

    #[test]
    fn content_size_subtracts_margins() {
        let page = PageGeometry::new(PageSize::A4, margin(72.0));
        let content = page.content_size();
        assert_eq!(content.width, PageSize::A4.width - 144.0);
        assert_eq!(content.height, PageSize::A4.height - 144.0);
    }

    #[test]
    fn content_size_clamps_at_zero() {
        let page = PageGeometry::new(PageSize::A4, margin(10_000.0));
        let content = page.content_size();
        assert_eq!(content.width, 0.0);
        assert_eq!(content.height, 0.0);
    }

    #[test]
    fn page_count_counts_pages() {
        let page = PageGeometry::new(PageSize::A4, margin(0.0));
        assert_eq!(PageLayout::default().page_count(), 0);
        assert_eq!(PageLayout::new(vec![page, page]).page_count(), 2);
    }
}
