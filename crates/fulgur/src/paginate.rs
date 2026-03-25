use crate::pageable::{Pageable, Pt};

/// Split a Pageable tree into per-page fragments.
pub fn paginate(
    mut root: Box<dyn Pageable>,
    page_width: Pt,
    page_height: Pt,
) -> Vec<Box<dyn Pageable>> {
    root.wrap(page_width, page_height);

    let mut pages = vec![];
    let mut remaining = root;

    loop {
        match remaining.split_boxed(page_width, page_height) {
            Ok((this_page, rest)) => {
                pages.push(this_page);
                remaining = rest;
                // Re-wrap the remaining content
                remaining.wrap(page_width, page_height);
            }
            Err(unsplit) => {
                pages.push(unsplit);
                break;
            }
        }
    }

    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pageable::{BlockPageable, SpacerPageable};

    fn make_spacer(h: Pt) -> Box<dyn Pageable> {
        let mut s = SpacerPageable::new(h);
        s.wrap(100.0, 1000.0);
        Box::new(s)
    }

    #[test]
    fn test_paginate_single_page() {
        let block = BlockPageable::new(vec![make_spacer(100.0), make_spacer(100.0)]);
        let pages = paginate(Box::new(block), 200.0, 300.0);
        assert_eq!(pages.len(), 1);
    }

    #[test]
    fn test_paginate_two_pages() {
        let block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        let pages = paginate(Box::new(block), 200.0, 250.0);
        assert_eq!(pages.len(), 2);
    }

    #[test]
    fn test_paginate_three_pages() {
        let block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        // 500pt total, 200pt per page => 3 pages (200, 200, 100)
        let pages = paginate(Box::new(block), 200.0, 200.0);
        assert_eq!(pages.len(), 3);
    }
}
