//! The Krilla surface of a page, with the pushes it holds open counted.
//!
//! Krilla asserts that a surface has no push open when it is dropped. A paint
//! that fails between a push and its pop, and returns its error with `?`,
//! would turn the error into a panic. This surface counts what is open, so
//! that [`Surface::unwind`] can close it for the caller that returns the error.
//!
//! Only the pushes wrapped here are counted. One made through the dereferenced
//! Krilla surface is left open by `unwind`, so a new kind of push goes here.

use std::ops::{Deref, DerefMut};

use krilla::geom::{Path, Transform};
use krilla::mask::Mask;
use krilla::num::NormalizedF32;
use krilla::paint::FillRule;

/// A Krilla surface that knows how many pushes are open.
pub(crate) struct Surface<'a> {
    inner: krilla::surface::Surface<'a>,
    open: usize,
}

impl<'a> Surface<'a> {
    pub(crate) fn new(inner: krilla::surface::Surface<'a>) -> Self {
        Self { inner, open: 0 }
    }

    pub(crate) fn push_transform(&mut self, transform: &Transform) {
        self.inner.push_transform(transform);
        self.open += 1;
    }

    pub(crate) fn push_clip_path(&mut self, path: &Path, rule: &FillRule) {
        self.inner.push_clip_path(path, rule);
        self.open += 1;
    }

    pub(crate) fn push_opacity(&mut self, opacity: NormalizedF32) {
        self.inner.push_opacity(opacity);
        self.open += 1;
    }

    pub(crate) fn push_mask(&mut self, mask: Mask) {
        self.inner.push_mask(mask);
        self.open += 1;
    }

    /// Close the last push.
    pub(crate) fn pop(&mut self) {
        self.inner.pop();
        self.open -= 1;
    }

    /// Close every push still open, innermost first, so that a paint that
    /// failed halfway leaves a surface that can be finished.
    pub(crate) fn unwind(&mut self) {
        while self.open > 0 {
            self.pop();
        }
    }

    pub(crate) fn finish(self) {
        self.inner.finish();
    }
}

impl<'a> Deref for Surface<'a> {
    type Target = krilla::surface::Surface<'a>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Surface<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[cfg(test)]
mod tests;
