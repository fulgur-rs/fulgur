//! Typed coordinate units: [`Px`] (CSS pixels) and [`Pt`] (PDF points).
//!
//! fulgur's pipeline mixes two coordinate spaces — CSS px (Blitz/Taffy) and
//! PDF pt (Krilla). Historically these were bare `f32` distinguished only by
//! `_px` / `_pt` field-name suffixes, which made the classic 4/3 scale bug
//! easy. These newtypes put the unit in the type so the compiler rejects
//! `Px + Pt`. Both are `#[repr(transparent)]` over `f32`, so they are
//! zero-cost at runtime; the migration that flips coordinate fields to these
//! types lands in later phases (tracked in the fulgur-2map epic). The legacy
//! `draw_primitives::Pt = f32` alias this migration upgrades to [`Pt`] has
//! been removed (P2a, fulgur-2map.8); these newtypes are still not
//! re-exported at the crate root.
//!
//! - Construct with [`F32Units::as_px`] / [`F32Units::as_pt`] (e.g.
//!   `frag.x.as_px()`).
//! - Convert with [`Px::in_pt`] / [`Pt::in_px`] — the only place [`PX_TO_PT`]
//!   is used.
//! - Arithmetic is same-unit only; mixing units is a compile error.
//! - Drop to raw `f32` with [`Px::to_f32`] / [`Pt::to_f32`] **only** at FFI
//!   boundaries (resvg / tiny-skia / krilla).
//!
//! Cross-unit arithmetic does not compile:
//!
//! ```compile_fail
//! use fulgur_core::units::F32Units;
//! let _ = 1.0_f32.as_px() + 1.0_f32.as_pt();
//! ```
//!
//! ```compile_fail
//! use fulgur_core::units::F32Units;
//! let _ = 1.0_f32.as_pt() + 1.0_f32.as_px();
//! ```
//!
//! # Reading the vocabulary in a diff
//!
//! The method names encode each value's unit *state* in plain text, so a
//! reviewer or coding agent reading a bare diff — with no IDE to peek types —
//! can still tell a raw external `f32` from an already-typed value. Read the
//! **first** conversion applied to a value:
//!
//! - `x.as_px()` / `x.as_pt()` — `x` is a raw `f32` (from an external crate
//!   such as usvg / taffy / stylo, or a literal) being *tagged* with its unit.
//!   [`F32Units`] is implemented for `f32` **only**, so a visible `.as_px()` /
//!   `.as_pt()` is a compiler-enforced proof that its receiver was untyped.
//! - `x.in_pt()` / `x.in_px()` — `x` is already a [`Px`] / [`Pt`] being
//!   *converted*.
//! - `x.to_f32()` — a [`Px`] / [`Pt`] being *dropped* back to raw `f32` at an
//!   f32 sink.
//!
//! So `size.width().as_px().in_pt()` reads as "raw f32 (px) → [`Px`] → [`Pt`]":
//! the leading `.as_px()` is what proves `size.width()` is an untyped external
//! `f32`. A trailing `.in_pt()` only converts the freshly-tagged [`Px`] and
//! says nothing about the origin — always read the *first* hop. `.as_px()` /
//! `.as_pt()` can never appear on an already-typed value ([`Px`] / [`Pt`] carry
//! no such method and no `Deref` to `f32`):
//!
//! ```compile_fail
//! use fulgur_core::units::F32Units;
//! // `.as_px()` exists on `f32` only — never on an already-typed `Pt`.
//! let _ = 1.0_f32.as_pt().as_px();
//! ```

/// 1 CSS px = 0.75 PDF pt. The single source of this constant in the crate.
pub const PX_TO_PT: f32 = 0.75;

/// A length in CSS pixels (Blitz / Taffy space).
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug, Default)]
pub struct Px(f32);

/// A length in PDF points (Krilla space). 1 px = [`PX_TO_PT`] pt.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug, Default)]
pub struct Pt(f32);

/// Same-unit arithmetic for a coordinate newtype. Cross-unit ops are
/// intentionally NOT implemented so the compiler rejects unit mix-ups.
macro_rules! impl_unit_arith {
    ($t:ident) => {
        impl core::ops::Add for $t {
            type Output = $t;
            #[inline]
            fn add(self, rhs: $t) -> $t {
                $t(self.0 + rhs.0)
            }
        }
        impl core::ops::Sub for $t {
            type Output = $t;
            #[inline]
            fn sub(self, rhs: $t) -> $t {
                $t(self.0 - rhs.0)
            }
        }
        impl core::ops::Mul<f32> for $t {
            type Output = $t;
            #[inline]
            fn mul(self, rhs: f32) -> $t {
                $t(self.0 * rhs)
            }
        }
        // Commutative scalar multiply: `2.0 * width`.
        impl core::ops::Mul<$t> for f32 {
            type Output = $t;
            #[inline]
            fn mul(self, rhs: $t) -> $t {
                $t(self * rhs.0)
            }
        }
        impl core::ops::Div<f32> for $t {
            type Output = $t;
            #[inline]
            fn div(self, rhs: f32) -> $t {
                $t(self.0 / rhs)
            }
        }
        // Same-unit division yields a dimensionless ratio.
        impl core::ops::Div for $t {
            type Output = f32;
            #[inline]
            fn div(self, rhs: $t) -> f32 {
                self.0 / rhs.0
            }
        }
        impl core::ops::Neg for $t {
            type Output = $t;
            #[inline]
            fn neg(self) -> $t {
                $t(-self.0)
            }
        }
        impl core::ops::AddAssign for $t {
            #[inline]
            fn add_assign(&mut self, rhs: $t) {
                self.0 += rhs.0;
            }
        }
        impl core::ops::SubAssign for $t {
            #[inline]
            fn sub_assign(&mut self, rhs: $t) {
                self.0 -= rhs.0;
            }
        }
        impl core::ops::MulAssign<f32> for $t {
            #[inline]
            fn mul_assign(&mut self, rhs: f32) {
                self.0 *= rhs;
            }
        }
        impl core::ops::DivAssign<f32> for $t {
            #[inline]
            fn div_assign(&mut self, rhs: f32) {
                self.0 /= rhs;
            }
        }
        impl core::iter::Sum for $t {
            #[inline]
            fn sum<I: Iterator<Item = $t>>(iter: I) -> $t {
                $t(iter.map(|v| v.0).sum())
            }
        }
    };
}

impl_unit_arith!(Px);
impl_unit_arith!(Pt);

impl Px {
    /// The zero length. Use instead of `0.0_f32.as_px()` for clamp idioms
    /// (`x.max(Px::ZERO)`) and sign comparisons (`x > Px::ZERO`); the private
    /// field means `Px(0.0)` is not constructible outside this module.
    pub const ZERO: Px = Px(0.0);

    /// Raw `f32` value. Use only at FFI boundaries.
    #[inline]
    pub const fn to_f32(self) -> f32 {
        self.0
    }

    /// Convert CSS px → PDF pt.
    #[inline]
    pub fn in_pt(self) -> Pt {
        Pt(self.0 * PX_TO_PT)
    }

    /// Larger of two lengths. Mirrors `f32::max` (same NaN handling) so a
    /// migrated `x.max(y)` stays byte-identical.
    #[inline]
    pub fn max(self, other: Px) -> Px {
        Px(self.0.max(other.0))
    }

    /// Smaller of two lengths. Mirrors `f32::min`.
    #[inline]
    pub fn min(self, other: Px) -> Px {
        Px(self.0.min(other.0))
    }

    /// `true` if this length is neither infinite nor NaN. Mirrors
    /// `f32::is_finite`.
    ///
    /// Needed because the usual sign guards (`x <= Px::ZERO`) are NaN-blind:
    /// every comparison against NaN is `false`, so a NaN slips through a
    /// `<= 0` rejection and only surfaces later as a violated ordering
    /// invariant. Validate with `is_finite` at the boundary where an
    /// untyped `f32` first becomes a `Px`.
    #[inline]
    pub fn is_finite(self) -> bool {
        self.0.is_finite()
    }
}

impl Pt {
    /// The zero length. Use instead of `0.0_f32.as_pt()` for clamp idioms
    /// (`x.max(Pt::ZERO)`); the private field means `Pt(0.0)` is not
    /// constructible outside this module.
    pub const ZERO: Pt = Pt(0.0);

    /// Raw `f32` value. Use only at FFI boundaries.
    #[inline]
    pub const fn to_f32(self) -> f32 {
        self.0
    }

    /// Convert PDF pt → CSS px.
    #[inline]
    pub fn in_px(self) -> Px {
        Px(self.0 / PX_TO_PT)
    }

    /// Larger of two lengths. Mirrors `f32::max` (same NaN handling) so a
    /// migrated `x.max(y)` stays byte-identical.
    #[inline]
    pub fn max(self, other: Pt) -> Pt {
        Pt(self.0.max(other.0))
    }

    /// Smaller of two lengths. Mirrors `f32::min`.
    #[inline]
    pub fn min(self, other: Pt) -> Pt {
        Pt(self.0.min(other.0))
    }

    /// Absolute value. Mirrors `f32::abs` so a migrated `(a - b).abs()`
    /// stays byte-identical.
    #[inline]
    pub fn abs(self) -> Pt {
        Pt(self.0.abs())
    }
}

/// Constructor sugar so `value.as_px()` / `value.as_pt()` reads naturally and
/// the newtype field can stay private.
///
/// Takes `self` by value, not `&self`: `f32` is `Copy` (borrowing a 4-byte
/// scalar buys nothing) and by-value keeps `F32Units::as_px` usable as a
/// `fn(f32) -> Px` in `iter.map(F32Units::as_px)`. That trips clippy's
/// `as_*`-should-borrow convention, which does not fit a Copy-scalar tag.
#[allow(clippy::wrong_self_convention)]
pub trait F32Units {
    /// Tag this `f32` as CSS pixels.
    fn as_px(self) -> Px;
    /// Tag this `f32` as PDF points.
    fn as_pt(self) -> Pt;
}

impl F32Units for f32 {
    #[inline]
    fn as_px(self) -> Px {
        Px(self)
    }
    #[inline]
    fn as_pt(self) -> Pt {
        Pt(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn px_pt_roundtrip() {
        assert_eq!(Px(4.0).in_pt(), Pt(3.0));
        assert_eq!(Pt(3.0).in_px(), Px(4.0));
    }

    #[test]
    fn same_unit_arithmetic() {
        assert_eq!(Pt(1.0) + Pt(2.0), Pt(3.0));
        assert_eq!(Px(5.0) - Px(2.0), Px(3.0));
        assert_eq!(Pt(2.0) * 3.0, Pt(6.0));
        assert_eq!(3.0 * Pt(2.0), Pt(6.0));
        assert_eq!(Pt(6.0) / 2.0, Pt(3.0));
        assert_eq!(Pt(6.0) / Pt(2.0), 3.0);
        assert_eq!(-Px(1.5), Px(-1.5));
        let mut a = Pt(1.0);
        a += Pt(2.0);
        a -= Pt(0.5);
        a *= 2.0;
        a /= 5.0;
        assert_eq!(a, Pt(1.0));
        let s: Pt = [Pt(1.0), Pt(2.0), Pt(3.0)].into_iter().sum();
        assert_eq!(s, Pt(6.0));
    }

    #[test]
    fn pt_max_min_mirror_f32() {
        assert_eq!(Pt::ZERO, Pt(0.0));
        assert_eq!(Pt(1.0).max(Pt(2.0)), Pt(2.0));
        assert_eq!(Pt(1.0).min(Pt(2.0)), Pt(1.0));
        // identical to f32::max/min, including the 0.0 clamp idiom
        assert_eq!(Pt(-3.0).max(Pt(0.0)), Pt(0.0));
        assert_eq!(
            [Pt(0.0), Pt(2.0), Pt(1.0)]
                .into_iter()
                .fold(Pt(0.0), Pt::max),
            Pt(2.0)
        );
    }

    #[test]
    fn px_zero_is_zero() {
        assert_eq!(Px::ZERO, Px(0.0));
        assert!(Px(1.0) > Px::ZERO);
        // boundary: zero is not strictly greater than zero (the `> Px::ZERO` idiom)
        assert!(Px(0.0) <= Px::ZERO);
    }

    #[test]
    fn px_max_min_mirror_f32() {
        assert_eq!(Px(1.0).max(Px(2.0)), Px(2.0));
        assert_eq!(Px(1.0).min(Px(2.0)), Px(1.0));
        // identical to f32::max/min, including the 0.0 clamp idiom
        assert_eq!(Px(-3.0).max(Px::ZERO), Px::ZERO);
        assert_eq!(Px(-3.0).min(Px::ZERO), Px(-3.0));
    }

    #[test]
    fn abs_mirrors_f32() {
        assert_eq!((-3.5_f32).as_pt().abs(), 3.5_f32.as_pt());
        assert_eq!(2.0_f32.as_pt().abs(), 2.0_f32.as_pt());
        // Byte-identical to the raw f32 op it replaces.
        let v = -7.25_f32;
        assert_eq!(v.as_pt().abs().to_f32(), v.abs());
    }

    #[test]
    fn ctor_sugar_and_escape() {
        assert_eq!(4.0_f32.as_px(), Px(4.0));
        assert_eq!(3.0_f32.as_pt(), Pt(3.0));
        assert_eq!(Pt(3.0).to_f32(), 3.0);
        assert_eq!(Px(4.0).to_f32(), 4.0);
    }

    #[test]
    fn repr_transparent_is_zero_cost() {
        assert_eq!(core::mem::size_of::<Px>(), core::mem::size_of::<f32>());
        assert_eq!(core::mem::size_of::<Pt>(), core::mem::size_of::<f32>());
    }

    #[test]
    fn pt_const_is_single_source() {
        assert_eq!(PX_TO_PT, 0.75);
    }
}
