// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

/*!
This module contains border radius related types for the run-time library.
*/

use core::fmt;
use core::marker::PhantomData;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};
use euclid::approxord::{max, min};
use euclid::num::Zero;
use euclid::{Length, Scale};
#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use num_traits::Float;
use num_traits::NumCast;

/// Top-left, top-right, bottom-right, and bottom-left border radius, optionally
/// tagged with a unit.
#[repr(C)]
pub struct BorderRadius<T, U> {
    /// The top-left radius.
    pub top_left: T,
    /// The top-right radius.
    pub top_right: T,
    /// The bottom-right radius.
    pub bottom_right: T,
    /// The bottom-left radius.
    pub bottom_left: T,
    #[doc(hidden)]
    pub _unit: PhantomData<U>,
}

impl<T, U> Copy for BorderRadius<T, U> where T: Copy {}

impl<T, U> Clone for BorderRadius<T, U>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        BorderRadius {
            top_left: self.top_left.clone(),
            top_right: self.top_right.clone(),
            bottom_right: self.bottom_right.clone(),
            bottom_left: self.bottom_left.clone(),
            _unit: PhantomData,
        }
    }
}

impl<T, U> Eq for BorderRadius<T, U> where T: Eq {}

impl<T, U> PartialEq for BorderRadius<T, U>
where
    T: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.top_left == other.top_left
            && self.top_right == other.top_right
            && self.bottom_right == other.bottom_right
            && self.bottom_left == other.bottom_left
    }
}

impl<T, U> fmt::Debug for BorderRadius<T, U>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "BorderRadius({:?}, {:?}, {:?}, {:?})",
            self.top_left, self.top_right, self.bottom_right, self.bottom_left
        )
    }
}

impl<T, U> Default for BorderRadius<T, U>
where
    T: Default,
{
    fn default() -> Self {
        BorderRadius::new(T::default(), T::default(), T::default(), T::default())
    }
}

impl<T, U> Zero for BorderRadius<T, U>
where
    T: Zero,
{
    fn zero() -> Self {
        BorderRadius::new(T::zero(), T::zero(), T::zero(), T::zero())
    }
}

impl<T, U> BorderRadius<T, U> {
    /// Constructor taking a scalar for each radius.
    ///
    /// Radii are specified in top-left, top-right, bottom-right, bottom-left
    /// order following CSS's convention.
    pub const fn new(top_left: T, top_right: T, bottom_right: T, bottom_left: T) -> Self {
        BorderRadius { top_left, top_right, bottom_right, bottom_left, _unit: PhantomData }
    }

    /// Constructor taking a typed Length for each radius.
    ///
    /// Radii are specified in top-left, top-right, bottom-right, bottom-left
    /// order following CSS's convention.
    pub fn from_lengths(
        top_left: Length<T, U>,
        top_right: Length<T, U>,
        bottom_right: Length<T, U>,
        bottom_left: Length<T, U>,
    ) -> Self {
        BorderRadius::new(top_left.0, top_right.0, bottom_right.0, bottom_left.0)
    }

    /// Constructor taking the same scalar value for all radii.
    pub fn new_uniform(all: T) -> Self
    where
        T: Copy,
    {
        BorderRadius::new(all, all, all, all)
    }

    /// Constructor taking the same typed Length for all radii.
    pub fn from_length(all: Length<T, U>) -> Self
    where
        T: Copy,
    {
        BorderRadius::new_uniform(all.0)
    }

    /// Returns `true` if all radii are equal.
    pub fn is_uniform(&self) -> bool
    where
        T: ApproxEq<T>,
    {
        self.top_left.approx_eq(&self.top_right)
            && self.top_left.approx_eq(&self.bottom_right)
            && self.top_left.approx_eq(&self.bottom_left)
    }

    /// Returns the uniform radius if all are equal, or `None` otherwise.
    pub fn as_uniform(&self) -> Option<T>
    where
        T: Copy + ApproxEq<T>,
    {
        if self.is_uniform() { Some(self.top_left) } else { None }
    }

    /// Returns `true` if all radii are zero.
    pub fn is_zero(&self) -> bool
    where
        T: ApproxEq<T> + Zero,
    {
        let zero = T::zero();
        self.top_left.approx_eq(&zero)
            && self.top_right.approx_eq(&zero)
            && self.bottom_right.approx_eq(&zero)
            && self.bottom_left.approx_eq(&zero)
    }

    /// Returns the outer radius.
    ///
    /// For any corner with a positive radius, the radius is ensured to be at
    /// least `half_border_width`.
    pub fn outer(&self, half_border_width: Length<T, U>) -> Self
    where
        T: Copy + PartialOrd + Zero,
    {
        let zero = T::zero();
        BorderRadius::new(
            if self.top_left > zero {
                max(self.top_left, half_border_width.0)
            } else {
                self.top_left
            },
            if self.top_right > zero {
                max(self.top_right, half_border_width.0)
            } else {
                self.top_right
            },
            if self.bottom_right > zero {
                max(self.bottom_right, half_border_width.0)
            } else {
                self.bottom_right
            },
            if self.bottom_left > zero {
                max(self.bottom_left, half_border_width.0)
            } else {
                self.bottom_left
            },
        )
    }

    /// Returns the inner radius.
    ///
    /// A positive radius of each corner is subtracted by `half_border_width`
    /// and min-clamped to zero.
    pub fn inner(&self, half_border_width: Length<T, U>) -> Self
    where
        T: Copy + PartialOrd + Sub<T, Output = T> + Zero,
    {
        BorderRadius::new(
            self.top_left - half_border_width.0,
            self.top_right - half_border_width.0,
            self.bottom_right - half_border_width.0,
            self.bottom_left - half_border_width.0,
        )
        .max(Self::zero())
    }
}

/// Trait for testing approximate equality
pub trait ApproxEq<Eps> {
    /// Returns `true` is this object is approximately equal to the other one.
    fn approx_eq(&self, other: &Self) -> bool;
}

macro_rules! approx_eq {
    ($ty:ty, $eps:expr) => {
        impl ApproxEq<$ty> for $ty {
            #[inline]
            fn approx_eq(&self, other: &$ty) -> bool {
                num_traits::sign::abs(*self - *other) <= $eps
            }
        }
    };
}

approx_eq!(i16, 0);
approx_eq!(i32, 0);
approx_eq!(f32, f32::EPSILON);

impl<T, U> Add for BorderRadius<T, U>
where
    T: Add<T, Output = T>,
{
    type Output = Self;
    fn add(self, other: Self) -> Self {
        BorderRadius::new(
            self.top_left + other.top_left,
            self.top_right + other.top_right,
            self.bottom_right + other.bottom_right,
            self.bottom_left + other.bottom_left,
        )
    }
}

impl<T, U> AddAssign<Self> for BorderRadius<T, U>
where
    T: AddAssign<T>,
{
    fn add_assign(&mut self, other: Self) {
        self.top_left += other.top_left;
        self.top_right += other.top_right;
        self.bottom_right += other.bottom_right;
        self.bottom_left += other.bottom_left;
    }
}

impl<T, U> Sub for BorderRadius<T, U>
where
    T: Sub<T, Output = T>,
{
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        BorderRadius::new(
            self.top_left - other.top_left,
            self.top_right - other.top_right,
            self.bottom_right - other.bottom_right,
            self.bottom_left - other.bottom_left,
        )
    }
}

impl<T, U> SubAssign<Self> for BorderRadius<T, U>
where
    T: SubAssign<T>,
{
    fn sub_assign(&mut self, other: Self) {
        self.top_left -= other.top_left;
        self.top_right -= other.top_right;
        self.bottom_right -= other.bottom_right;
        self.bottom_left -= other.bottom_left;
    }
}

impl<T, U> Neg for BorderRadius<T, U>
where
    T: Neg<Output = T>,
{
    type Output = Self;
    fn neg(self) -> Self {
        BorderRadius {
            top_left: -self.top_left,
            top_right: -self.top_right,
            bottom_right: -self.bottom_right,
            bottom_left: -self.bottom_left,
            _unit: PhantomData,
        }
    }
}

impl<T, U> Mul<T> for BorderRadius<T, U>
where
    T: Copy + Mul,
{
    type Output = BorderRadius<T::Output, U>;

    #[inline]
    fn mul(self, scale: T) -> Self::Output {
        BorderRadius::new(
            self.top_left * scale,
            self.top_right * scale,
            self.bottom_right * scale,
            self.bottom_left * scale,
        )
    }
}

impl<T, U> MulAssign<T> for BorderRadius<T, U>
where
    T: Copy + MulAssign,
{
    #[inline]
    fn mul_assign(&mut self, other: T) {
        self.top_left *= other;
        self.top_right *= other;
        self.bottom_right *= other;
        self.bottom_left *= other;
    }
}

impl<T, U1, U2> Mul<Scale<T, U1, U2>> for BorderRadius<T, U1>
where
    T: Copy + Mul,
{
    type Output = BorderRadius<T::Output, U2>;

    #[inline]
    fn mul(self, scale: Scale<T, U1, U2>) -> Self::Output {
        BorderRadius::new(
            self.top_left * scale.0,
            self.top_right * scale.0,
            self.bottom_right * scale.0,
            self.bottom_left * scale.0,
        )
    }
}

impl<T, U> MulAssign<Scale<T, U, U>> for BorderRadius<T, U>
where
    T: Copy + MulAssign,
{
    #[inline]
    fn mul_assign(&mut self, other: Scale<T, U, U>) {
        *self *= other.0;
    }
}

impl<T, U> Div<T> for BorderRadius<T, U>
where
    T: Copy + Div,
{
    type Output = BorderRadius<T::Output, U>;

    #[inline]
    fn div(self, scale: T) -> Self::Output {
        BorderRadius::new(
            self.top_left / scale,
            self.top_right / scale,
            self.bottom_right / scale,
            self.bottom_left / scale,
        )
    }
}

impl<T, U> DivAssign<T> for BorderRadius<T, U>
where
    T: Copy + DivAssign,
{
    #[inline]
    fn div_assign(&mut self, other: T) {
        self.top_left /= other;
        self.top_right /= other;
        self.bottom_right /= other;
        self.bottom_left /= other;
    }
}

impl<T, U1, U2> Div<Scale<T, U1, U2>> for BorderRadius<T, U2>
where
    T: Copy + Div,
{
    type Output = BorderRadius<T::Output, U1>;

    #[inline]
    fn div(self, scale: Scale<T, U1, U2>) -> Self::Output {
        BorderRadius::new(
            self.top_left / scale.0,
            self.top_right / scale.0,
            self.bottom_right / scale.0,
            self.bottom_left / scale.0,
        )
    }
}

impl<T, U> DivAssign<Scale<T, U, U>> for BorderRadius<T, U>
where
    T: Copy + DivAssign,
{
    fn div_assign(&mut self, other: Scale<T, U, U>) {
        *self /= other.0;
    }
}

impl<T, U> BorderRadius<T, U>
where
    T: PartialOrd,
{
    /// Returns the minimum of the two radii.
    #[inline]
    pub fn min(self, other: Self) -> Self {
        BorderRadius::new(
            min(self.top_left, other.top_left),
            min(self.top_right, other.top_right),
            min(self.bottom_right, other.bottom_right),
            min(self.bottom_left, other.bottom_left),
        )
    }

    /// Returns the maximum of the two radii.
    #[inline]
    pub fn max(self, other: Self) -> Self {
        BorderRadius::new(
            max(self.top_left, other.top_left),
            max(self.top_right, other.top_right),
            max(self.bottom_right, other.bottom_right),
            max(self.bottom_left, other.bottom_left),
        )
    }
}

impl<T, U> BorderRadius<T, U>
where
    T: NumCast + Copy,
{
    /// Cast from one numeric representation to another, preserving the units.
    #[inline]
    pub fn cast<NewT: NumCast>(self) -> BorderRadius<NewT, U> {
        self.try_cast().unwrap()
    }

    /// Fallible cast from one numeric representation to another, preserving the units.
    pub fn try_cast<NewT: NumCast>(self) -> Option<BorderRadius<NewT, U>> {
        match (
            NumCast::from(self.top_left),
            NumCast::from(self.top_right),
            NumCast::from(self.bottom_right),
            NumCast::from(self.bottom_left),
        ) {
            (Some(top_left), Some(top_right), Some(bottom_right), Some(bottom_left)) => {
                Some(BorderRadius::new(top_left, top_right, bottom_right, bottom_left))
            }
            _ => None,
        }
    }
}

#[repr(C)]
#[derive(Default, Debug, Clone, PartialEq, Copy)]
/// Shape of a corner as defined by (x/a)^2^k + (y/b)^2^k = 1 (a superellipse)
/// Where a and b are the sides of the rectangle, and k is a constant
pub enum CornerShape {
    #[default]
    /// Same as just setting border radius, k = 1
    Round,
    /// k = -infinity
    Notch,
    /// k = -1, the inverse of Round
    Scoop,
    /// k = 0
    Bevel,
    /// k = 2
    Squircle,
    /// k = infinity
    Square,
    /// Custom k
    Superellipse(f32),
}

impl ApproxEq<CornerShape> for CornerShape {
    #[inline]
    fn approx_eq(&self, other: &Self) -> bool {
        self == other
    }
}

/// The CSS `corner-shape: superellipse(k)` parameter that renders
/// [`CornerShape::Squircle`](crate::items::CornerShape::Squircle).
pub const SQUIRCLE_K: f32 = 2.0;

/// Larger `|k|` is clamped to this: `2^16` is already indistinguishable from `Square`
/// (or `Notch`) at any radius, and an unclamped `n` overflows to infinity.
const MAX_SUPERELLIPSE_K: f32 = 16.0;

/// The exponent for CSS `corner-shape: superellipse(k)`: `n = 2^|k|`, so `n >= 1`.
///
/// `k`'s sign selects the convex or concave branch, not the exponent. See [`corner_boundary`].
pub fn superellipse_n_from_k(k: f32) -> f32 {
    2f32.powf(k.abs().min(MAX_SUPERELLIPSE_K))
}

/// `(p, q)` for the ray of slope `(1 - s) / s`, scaled onto the curve so `p^n + q^n == 1`.
/// Shared with [`corner_path`](super::corner_path)'s `superellipse_at`, which explains the
/// `s` vs. the CSS spec's `t`.
pub(crate) fn superellipse_pq(n: f32, s: f32) -> (f32, f32) {
    // Factoring out the larger keeps the power in `0..=1`, so a large `n` can't overflow it.
    let (larger, smaller) = (s.max(1.0 - s), s.min(1.0 - s));
    let scale = larger * (1.0 + (smaller / larger).powf(n)).powf(1.0 / n);
    (s / scale, (1.0 - s) / scale)
}

/// The superellipse boundary shared by [`CornerShape::Squircle`] and
/// [`CornerShape::Superellipse`](crate::items::CornerShape::Superellipse).
///
/// Positive `k` gives the convex branch, `(1 - x/r)^n + (1 - y/r)^n = 1`, bowing toward
/// the tip. Negative `k` mirrors it about the bevel chord to the concave branch,
/// `(x/r)^n + (y/r)^n = 1`, making `superellipse(-1)` (`Scoop`) the mirror of
/// `superellipse(1)` (`Round`).
fn superellipse_boundary(k: f32, r: f32, y: f32) -> f32 {
    let n = superellipse_n_from_k(k);
    if k < 0.0 {
        r * (1.0 - (y / r).powf(n)).max(0.0).powf(1.0 / n)
    } else {
        let u = (r - y) / r;
        r * (1.0 - (1.0 - u.powf(n)).max(0.0).powf(1.0 / n))
    }
}

/// Column offset from the corner's tip where its boundary crosses row `y`
/// (`y` from 0 at the tip to `r` at the straight edge).
///
/// Every [`CornerShape`](crate::items::CornerShape) is one CSS `superellipse(k)` value:
/// `Notch` is `k = -∞`, `Scoop` is `-1`, `Bevel` is `0`, `Round` is `1`, `Squircle` is `2`,
/// `Square` is `∞`. Named shapes keep their cheaper, exact closed form; only `Squircle`
/// and `Superellipse` evaluate the superellipse itself.
pub fn corner_boundary(shape: CornerShape, r: f32, y: f32) -> f32 {
    if r <= 0.0 {
        return 0.0;
    }
    let y = y.clamp(0.0, r);
    match shape {
        CornerShape::Square => 0.0,
        CornerShape::Notch => {
            if y >= r {
                0.0
            } else {
                r
            }
        }
        CornerShape::Bevel => r - y,
        CornerShape::Round => r - (r * r - (r - y) * (r - y)).max(0.0).sqrt(),
        CornerShape::Squircle => superellipse_boundary(SQUIRCLE_K, r, y),
        CornerShape::Scoop => (r * r - y * y).max(0.0).sqrt(),
        CornerShape::Superellipse(k) => superellipse_boundary(k, r, y),
    }
}

/// Position (`x/r`, `y/r`) and unit tangent of the superellipse, in the corner's frame,
/// at ray parameter `s`.
/// Scalar counterpart of [`corner_path`](super::corner_path)'s `superellipse_at`, which
/// returns a `Vector` for its path-building callers.
fn superellipse_point_and_tangent(k: f32, n: f32, s: f32) -> ((f32, f32), (f32, f32)) {
    let (p, q) = superellipse_pq(n, s);
    let (gp, gq) = (p.powf(n - 1.0), q.powf(n - 1.0));
    let (pos, tangent) =
        if k < 0.0 { ((q, p), (-gp, gq)) } else { ((1.0 - p, 1.0 - q), (-gq, gp)) };
    let len = (tangent.0 * tangent.0 + tangent.1 * tangent.1).sqrt();
    (pos, (tangent.0 / len, tangent.1 / len))
}

/// The true perpendicular offset of the `superellipse(k)` boundary, moved a normalized
/// distance `wn` (border width `/ r`) into the material, at normalized row `yn` (`/ r`).
/// `None` when `yn` is closer to the tip than the offset curve reaches. See
/// [`inner_corner_boundary`].
///
/// Solved by bisection on the ray parameter `s`: the offset point's `y` is monotonic in
/// `s` for `wn` well under 1, covering any border width up to its inset radius.
fn superellipse_offset_x(k: f32, n: f32, wn: f32, yn: f32) -> Option<f32> {
    let offset_y = |s: f32| -> f32 {
        let (pos, tangent) = superellipse_point_and_tangent(k, n, s);
        // The inward normal: the tangent rotated -90 degrees, into the material on both
        // branches.
        pos.1 - wn * tangent.0
    };
    if yn < offset_y(0.0) {
        return None;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = (lo + hi) * 0.5;
        if offset_y(mid) < yn { lo = mid } else { hi = mid }
    }
    let s = (lo + hi) * 0.5;
    let (pos, tangent) = superellipse_point_and_tangent(k, n, s);
    Some(pos.0 + wn * tangent.1)
}

/// How far past `y == r` [`inner_corner_boundary`] must be evaluated for `shape`'s inner
/// curve to reach the flat border width `r - r2`.
///
/// Shapes that meet the straight edge tangentially (`Round`, `Squircle`, convex
/// `Superellipse`) reach it exactly at `y == r`, as does `Square`, which has no curve.
/// The rest meet the edge at an angle, so their offset only comes back to `r - r2` past
/// the corner's own radius. [`corner_boundary`] widens `Notch`'s zone by this amount, and
/// the software renderer does the same for every shape through this function.
pub fn inner_corner_extra(shape: CornerShape, r: f32, r2: f32) -> f32 {
    if r <= 0.0 || r2 <= 0.0 {
        return 0.0;
    }
    let w = r - r2;
    match shape {
        // The reflex corner at `(r, r)` offsets outward to `(r + w, r + w)`.
        CornerShape::Notch => w,
        // The offset diagonal `x + y = r + w * sqrt(2)` meets the offset edge `y = w` at
        // `y = r + w * (sqrt(2) - 1)`.
        CornerShape::Bevel => w * (core::f32::consts::SQRT_2 - 1.0),
        // The tip-centered circle grown to radius `r + w` meets the offset edge `x = w` at
        // `y = sqrt((r + w)^2 - w^2)`.
        CornerShape::Scoop => (r * (r + 2.0 * w)).sqrt() - r,
        _ => 0.0,
    }
}

/// Like [`corner_boundary`], but for the border's inner edge: the outer curve offset
/// perpendicular to itself by the border width `r - r2`, as a stroked path would be.
/// Keeps the border a uniform width all the way around, whatever the corner's shape.
///
/// A shape that meets the straight edge at an angle reaches its flat width only past
/// `y == r`; see [`inner_corner_extra`] for how far. The software renderer's
/// `draw_rounded_rectangle_line` evaluates past that point instead of clamping, to
/// avoid a seam where the corner meets the straight run.
pub fn inner_corner_boundary(shape: CornerShape, r: f32, r2: f32, y: f32) -> f32 {
    if r <= 0.0 {
        return 0.0;
    }
    if r2 <= 0.0 {
        // The border is at least as wide as the radius, so no fill shows through this
        // corner: its boundary coincides with the outer one.
        return r;
    }
    let w = r - r2;
    let y = y.clamp(0.0, r + inner_corner_extra(shape, r, r2));
    match shape {
        CornerShape::Square => {
            // A plain miter: offsetting the two edges by `w` meets at `(w, w)`. Above it,
            // same as the `r2 <= 0` case: no fill shows, and `y < w` always holds here.
            if y < w { r } else { w }
        }
        CornerShape::Notch => {
            // Same miter threshold as `Square`. Above `y < w`, offsetting the reflex
            // corner at `(r, r)` outward by `w` moves this wall to `r + w`.
            if y < w { r } else { r + w }
        }
        CornerShape::Bevel => {
            // A diagonal offset perpendicular to itself by `w` moves `w * sqrt(2)` along
            // `x + y`. Below `y = w` the offset segment hasn't started yet.
            if y < w { r } else { r + w * core::f32::consts::SQRT_2 - y }
        }
        CornerShape::Round => {
            // The circle centered on `(r, r)`, shrunk from radius `r` to `r - w`; it
            // doesn't reach rows closer than `w` to the tip.
            if y < w { r } else { r - ((r - w) * (r - w) - (y - r) * (y - r)).max(0.0).sqrt() }
        }
        CornerShape::Scoop => {
            // The circle centered on the tip grown from radius `r` to `r + w`.
            if y < w { r } else { ((r + w) * (r + w) - y * y).max(0.0).sqrt() }
        }
        CornerShape::Squircle | CornerShape::Superellipse(_) => {
            let k = match shape {
                CornerShape::Squircle => SQUIRCLE_K,
                CornerShape::Superellipse(k) => k,
                _ => unreachable!(),
            };
            let n = superellipse_n_from_k(k);
            match superellipse_offset_x(k, n, w / r, y / r) {
                Some(x) => r * x,
                None => r,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lengths::{LogicalBorderRadius, LogicalLength, PhysicalPx, ScaleFactor};
    use alloc::vec::Vec;
    use euclid::UnknownUnit;

    type BorderRadius = super::BorderRadius<f32, UnknownUnit>;
    type IntBorderRadius = super::BorderRadius<i16, UnknownUnit>;
    type PhysicalBorderRadius = super::BorderRadius<f32, PhysicalPx>;

    #[test]
    fn test_eq() {
        let a = BorderRadius::new(1., 2., 3., 4.);
        let b = BorderRadius::new(1., 2., 3., 4.);
        let c = BorderRadius::new(4., 3., 2., 1.);
        let d = BorderRadius::new(
            c.top_left + f32::EPSILON / 2.,
            c.top_right - f32::EPSILON / 2.,
            c.bottom_right - f32::EPSILON / 2.,
            c.bottom_left + f32::EPSILON / 2.,
        );
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(c, d);
    }

    #[test]
    fn test_min_max() {
        let a = BorderRadius::new(1., 2., 3., 4.);
        let b = BorderRadius::new(4., 3., 2., 1.);
        assert_eq!(a.min(b), BorderRadius::new(1., 2., 2., 1.));
        assert_eq!(a.max(b), BorderRadius::new(4., 3., 3., 4.));
    }

    #[test]
    fn test_scale() {
        let scale = ScaleFactor::new(2.);
        let logical_radius = LogicalBorderRadius::new(1., 2., 3., 4.);
        let physical_radius = PhysicalBorderRadius::new(2., 4., 6., 8.);
        assert_eq!(logical_radius * scale, physical_radius);
        assert_eq!(physical_radius / scale, logical_radius);
    }

    #[test]
    fn test_zero() {
        assert!(BorderRadius::new_uniform(0.).is_zero());
        assert!(BorderRadius::new_uniform(1.0e-9).is_zero());
        assert!(!BorderRadius::new_uniform(1.0e-3).is_zero());
        assert!(IntBorderRadius::new_uniform(0).is_zero());
        assert!(!IntBorderRadius::new_uniform(1).is_zero());
    }

    #[test]
    fn test_inner_outer() {
        let radius = LogicalBorderRadius::new(0., 2.5, 5., 10.);
        let half_border_width = LogicalLength::new(5.);
        assert_eq!(radius.inner(half_border_width), LogicalBorderRadius::new(0., 0., 0., 5.));
        assert_eq!(radius.outer(half_border_width), LogicalBorderRadius::new(0., 5., 5., 10.));
    }

    /// The closed-form perpendicular offset for the three shapes easy to derive by hand,
    /// checked independently of `inner_corner_boundary`'s own derivation. Sampled up to
    /// `y == r` plus [`inner_corner_extra`], the range `inner_corner_boundary` evaluates
    /// over.
    #[test]
    fn inner_corner_boundary_matches_closed_form() {
        use crate::graphics::border_radius::{CornerShape, inner_corner_extra};
        let cases: &[(CornerShape, fn(f32, f32, f32) -> f32)] = &[
            // Same derivation as the `Bevel` arm of `inner_corner_boundary`.
            (
                CornerShape::Bevel,
                |r, w, y| {
                    if y < w { r } else { r + w * core::f32::consts::SQRT_2 - y }
                },
            ),
            // Same derivation as the `Scoop` arm of `inner_corner_boundary`.
            (CornerShape::Scoop, |r, w, y| {
                if y < w { r } else { ((r + w) * (r + w) - y * y).max(0.0).sqrt() }
            }),
            // Same derivation as the `Round` arm of `inner_corner_boundary`.
            (CornerShape::Round, |r, w, y| {
                if y < w { r } else { r - ((r - w) * (r - w) - (y - r) * (y - r)).max(0.0).sqrt() }
            }),
        ];
        for &(shape, closed_form) in cases {
            for r in [10.0f32, 40.0, 128.0] {
                for w in [1.0f32, r * 0.2, r * 0.6] {
                    let r2 = r - w;
                    let w = r - r2; // the value the function itself recovers from `r2`
                    let y_max = r + inner_corner_extra(shape, r, r2);
                    for i in 0..=20 {
                        let y = y_max * i as f32 / 20.0;
                        if (y - w).abs() < 1e-2 {
                            // Right at the miter/tangency threshold, either side is valid.
                            continue;
                        }
                        let got = super::inner_corner_boundary(shape, r, r2, y);
                        let want = closed_form(r, w, y);
                        assert!(
                            (got - want).abs() < 0.01 * r,
                            "{shape:?} r={r} w={w} y={y}: got {got}, want {want}"
                        );
                    }
                }
            }
        }
    }

    /// `Notch` and `Square` are exact miters: their offset is a two-piece step. Nothing
    /// shows through below `y = w`, where the row is nearer the straight edge than
    /// either wall's offset; above it the offset is constant, unlike the smooth shapes'
    /// bisection.
    #[test]
    fn inner_corner_boundary_matches_notch_and_square() {
        use crate::graphics::border_radius::CornerShape;
        for r in [10.0f32, 40.0, 100.0] {
            for w in [1.0f32, r * 0.3, r * 0.8] {
                let r2 = r - w;
                let w = r - r2; // the value the function itself recovers from `r2`
                for i in 0..=10 {
                    let y = r * i as f32 / 10.0;
                    if (y - w).abs() < 1e-2 {
                        // Right at the miter, where either side of the step is a valid answer.
                        continue;
                    }
                    let notch = super::inner_corner_boundary(CornerShape::Notch, r, r2, y);
                    let want_notch = if y < w { r } else { r + w };
                    assert!(
                        (notch - want_notch).abs() < 1e-3,
                        "Notch r={r} w={w} y={y}: got {notch}, want {want_notch}"
                    );
                    let square = super::inner_corner_boundary(CornerShape::Square, r, r2, y);
                    let want = if y < w { r } else { w };
                    assert!(
                        (square - want).abs() < 1e-3,
                        "Square r={r} w={w} y={y}: got {square}, want {want}"
                    );
                }
            }
        }
    }

    /// `y == r + inner_corner_extra(...)` is where the scan line renderer hands off to the
    /// border's flat width `w` (zero extra for shapes reaching `w` at `y == r` itself).
    /// Every shape but `Notch` must land on `w` there, or the fill boundary steps.
    ///
    /// A concave `Superellipse` has this same gap, uncovered here: `inner_corner_extra`
    /// doesn't widen its zone to match.
    #[test]
    fn inner_corner_boundary_meets_flat_border_at_r() {
        use crate::graphics::border_radius::{CornerShape, inner_corner_extra};
        let shapes = [
            CornerShape::Square,
            CornerShape::Bevel,
            CornerShape::Round,
            CornerShape::Scoop,
            CornerShape::Squircle,
            CornerShape::Superellipse(0.5),
            CornerShape::Superellipse(4.0),
        ];
        for shape in shapes {
            for r in [10.0f32, 40.0, 128.0] {
                // `superellipse_offset_x`'s bisection assumes monotonicity, which the module
                // doc only guarantees for `wn` well under 1; `r * 0.6` is wide enough to
                // break that for some `n`, independent of the gap this test pins.
                for w in [1.0f32, r * 0.2] {
                    let r2 = r - w;
                    let y = r + inner_corner_extra(shape, r, r2);
                    let got = super::inner_corner_boundary(shape, r, r2, y);
                    assert!(
                        (got - w).abs() < 0.01 * r,
                        "{shape:?} r={r} w={w}: got {got} at y={y}, want {w}"
                    );
                }
            }
        }
    }

    fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
        let d = (b.0 - a.0, b.1 - a.1);
        let len2 = d.0 * d.0 + d.1 * d.1;
        let t = if len2 > 0.0 {
            (((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let proj = (a.0 + d.0 * t, a.1 + d.1 * t);
        ((p.0 - proj.0).powi(2) + (p.1 - proj.1).powi(2)).sqrt()
    }

    /// Measures the perpendicular distance from the outer curve to the constructed inner
    /// curve by nearest-point-on-polyline, not by reusing `inner_corner_boundary`'s own
    /// offset math. Catches a "scaled about the wrong point" mistake the closed-form tests
    /// can't reach for `Squircle` and `Superellipse`, which have no closed form to check.
    #[test]
    fn inner_corner_boundary_is_a_uniform_perpendicular_offset() {
        use crate::graphics::border_radius::CornerShape;
        // `Notch` and `Square` step at `y == w`; bridging that jump linearly, as sampling
        // any other shape into a polyline does, would put spurious close points next to
        // the step.
        // `Bevel` and `Scoop` don't meet the edge tangentially, so their true offset runs
        // past `y == r`, which this polyline (sampled only up to `r`) can't reach either.
        // All three are covered exactly instead by `inner_corner_boundary_matches_closed_form`
        // and `..._matches_notch_and_square` above.
        let shapes = [
            CornerShape::Round,
            CornerShape::Squircle,
            CornerShape::Superellipse(-3.0),
            CornerShape::Superellipse(0.5),
            CornerShape::Superellipse(4.0),
        ];
        const SAMPLES: usize = 400;
        for shape in shapes {
            for r in [20.0f32, 80.0] {
                for w in [2.0f32, r * 0.3] {
                    let r2 = r - w;
                    let inner: Vec<(f32, f32)> = (0..=SAMPLES)
                        .map(|i| {
                            let y = r * i as f32 / SAMPLES as f32;
                            (super::inner_corner_boundary(shape, r, r2, y), y)
                        })
                        .collect();
                    for i in 1..40 {
                        // Skip the near-edge band, already checked by the closed-form tests
                        // above, and `y == w`, the boundary itself.
                        // Stay a further `w` short of `y == r`: on a concave curve, the true
                        // offset widens as it goes (the mirror of the convex case's `y == w`
                        // margin), so near `y == r` its nearest point has itself moved past
                        // `r`, beyond where the polyline below, sampled only up to `r`, can
                        // find it.
                        let y = w + (r - 2.0 * w) * i as f32 / 40.0;
                        let x = super::corner_boundary(shape, r, y);
                        let d = inner
                            .windows(2)
                            .map(|seg| distance_to_segment((x, y), seg[0], seg[1]))
                            .fold(f32::INFINITY, f32::min);
                        assert!(
                            (d - w).abs() < 0.02 * r,
                            "{shape:?} r={r} w={w} y={y}: perpendicular distance {d}, want {w}"
                        );
                    }
                }
            }
        }
    }
}
