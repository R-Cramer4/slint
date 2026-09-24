// Copyright © Klarälvdalens Datakonsult AB, a KDAB Group company , info@kdab.com, author Robin Cramer <robin.cramer@kdab.com>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

/*!
Paths of rectangles whose corners have a [`CornerShape`], for renderers that draw paths.
Renderers keep drawing a rectangle whose corners are all round with their own rounded
rectangle primitive; see [`CornerShapes::is_all_round`].
*/

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use num_traits::Float;

use super::BorderRadius;
use super::border_radius::{CornerShape, CornerShapes};
use super::corner_geometry::{
    BorderContours, corner_frames, corners_in_path_order, superellipse_at, superellipse_k,
    superellipse_n,
};
use euclid::num::Zero;
use lyon_path::math::{Point, Vector};
use lyon_path::traits::SvgPathBuilder;

/// The standard cubic-bezier approximation factor for a quarter circle.
const KAPPA: f32 = 0.552_284_7;

/// Largest deviation, in physical pixels, allowed between an emitted superellipse corner
/// and the true curve.
const SUPERELLIPSE_TOLERANCE: f32 = 0.05;

/// Deepest a span is halved by [`emit_superellipse`].
/// A high `|k|` needs depth, not breadth: its fillet is only a thousandth of `s` wide,
/// and bisection must reach that before the spans either side are accepted.
const MAX_SUPERELLIPSE_DEPTH: u32 = 12;

/// Cosine of the most a single cubic may turn through, here 30 degrees.
/// Sampling a span only bounds the error at the sampled points, so a sharply bending
/// span is split on turn alone — also what drives bisection down to a high `|k|`'s fillet.
const SUPERELLIPSE_TURN_COS: f32 = 0.866;

/// How far `v` lies off the curve, in units of `r`.
///
/// Measured to the point the curve reaches along the same ray, so it never reports less
/// than the true distance — never accepted on its own to fit a span.
fn superellipse_distance(k: f32, n: f32, v: Vector) -> f32 {
    let (p, q) = if k < 0. { (v.y, v.x) } else { (1. - v.x, 1. - v.y) };
    let total = p + q;
    let s = if total > 0. { (p / total).clamp(0., 1.) } else { 0.5 };
    (v - superellipse_at(k, n, s).0).length()
}

/// The two control points of the cubic that leaves `p0` along `d0`, arrives at `p1` along
/// `d1`, and passes through `mid` at its own halfway point.
///
/// Those three conditions pin down how far along the tangents the control points sit.
/// For a quarter circle they recover [`KAPPA`] exactly.
fn fit_cubic(p0: Vector, d0: Vector, p1: Vector, d1: Vector, mid: Vector) -> (Vector, Vector) {
    // The cubic's halfway point is `(p0 + 3*c0 + 3*c1 + p1) / 8`.
    let rhs = (mid * 8.0 - p0 * 4.0 - p1 * 4.0) / 3.0;
    let det = d1.x * d0.y - d0.x * d1.y;
    let (start, end) = if det.abs() < 1e-6 {
        // Parallel tangents mean a straight span and a singular system; evenly spaced
        // control points reproduce the line.
        let third = (p1 - p0).length() / 3.0;
        (third, third)
    } else {
        ((d1.x * rhs.y - d1.y * rhs.x) / det, (d0.x * rhs.y - d0.y * rhs.x) / det)
    };
    (p0 + d0 * start.max(0.0), p1 - d1 * end.max(0.0))
}

/// Draws the span of the corner between `s0` and `s1`, halving it until one cubic holds it
/// within `tolerance`. See [`superellipse_at`] for what `s` measures.
fn emit_superellipse_span(
    b: &mut impl SvgPathBuilder,
    to_point: &impl Fn(Vector) -> Point,
    k: f32,
    n: f32,
    tolerance: f32,
    s0: f32,
    s1: f32,
    depth: u32,
) {
    let (p0, d0) = superellipse_at(k, n, s0);
    let (p1, d1) = superellipse_at(k, n, s1);
    let middle = (s0 + s1) * 0.5;
    let (mid, _) = superellipse_at(k, n, middle);
    let (c0, c1) = fit_cubic(p0, d0, p1, d1, mid);

    let fits = d0.dot(d1) >= SUPERELLIPSE_TURN_COS
        && (1..8).all(|i| {
            let t = i as f32 / 8.0;
            let u = 1.0 - t;
            let on_cubic = p0 * (u * u * u)
                + c0 * (3.0 * u * u * t)
                + c1 * (3.0 * u * t * t)
                + p1 * (t * t * t);
            superellipse_distance(k, n, on_cubic) <= tolerance
        });
    if fits || depth == 0 {
        b.cubic_bezier_to(to_point(c0), to_point(c1), to_point(p1));
    } else {
        emit_superellipse_span(b, to_point, k, n, tolerance, s0, middle, depth - 1);
        emit_superellipse_span(b, to_point, k, n, tolerance, middle, s1, depth - 1);
    }
}

/// Draws a `Squircle` or `Superellipse` corner as a chain of cubic segments. See
/// [`emit_corner`] for the frame the corner is drawn in.
///
/// A superellipse has no closed-form Bezier, and no fixed segment schedule suits the
/// whole `k` range: past `|k| ≈ 2` the corner is two nearly straight runs either side of
/// a short fillet, so spacing evenly by length or turn starves one of the two. Instead
/// the span is halved until each cubic is within tolerance.
fn emit_superellipse(
    b: &mut impl SvgPathBuilder,
    tip: Point,
    e_in: Vector,
    e_out: Vector,
    r: f32,
    k: f32,
) {
    let n = superellipse_n(k);
    let to_point = |v: Vector| tip + e_in * (v.x * r) + e_out * (v.y * r);
    emit_superellipse_span(
        b,
        &to_point,
        k,
        n,
        SUPERELLIPSE_TOLERANCE / r,
        0.0,
        1.0,
        MAX_SUPERELLIPSE_DEPTH,
    );
}

/// Draws the corner at `tip`, from the current position (`tip + r * e_in`) to
/// `tip + r * e_out`. `e_in`/`e_out` are unit vectors along the incoming/outgoing
/// straight edges; the caller has already moved to `tip + r * e_in`.
fn emit_corner(
    b: &mut impl SvgPathBuilder,
    tip: Point,
    e_in: Vector,
    e_out: Vector,
    r: f32,
    shape: CornerShape,
) {
    if r <= 0.0 {
        return;
    }
    let tangent2 = tip + e_out * r;
    match shape {
        CornerShape::Square => {}
        CornerShape::Notch => {
            b.line_to(tip + e_in * r + e_out * r);
            b.line_to(tangent2);
        }
        CornerShape::Bevel => {
            b.line_to(tangent2);
        }
        // The arc is centered on `tip + r * e_in + r * e_out`, so it meets both straight
        // edges tangentially. `Scoop` below is the same arc centered on `tip` instead.
        CornerShape::Round => {
            b.cubic_bezier_to(
                tip + e_in * (r - r * KAPPA),
                tip + e_out * (r - r * KAPPA),
                tangent2,
            );
        }
        CornerShape::Scoop => {
            b.cubic_bezier_to(
                tip + e_in * r + e_out * (r * KAPPA),
                tip + e_in * (r * KAPPA) + e_out * r,
                tangent2,
            );
        }
        CornerShape::Squircle | CornerShape::Superellipse(_) => {
            emit_superellipse(b, tip, e_in, e_out, r, superellipse_k(shape).unwrap())
        }
    }
}

/// The curve's perpendicular offset, `dn` outward in units of `r` (negative moves inward),
/// at ray parameter `s`. See [`superellipse_at`] for what `s` measures.
fn superellipse_offset_point(k: f32, n: f32, dn: f32, s: f32) -> Vector {
    let (pos, tangent) = superellipse_at(k, n, s);
    // The tangent rotated 90 degrees, away from the material.
    pos + Vector::new(-tangent.y, tangent.x) * dn
}

/// Like [`emit_superellipse_span`], but fits cubics to the curve's offset.
/// The offset curve has no closed-form distance, so this measures against the offset point
/// at the same ray parameter rather than the true nearest point.
fn emit_superellipse_offset_span(
    b: &mut impl SvgPathBuilder,
    to_point: &impl Fn(Vector) -> Point,
    k: f32,
    n: f32,
    dn: f32,
    tolerance: f32,
    s0: f32,
    s1: f32,
    depth: u32,
) {
    let p0 = superellipse_offset_point(k, n, dn, s0);
    let p1 = superellipse_offset_point(k, n, dn, s1);
    let (_, d0) = superellipse_at(k, n, s0);
    let (_, d1) = superellipse_at(k, n, s1);
    let middle = (s0 + s1) * 0.5;
    let mid = superellipse_offset_point(k, n, dn, middle);
    let (c0, c1) = fit_cubic(p0, d0, p1, d1, mid);

    let fits = d0.dot(d1) >= SUPERELLIPSE_TURN_COS
        && (1..8).all(|i| {
            let t = i as f32 / 8.0;
            let s = s0 + (s1 - s0) * t;
            let u = 1.0 - t;
            let on_cubic = p0 * (u * u * u)
                + c0 * (3.0 * u * u * t)
                + c1 * (3.0 * u * t * t)
                + p1 * (t * t * t);
            (on_cubic - superellipse_offset_point(k, n, dn, s)).length() <= tolerance
        });
    if fits || depth == 0 {
        b.cubic_bezier_to(to_point(c0), to_point(c1), to_point(p1));
    } else {
        emit_superellipse_offset_span(b, to_point, k, n, dn, tolerance, s0, middle, depth - 1);
        emit_superellipse_offset_span(b, to_point, k, n, dn, tolerance, middle, s1, depth - 1);
    }
}

/// Where the offset curve crosses the corner's diagonal ahead of `s == 0.5`: past this
/// point a shrinking spread has outrun the curve's curvature, so the curve doubles back
/// over its mirror image and both halves meet here instead.
///
/// Returns `Some(0.5)` if the curve doesn't fold, and `None` once the fold swallows the
/// whole corner.
fn fold_trim(k: f32, n: f32, dn: f32) -> Option<f32> {
    // `x - y` starts at `1 - |dn|` and is `0` at `s == 0.5` by symmetry.
    let across = |s: f32| {
        let v = superellipse_offset_point(k, n, dn, s);
        v.x - v.y
    };
    if across(0.0) <= 0.0 {
        return None;
    }
    let (mut lo, mut hi) = (0.0f32, 0.5f32);
    for _ in 0..24 {
        let mid = (lo + hi) * 0.5;
        if across(mid) > 0.0 { lo = mid } else { hi = mid }
    }
    Some(hi)
}

/// Bisects `covered`, true up to one change-over and false past it, for that change-over
/// in `[0, limit]`.
///
/// Never evaluates `covered(0)`: the offset point there sits exactly on the edge it's
/// measured against, and rounding decides which side it lands on.
fn trim_at(limit: f32, covered: impl Fn(f32) -> bool) -> f32 {
    let (mut lo, mut hi) = (0.0f32, limit);
    for _ in 0..30 {
        let mid = (lo + hi) * 0.5;
        if covered(mid) { lo = mid } else { hi = mid }
    }
    hi
}

/// Where the offset curve clears the incoming edge and stays `|dn|` away from it.
/// The corner starts there, since the straight run already covers the rest.
///
/// Erosion sweeps a concave corner's curve past the edge, and growth folds an `n < 2`
/// corner over itself; both show up as points closer than `|dn|` to the edge's ray.
fn edge_trim(k: f32, n: f32, dn: f32, fold: f32) -> f32 {
    trim_at(fold, |s| {
        let v = superellipse_offset_point(k, n, dn, s);
        // The edge is a ray from `(1, 0)`; a point short of its end measures to that end.
        let d = if v.x >= 1.0 { v.y.abs() } else { (v - Vector::new(1.0, 0.0)).length() };
        d < dn.abs()
    })
}

/// The `superellipse(k)` parameter of the shapes whose spread is an offset curve rather
/// than the same shape at a larger radius.
fn offset_superellipse_k(shape: CornerShape) -> Option<f32> {
    match shape {
        CornerShape::Scoop => Some(-1.),
        _ => superellipse_k(shape),
    }
}

/// One corner of a shape spread outward by `spread`.
#[derive(Clone, Copy)]
struct SpreadCorner {
    /// The ungrown rectangle's corner.
    tip: Point,
    /// Unit vectors along the incoming and outgoing edges. See [`emit_corner`].
    e_in: Vector,
    e_out: Vector,
    spread: f32,
    kind: SpreadCornerKind,
}

#[derive(Clone, Copy)]
enum SpreadCornerKind {
    /// The same shape at the radius [`CornerShape::spread_scale`] gives.
    Scaled { shape: CornerShape, r: f32 },
    /// A `superellipse(k)` corner's offset curve over `[start, fold]` and its mirror
    /// `[1 - fold, 1 - start]`, joined to each edge by a [miter](SpreadCorner::miter).
    Offset { k: f32, n: f32, r: f32, start: f32, fold: f32 },
    /// Nothing of the corner survives the spread, so the two edges meet in a point.
    Gone,
}

impl SpreadCorner {
    /// The grown rectangle's corner.
    fn grown_tip(&self) -> Point {
        self.tip - (self.e_in + self.e_out) * self.spread
    }

    /// `v`, in units of `r` along the corner's two edges, as a point.
    fn point_at(&self, r: f32, v: Vector) -> Point {
        self.tip + self.e_in * (v.x * r) + self.e_out * (v.y * r)
    }

    /// The same corner reflected about its diagonal, so [`Self::end`] mirrors [`Self::start`].
    fn mirrored(&self) -> Self {
        Self { e_in: self.e_out, e_out: self.e_in, ..*self }
    }

    fn on_curve(&self, k: f32, n: f32, r: f32, start: f32) -> Point {
        self.point_at(r, superellipse_offset_point(k, n, self.spread / r, start))
    }

    /// Where the incoming edge's offset line meets the offset curve's tangent at `start`.
    ///
    /// A grown concave corner meets the edge at a right angle no offset reaches, so the
    /// miter keeps it sharp like CSS's `box-shadow` spread and `Notch`.
    /// For every other corner `start` is already on the line and comes back unchanged.
    fn miter(&self, k: f32, n: f32, r: f32, start: f32) -> Point {
        let from = self.on_curve(k, n, r, start);
        // The curve's tangent is parallel to the offset's until the offset folds, which
        // `edge_trim` keeps `start` clear of.
        let tangent = superellipse_at(k, n, start).1;
        let direction = self.e_in * tangent.x + self.e_out * tangent.y;
        let across = direction.dot(self.e_out);
        if across.abs() < 1.0e-3 {
            return from;
        }
        from - direction * ((from - self.tip).dot(self.e_out) + self.spread) / across
    }

    /// Where the corner takes over from the straight run along its incoming edge.
    fn start(&self) -> Point {
        match self.kind {
            SpreadCornerKind::Gone => self.grown_tip(),
            SpreadCornerKind::Scaled { r, .. } => self.grown_tip() + self.e_in * r,
            SpreadCornerKind::Offset { k, n, r, start, .. } => self.miter(k, n, r, start),
        }
    }

    /// Where the straight run along the outgoing edge takes over from the corner.
    fn end(&self) -> Point {
        self.mirrored().start()
    }

    /// Draws the corner from [`Self::start`], where the builder must already be, to
    /// [`Self::end`].
    fn emit(&self, b: &mut impl SvgPathBuilder) {
        match self.kind {
            SpreadCornerKind::Gone => {}
            SpreadCornerKind::Scaled { shape, r } => {
                emit_corner(b, self.grown_tip(), self.e_in, self.e_out, r, shape)
            }
            SpreadCornerKind::Offset { k, n, r, start, fold } => {
                let dn = self.spread / r;
                let to_point = |v: Vector| self.point_at(r, v);
                // Degenerate unless the corner is a grown concave one. See `miter`.
                b.line_to(self.on_curve(k, n, r, start));
                // The two halves meet at a point wherever `fold` cuts the curve short.
                for (s0, s1) in [(start, fold), (1.0 - fold, 1.0 - start)] {
                    emit_superellipse_offset_span(
                        b,
                        &to_point,
                        k,
                        n,
                        dn,
                        SUPERELLIPSE_TOLERANCE / r,
                        s0,
                        s1,
                        MAX_SUPERELLIPSE_DEPTH,
                    );
                }
                b.line_to(self.end());
            }
        }
    }
}

/// Resolves the corner at `grown_tip` of a shape spread outward by `spread`.
/// `r`, `shape`, and `max_r` describe the ungrown rectangle.
fn spread_corner(
    grown_tip: Point,
    e_in: Vector,
    e_out: Vector,
    r: f32,
    shape: CornerShape,
    spread: f32,
    max_r: f32,
) -> SpreadCorner {
    let corner =
        |kind| SpreadCorner { tip: grown_tip + (e_in + e_out) * spread, e_in, e_out, spread, kind };
    let r = if shape == CornerShape::Square { 0. } else { r.clamp(0., max_r) };
    if r <= 0. {
        // A corner with no radius stays sharp, per CSS's `box-shadow` spread.
        return corner(SpreadCornerKind::Gone);
    }
    let Some(k) = offset_superellipse_k(shape) else {
        return corner(SpreadCornerKind::Scaled {
            shape,
            r: (r + spread * shape.spread_scale()).clamp(0., (max_r + spread).max(0.)),
        });
    };
    let n = superellipse_n(k);
    let dn = spread / r;
    match fold_trim(k, n, dn).map(|fold| (edge_trim(k, n, dn, fold), fold)) {
        Some((start, fold)) if start < fold => {
            corner(SpreadCornerKind::Offset { k, n, r, start, fold })
        }
        // A concave corner grown until it closes over leaves its two walls, still `r` deep.
        _ if k < 0. && spread > 0. => {
            corner(SpreadCornerKind::Scaled { shape: CornerShape::Notch, r })
        }
        _ => corner(SpreadCornerKind::Gone),
    }
}

/// Builds the path of a rectangle with per-corner `radius` and `corner_shape`, grown
/// outward by `spread` to `rect`.
/// A negative `spread` shrinks it instead, for an inner shadow's hole.
///
/// Each point of the path is `spread` away from the ungrown shape, except at a miter.
/// Only for `Round` corners does [`rounded_rect_path`] at a scaled-up radius give the same.
pub fn spread_rounded_rect_path<U>(
    rect: euclid::default::Rect<f32>,
    radius: BorderRadius<f32, U>,
    corner_shape: CornerShapes,
    spread: f32,
) -> lyon_path::Path {
    let (width, height) = (rect.width() - 2. * spread, rect.height() - 2. * spread);
    let radius = radius.max(BorderRadius::zero()).fit_to_size(width, height);
    let max_r = (width.min(height) / 2.).max(0.);
    let frames = corner_frames(rect);
    let shapes = corners_in_path_order(radius, corner_shape);
    let corners: [SpreadCorner; 4] = core::array::from_fn(|i| {
        let ((tip, e_in, e_out), (r, shape)) = (frames[i], shapes[i]);
        spread_corner(tip, e_in, e_out, r, shape, spread, max_r)
    });

    let mut b = lyon_path::Path::builder().with_svg();
    b.move_to(corners[3].end());
    for corner in &corners {
        b.line_to(corner.start());
        corner.emit(&mut b);
    }
    b.close();
    b.build()
}

/// Builds the path of `rect` with per-corner `radius` and `corner_shape`.
/// A [`CornerShape::Square`] corner is sharp whatever its radius.
pub fn rounded_rect_path<U>(
    rect: euclid::default::Rect<f32>,
    radius: BorderRadius<f32, U>,
    corner_shape: CornerShapes,
) -> lyon_path::Path {
    let radius = radius.max(BorderRadius::zero()).fit_to_size(rect.width(), rect.height());
    let corners = corners_in_path_order(radius, corner_shape)
        .map(|(r, shape)| (if shape == CornerShape::Square { 0. } else { r }, shape));
    let frames = corner_frames(rect);

    let mut b = lyon_path::Path::builder().with_svg();
    let (tl_tip, _, tl_out) = frames[3];
    b.move_to(tl_tip + tl_out * corners[3].0);
    for ((tip, e_in, e_out), (r, shape)) in frames.into_iter().zip(corners) {
        b.line_to(tip + e_in * r);
        emit_corner(&mut b, tip, e_in, e_out, r, shape);
    }
    b.close();
    b.build()
}

/// Builds the closed path through `points`.
pub fn polygon_path(points: &[Point]) -> lyon_path::Path {
    let mut b = lyon_path::Path::builder();
    add_polygon(&mut b, points.iter().copied());
    b.build()
}

/// Builds the path of a border ring from its [`BorderContours`], with the inner contour
/// reversed so that filling with the nonzero rule paints only the ring.
pub fn border_path(contours: &BorderContours) -> lyon_path::Path {
    let mut b = lyon_path::Path::builder();
    add_polygon(&mut b, contours.outer.iter().copied());
    if let Some(inner) = &contours.inner {
        add_polygon(&mut b, inner.iter().rev().copied());
    }
    b.build()
}

fn add_polygon(b: &mut lyon_path::path::Builder, mut points: impl Iterator<Item = Point>) {
    let Some(first) = points.next() else { return };
    b.begin(first);
    for p in points {
        b.line_to(p);
    }
    b.end(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Samples the emitted path in the corner frame of the top-right corner of a `2r` square,
    /// keeping the samples that belong to that corner.
    fn emitted_corner(shape: CornerShape, r: f32) -> Vec<Vector> {
        let rect = euclid::default::Rect::new(Point::zero(), euclid::size2(2. * r, 2. * r));
        let path = rounded_rect_path::<()>(
            rect,
            BorderRadius::new_uniform(r),
            CornerShapes::new_uniform(shape),
        );
        let mut out = Vec::new();
        let mut push = |p: Point| {
            let local = Vector::new(2. * r - p.x, p.y);
            if local.x >= -0.001 && local.x <= r + 0.001 && local.y <= r + 0.001 {
                out.push(local);
            }
        };
        const SAMPLES: u32 = 128;
        for event in path.iter() {
            match event {
                lyon_path::Event::Begin { at } => push(at),
                lyon_path::Event::Line { from, to } => {
                    for i in 1..=SAMPLES {
                        push(from.lerp(to, i as f32 / SAMPLES as f32));
                    }
                }
                lyon_path::Event::Quadratic { to, .. } => push(to),
                lyon_path::Event::Cubic { from, ctrl1, ctrl2, to } => {
                    let curve = lyon_path::geom::CubicBezierSegment { from, ctrl1, ctrl2, to };
                    for i in 1..=SAMPLES {
                        push(curve.sample(i as f32 / SAMPLES as f32));
                    }
                }
                lyon_path::Event::End { .. } => {}
            }
        }
        out
    }

    /// How far `v`, in the corner frame, lies from the true corner of radius `r`, from the
    /// shape's own equation.
    fn distance_to_corner(shape: CornerShape, r: f32, v: Vector) -> f32 {
        let segment = |a: Vector, b: Vector| {
            let d = b - a;
            let t = ((v - a).dot(d) / d.square_length()).clamp(0., 1.);
            (v - (a + d * t)).length()
        };
        let edges = segment(Vector::new(r, 0.), Vector::new(r + 1., 0.))
            .min(segment(Vector::new(0., r), Vector::new(0., r + 1.)));
        let corner = match shape {
            CornerShape::Square => segment(Vector::zero(), Vector::new(r, 0.))
                .min(segment(Vector::zero(), Vector::new(0., r))),
            CornerShape::Notch => segment(Vector::new(r, 0.), Vector::new(r, r))
                .min(segment(Vector::new(r, r), Vector::new(0., r))),
            CornerShape::Bevel => segment(Vector::new(r, 0.), Vector::new(0., r)),
            CornerShape::Round => ((v - Vector::new(r, r)).length() - r).abs(),
            CornerShape::Scoop => (v.length() - r).abs(),
            _ => {
                let k = superellipse_k(shape).unwrap();
                superellipse_distance(k, superellipse_n(k), v / r) * r
            }
        };
        corner.min(edges)
    }

    #[test]
    fn corner_path_matches_the_shape() {
        let shapes = [
            CornerShape::Square,
            CornerShape::Notch,
            CornerShape::Bevel,
            CornerShape::Round,
            CornerShape::Scoop,
            CornerShape::Squircle,
            CornerShape::Superellipse(-4.),
            CornerShape::Superellipse(-1.),
            CornerShape::Superellipse(-0.5),
            CornerShape::Superellipse(0.),
            CornerShape::Superellipse(0.5),
            CornerShape::Superellipse(3.),
            CornerShape::Superellipse(6.),
            CornerShape::Superellipse(f32::NAN),
        ];
        for shape in shapes {
            for r in [4f32, 16., 64., 256.] {
                // `Round` and `Scoop` are a single Bezier per corner, whose error against a
                // quarter circle grows with the radius rather than being budgeted.
                let tolerance = SUPERELLIPSE_TOLERANCE.max(3.0e-4 * r);
                let deviation = emitted_corner(shape, r)
                    .into_iter()
                    .map(|v| distance_to_corner(shape, r, v))
                    .fold(0f32, f32::max);
                assert!(
                    deviation <= tolerance,
                    "{shape:?} at r={r}: deviation {deviation} exceeds {tolerance}"
                );
            }
        }
    }

    /// Flattens `path` into a polyline within `tolerance` of it.
    fn flatten(path: &lyon_path::Path, tolerance: f32) -> Vec<Point> {
        use lyon_path::iterator::PathIterator;
        path.iter()
            .flattened(tolerance)
            .flat_map(|event| match event {
                lyon_path::Event::Begin { at } => Some(at),
                lyon_path::Event::Line { to, .. } => Some(to),
                lyon_path::Event::End { first, .. } => Some(first),
                _ => None,
            })
            .collect()
    }

    fn distance_to_polyline(p: Point, line: &[Point]) -> f32 {
        line.windows(2)
            .map(|w| lyon_path::geom::LineSegment { from: w[0], to: w[1] }.distance_to_point(p))
            .fold(f32::MAX, f32::min)
    }

    /// Every point of a spread path is `spread` away from the ungrown shape, except at a
    /// miter, where two offset runs meet at a right angle and reach `spread * sqrt(2)`.
    #[test]
    fn spread_path_keeps_its_distance() {
        // Both the reference outline and the spread path are within `SUPERELLIPSE_TOLERANCE`
        // of their true curves.
        const TOLERANCE: f32 = 2. * SUPERELLIPSE_TOLERANCE;
        let ungrown = euclid::default::Rect::new(Point::zero(), euclid::size2(200., 160.));
        let shapes = [
            CornerShape::Square,
            CornerShape::Notch,
            CornerShape::Bevel,
            CornerShape::Round,
            CornerShape::Scoop,
            CornerShape::Squircle,
            CornerShape::Superellipse(-4.),
            CornerShape::Superellipse(-0.5),
            CornerShape::Superellipse(0.5),
            CornerShape::Superellipse(3.),
        ];
        for shape in shapes {
            for r in [8f32, 40.] {
                let radius = BorderRadius::<f32, ()>::new_uniform(r);
                let shapes = CornerShapes::new_uniform(shape);
                let reference = flatten(&rounded_rect_path(ungrown, radius, shapes), 0.001);
                for spread in [-16f32, -4., 4., 16.] {
                    let path = spread_rounded_rect_path(
                        ungrown.inflate(spread, spread),
                        radius,
                        shapes,
                        spread,
                    );
                    for p in flatten(&path, 0.01) {
                        let d = distance_to_polyline(p, &reference);
                        assert!(
                            d >= spread.abs() - TOLERANCE
                                && d <= spread.abs() * core::f32::consts::SQRT_2 + TOLERANCE,
                            "{shape:?} r={r} spread={spread}: {p:?} is {d} from the shape"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn border_path_reverses_the_inner_contour() {
        let contours = BorderContours {
            outer: [(0., 0.), (10., 0.), (10., 10.), (0., 10.)].map(Point::from).into(),
            inner: Some([(2., 2.), (8., 2.), (8., 8.), (2., 8.)].map(Point::from).into()),
        };
        let points: Vec<Point> = border_path(&contours)
            .iter()
            .filter_map(|event| match event {
                lyon_path::Event::Begin { at } => Some(at),
                lyon_path::Event::Line { to, .. } => Some(to),
                _ => None,
            })
            .collect();
        let inner = contours.inner.unwrap();
        assert_eq!(&points[..4], &contours.outer[..]);
        assert_eq!(points[4..].iter().rev().copied().collect::<Vec<_>>(), inner);
    }
}
